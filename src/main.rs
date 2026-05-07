mod alloc_track;
mod app;
mod bench_report;
mod bitmap_font;
mod cli;
mod format;
mod lang;
mod perf;
mod scanner;
mod tree;
mod treemap;
mod ui;
mod watcher;

use std::io::{self, BufWriter, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::alloc_track::CountingAllocator;
use crate::app::{App, NavDir, TileTarget};
use crate::cli::Cli;
use crate::scanner::ScanEvent;
use crate::watcher::{CachedMeta, MetaCache, WatchEvent};

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let root = cli.path.canonicalize().unwrap_or_else(|_| cli.path.clone());
    let threads = cli.thread_count();
    let bench_enabled = cli.bench;
    let watch_enabled = cli.watch;
    let auto_exit_ms = cli.auto_exit_ms;
    let bench_vcs = cli.vcs.clone();
    let scan_cfg = cli.scan_config(root.clone());

    let (tx, rx) = mpsc::channel::<ScanEvent>();
    scanner::spawn(scan_cfg.clone(), tx);

    // Watch channel + shared metadata cache. The watcher is spawned
    // after the initial scan finishes (see `run`) so it doesn't race
    // the walker on the same tree. The cache is filled here as scan
    // events arrive, so by the time the watcher comes online it
    // already knows every file's `(mtime, size)` and can short-circuit
    // spurious events from the first keystroke.
    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>();
    let meta_cache: MetaCache =
        std::sync::Arc::new(std::sync::Mutex::new(hashbrown::HashMap::new()));

    if bench_enabled {
        perf::enable();
    }

    let mut app = App::new(root);
    app.bench.enabled = bench_enabled;
    // Mark watch mode at startup so the status badge skips the "DONE"
    // intermediate state and goes straight from SCANNING to WATCHING.
    app.watching = watch_enabled;

    let scan_started = Instant::now();
    let alloc_baseline = alloc_track::snapshot();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Wrap stdout in a BufWriter so crossterm's many small per-cell writes
    // are coalesced into one syscall per frame instead of one per cell.
    // 256 KiB is enough to hold a full 240×60-cell diff worth of escape
    // sequences without ever flushing mid-frame; ratatui calls flush()
    // exactly once at the end of `terminal.draw`.
    let buffered = io::BufWriter::with_capacity(256 * 1024, stdout);
    let mut terminal = Terminal::new(CrosstermBackend::new(buffered))?;

    let res = run(
        &mut terminal,
        app,
        rx,
        watch_rx,
        watch_tx,
        meta_cache,
        scan_cfg,
        watch_enabled,
        auto_exit_ms,
    );

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    let final_app = res?;
    if bench_enabled {
        let alloc_delta = alloc_track::delta(alloc_baseline, alloc_track::snapshot());
        bench_report::print(
            &final_app,
            scan_started,
            threads,
            bench_vcs.as_deref(),
            alloc_delta,
        );
        perf::print_summary();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run(
    terminal: &mut Terminal<CrosstermBackend<BufWriter<Stdout>>>,
    mut app: App,
    rx: mpsc::Receiver<ScanEvent>,
    watch_rx: mpsc::Receiver<WatchEvent>,
    watch_tx: mpsc::Sender<WatchEvent>,
    meta_cache: MetaCache,
    scan_cfg: scanner::ScanConfig,
    watch_enabled: bool,
    auto_exit_ms: Option<u64>,
) -> io::Result<App> {
    let base_interval = Duration::from_millis(33);
    let drain_budget = Duration::from_millis(8);
    let mut frame_interval = base_interval;
    let mut last_draw = Instant::now() - frame_interval;
    // Distinguish input-driven redraws (preempt the cadence for snappy
    // feedback) from scan-driven redraws (respect frame_interval so a
    // huge terminal isn't pinned at 100% emitting escape sequences).
    let mut input_dirty = true;
    let mut scan_dirty = false;
    let mut done_at: Option<Instant> = None;
    let mut pending_input_at: Option<Instant> = None;
    // Live debouncer handle. Held for the program lifetime — dropping
    // it stops the watcher. `None` until the initial scan finishes.
    let mut _watcher_handle: Option<
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
    > = None;
    // Distinct from `_watcher_handle.is_none()`: we tried once and
    // gave up. Without this we'd retry on every subsequent drain that
    // returns true, spamming logs and CPU.
    let mut watcher_tried = false;

    loop {
        if let (Some(ms), Some(t)) = (auto_exit_ms, done_at)
            && t.elapsed() >= Duration::from_millis(ms)
        {
            return Ok(app);
        }

        if drain_events(&rx, &mut app, &meta_cache, &mut done_at, drain_budget) {
            scan_dirty = true;
            // Bring the watcher online the first time we see a Done,
            // but only when --watch is set. The cache is already
            // populated with every file's (mtime, size), so editor
            // saves on day-old files don't trigger redundant
            // re-counts.
            if watch_enabled && app.done && !watcher_tried {
                watcher_tried = true;
                _watcher_handle = watcher::spawn(
                    scan_cfg.clone(),
                    watch_tx.clone(),
                    std::sync::Arc::clone(&meta_cache),
                );
                // If the watcher fails to spawn (e.g. inotify limits on
                // Linux), drop the WATCHING badge back to DONE so the
                // user isn't lied to about live updates being on.
                if _watcher_handle.is_none() {
                    app.watching = false;
                }
            }
        }
        if drain_watch_events(&watch_rx, &mut app, &meta_cache) {
            scan_dirty = true;
        }
        // While any pulse is mid-fade, keep redrawing on the cadence so
        // the animation actually animates. `gc_pulses` keeps the map
        // bounded under sustained file churn.
        if app.has_active_pulses() {
            scan_dirty = true;
        }

        let interval_elapsed = last_draw.elapsed() >= frame_interval;
        if input_dirty || (scan_dirty && interval_elapsed) {
            let t0 = Instant::now();
            draw_frame(terminal, &mut app, pending_input_at.take())?;
            // Adaptive cadence: never schedule the next frame closer than
            // ~1.2× the last frame's wall time (capped at 200ms).
            let adaptive = t0
                .elapsed()
                .saturating_mul(6)
                .checked_div(5)
                .unwrap_or(base_interval);
            frame_interval = adaptive.max(base_interval).min(Duration::from_millis(200));
            last_draw = Instant::now();
            input_dirty = false;
            scan_dirty = false;
        }

        let poll_for = frame_interval
            .checked_sub(last_draw.elapsed())
            .unwrap_or(Duration::from_millis(1))
            .max(Duration::from_millis(1));
        if event::poll(poll_for)? {
            let received_at = Instant::now();
            match handle_event(event::read()?, &mut app) {
                EventOutcome::Quit => return Ok(app),
                EventOutcome::Redraw => {
                    input_dirty = true;
                    if pending_input_at.is_none() {
                        pending_input_at = Some(received_at);
                    }
                }
                EventOutcome::OpenEditor(path) => {
                    open_in_editor(terminal, &path)?;
                    input_dirty = true;
                }
                EventOutcome::Ignored => {}
            }
        }
    }
}

/// Suspend the TUI, hand the terminal to `$EDITOR` (or `vi`), then restore
/// alternate screen + raw mode so the app can keep running. The editor
/// inherits stdio so it works for full-screen editors like vim/nvim.
fn open_in_editor(
    terminal: &mut Terminal<CrosstermBackend<BufWriter<Stdout>>>,
    path: &std::path::Path,
) -> io::Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let mut parts = editor.split_whitespace();
    let Some(cmd) = parts.next() else {
        return Ok(());
    };
    let extra: Vec<&str> = parts.collect();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;

    let _ = std::process::Command::new(cmd)
        .args(&extra)
        .arg(path)
        .status();

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;
    Ok(())
}

/// Pull scan events out of the channel until empty or budget exhausted.
/// Returns true if any state-changing event was received (caller redraws).
fn drain_events(
    rx: &mpsc::Receiver<ScanEvent>,
    app: &mut App,
    meta: &MetaCache,
    done_at: &mut Option<Instant>,
    budget: Duration,
) -> bool {
    let start = Instant::now();
    let mut changed = false;
    loop {
        match rx.try_recv() {
            Ok(ScanEvent::File {
                path,
                lang,
                lines,
                bytes,
                mtime,
                count_nanos,
            }) => {
                if let Ok(mut m) = meta.lock() {
                    m.insert(path.clone(), CachedMeta { mtime, bytes });
                }
                app.record(path, lang, lines, bytes, count_nanos);
                changed = true;
            }
            Ok(ScanEvent::Done { at }) => {
                app.mark_done(at);
                if done_at.is_none() {
                    *done_at = Some(Instant::now());
                }
                changed = true;
            }
            Err(mpsc::TryRecvError::Empty) => return changed,
            Err(mpsc::TryRecvError::Disconnected) => {
                app.mark_done(Instant::now());
                return changed;
            }
        }
        if start.elapsed() > budget {
            return changed;
        }
    }
}

/// Drain whatever the watcher has produced. Each batch ends with a
/// `BatchDone` marker so the caller can collapse "20 files just got
/// touched by a save" into a single redraw without flashing.
fn drain_watch_events(rx: &mpsc::Receiver<WatchEvent>, app: &mut App, meta: &MetaCache) -> bool {
    let mut changed = false;
    loop {
        match rx.try_recv() {
            Ok(WatchEvent::Upsert {
                path,
                lang,
                lines,
                bytes,
                mtime,
            }) => {
                if let Ok(mut m) = meta.lock() {
                    m.insert(path.clone(), CachedMeta { mtime, bytes });
                }
                app.watch_upsert(path, lang, lines, bytes);
                changed = true;
            }
            Ok(WatchEvent::Remove { path }) => {
                if let Ok(mut m) = meta.lock() {
                    m.remove(&path);
                }
                app.watch_remove(&path);
                changed = true;
            }
            Ok(WatchEvent::BatchDone) => {
                // Sentinel only: lets us bound the loop per batch
                // rather than starving input handling on huge bursts.
                return changed;
            }
            Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => {
                return changed;
            }
        }
    }
}

fn draw_frame(
    terminal: &mut Terminal<CrosstermBackend<BufWriter<Stdout>>>,
    app: &mut App,
    input_at: Option<Instant>,
) -> io::Result<()> {
    let frame_t0 = Instant::now();
    let alloc_pre = if app.bench.enabled {
        Some(alloc_track::snapshot())
    } else {
        None
    };
    let _g = perf::begin("loop.terminal_draw");
    terminal.draw(|f| ui::render(f, app))?;
    drop(_g);
    let frame_d = frame_t0.elapsed();
    if let Some(pre) = alloc_pre {
        let post = alloc_track::snapshot();
        let bytes_delta = post.bytes_allocated.saturating_sub(pre.bytes_allocated);
        let alloc_delta = post.allocs.saturating_sub(pre.allocs);
        app.bench.last_full_render = frame_d;
        app.bench.record_frame(frame_d, bytes_delta, alloc_delta);
    }
    if let Some(t) = input_at {
        let lat = t.elapsed();
        app.bench.last_input_latency = Some(lat);
        if app.bench.enabled {
            perf::record_value("ui.input_to_draw", lat.as_micros() as u64);
        }
    }
    Ok(())
}

enum EventOutcome {
    Quit,
    Redraw,
    Ignored,
    OpenEditor(std::path::PathBuf),
}

fn handle_event(ev: Event, app: &mut App) -> EventOutcome {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(key, app),
        Event::Mouse(me) => handle_mouse(me, app),
        Event::Resize(_, _) => {
            // Tile rects are sized in cells, so a resize invalidates the
            // cached layout. Items themselves don't change.
            app.last_tiles.clear();
            EventOutcome::Redraw
        }
        _ => EventOutcome::Ignored,
    }
}

