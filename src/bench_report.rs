use std::time::{Duration, Instant};

use crate::alloc_track::AllocStats;
use crate::app::App;
use crate::format::{fmt_bytes, fmt_int};

pub fn print(
    app: &App,
    scan_started: Instant,
    threads: usize,
    vcs: Option<&str>,
    alloc: AllocStats,
) {
    let elapsed = app
        .finished_at
        .unwrap_or_else(Instant::now)
        .duration_since(scan_started);
    let elapsed_s = elapsed.as_secs_f64().max(1e-9);
    let files = app.total_files;
    let lines = app.total_lines;
    let bytes = app.total_bytes;

    let stats = app.bench.frame_stats();
    let avg_count_ns = if files > 0 {
        app.bench.total_count_nanos / files as u128
    } else {
        0
    };

    eprintln!();
    eprintln!("=== tcloc bench ===");
    eprintln!("path        : {}", app.root.display());
    eprintln!("vcs         : {}", vcs.unwrap_or("none"));
    eprintln!("threads     : {threads}");
    eprintln!();
    eprintln!("[scanner]");
    eprintln!(
        "  files     : {:>14}    lines : {:>14}",
        fmt_int(files),
        fmt_int(lines)
    );
    eprintln!(
        "  bytes     : {:>14}    elapsed: {:.3} s",
        fmt_bytes(bytes),
        elapsed_s,
    );
    eprintln!(
        "  files/s   : {:>14}    lines/s: {:>10}",
        fmt_int((files as f64 / elapsed_s) as u64),
        fmt_int((lines as f64 / elapsed_s) as u64),
    );
    eprintln!(
        "  bytes/s   : {:>14}/s",
        fmt_bytes((bytes as f64 / elapsed_s) as u64),
    );
    eprintln!(
        "  per-file  : avg {:.1} µs  min {:.1} µs  max {:.1} µs",
        avg_count_ns as f64 / 1000.0,
        app.bench.min_count_nanos as f64 / 1000.0,
        app.bench.max_count_nanos as f64 / 1000.0,
    );
    eprintln!();
    eprintln!("[tui]");
    eprintln!("  frames    : {}", app.bench.frames_rendered);
    if let Some(s) = stats {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let fps = 1.0 / s.avg.as_secs_f64().max(1e-9);
        eprintln!(
            "  frame     : avg {:.3} ms ({:.0} fps)  p50 {:.3}  p95 {:.3}  p99 {:.3}  max {:.3}",
            ms(s.avg),
            fps,
            ms(s.p50),
            ms(s.p95),
            ms(s.p99),
            ms(s.max),
        );
    }
    eprintln!(
        "  layout    : {:.3} ms (iters {}, excluded {}, tiles {})",
        app.bench.last_treemap_layout.as_secs_f64() * 1000.0,
        app.bench.last_treemap_iters,
        app.bench.last_layout_excluded,
        app.bench.last_tiles_drawn,
    );
    eprintln!(
        "  halfblock : {:.3} ms",
        app.bench.last_halfblock.as_secs_f64() * 1000.0
    );
    eprintln!(
        "  text      : {:.3} ms",
        app.bench.last_text_overlay.as_secs_f64() * 1000.0
    );
    eprintln!();
    eprintln!("[memory]");
    eprintln!(
        "  resident  : current {}  peak {}",
        fmt_bytes(alloc.current_bytes as u64),
        fmt_bytes(alloc.peak_bytes as u64),
    );
    eprintln!(
        "  process   : {} allocs, {} allocated",
        fmt_int(alloc.allocs),
        fmt_bytes(alloc.bytes_allocated),
    );
    eprintln!();
}
