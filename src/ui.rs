use std::cell::RefCell;
use std::io::Write;

use smelt_term::grid::{Color, GridSlice, Style};
use smelt_term::layout::BorderStyle;
use smelt_term::{Border, Constraint, LayoutTree, Line, PaintId, Rect, Span, Surface};

use crate::app::{App, TileTarget, View};
use crate::bitmap_font;
use crate::format::{fmt_bytes_short, fmt_compact, fmt_int, fmt_pct, truncate, truncate_left};
use crate::lang;
use crate::tree::{FolderNode, Node};
use crate::treemap::{self, Item};

// Paint-leaf identifiers shared between the layout tree builder and
// the dispatch closure handed to `Surface::render`.
const PAINT_HEADER: PaintId = PaintId(1);
const PAINT_TREEMAP: PaintId = PaintId(2);
const PAINT_LEGEND: PaintId = PaintId(3);
const PAINT_FOOTER: PaintId = PaintId(4);
const PAINT_BENCH: PaintId = PaintId(5);

const GAP_SUBCELLS: u16 = 1;
const MIN_TILE_SUBCELLS: u16 = 3;
const MAX_LAYOUT_PASSES: u32 = 16;
const BITMAP_MIN_TERMINAL_W: u16 = 144;
const BITMAP_MIN_TERMINAL_H: u16 = 44;

fn bitmap_enabled(area: Rect) -> bool {
    area.width >= BITMAP_MIN_TERMINAL_W && area.height >= BITMAP_MIN_TERMINAL_H
}

fn tile_color(base: Color, selected: bool, pulse: f32) -> Color {
    let mut c = base;
    if selected {
        c = brighten(c, 0.4);
    }
    if pulse > 0.0 {
        c = brighten(c, pulse as f64);
    }
    c
}

const SCALE_MIN_AREA_CELLS: [(u16, u16); bitmap_font::MAX_SCALE as usize] =
    [(96, 30), (264, 80), (312, 92)];
const LABEL_MAX_TILE_FRAC_NUM: u16 = 3;
const LABEL_MAX_TILE_FRAC_DEN: u16 = 5;

fn max_label_scale(area: Rect) -> u16 {
    (1..=bitmap_font::MAX_SCALE)
        .rev()
        .find(|&s| {
            let (w, h) = SCALE_MIN_AREA_CELLS[(s - 1) as usize];
            area.width >= w && area.height >= h
        })
        .unwrap_or(0)
}

const NESTED_BAND_PAD_SUBROWS: u16 = 1;

fn pick_label_scale(name: &str, tile_w: u16, tile_h: u16, max_scale: u16) -> Option<u16> {
    if name.is_empty() || !name.is_ascii() || max_scale == 0 {
        return None;
    }
    let pad_w = tile_w.saturating_sub(2);
    let pad_h = tile_h.saturating_sub(2);
    let frac_w =
        ((tile_w as u32 * LABEL_MAX_TILE_FRAC_NUM as u32) / LABEL_MAX_TILE_FRAC_DEN as u32) as u16;
    let frac_h =
        ((tile_h as u32 * LABEL_MAX_TILE_FRAC_NUM as u32) / LABEL_MAX_TILE_FRAC_DEN as u32) as u16;
    (1..=max_scale).rev().find(|&s| {
        let lw = bitmap_font::label_width(name, s);
        let lh = bitmap_font::label_height(s);
        if s == 1 {
            lw <= pad_w && lh <= pad_h
        } else {
            lw <= pad_w.min(frac_w) && lh <= pad_h.min(frac_h)
        }
    })
}

thread_local! {
    static ITEMS_BUF: RefCell<Vec<TileItem>> = const { RefCell::new(Vec::new()) };
    static GRID_BUF: RefCell<Vec<Option<Color>>> = const { RefCell::new(Vec::new()) };
    static LAYOUT_SORTED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static LAYOUT_EXCLUDED: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    static LAYOUT_TM_ITEMS: RefCell<Vec<Item<usize>>> = const { RefCell::new(Vec::new()) };
    static VISIBLE_BUF: RefCell<Vec<(Rect, usize)>> = const { RefCell::new(Vec::new()) };
    static NESTED_BUF: RefCell<Vec<NestedNode>> = const { RefCell::new(Vec::new()) };
    static SCALED_PAINTED_BUF: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
}

const NESTED_INNER_PAD: u16 = 1;

struct NestedNode {
    rect: Rect,
    color: Color,
    target: TileTarget,
    name: String,
    subtitle1: String,
    subtitle2: String,
    is_folder: bool,
    label_rows: u16,
    bitmap_scale: Option<u16>,
}

#[derive(Clone)]
struct TileItem {
    value: u64,
    color: Color,
    name: String,
    subtitle1: String,
    subtitle2: String,
    target: TileTarget,
}

