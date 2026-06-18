mod app;
mod bench_report;
mod bitmap_font;
mod cli;
mod format;
mod lang;
mod scanner;
mod tree;
mod treemap;
mod ui;
mod watcher;

use std::io::{self, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use smelt_term::{Rect, Surface, SuspendScreen, TerminalSession};

use crate::app::{App, NavDir, TileTarget};
use crate::cli::Cli;
use crate::scanner::ScanEvent;
use crate::watcher::{CachedMeta, MetaCache, WatchEvent};
use smelt_perf::alloc::Counting;

#[global_allocator]
static GLOBAL: Counting = Counting;

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

    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>();
    let meta_cache: MetaCache =
        std::sync::Arc::new(std::sync::Mutex::new(hashbrown::HashMap::new()));

    if bench_enabled {
        smelt_perf::perf::enable();
        smelt_perf::alloc::enable();
    }

    let mut app = App::new(root);
    app.bench.enabled = bench_enabled;
    app.watching = watch_enabled;

    let scan_started = Instant::now();
    let alloc_baseline = smelt_perf::alloc::snapshot();

    let mut term = TerminalSession::builder()
        .buffer_capacity(256 * 1024)
        .hide_cursor(false)
        .enter_stdout()?;
    let (term_w, term_h) = term.size()?;
    let mut ui = Surface::new(term_w, term_h);

    let final_app = run(
        &mut ui,
        &mut term,
        app,
        rx,
        watch_rx,
        watch_tx,
        meta_cache,
        scan_cfg,
        watch_enabled,
        auto_exit_ms,
    )?;
    if bench_enabled {
        let alloc_delta = smelt_perf::alloc::delta(alloc_baseline, smelt_perf::alloc::snapshot());
        bench_report::print(
            &final_app,
            scan_started,
            threads,
            bench_vcs.as_deref(),
            alloc_delta,
        );
        smelt_perf::perf::print_summary();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run<W: Write>(
    ui: &mut Surface,
    term: &mut TerminalSession<W>,
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
    let mut input_dirty = true;
    let mut scan_dirty = false;
    let mut done_at: Option<Instant> = None;
    let mut pending_input_at: Option<Instant> = None;
    let mut _watcher_handle: Option<
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
    > = None;
    let mut watcher_tried = false;

    loop {
        if let (Some(ms), Some(t)) = (auto_exit_ms, done_at)
            && t.elapsed() >= Duration::from_millis(ms)
        {
            return Ok(app);
        }

        if drain_events(&rx, &mut app, &meta_cache, &mut done_at, drain_budget) {
            scan_dirty = true;
            if watch_enabled && app.done && !watcher_tried {
                watcher_tried = true;
                _watcher_handle = watcher::spawn(
                    scan_cfg.clone(),
                    watch_tx.clone(),
                    std::sync::Arc::clone(&meta_cache),
                );
                if _watcher_handle.is_none() {
                    app.watching = false;
                }
            }
        }
        if drain_watch_events(&watch_rx, &mut app, &meta_cache) {
            scan_dirty = true;
        }
        if app.has_active_pulses() {
            scan_dirty = true;
        }

        let interval_elapsed = last_draw.elapsed() >= frame_interval;
        if input_dirty || (scan_dirty && interval_elapsed) {
            let t0 = Instant::now();
            draw_frame(ui, term.writer(), &mut app, pending_input_at.take())?;
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
            // Drain everything that's already queued. During a fast
            // resize drag the terminal queues many Resize events; we
            // keep only the latest size and drop intermediates so
            // smelt-term doesn't full-flush the screen for every pixel
            // of motion. Non-resize events fall through unchanged.
            let mut events = vec![event::read()?];
            while event::poll(Duration::ZERO)? {
                events.push(event::read()?);
            }
            let received_at = Instant::now();
            let last_resize_idx = events
                .iter()
                .rposition(|e| matches!(e, Event::Resize(_, _)));
            for (i, ev) in events.into_iter().enumerate() {
                if matches!(ev, Event::Resize(_, _)) && Some(i) != last_resize_idx {
                    continue;
                }
                match handle_event(ev, ui, &mut app) {
                    EventOutcome::Quit => return Ok(app),
                    EventOutcome::Redraw => {
                        input_dirty = true;
                        if pending_input_at.is_none() {
                            pending_input_at = Some(received_at);
                        }
                    }
                    EventOutcome::OpenEditor(path) => {
                        open_in_editor(ui, term, &path)?;
                        input_dirty = true;
                    }
                    EventOutcome::Ignored => {}
                }
            }
        }
    }
}

/// Suspend the TUI, hand the terminal to `$EDITOR` (or `vi`), then restore
/// alternate screen + raw mode so the app can keep running. The editor
/// inherits stdio so it works for full-screen editors like vim/nvim.
fn open_in_editor<W: Write>(
    ui: &mut Surface,
    term: &mut TerminalSession<W>,
    path: &std::path::Path,
) -> io::Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let mut parts = editor.split_whitespace();
    let Some(cmd) = parts.next() else {
        return Ok(());
    };
    let extra: Vec<&str> = parts.collect();

    term.suspend_with(SuspendScreen::LeaveAlternate, || {
        let _ = std::process::Command::new(cmd)
            .args(&extra)
            .arg(path)
            .status();
    });

    // Force the next frame to repaint everything — the editor wiped the
    // alt screen, so smelt-ui's diff-against-previous would no-op the
    // unchanged cells and leave a blank screen behind.
    ui.force_redraw();
    Ok(())
}

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
                return changed;
            }
            Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => {
                return changed;
            }
        }
    }
}

fn draw_frame<W: Write>(
    ui: &mut Surface,
    writer: &mut W,
    app: &mut App,
    input_at: Option<Instant>,
) -> io::Result<()> {
    let frame_t0 = Instant::now();
    let alloc_pre = if app.bench.enabled {
        Some(smelt_perf::alloc::snapshot())
    } else {
        None
    };
    let _g = smelt_perf::perf::begin("loop.terminal_draw");
    crate::ui::render(ui, app, writer)?;
    drop(_g);
    let frame_d = frame_t0.elapsed();
    if let Some(pre) = alloc_pre {
        let post = smelt_perf::alloc::snapshot();
        let bytes_delta = post.bytes_allocated.saturating_sub(pre.bytes_allocated);
        let alloc_delta = post.allocs.saturating_sub(pre.allocs);
        app.bench.last_full_render = frame_d;
        app.bench.record_frame(frame_d, bytes_delta, alloc_delta);
    }
    if let Some(t) = input_at {
        let lat = t.elapsed();
        app.bench.last_input_latency = Some(lat);
        if app.bench.enabled {
            smelt_perf::perf::record_value("ui.input_to_draw", lat.as_micros() as u64);
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

fn handle_event(ev: Event, ui: &mut Surface, app: &mut App) -> EventOutcome {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(key, app),
        Event::Mouse(me) => handle_mouse(me, app),
        Event::Resize(w, h) => {
            ui.set_terminal_size(w, h);
            // Resize invalidates tile rects → drop hit regions and
            // force the nested layout cache to rebuild.
            app.last_tiles.clear();
            app.mark_layout_dirty();
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
                app.mark_layout_dirty();
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
    r.contains(y, x)
}
