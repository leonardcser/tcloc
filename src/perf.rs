use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::alloc_track;

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn samples() -> &'static Mutex<HashMap<&'static str, Vec<Duration>>> {
    static S: OnceLock<Mutex<HashMap<&'static str, Vec<Duration>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

type AllocSamples = Mutex<HashMap<&'static str, Vec<(u64, u64)>>>;

fn alloc_samples() -> &'static AllocSamples {
    static A: OnceLock<AllocSamples> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

fn value_samples() -> &'static Mutex<HashMap<&'static str, Vec<u64>>> {
    static V: OnceLock<Mutex<HashMap<&'static str, Vec<u64>>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a raw numeric sample under `label`. Useful for non-duration
/// metrics like input-to-draw latency in microseconds, byte counts, etc.
pub fn record_value(label: &'static str, value: u64) {
    if !enabled() {
        return;
    }
    if let Ok(mut m) = value_samples().lock() {
        m.entry(label).or_default().push(value);
    }
}

pub fn begin(label: &'static str) -> Option<Guard> {
    if !enabled() {
        return None;
    }
    Some(Guard {
        label,
        start: Instant::now(),
        allocs_start: alloc_track::snapshot(),
    })
}

pub struct Guard {
    label: &'static str,
    start: Instant,
    allocs_start: alloc_track::AllocStats,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let dur = self.start.elapsed();
        if let Ok(mut s) = samples().lock() {
            s.entry(self.label).or_default().push(dur);
        }
        let end = alloc_track::snapshot();
        let dc = end.allocs.saturating_sub(self.allocs_start.allocs);
        let db = end
            .bytes_allocated
            .saturating_sub(self.allocs_start.bytes_allocated);
        if let Ok(mut m) = alloc_samples().lock() {
            m.entry(self.label).or_default().push((dc, db));
        }
    }
}

const TABLE_WIDTH: usize = 115;

pub fn print_summary() {
    if !enabled() {
        return;
    }
    let map = samples().lock().unwrap();
    if map.is_empty() {
        return;
    }
    let mut groups: Vec<(&'static str, Vec<Duration>)> =
        map.iter().map(|(k, v)| (*k, v.clone())).collect();
    drop(map);
    groups.sort_by(|a, b| {
        let ta: Duration = a.1.iter().sum();
        let tb: Duration = b.1.iter().sum();
        tb.cmp(&ta)
    });
    let max_total: Duration = groups
        .iter()
        .map(|(_, ds)| ds.iter().sum::<Duration>())
        .max()
        .unwrap_or_default();

    let bar = "─".repeat(TABLE_WIDTH);
    let title = "── perf ";
    let title_bar = format!(
        "{}{}",
        title,
        "─".repeat(TABLE_WIDTH - title.chars().count())
    );
    eprintln!("\n{}", title_bar);
    print_header("function", &bar);
    for (label, mut durs) in groups {
        durs.sort();
        let total: Duration = durs.iter().sum();
        let avg = total / durs.len() as u32;
        let row = format_row(label, &durs, total, avg, fmt_dur);
        eprintln!("{}", colorize_row(&row, total, max_total));
    }
    eprintln!("{}", bar);

    let alloc_map = alloc_samples().lock().unwrap();
    if !alloc_map.is_empty() {
        let mut agroups: Vec<(&'static str, Vec<(u64, u64)>)> =
            alloc_map.iter().map(|(k, v)| (*k, v.clone())).collect();
        drop(alloc_map);
        agroups.sort_by(|a, b| {
            let ta: u64 = a.1.iter().map(|(_, b)| *b).sum();
            let tb: u64 = b.1.iter().map(|(_, b)| *b).sum();
            tb.cmp(&ta)
        });
        print_header("allocs", &bar);
        for (label, samples) in agroups {
            let mut counts: Vec<u64> = samples.iter().map(|(c, _)| *c).collect();
            let mut bytes: Vec<u64> = samples.iter().map(|(_, b)| *b).collect();
            counts.sort();
            bytes.sort();
            let total_bytes: u64 = bytes.iter().sum();
            let avg_bytes = if !bytes.is_empty() {
                total_bytes / bytes.len() as u64
            } else {
                0
            };
            let total_count: u64 = counts.iter().sum();
            let avg_count = if !counts.is_empty() {
                total_count / counts.len() as u64
            } else {
                0
            };
            eprintln!(
                "{}",
                format_row(
                    &format!("{label}  (n)"),
                    &counts,
                    total_count,
                    avg_count,
                    |v| v.to_string(),
                )
            );
            eprintln!(
                "{}",
                format_row(
                    &format!("{label}  (bytes)"),
                    &bytes,
                    total_bytes,
                    avg_bytes,
                    fmt_bytes,
                )
            );
        }
        eprintln!("{}", bar);
    } else {
        drop(alloc_map);
    }

    let value_map = value_samples().lock().unwrap();
    if !value_map.is_empty() {
        let mut groups: Vec<(&'static str, Vec<u64>)> =
            value_map.iter().map(|(k, v)| (*k, v.clone())).collect();
        drop(value_map);
        groups.sort_by_key(|(k, _)| *k);
        print_header("value (µs)", &bar);
        for (label, mut vs) in groups {
            vs.sort();
            let total: u64 = vs.iter().sum();
            let avg = if vs.is_empty() {
                0
            } else {
                total / vs.len() as u64
            };
            eprintln!("{}", format_row(label, &vs, total, avg, |v| format!("{v}")));
        }
        eprintln!("{}", bar);
    } else {
        drop(value_map);
    }
}

fn print_header(first: &str, bar: &str) {
    eprintln!(
        "{:<40} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        first, "count", "total", "avg", "p50", "p95", "p99", "max"
    );
    eprintln!("{}", bar);
}

fn format_row<T, F>(label: &str, samples: &[T], total: T, avg: T, fmt: F) -> String
where
    T: Copy,
    F: Fn(T) -> String,
{
    let count = samples.len();
    let pct = |p: usize| -> T {
        let idx = ((count * p) / 100).min(count - 1);
        samples[idx]
    };
    let max = *samples.last().unwrap();
    format!(
        "{:<40} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        label,
        count,
        fmt(total),
        fmt(avg),
        fmt(pct(50)),
        fmt(pct(95)),
        fmt(pct(99)),
        fmt(max),
    )
}

fn colorize_row(row: &str, total: Duration, max_total: Duration) -> String {
    let code = severity_color(total, max_total);
    format!("\x1b[{}m{}\x1b[0m", code, row)
}

fn severity_color(total: Duration, max_total: Duration) -> &'static str {
    let t = total.as_secs_f64();
    let m = max_total.as_secs_f64().max(1e-9);
    let ratio = (1.0 + t * 1000.0).ln() / (1.0 + m * 1000.0).ln();
    let ratio = ratio.clamp(0.0, 1.0);
    match ratio {
        r if r >= 0.85 => "1;91",
        r if r >= 0.65 => "91",
        r if r >= 0.45 => "33",
        r if r >= 0.25 => "36",
        r if r >= 0.10 => "37",
        _ => "2;37",
    }
}

fn fmt_dur(d: Duration) -> String {
    let us = d.as_micros();
    if us < 1_000 {
        format!("{}µs", us)
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{}B", n)
    } else if n < 1024 * 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{:.2}MB", n as f64 / (1024.0 * 1024.0))
    }
}