fn handle_key(key: event::KeyEvent, app: &mut App) -> EventOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Char('q')
        || (ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')))
    {
        return EventOutcome::Quit;
    }
    match key.code {
        KeyCode::Esc => {
            if !app.up() && app.selected.is_some() {
                app.selected = None;
            }
        }
        KeyCode::Tab => app.toggle_view(),
        KeyCode::Backspace => {
            app.up();
        }
        KeyCode::Enter => app.enter_selected(),
        KeyCode::Char('o') => {
            if let Some(TileTarget::File(p)) = &app.selected {
                return EventOutcome::OpenEditor(p.clone());
            }
            return EventOutcome::Ignored;
        }
        KeyCode::Left | KeyCode::Char('h') => app.navigate(NavDir::Left),
        KeyCode::Down | KeyCode::Char('j') => app.navigate(NavDir::Down),
        KeyCode::Up | KeyCode::Char('k') => app.navigate(NavDir::Up),
        KeyCode::Right | KeyCode::Char('l') => app.navigate(NavDir::Right),
        _ => return EventOutcome::Ignored,
    }
    EventOutcome::Redraw
}

fn handle_mouse(me: event::MouseEvent, app: &mut App) -> EventOutcome {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(TileTarget::Folder(path)) = app.hit(me.column, me.row).cloned() {
                app.current_path = path;
                app.items_dirty = true;
                return EventOutcome::Redraw;
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            app.up();
            return EventOutcome::Redraw;
        }
        MouseEventKind::ScrollUp if rect_contains(app.legend_rect, me.column, me.row) => {
            app.legend_scroll = app.legend_scroll.saturating_sub(1);
            return EventOutcome::Redraw;
        }
        MouseEventKind::ScrollDown if rect_contains(app.legend_rect, me.column, me.row) => {
            app.legend_scroll = (app.legend_scroll + 1).min(app.legend_max_scroll);
            return EventOutcome::Redraw;
        }
        _ => {}
    }
    EventOutcome::Ignored
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}
