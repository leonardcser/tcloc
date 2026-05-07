use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use ignore::{WalkBuilder, WalkState};

use crate::lang::{self, Lang};

#[derive(Debug, Clone, Copy)]
pub enum VcsMode {
    None,
    Git,
}

#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub root: PathBuf,
    pub vcs: VcsMode,
    pub threads: usize,
    pub max_file_size: u64,
    pub exclude_dirs: HashSet<String>,
    pub include_dirs: HashSet<String>,
    pub exclude_exts: HashSet<String>,
    pub include_exts: HashSet<String>,
    pub exclude_langs: HashSet<String>,
    pub include_langs: HashSet<String>,
    pub follow_links: bool,
    pub hidden: bool,
    pub no_ignore: bool,
}

pub enum ScanEvent {
    File {
        path: PathBuf,
        lang: Lang,
        lines: u64,
        bytes: u64,
        mtime: Option<SystemTime>,
        count_nanos: u64,
    },
    Done {
        at: std::time::Instant,
    },
}

pub fn spawn(cfg: ScanConfig, tx: Sender<ScanEvent>) {
    std::thread::spawn(move || {
        match cfg.vcs {
            VcsMode::Git => scan_git(&cfg, &tx),
            VcsMode::None => scan_walk(&cfg, &tx),
        }
        let _ = tx.send(ScanEvent::Done {
            at: std::time::Instant::now(),
        });
    });
}

fn scan_walk(cfg: &ScanConfig, tx: &Sender<ScanEvent>) {
    let mut builder = WalkBuilder::new(&cfg.root);
    builder
        .threads(cfg.threads)
        .follow_links(cfg.follow_links)
        .hidden(!cfg.hidden)
        .git_ignore(!cfg.no_ignore)
        .git_exclude(!cfg.no_ignore)
        .git_global(!cfg.no_ignore)
        .ignore(!cfg.no_ignore);

    let walker = builder.build_parallel();
    walker.run(|| {
        let tx = tx.clone();
        let cfg = cfg.clone();
        Box::new(move |result| {
            let entry = match result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if cfg.exclude_dirs.contains(name) {
                        return WalkState::Skip;
                    }
                    if entry.depth() == 1
                        && !cfg.include_dirs.is_empty()
                        && !cfg.include_dirs.contains(name)
                    {
                        return WalkState::Skip;
                    }
                }
                return WalkState::Continue;
            }
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                return WalkState::Continue;
            }
            if process_file(path, &cfg, &tx).is_err() {
                return WalkState::Quit;
            }
            WalkState::Continue
        })
    });
}

fn scan_git(cfg: &ScanConfig, tx: &Sender<ScanEvent>) {
    let paths = match collect_git_paths(&cfg.root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tcloc: --vcs git: {}", e);
            return;
        }
    };

    let paths: Vec<PathBuf> = if cfg.include_dirs.is_empty() {
        paths
    } else {
        paths
            .into_iter()
            .filter(|p| {
                let Ok(rel) = p.strip_prefix(&cfg.root) else {
                    return true;
                };
                let mut comps = rel.components();
                let Some(first) = comps.next() else {
                    return true;
                };
                if comps.next().is_none() {
                    return true;
                }
                first
                    .as_os_str()
                    .to_str()
                    .map(|s| cfg.include_dirs.contains(s))
                    .unwrap_or(false)
            })
            .collect()
    };

    let paths = Arc::new(paths);
    let cursor = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(cfg.threads);
    for _ in 0..cfg.threads {
        let paths = Arc::clone(&paths);
        let cursor = Arc::clone(&cursor);
        let cfg = cfg.clone();
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            loop {
                let i = cursor.fetch_add(1, Ordering::Relaxed);
                if i >= paths.len() {
                    return;
                }
                let path = &paths[i];
                if process_file(path, &cfg, &tx).is_err() {
                    return;
                }
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}

fn collect_git_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .arg("-z")
        .arg("--cached")
        .arg("--others")
        .arg("--exclude-standard")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut v = Vec::new();
    for chunk in out.stdout.split(|&b| b == 0) {
        if chunk.is_empty() {
            continue;
        }
        let s = match std::str::from_utf8(chunk) {
            Ok(s) => s,
            Err(_) => continue,
        };
        v.push(root.join(s));
    }
    Ok(v)
}

fn process_file(
    path: &Path,
    cfg: &ScanConfig,
    tx: &Sender<ScanEvent>,
) -> Result<(), std::sync::mpsc::SendError<ScanEvent>> {
    let _g = crate::perf::begin("scan.process_file");
    let Some(lang) = classify(path, cfg) else {
        return Ok(());
    };

    let t0 = std::time::Instant::now();
    let Some((lines, bytes, mtime)) = count(path, cfg.max_file_size) else {
        return Ok(());
    };
    let count_nanos = t0.elapsed().as_nanos() as u64;

    tx.send(ScanEvent::File {
        path: path.to_path_buf(),
        lang,
        lines,
        bytes,
        mtime,
        count_nanos,
    })
}

thread_local! {
    static READ_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Open + line-count a single file. Public so the watcher can re-use
/// the exact same logic for incremental updates.
pub fn count(path: &Path, max_size: u64) -> Option<(u64, u64, Option<SystemTime>)> {
    let _g = crate::perf::begin("scan.count");
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let len = meta.len();
    if max_size > 0 && len > max_size {
        return None;
    }
    let mtime = meta.modified().ok();
    if len == 0 {
        return Some((0, 0, mtime));
    }
    READ_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        buf.clear();
        buf.reserve(len as usize);
        use std::io::Read;
        let mut f = file;
        f.read_to_end(&mut buf).ok()?;
        count_bytes(&buf).map(|lines| (lines, buf.len() as u64, mtime))
    })
}

/// Same filter logic the scanner applies inside `process_file`, exposed
/// so the watcher can reject events on files the initial scan would
/// have skipped (wrong extension, excluded language, etc.). Returns the
/// detected language if the path passes every filter.
pub fn classify(path: &Path, cfg: &ScanConfig) -> Option<Lang> {
    if !cfg.include_dirs.is_empty()
        && let Ok(rel) = path.strip_prefix(&cfg.root)
    {
        let mut comps = rel.components();
        if let Some(first) = comps.next()
            && comps.next().is_some()
        {
            let name = first.as_os_str().to_str()?;
            if !cfg.include_dirs.contains(name) {
                return None;
            }
        }
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_ascii_lowercase();
        if !cfg.include_exts.is_empty() && !cfg.include_exts.contains(&ext_lower) {
            return None;
        }
        if cfg.exclude_exts.contains(&ext_lower) {
            return None;
        }
    } else if !cfg.include_exts.is_empty() {
        return None;
    }
    let lang = lang::detect(path)?;
    if !cfg.include_langs.is_empty() && !cfg.include_langs.contains(lang.0) {
        return None;
    }
    if cfg.exclude_langs.contains(lang.0) {
        return None;
    }
    Some(lang)
}

fn count_bytes(buf: &[u8]) -> Option<u64> {
    let _g = crate::perf::begin("scan.count_bytes");
    let head = &buf[..buf.len().min(8192)];
    if memchr::memchr(0, head).is_some() {
        return None;
    }
    let mut lines = memchr::memchr_iter(b'\n', buf).count() as u64;
    if !buf.is_empty() && *buf.last().unwrap() != b'\n' {
        lines += 1;
    }
    Some(lines)
}
