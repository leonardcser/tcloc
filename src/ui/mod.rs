mod color;
mod items;
mod legend;
mod map;

use std::io::Write;

use smelt_term::grid::{Color, GridSlice, Style};
use smelt_term::{Border, Constraint, LayoutTree, Line, PaintId, Span, Surface};

use crate::app::{App, View};
use crate::format::{fmt_bytes_short, fmt_compact, truncate_left};

use self::legend::render_legend;
use self::map::render_treemap;

// Paint-leaf identifiers shared between the layout tree builder and
// the dispatch closure handed to `Surface::render`.
pub(super) const PAINT_HEADER: PaintId = PaintId(1);
pub(super) const PAINT_TREEMAP: PaintId = PaintId(2);
pub(super) const PAINT_LEGEND: PaintId = PaintId(3);
pub(super) const PAINT_FOOTER: PaintId = PaintId(4);
pub(super) const PAINT_BENCH: PaintId = PaintId(5);

pub fn render<W: Write>(ui: &mut Surface, app: &mut App, w: &mut W) -> std::io::Result<()> {
    let _g = smelt_perf::perf::begin("ui.render");
    app.gc_pulses();

    // Build the splits layout for this frame. Header (2 = 1 content +
    // 1 border) → body (fill) → optional bench HUD (1) → footer (1).
    // Body horizontally splits into treemap (fill) | legend (34).
    // Chrome (border + title) attaches to leaves directly via
    // `with_border` / `with_title`; the renderer auto-wraps the leaf
    // and feeds each render callback the inset slice — no manual
    // border drawing or rect math on the consumer side.
    let view_label = match app.view {
        View::Tree => " tree ",
        View::Files => " files ",
        View::Nested => " nested ",
    };
    let treemap_title = Line::new()
        .push(Span::styled(view_label, Style::new().fg(Color::White)))
        .push(Span::styled(
            "(area = lines, color = language) ",
            Style::new().fg(Color::DarkGrey),
        ));
    {
        let _p = smelt_perf::perf::begin("ui.layout");
        let mut items = vec![
            (
                Constraint::Length(2),
                LayoutTree::leaf(PAINT_HEADER).with_border(Border::single().bottom(())),
            ),
            (
                Constraint::Fill,
                LayoutTree::hbox(vec![
                    (
                        Constraint::Fill,
                        LayoutTree::leaf(PAINT_TREEMAP)
                            .with_border(Border::SINGLE)
                            .with_title(treemap_title),
                    ),
                    (
                        // 33 cells of content (3 + 11 + 6 + 8 + 5) + 2 for
                        // the side borders.
                        Constraint::Length(35),
                        LayoutTree::leaf(PAINT_LEGEND)
                            .with_border(Border::SINGLE)
                            .with_title(" languages "),
                    ),
                ]),
            ),
        ];
        if app.bench.enabled {
            items.push((Constraint::Length(1), LayoutTree::leaf(PAINT_BENCH)));
        }
        items.push((Constraint::Length(1), LayoutTree::leaf(PAINT_FOOTER)));
        ui.set_layout(LayoutTree::vbox(items));
    }

    let _p = smelt_perf::perf::begin("ui.render_flush");
    ui.render(w, |id, slice| match id {
        PAINT_HEADER => render_header(slice, app),
        PAINT_TREEMAP => render_treemap(slice, app),
        PAINT_LEGEND => render_legend(slice, app),
        PAINT_FOOTER => render_footer(slice, app),
        PAINT_BENCH => render_bench_hud(slice, app),
        _ => {}
    })
    .map(|_| ())
}

pub(super) fn render_header(slice: &mut GridSlice<'_>, app: &App) {
    let _g = smelt_perf::perf::begin("ui.header");
    let (status_text, status_bg) = if !app.done {
        (" SCANNING ", Color::Yellow)
    } else if app.watching {
        (" WATCHING ", Color::Red)
    } else {
        (" DONE ", Color::Green)
    };
    let stat_items = [
        format!("{} files", fmt_compact(app.total_files)),
        format!("{} lines", fmt_compact(app.total_lines)),
        fmt_bytes_short(app.total_bytes),
        format!("{:.1}s", app.elapsed_secs()),
    ];

    let w = slice.width();

    // Left: status badge + breadcrumb on row 0.
    let badge_style = Style::new().fg(Color::Black).bg(status_bg).bold();
    let mut col: u16 = 0;
    col = put_str_clip(slice, col, 0, status_text, badge_style);
    col = put_str_clip(slice, col, 0, " ", Style::default());
    let (root, zoom) = app.breadcrumb_parts();
    col = put_str_clip(slice, col, 0, &root, Style::new().fg(Color::White));
    let _ = put_str_clip(slice, col, 0, &zoom, Style::new().fg(Color::DarkGrey));

    // Right: stats, dot-separated, right-aligned. Compute width first so
    // we can pin it to the trailing edge.
    let stats_width: usize = stat_items.iter().map(|s| s.chars().count()).sum::<usize>()
        + 3 * stat_items.len().saturating_sub(1);
    if stats_width as u16 > w {
        return;
    }
    let mut col = w - stats_width as u16;
    for (i, item) in stat_items.iter().enumerate() {
        if i > 0 {
            col = put_str_clip(slice, col, 0, " · ", Style::new().fg(Color::DarkGrey));
        }
        col = put_str_clip(slice, col, 0, item, Style::new().fg(Color::Grey));
    }
}