// ── top-level layout / dispatch ─────────────────────────────────────────────

pub fn render<W: Write>(ui: &mut Surface, app: &mut App, w: &mut W) -> std::io::Result<()> {
    let _g = crate::perf::begin("ui.render");
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
    let mut items = vec![
        (
            Constraint::Length(2),
            LayoutTree::leaf(PAINT_HEADER).with_border(Border::bottom(BorderStyle::Single)),
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

    ui.render(w, |id, slice, _ctx| match id {
        PAINT_HEADER => render_header(slice, app),
        PAINT_TREEMAP => render_treemap(slice, app),
        PAINT_LEGEND => render_legend(slice, app),
        PAINT_FOOTER => render_footer(slice, app),
        PAINT_BENCH => render_bench_hud(slice, app),
        _ => {}
    })
}

// ── chrome (header / footer / bench HUD) ────────────────────────────────────

fn render_header(slice: &mut GridSlice<'_>, app: &App) {
    let _g = crate::perf::begin("ui.header");
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

fn render_footer(slice: &mut GridSlice<'_>, app: &App) {
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

fn render_bench_hud(slice: &mut GridSlice<'_>, app: &App) {
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

// ── treemap ─────────────────────────────────────────────────────────────────

fn render_treemap(slice: &mut GridSlice<'_>, app: &mut App) {
    let _g = crate::perf::begin("ui.treemap");
    app.last_tiles.clear();

    if slice.width() == 0 || slice.height() == 0 {
        return;
    }
    let inner = slice.screen_rect();

    if matches!(app.view, View::Nested) {
        render_nested(slice, inner, app);
    } else {
        render_flat(slice, inner, app);
    }
}

/// Flat treemap path used by the Tree and Files views: every tile is a
/// sibling competing for the same rect. `inner` is in absolute screen
/// coordinates; we use `slice.cell_mut` with slice-local coords for
/// the half-block fill.
fn render_flat(slice: &mut GridSlice<'_>, inner: Rect, app: &mut App) {
    ITEMS_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        if app.items_dirty {
            buf.clear();
            build_items_into(app, &mut buf);
            app.items_dirty = false;
        }
        if buf.is_empty() {
            return;
        }

        let tiles = layout_tiles(&buf, inner, app);
        if tiles.is_empty() {
            return;
        }

        let cols = inner.width as usize;
        let sub_rows = (inner.height as usize) * 2;
        GRID_BUF.with(|g| {
            let mut grid = g.borrow_mut();
            grid.clear();
            grid.resize(cols * sub_rows, None);
            VISIBLE_BUF.with(|v| {
                let mut visible = v.borrow_mut();
                visible.clear();
                rasterize_tiles(&buf, &tiles, &mut grid, cols, sub_rows, app, &mut visible);

                SCALED_PAINTED_BUF.with(|sp| {
                    let mut scaled_painted = sp.borrow_mut();
                    scaled_painted.clear();
                    scaled_painted.resize(visible.len(), false);

                    let _g = crate::perf::begin("ui.bitmap_labels");
                    if bitmap_enabled(inner) {
                        let max_scale = max_label_scale(inner);
                        for (i, (r, idx)) in visible.iter().enumerate() {
                            let item = &buf[*idx];
                            let Some(scale) =
                                pick_label_scale(&item.name, r.width, r.height, max_scale)
                            else {
                                continue;
                            };
                            let selected = app.selected.as_ref() == Some(&item.target);
                            let pulse = app.tile_pulse(&item.target);
                            let bg = tile_color(item.color, selected, pulse);
                            let fg = readable_fg(bg);
                            let label_w = bitmap_font::label_width(&item.name, scale);
                            let label_h = bitmap_font::label_height(scale);
                            let x0 = r.left as i32 + ((r.width.saturating_sub(label_w)) / 2) as i32;
                            let y0 = r.top as i32 + ((r.height.saturating_sub(label_h)) / 2) as i32;
                            bitmap_font::paint(
                                &mut grid, cols, sub_rows, x0, y0, &item.name, fg, scale,
                            );
                            scaled_painted[i] = true;
                        }
                    }
                    drop(_g);

                    composite_halfblocks(slice, inner, &grid, cols);
                    overlay_labels(slice, inner, &visible, &buf, app, &scaled_painted);
                });
            });
        });

        record_hit_regions(&tiles, &buf, inner, &mut app.last_tiles);
    });
}

fn render_nested(slice: &mut GridSlice<'_>, inner: Rect, app: &mut App) {
    if app.current_folder().total_files == 0 {
        return;
    }
    let cols = inner.width as usize;
    let sub_rows = (inner.height as usize) * 2;
    // Inflated root area so edge tiles (after the gap shrink in
    // build_nested_at) reach exactly to the panel boundary.
    let root_rect = Rect::new(
        0,
        0,
        inner.width + GAP_SUBCELLS,
        sub_rows as u16 + GAP_SUBCELLS,
    );

    let bitmap_on = bitmap_enabled(inner);
    NESTED_BUF.with(|n| {
        let mut nodes = n.borrow_mut();
        nodes.clear();
        let layout_t0 = std::time::Instant::now();
        {
            let folder = app.current_folder();
            let base_path = app.current_path.clone();
            build_nested(
                folder,
                root_rect,
                &base_path,
                bitmap_on,
                max_label_scale(inner),
                &mut nodes,
            );
        }
        if nodes.is_empty() {
            return;
        }
        record_layout_bench(app, layout_t0.elapsed(), 1, 0, nodes.len() as u32);

        SCALED_PAINTED_BUF.with(|sp| {
            let mut scaled_painted = sp.borrow_mut();
            scaled_painted.clear();
            scaled_painted.resize(nodes.len(), false);

            GRID_BUF.with(|g| {
                let mut grid = g.borrow_mut();
                grid.clear();
                grid.resize(cols * sub_rows, None);
                for node in nodes.iter() {
                    let selected = app.selected.as_ref() == Some(&node.target);
                    let pulse = app.tile_pulse(&node.target);
                    let color = tile_color(node.color, selected, pulse);
                    let r = node.rect;
                    let sx_end = (r.left + r.width).min(cols as u16);
                    let sy_end = (r.top + r.height).min(sub_rows as u16);
                    for sy in r.top..sy_end {
                        let row_base = sy as usize * cols;
                        for sx in r.left..sx_end {
                            grid[row_base + sx as usize] = Some(color);
                        }
                    }
                }
                if bitmap_on {
                    let max_scale = max_label_scale(inner);
                    for (i, node) in nodes.iter().enumerate() {
                        let r = node.rect;
                        let selected = app.selected.as_ref() == Some(&node.target);
                        let pulse = app.tile_pulse(&node.target);
                        let bg = tile_color(node.color, selected, pulse);
                        let fg = readable_fg(bg);
                        let (scale, y0) = if node.is_folder {
                            let Some(s) = node.bitmap_scale else { continue };
                            (s, r.top as i32 + NESTED_BAND_PAD_SUBROWS as i32)
                        } else {
                            let Some(s) =
                                pick_label_scale(&node.name, r.width, r.height, max_scale)
                            else {
                                continue;
                            };
                            let label_h = bitmap_font::label_height(s);
                            (
                                s,
                                r.top as i32 + ((r.height.saturating_sub(label_h)) / 2) as i32,
                            )
                        };
                        let label_w = bitmap_font::label_width(&node.name, scale);
                        let x0 = r.left as i32 + ((r.width.saturating_sub(label_w)) / 2) as i32;
                        bitmap_font::paint(
                            &mut grid, cols, sub_rows, x0, y0, &node.name, fg, scale,
                        );
                        scaled_painted[i] = true;
                    }
                }
                composite_halfblocks(slice, inner, &grid, cols);
            });

            for (i, node) in nodes.iter().enumerate() {
                if scaled_painted[i] {
                    continue;
                }
                if node.label_rows == 0 {
                    continue;
                }
                let r = node.rect;
                let y_start = (r.top as i32 + 1) / 2;
                let y_end = (r.top as i32 + r.height as i32) / 2;
                if y_end <= y_start {
                    continue;
                }
                let abs_x = inner.left as i32 + r.left as i32;
                let abs_y = inner.top as i32 + y_start;
                let abs_w = r.width as i32;
                let abs_h = y_end - y_start;
                if abs_w < 3 || abs_h < 1 {
                    continue;
                }
                let max_rows = (node.label_rows as i32).min(abs_h);
                if max_rows < 1 {
                    continue;
                }
                let selected = app.selected.as_ref() == Some(&node.target);
                let pulse = app.tile_pulse(&node.target);
                let bg = tile_color(node.color, selected, pulse);
                let fg = readable_fg(bg);
                let primary = if node.is_folder {
                    Style::new().fg(fg).bold()
                } else {
                    Style::new().fg(fg)
                };
                write_row_abs(
                    slice,
                    &truncate(&node.name, abs_w as usize),
                    primary,
                    abs_x,
                    abs_y,
                    abs_w,
                );
                if max_rows >= 2 && (node.subtitle1.chars().count() as i32) <= abs_w {
                    write_row_abs(
                        slice,
                        &node.subtitle1,
                        Style::new().fg(fg),
                        abs_x,
                        abs_y + 1,
                        abs_w,
                    );
                }
                if max_rows >= 3 && !node.subtitle2.is_empty() {
                    write_row_abs(
                        slice,
                        &truncate(&node.subtitle2, abs_w as usize),
                        Style::new().fg(fg).dim(),
                        abs_x,
                        abs_y + 2,
                        abs_w,
                    );
                }
            }

            for node in nodes.iter() {
                let r = node.rect;
                let cy0 = r.top as i32 / 2;
                let cy1 = (r.top as i32 + r.height as i32 + 1) / 2;
                let cell_rect = Rect::new(
                    (inner.top as i32 + cy0).max(0) as u16,
                    (inner.left as i32 + r.left as i32).max(0) as u16,
                    r.width,
                    (cy1 - cy0).max(0) as u16,
                );
                if cell_rect.width == 0 || cell_rect.height == 0 {
                    continue;
                }
                app.last_tiles.push((cell_rect, node.target.clone()));
            }
        });
    });
}

fn build_nested(
    folder: &FolderNode,
    rect: Rect,
    base_path: &[String],
    bitmap_enabled: bool,
    max_scale: u16,
    out: &mut Vec<NestedNode>,
) {
    build_nested_at(
        folder,
        rect,
        base_path,
        0,
        0,
        bitmap_enabled,
        max_scale,
        out,
    );
}

const FOLDER_BOTTOM_PAD: u16 = 1;

fn folder_label_info(
    rect: Rect,
    name: &str,
    bitmap_enabled: bool,
    max_scale: u16,
) -> (Option<u16>, u16, u16) {
    if rect.width < 4 {
        return (None, 0, 0);
    }
    if bitmap_enabled
        && let Some(scale) = pick_label_scale(
            name,
            rect.width,
            rect.height.saturating_sub(FOLDER_BOTTOM_PAD),
            max_scale,
        )
    {
        let band = 2 * NESTED_BAND_PAD_SUBROWS + bitmap_font::label_height(scale);
        if rect.height >= band + FOLDER_BOTTOM_PAD {
            return (Some(scale), 0, band);
        }
    }
    let plain_band = NESTED_BAND_PAD_SUBROWS + 2;
    if rect.height >= plain_band + FOLDER_BOTTOM_PAD {
        return (None, 1, plain_band);
    }
    (None, 0, 0)
}

#[allow(clippy::too_many_arguments)]
fn build_nested_at(
    folder: &FolderNode,
    rect: Rect,
    base_path: &[String],
    band_subrows: u16,
    depth: u32,
    bitmap_enabled: bool,
    max_scale: u16,
    out: &mut Vec<NestedNode>,
) {
    let (side_pad, top_band, bottom_pad) = if depth == 0 {
        (0, 0, 0)
    } else {
        (NESTED_INNER_PAD, band_subrows, 1)
    };
    if rect.width <= 2 * side_pad || rect.height <= top_band + bottom_pad {
        return;
    }
    let inner = Rect::new(
        rect.top + top_band,
        rect.left + side_pad,
        rect.width - 2 * side_pad,
        rect.height - top_band - bottom_pad,
    );

    let mut entries: Vec<(&str, &Node, u64)> = folder
        .children
        .iter()
        .filter_map(|(name, child)| {
            let v = match child {
                Node::File(f) => f.lines,
                Node::Folder(s) => s.total_lines,
            };
            if v == 0 {
                None
            } else {
                Some((name.as_str(), child, v))
            }
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.2));
    if entries.is_empty() {
        return;
    }
    let mut excluded = vec![false; entries.len()];
    let mut excluded_count: usize = 0;
    let mut tiles: Vec<treemap::Tile<usize>> = Vec::new();
    for _ in 0..MAX_LAYOUT_PASSES {
        let active: Vec<Item<usize>> = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| !excluded[*i])
            .map(|(i, e)| Item {
                value: e.2 as f64,
                data: i,
            })
            .collect();
        if active.is_empty() {
            break;
        }
        tiles = treemap::squarify(&active, inner);
        let before = excluded_count;
        for t in &tiles {
            if (t.rect.width < MIN_TILE_SUBCELLS
                || t.rect.height < MIN_TILE_SUBCELLS
                || t.rect.width <= GAP_SUBCELLS
                || t.rect.height <= GAP_SUBCELLS)
                && !excluded[t.data]
            {
                excluded[t.data] = true;
                excluded_count += 1;
            }
        }
        if excluded_count == before {
            break;
        }
    }
    for tile in tiles {
        if tile.rect.width < MIN_TILE_SUBCELLS
            || tile.rect.height < MIN_TILE_SUBCELLS
            || tile.rect.width <= GAP_SUBCELLS
            || tile.rect.height <= GAP_SUBCELLS
        {
            continue;
        }
        let mut r = tile.rect;
        r.width -= GAP_SUBCELLS;
        r.height -= GAP_SUBCELLS;
        let (name, child, _) = entries[tile.data];
        match child {
            Node::File(f) => {
                out.push(NestedNode {
                    rect: r,
                    color: lang::color(f.lang),
                    target: TileTarget::File(f.path.clone()),
                    name: name.to_string(),
                    subtitle1: fmt_compact(f.lines),
                    subtitle2: String::new(),
                    is_folder: false,
                    label_rows: 2,
                    bitmap_scale: None,
                });
            }
            Node::Folder(sub) => {
                let mut sub_path = base_path.to_vec();
                sub_path.push(name.to_string());
                let base = sub.dominant_lang().map(lang::color).unwrap_or(Color::Rgb {
                    r: 120,
                    g: 120,
                    b: 120,
                });
                let darken_amt = (0.35 + 0.10 * depth as f64).min(0.70);
                let color = darken(base, darken_amt);
                let display_name = format!("{name}/");
                let (bitmap_scale, label_rows, child_band) =
                    folder_label_info(r, &display_name, bitmap_enabled, max_scale);
                out.push(NestedNode {
                    rect: r,
                    color,
                    target: TileTarget::Folder(sub_path.clone()),
                    name: display_name,
                    subtitle1: String::new(),
                    subtitle2: String::new(),
                    is_folder: true,
                    label_rows,
                    bitmap_scale,
                });
                let (child_bitmap_enabled, child_max_scale) = match bitmap_scale {
                    Some(s) => (bitmap_enabled, s),
                    None => (false, 0),
                };
                build_nested_at(
                    sub,
                    r,
                    &sub_path,
                    child_band,
                    depth + 1,
                    child_bitmap_enabled,
                    child_max_scale,
                    out,
                );
            }
        }
    }
}

fn layout_tiles(items: &[TileItem], inner: Rect, app: &mut App) -> Vec<treemap::Tile<usize>> {
    let _g = crate::perf::begin("ui.halfblock.layout");
    let cols = inner.width;
    let sub_rows = inner.height.saturating_mul(2);
    let area_subcells = (cols as u64) * (sub_rows as u64);
    let total_value: u64 = items.iter().map(|i| i.value).sum();
    if total_value == 0 || area_subcells == 0 {
        record_layout_bench(app, std::time::Duration::ZERO, 0, 0, 0);
        return Vec::new();
    }

    // Inflate the layout area by one gutter on the right and bottom so
    // the edge tiles, after their per-tile gap shrink in rasterize_tiles,
    // snap back exactly to the panel boundary instead of leaving a dead
    // column / half-row of empty space.
    let layout_area = Rect::new(0, 0, cols + GAP_SUBCELLS, sub_rows + GAP_SUBCELLS);
    let cell_value = total_value as f64 / area_subcells as f64;
    let pre_min = (cell_value * (MIN_TILE_SUBCELLS as f64).powi(2)).max(1.0);

    let (tiles, iters, excluded_count, t0) = LAYOUT_SORTED.with(|s| {
        LAYOUT_EXCLUDED.with(|e| {
            LAYOUT_TM_ITEMS.with(|tm| {
                let mut sorted = s.borrow_mut();
                let mut excluded = e.borrow_mut();
                let mut tm_items = tm.borrow_mut();

                sorted.clear();
                sorted.extend(items.iter().enumerate().filter_map(|(i, it)| {
                    (it.value > 0 && (it.value as f64) >= pre_min).then_some(i)
                }));
                sorted.sort_by_key(|&i| std::cmp::Reverse(items[i].value));

                excluded.clear();
                excluded.resize(items.len(), false);

                let t0 = std::time::Instant::now();
                let mut tiles: Vec<treemap::Tile<usize>> = Vec::new();
                let mut iters: u32 = 0;
                let mut excluded_count: usize = 0;
                if sorted.is_empty() {
                    return (tiles, iters, excluded_count, t0);
                }
                for _ in 0..MAX_LAYOUT_PASSES {
                    iters += 1;
                    tm_items.clear();
                    tm_items.extend(sorted.iter().copied().filter(|&i| !excluded[i]).map(|i| {
                        Item {
                            value: items[i].value as f64,
                            data: i,
                        }
                    }));
                    if tm_items.is_empty() {
                        break;
                    }
                    tiles = treemap::squarify(&tm_items, layout_area);
                    let before = excluded_count;
                    for t in &tiles {
                        if (t.rect.width < MIN_TILE_SUBCELLS || t.rect.height < MIN_TILE_SUBCELLS)
                            && !excluded[t.data]
                        {
                            excluded[t.data] = true;
                            excluded_count += 1;
                        }
                    }
                    if excluded_count == before {
                        break;
                    }
                }
                tiles.retain(|t| {
                    t.rect.width >= MIN_TILE_SUBCELLS && t.rect.height >= MIN_TILE_SUBCELLS
                });
                (tiles, iters, excluded_count, t0)
            })
        })
    });
    record_layout_bench(
        app,
        t0.elapsed(),
        iters,
        excluded_count as u32,
        tiles.len() as u32,
    );
    tiles
}

fn record_layout_bench(
    app: &mut App,
    elapsed: std::time::Duration,
    iters: u32,
    excluded: u32,
    drawn: u32,
) {
    if !app.bench.enabled {
        return;
    }
    app.bench.last_treemap_layout = elapsed;
    app.bench.last_treemap_iters = iters;
    app.bench.last_layout_excluded = excluded;
    app.bench.last_tiles_drawn = drawn;
}

fn rasterize_tiles(
    items: &[TileItem],
    tiles: &[treemap::Tile<usize>],
    grid: &mut [Option<Color>],
    cols: usize,
    sub_rows: usize,
    app: &App,
    visible: &mut Vec<(Rect, usize)>,
) {
    visible.reserve(tiles.len());
    for tile in tiles {
        let mut r = tile.rect;
        if r.width <= GAP_SUBCELLS || r.height <= GAP_SUBCELLS {
            continue;
        }
        r.width -= GAP_SUBCELLS;
        r.height -= GAP_SUBCELLS;

        let item = &items[tile.data];
        let selected = app.selected.as_ref() == Some(&item.target);
        let pulse = app.tile_pulse(&item.target);
        let color = tile_color(item.color, selected, pulse);

        let sx_end = (r.left + r.width).min(cols as u16);
        let sy_end = (r.top + r.height).min(sub_rows as u16);
        for sy in r.top..sy_end {
            for sx in r.left..sx_end {
                grid[sy as usize * cols + sx as usize] = Some(color);
            }
        }
        visible.push((r, tile.data));
    }
}

/// Render the colour grid as half-block characters into the slice.
/// `inner` is in screen-coordinates; we translate to slice-local coords
/// before writing each cell.
fn composite_halfblocks(
    slice: &mut GridSlice<'_>,
    inner: Rect,
    grid: &[Option<Color>],
    cols: usize,
) {
    let _g = crate::perf::begin("ui.halfblock.fill");
    let slice_origin = slice.screen_rect();
    let local_x0 = inner.left.saturating_sub(slice_origin.left);
    let local_y0 = inner.top.saturating_sub(slice_origin.top);
    for cy in 0..(inner.height as usize) {
        for cx in 0..cols {
            let top = grid[(cy * 2) * cols + cx];
            let bot = grid[(cy * 2 + 1) * cols + cx];
            let lx = local_x0 + cx as u16;
            let ly = local_y0 + cy as u16;
            let Some(cell) = slice.cell_mut(lx, ly) else {
                continue;
            };
            match (top, bot) {
                (Some(t), Some(b)) if t == b => {
                    cell.symbol = ' ';
                    cell.style = Style::default().bg(t);
                }
                (Some(t), Some(b)) => {
                    cell.symbol = '▀';
                    cell.style = Style::default().fg(t).bg(b);
                }
                (Some(t), None) => {
                    cell.symbol = '▀';
                    cell.style = Style::default().fg(t).bg(Color::Reset);
                }
                (None, Some(b)) => {
                    cell.symbol = '▄';
                    cell.style = Style::default().fg(b).bg(Color::Reset);
                }
                (None, None) => {
                    cell.symbol = ' ';
                    cell.style = Style::default().bg(Color::Reset);
                }
            }
        }
    }
}

fn overlay_labels(
    slice: &mut GridSlice<'_>,
    inner: Rect,
    visible: &[(Rect, usize)],
    items: &[TileItem],
    app: &App,
    scaled_painted: &[bool],
) {
    let _g = crate::perf::begin("ui.halfblock.text");
    let t0 = std::time::Instant::now();
    for (i, (r, idx)) in visible.iter().enumerate() {
        if scaled_painted.get(i).copied().unwrap_or(false) {
            continue;
        }
        let y_start = (r.top as i32 + 1) / 2;
        let y_end = (r.top as i32 + r.height as i32) / 2;
        if y_end <= y_start {
            continue;
        }
        let abs_x0 = inner.left as i32 + r.left as i32;
        let abs_y0 = inner.top as i32 + y_start;
        let abs_w = r.width as i32;
        let abs_h = y_end - y_start;
        if abs_w <= 0 || abs_h <= 0 {
            continue;
        }
        let item = &items[*idx];
        let selected = app.selected.as_ref() == Some(&item.target);
        let pulse = app.tile_pulse(&item.target);
        let bg = tile_color(item.color, selected, pulse);
        write_label(slice, item, bg, abs_x0, abs_y0, abs_w, abs_h);
    }
    if app.bench.enabled {
        let _ = t0;
    }
}

fn record_hit_regions(
    tiles: &[treemap::Tile<usize>],
    items: &[TileItem],
    inner: Rect,
    out: &mut Vec<(Rect, TileTarget)>,
) {
    for tile in tiles {
        let r = tile.rect;
        if r.width <= GAP_SUBCELLS || r.height <= GAP_SUBCELLS {
            continue;
        }
        let visible_r = Rect::new(
            r.top,
            r.left,
            r.width - GAP_SUBCELLS,
            r.height - GAP_SUBCELLS,
        );
        let cy0 = visible_r.top as i32 / 2;
        let cy1 = (visible_r.top as i32 + visible_r.height as i32 + 1) / 2;
        let cx0 = visible_r.left as i32;
        let cx1 = visible_r.left as i32 + visible_r.width as i32;
        let abs = Rect::new(
            (inner.top as i32 + cy0).max(0) as u16,
            (inner.left as i32 + cx0).max(0) as u16,
            (cx1 - cx0).max(0) as u16,
            (cy1 - cy0).max(0) as u16,
        );
        if abs.width == 0 || abs.height == 0 {
            continue;
        }
        out.push((abs, items[tile.data].target.clone()));
    }
}

fn write_label(
    slice: &mut GridSlice<'_>,
    item: &TileItem,
    bg: Color,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    if w < 3 || h < 1 {
        return;
    }
    let fg = readable_fg(bg);
    write_row_abs(
        slice,
        &truncate(&item.name, w as usize),
        Style::new().fg(fg).bold(),
        x,
        y,
        w,
    );
    if h >= 2 && (item.subtitle1.chars().count() as i32) <= w {
        write_row_abs(slice, &item.subtitle1, Style::new().fg(fg), x, y + 1, w);
    }
    if h >= 3 && !item.subtitle2.is_empty() {
        let s = truncate_left(&item.subtitle2, w as usize);
        write_row_abs(slice, &s, Style::new().fg(fg).dim(), x, y + 2, w);
    }
}

/// Write `text` into the grid at *screen-absolute* `(x, y)` clipped to
/// the slice's underlying width-`w` window. Negative coords are clipped
/// (each char advances `col`); chars past `x + w` stop the loop.
///
/// Preserves the existing cell's `bg` when `style.bg` is `None`, so
/// labels painted over a coloured half-block tile inherit the tile's
/// background instead of resetting it to the terminal default. Mirrors
/// ratatui's `cell.set_char(ch).set_fg(fg)` partial-update behaviour.
fn write_row_abs(slice: &mut GridSlice<'_>, text: &str, style: Style, x: i32, y: i32, w: i32) {
    let origin = slice.screen_rect();
    if y < 0 {
        return;
    }
    let local_y = y - origin.top as i32;
    if local_y < 0 || local_y >= slice.height() as i32 {
        return;
    }
    for (offset, ch) in text.chars().enumerate() {
        if offset as i32 >= w {
            break;
        }
        let col = x + offset as i32;
        if col < 0 {
            continue;
        }
        let local_x = col - origin.left as i32;
        if local_x >= 0
            && local_x < slice.width() as i32
            && let Some(cell) = slice.cell_mut(local_x as u16, local_y as u16)
        {
            cell.symbol = ch;
            let mut new_style = style;
            if new_style.bg.is_none() {
                new_style.bg = cell.style.bg;
            }
            cell.style = new_style;
        }
    }
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

// ── item building (tree / files view) ───────────────────────────────────────

fn build_items_into(app: &App, out: &mut Vec<TileItem>) {
    let _g = crate::perf::begin("ui.build_items");
    let folder = app.current_folder();
    let total = folder.total_lines.max(1);
    match app.view {
        View::Tree => build_tree_items(app, folder, total, out),
        View::Files => build_files_items(app, folder, total, out),
        View::Nested => {}
    }
}

fn build_tree_items(app: &App, folder: &FolderNode, total: u64, out: &mut Vec<TileItem>) {
    out.reserve(folder.children.len());
    for (name, child) in &folder.children {
        match child {
            Node::File(file) => {
                if file.lines == 0 {
                    continue;
                }
                let pct = 100.0 * file.lines as f64 / total as f64;
                out.push(TileItem {
                    value: file.lines,
                    color: lang::color(file.lang),
                    name: name.clone(),
                    subtitle1: format!("{} ({:.1}%)", fmt_int(file.lines), pct),
                    subtitle2: String::new(),
                    target: TileTarget::File(file.path.clone()),
                });
            }
            Node::Folder(sub) => {
                if sub.total_lines == 0 {
                    continue;
                }
                let color = sub.dominant_lang().map(lang::color).unwrap_or(Color::Rgb {
                    r: 120,
                    g: 120,
                    b: 120,
                });
                let pct = 100.0 * sub.total_lines as f64 / total as f64;
                let mut path = app.current_path.clone();
                path.push(name.clone());
                out.push(TileItem {
                    value: sub.total_lines,
                    color,
                    name: format!("{name}/"),
                    subtitle1: format!("{} ({:.1}%)", fmt_int(sub.total_lines), pct),
                    subtitle2: format!("{} files", fmt_int(sub.total_files)),
                    target: TileTarget::Folder(path),
                });
            }
        }
    }
}

const MAX_FILE_ITEMS: usize = 8192;

fn build_files_items(app: &App, folder: &FolderNode, total: u64, out: &mut Vec<TileItem>) {
    let mut files = Vec::new();
    crate::tree::collect_files(folder, &mut files);
    files.sort_unstable_by_key(|f| std::cmp::Reverse(f.lines));
    files.truncate(MAX_FILE_ITEMS);

    let scope_path = app
        .current_path
        .iter()
        .fold(app.root.clone(), |p, s| p.join(s));

    out.reserve(files.len());
    for f in files {
        let pct = 100.0 * f.lines as f64 / total as f64;
        let rel = f.path.strip_prefix(&scope_path).unwrap_or(&f.path);
        let name = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let parent = rel
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(TileItem {
            value: f.lines,
            color: lang::color(f.lang),
            name,
            subtitle1: format!("{} ({:.1}%)", fmt_int(f.lines), pct),
            subtitle2: parent,
            target: TileTarget::File(f.path.clone()),
        });
    }
}

// ── legend ──────────────────────────────────────────────────────────────────

fn render_legend(slice: &mut GridSlice<'_>, app: &mut App) {
    let _g = crate::perf::begin("ui.legend");

    if slice.width() == 0 || slice.height() == 0 {
        return;
    }

    // Slice already lives inside the layout-tree-painted border; its
    // screen_rect is the inner area. Tracked on `App` so the host's
    // mouse handler can detect wheel events inside the legend.
    let inner = slice.screen_rect();
    app.legend_rect = inner;

    app.ensure_ranked();
    let total = app.total_lines.max(1);
    let visible_rows = inner.height as usize;
    let header_rows = 1usize;
    let body_capacity = visible_rows.saturating_sub(header_rows);
    let total_items = app.ranked().len();
    let max_scroll = total_items.saturating_sub(body_capacity);
    if app.legend_scroll > max_scroll {
        app.legend_scroll = max_scroll;
    }
    app.legend_max_scroll = max_scroll;
    let scroll = app.legend_scroll;
    let ranked = app.ranked();

    // The slice is already inset by the layout-tree-painted border, so
    // the header sits at the top row and content starts at column 0.
    let dim = Style::new().fg(Color::DarkGrey);
    let header_y: u16 = 0;
    let mut col: u16 = 0;
    col = put_str_clip(slice, col, header_y, &format!("{:<14}", "language"), dim);
    col = put_str_clip(slice, col, header_y, &format!("{:>6}", "files"), dim);
    let _ = put_str_clip(slice, col, header_y, &format!("{:>8}", "lines"), dim);

    for (row_idx, (lang_, stats)) in ranked.iter().skip(scroll).take(body_capacity).enumerate() {
        let pct = 100.0 * stats.lines as f64 / total as f64;
        let y: u16 = header_y + 1 + row_idx as u16;
        if y >= slice.height() {
            break;
        }
        let mut col: u16 = 0;
        col = put_str_clip(slice, col, y, "██ ", Style::new().fg(lang::color(*lang_)));
        col = put_str_clip(
            slice,
            col,
            y,
            &format!("{:<11}", truncate(lang_.0, 11)),
            Style::default(),
        );
        col = put_str_clip(
            slice,
            col,
            y,
            &format!("{:>6}", fmt_compact(stats.files)),
            Style::default(),
        );
        col = put_str_clip(
            slice,
            col,
            y,
            &format!("{:>8}", fmt_compact(stats.lines)),
            Style::default(),
        );
        let _ = put_str_clip(slice, col, y, &format!("{:>5}", fmt_pct(pct)), dim);
    }
}

// ── colour helpers ──────────────────────────────────────────────────────────

fn brighten(c: Color, amount: f64) -> Color {
    match c {
        Color::Rgb { r, g, b } => {
            let f = amount.clamp(0.0, 1.0);
            let nr = r as f64 + (255.0 - r as f64) * f;
            let ng = g as f64 + (255.0 - g as f64) * f;
            let nb = b as f64 + (255.0 - b as f64) * f;
            Color::Rgb {
                r: nr as u8,
                g: ng as u8,
                b: nb as u8,
            }
        }
        _ => c,
    }
}

fn darken(c: Color, amount: f64) -> Color {
    match c {
        Color::Rgb { r, g, b } => {
            let f = (1.0 - amount).clamp(0.0, 1.0);
            Color::Rgb {
                r: (r as f64 * f) as u8,
                g: (g as f64 * f) as u8,
                b: (b as f64 * f) as u8,
            }
        }
        _ => c,
    }
}

fn readable_fg(bg: Color) -> Color {
    let (r, g, b) = match bg {
        Color::Rgb { r, g, b } => (r as f64, g as f64, b as f64),
        _ => return Color::White,
    };
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if lum > 140.0 {
        Color::Black
    } else {
        Color::White
    }
}
