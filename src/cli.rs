use std::path::PathBuf;

use clap::Parser;

use crate::scanner::{ScanConfig, VcsMode};

#[derive(Parser, Debug)]
#[command(
    name = "tcloc",
    version,
    about = "Live treemap of code, by language. Inspired by cloc."
)]
pub struct Cli {
    /// Path to scan
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Use a VCS to enumerate files (git only). Counts tracked files.
    #[arg(long, value_name = "VCS")]
    pub vcs: Option<String>,

    /// Worker threads (default: logical CPUs)
    #[arg(short = 'j', long)]
    pub threads: Option<usize>,

    /// Skip files larger than this many MB
    #[arg(long, default_value_t = 100)]
    pub max_file_size: u64,

    /// Comma-separated directory names to exclude
    #[arg(long, value_delimiter = ',')]
    pub exclude_dir: Vec<String>,

    /// Comma-separated top-level directory names to include (relative to PATH)
    #[arg(long, value_delimiter = ',')]
    pub include_dir: Vec<String>,

    /// Comma-separated file extensions to exclude (no leading dot)
    #[arg(long, value_delimiter = ',')]
    pub exclude_ext: Vec<String>,

    /// Comma-separated file extensions to include (no leading dot)
    #[arg(long, value_delimiter = ',')]
    pub include_ext: Vec<String>,

    /// Comma-separated languages to exclude (e.g. "Rust,JSON")
    #[arg(long, value_delimiter = ',')]
    pub exclude_lang: Vec<String>,

    /// Comma-separated languages to include
    #[arg(long, value_delimiter = ',')]
    pub include_lang: Vec<String>,

    /// Follow symbolic links
    #[arg(short = 'L', long)]
    pub follow_links: bool,

    /// Include hidden files and directories
    #[arg(short = 'H', long)]
    pub hidden: bool,

    /// Do not honor .gitignore / .ignore files
    #[arg(short = 'I', long)]
    pub no_ignore: bool,

    /// Watch the scan root and apply incremental updates when files
    /// change. Off by default — opt in with --watch.
    #[arg(short = 'w', long)]
    pub watch: bool,

    /// Show live performance HUD and print a benchmark report on exit
    #[arg(short = 'b', long)]
    pub bench: bool,

    /// Auto-exit N milliseconds after the scan finishes (useful with --bench)
    #[arg(long)]
    pub auto_exit_ms: Option<u64>,
}

impl Cli {
    /// Resolve `--vcs` to a [`VcsMode`]. Exits the process on invalid input.
    pub fn vcs_mode(&self) -> VcsMode {
        match self.vcs.as_deref() {
            None => VcsMode::None,
            Some(v) if v.eq_ignore_ascii_case("git") => VcsMode::Git,
            Some(v) => {
                eprintln!("unsupported --vcs: {v}. only 'git' is supported");
                std::process::exit(2);
            }
        }
    }

    /// Default to logical CPU count, clamp to >= 1.
    pub fn thread_count(&self) -> usize {
        self.threads
            .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
            .unwrap_or(4)
            .max(1)
    }

    /// Build a [`ScanConfig`] suitable for [`crate::scanner::spawn`].
    pub fn scan_config(&self, root: PathBuf) -> ScanConfig {
        ScanConfig {
            root,
            vcs: self.vcs_mode(),
            threads: self.thread_count(),
            max_file_size: self.max_file_size.saturating_mul(1024 * 1024),
            exclude_dirs: self.exclude_dir.iter().cloned().collect(),
            include_dirs: self.include_dir.iter().cloned().collect(),
            exclude_exts: self
                .exclude_ext
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            include_exts: self
                .include_ext
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            exclude_langs: self.exclude_lang.iter().cloned().collect(),
            include_langs: self.include_lang.iter().cloned().collect(),
            follow_links: self.follow_links,
            hidden: self.hidden,
            no_ignore: self.no_ignore,
        }
    }
}