pub(super) fn render_footer(slice: &mut GridSlice<'_>, app: &App) {
    let _g = smelt_perf::perf::begin("ui.footer");
    let path = app
        .last_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let path = truncate_left(&path, slice.width().saturating_sub(2) as usize);
    let key = Style::new().fg(Color::Yellow);
    let dim = Style::new().fg(Color::DarkGrey);
    let mut col = 0u16;
    col = put_str_clip(slice, col, 0, "hjkl/↑↓←→", key);
    col = put_str_clip(slice, col, 0, " select  ", dim);
    col = put_str_clip(slice, col, 0, "⏎", key);
    col = put_str_clip(slice, col, 0, " zoom  ", dim);
    col = put_str_clip(slice, col, 0, "esc", key);
    col = put_str_clip(slice, col, 0, " up  ", dim);
    col = put_str_clip(slice, col, 0, "tab", key);
    col = put_str_clip(slice, col, 0, " view  ", dim);
    col = put_str_clip(slice, col, 0, "o", key);
    col = put_str_clip(slice, col, 0, " open  ", dim);
    col = put_str_clip(slice, col, 0, "q", key);
    col = put_str_clip(slice, col, 0, " quit  ", dim);
    let _ = put_str_clip(slice, col, 0, &path, dim);
}

pub(super) fn render_bench_hud(slice: &mut GridSlice<'_>, app: &App) {
    let _g = smelt_perf::perf::begin("ui.bench_hud");
    let elapsed = app.elapsed_secs().max(0.001);
    let files_per_s = app.total_files as f64 / elapsed;
    let lines_per_s = app.total_lines as f64 / elapsed;
    let mb_per_s = (app.total_bytes as f64 / 1024.0 / 1024.0) / elapsed;
    let stats = app.bench.frame_stats();
    let frame_avg = stats.map(|s| s.avg).unwrap_or_default();
    let frame_p95 = stats.map(|s| s.p95).unwrap_or_default();
    let fps = if frame_avg.as_secs_f64() > 0.0 {
        1.0 / frame_avg.as_secs_f64()
    } else {
        0.0
    };
    let avg_count = if app.total_files > 0 {
        app.bench.total_count_nanos / app.total_files as u128
    } else {
        0
    };
    let input_lat_ms = app
        .bench
        .last_input_latency
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    let mut col = 0u16;
    col = put_str_clip(
        slice,
        col,
        0,
        "BENCH ",
        Style::new().fg(Color::Magenta).bold(),
    );
    col = put_str_clip(
        slice,
        col,
        0,
        &format!(
            "frame {:.2}ms (p95 {:.2}ms, {:.0} fps)  ",
            frame_avg.as_secs_f64() * 1000.0,
            frame_p95.as_secs_f64() * 1000.0,
            fps,
        ),
        Style::new().fg(Color::White),
    );
    col = put_str_clip(
        slice,
        col,
        0,
        &format!("input→draw {input_lat_ms:.2}ms  "),
        Style::new().fg(Color::White),
    );
    col = put_str_clip(
        slice,
        col,
        0,
        &format!(
            "layout {:.2}ms iters {} drawn {}  ",
            app.bench.last_treemap_layout.as_secs_f64() * 1000.0,
            app.bench.last_treemap_iters,
            app.bench.last_tiles_drawn,
        ),
        Style::new().fg(Color::Cyan),
    );
    col = put_str_clip(
        slice,
        col,
        0,
        &format!(
            "hb {:.2}ms tx {:.2}ms  ",
            app.bench.last_halfblock.as_secs_f64() * 1000.0,
            app.bench.last_text_overlay.as_secs_f64() * 1000.0,
        ),
        Style::new().fg(Color::Cyan),
    );
    col = put_str_clip(
        slice,
        col,
        0,
        &format!(
            "scan {:.0} f/s {:.1} M ln/s {:.1} MB/s  per-file {:.0}µs  ",
            files_per_s,
            lines_per_s / 1e6,
            mb_per_s,
            avg_count as f64 / 1000.0,
        ),
        Style::new().fg(Color::Yellow),
    );
    let _ = put_str_clip(
        slice,
        col,
        0,
        &format!(
            "alloc {}/{}",
            app.bench.last_frame_allocs,
            fmt_bytes_short(app.bench.last_frame_alloc_bytes),
        ),
        Style::new().fg(Color::Green),
    );
}

/// Slice-local `put_str` returning the next column. Stops at the right
/// edge so callers can chain spans without re-checking bounds.
fn put_str_clip(slice: &mut GridSlice<'_>, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let mut col = x;
    let w = slice.width();
    for ch in text.chars() {
        if col >= w {
            break;
        }
        slice.set(col, y, ch, style);
        col += 1;
    }
    col
}
