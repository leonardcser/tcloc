//! Filesystem watcher that drives incremental updates after the initial
//! scan finishes.
//!
//! Pipeline:
//!   notify-debouncer-full (per-path coalescing, ~100 ms tick)
//!     → filter thread (drop events the scan would have ignored,
//!                      stat the path, skip if (mtime, size) matches
//!                      the cached metadata we already counted)
//!     → worker pool (re-count file lines for upserts)
//!     → main loop (apply to App via `watch_upsert` / `watch_remove`)
//!
//! The mtime-skip is the big perf lever for large repos: most events
//! fire on saves that don't actually change content (vim atomic-saves
//! touch the path twice per save) so we want to bail out before paying
//! for a re-read.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, SystemTime};

use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, new_debouncer};

use crate::lang::Lang;
use crate::scanner::{self, ScanConfig};

/// Update produced by the watcher pipeline, applied by the main loop.
pub enum WatchEvent {
    Upsert {
        path: PathBuf,
        lang: Lang,
        lines: u64,
        bytes: u64,
        mtime: Option<SystemTime>,
    },
    Remove {
        path: PathBuf,
    },
    /// One debounce batch finished. Lets the main loop trigger a single
    /// redraw per batch even if the batch contained dozens of upserts.
    BatchDone,
}

/// Snapshot of what we already counted for a given path, used by the
/// filter thread to decide if a notify event needs a full re-count.
#[derive(Clone, Copy)]
pub struct CachedMeta {
    pub mtime: Option<SystemTime>,
    pub bytes: u64,
}

/// Shared `path → (mtime, bytes)` cache the worker threads consult to
/// decide if an event needs a re-count. Main thread populates it as
/// scan/watch events flow through; workers only read.
pub type MetaCache = Arc<std::sync::Mutex<hashbrown::HashMap<PathBuf, CachedMeta>>>;

/// Spawn the watcher. Returns the underlying debouncer (kept alive for
/// the lifetime of the app — dropping it stops the watch). Errors here
/// (e.g. inotify limits on Linux) are non-fatal: we log and let the app
/// continue without live updates.
pub fn spawn(
    cfg: ScanConfig,
    out: Sender<WatchEvent>,
    meta: MetaCache,
) -> Option<
    notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
> {
    let root = cfg.root.clone();
    let (raw_tx, raw_rx) = channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(
        // `timeout` (1st arg) is the debounce window — how long to wait
        // for further events on the same path before firing. `tick_rate`
        // (2nd arg) is the scheduler's check interval. 300 ms window
        // collapses editor save-sequences (write, rename, chmod) into
        // one update; 100 ms tick keeps wall-clock latency snappy.
        Duration::from_millis(300),
        Some(Duration::from_millis(100)),
        move |res| {
            let _ = raw_tx.send(res);
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tcloc: watcher disabled — failed to create watcher: {e}");
            return None;
        }
    };
    if let Err(e) = debouncer.watch(&root, RecursiveMode::Recursive) {
        eprintln!(
            "tcloc: watcher disabled — failed to watch {}: {e}",
            root.display()
        );
        return None;
    }

    // Drain raw events on a dedicated thread so notify's internal buffer
    // never fills. Each batch is fanned out across `cfg.threads` workers
    // (re-using the scan thread budget) to keep wall-clock latency low
    // even when a single `git checkout` lands tens of thousands of
    // events at once.
    let cfg_arc = Arc::new(cfg);
    std::thread::spawn(move || {
        for batch in raw_rx {
            let events = match batch {
                Ok(events) => events,
                Err(errs) => {
                    for e in errs {
                        eprintln!("tcloc: watcher: {e}");
                    }
                    continue;
                }
            };
            handle_batch(&events, &cfg_arc, &meta, &out);
            let _ = out.send(WatchEvent::BatchDone);
        }
    });

    Some(debouncer)
}

fn handle_batch(
    events: &[notify_debouncer_full::DebouncedEvent],
    cfg: &Arc<ScanConfig>,
    meta: &MetaCache,
    out: &Sender<WatchEvent>,
) {
    // Bucket events by path so we only handle each path once per batch.
    // (Each `DebouncedEvent` already represents one logical change but
    // notify can still emit, say, a Modify and a Metadata for the same
    // path in the same batch.)
    let mut work: hashbrown::HashMap<PathBuf, EventKind> = hashbrown::HashMap::new();
    for ev in events {
        for path in &ev.paths {
            // Folder-level events (created/removed dirs) are handled
            // implicitly via per-file events — notify recursive mode
            // walks newly-created dirs for us. Dir-removed events
            // generate per-file Remove events for the contents.
            if path.is_dir() {
                continue;
            }
            // Last write wins, except: a Remove always trumps an
            // earlier Modify, since the file no longer exists.
            let entry = work.entry(path.clone()).or_insert(ev.kind);
            if matches!(ev.kind, EventKind::Remove(_)) {
                *entry = ev.kind;
            }
        }
    }

    // Fan out across the worker budget; bounded chunks so each thread
    // gets a reasonable share without per-path channel overhead.
    let paths: Vec<(PathBuf, EventKind)> = work.into_iter().collect();
    if paths.is_empty() {
        return;
    }
    let workers = cfg.threads.max(1).min(paths.len());
    let cursor = Arc::new(AtomicUsize::new(0));
    let paths = Arc::new(paths);
    std::thread::scope(|s| {
        for _ in 0..workers {
            let cfg = Arc::clone(cfg);
            let cursor = Arc::clone(&cursor);
            let paths = Arc::clone(&paths);
            let out = out.clone();
            let meta = Arc::clone(meta);
            s.spawn(move || {
                loop {
                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                    if i >= paths.len() {
                        return;
                    }
                    let (path, kind) = &paths[i];
                    process(path, kind, &cfg, &meta, &out);
                }
            });
        }
    });
}

fn process(
    path: &Path,
    kind: &EventKind,
    cfg: &ScanConfig,
    meta: &MetaCache,
    out: &Sender<WatchEvent>,
) {
    let _g = smelt_perf::perf::begin("watch.process");

    if matches!(kind, EventKind::Remove(_)) {
        let _ = out.send(WatchEvent::Remove {
            path: path.to_path_buf(),
        });
        return;
    }

    // Filter-parity with the initial scan: ext / lang / size / hidden /
    // gitignore matching. We don't replicate gitignore here — that lives
    // in the `ignore` walker — so a `.gitignore`d file that sneaks
    // through (e.g. user disabled `--no-ignore`) is treated like any
    // other file. Acceptable: the scanner respects `cfg.no_ignore` too.
    let Some(lang) = scanner::classify(path, cfg) else {
        return;
    };

    // Mtime+size short-circuit. Stat is ~1µs vs counting which can be
    // milliseconds on a big file; skipping spurious events is the
    // single biggest win on a 1M-file repo under save-storms.
    let prev = meta.lock().ok().and_then(|m| m.get(path).copied());
    if let Some(prev) = prev
        && let Ok(stat) = std::fs::metadata(path)
        && stat.modified().ok() == prev.mtime
        && stat.len() == prev.bytes
    {
        return;
    }

    let Some((lines, bytes, mtime)) = scanner::count(path, cfg.max_file_size) else {
        // File vanished between the event and the count: treat as remove.
        let _ = out.send(WatchEvent::Remove {
            path: path.to_path_buf(),
        });
        return;
    };
    let _ = out.send(WatchEvent::Upsert {
        path: path.to_path_buf(),
        lang,
        lines,
        bytes,
        mtime,
    });
}
