use std::cell::RefCell;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, TileTarget, View};
use crate::bitmap_font;
use crate::format::{fmt_bytes_short, fmt_compact, fmt_int, fmt_pct, truncate, truncate_left};
use crate::lang;
use crate::tree::{FolderNode, Node};
use crate::treemap::{self, Item};

const GAP_SUBCELLS: u16 = 1;
const MIN_TILE_SUBCELLS: u16 = 3;
const MAX_LAYOUT_PASSES: u32 = 16;
// Terminal-size gate: bitmap labels are only enabled when the render area
// is at least this big. Below the threshold every tile uses plain ASCII —
// keeps tiny terminals readable and skips the per-frame bitmap loop.
const BITMAP_MIN_TERMINAL_W: u16 = 120;
const BITMAP_MIN_TERMINAL_H: u16 = 36;

fn bitmap_enabled(area: Rect) -> bool {
    area.width >= BITMAP_MIN_TERMINAL_W && area.height >= BITMAP_MIN_TERMINAL_H
}

/// Compose the tile background colour from selection + pulse. Selected
/// tiles get a flat brightness boost; pulses fade from `PULSE_PEAK` to
/// 0 over `PULSE_DURATION` (handled by `App::tile_pulse`). Both are
/// stacked, with `brighten` clamping the combined amount.
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

// Per-scale minimum render-area size in cells. The largest scale whose
// minimum the area meets gets used. Indexed by `scale - 1`. Bumping an
// entry up means that scale only triggers on bigger terminals.
const SCALE_MIN_AREA_CELLS: [(u16, u16); bitmap_font::MAX_SCALE as usize] = [
    // (min_width, min_height) per scale
    (80, 24),  // scale 1 — kicks in once bitmaps are enabled at all
    (220, 64), // scale 2 — wants a roomy terminal before doubling up
    (260, 76), // scale 3 — only on near-fullscreen 4K-ish terminals
];
// Above scale 1, a label may not exceed this fraction of its tile on
// either axis. Stops short names like `src/` from ballooning to fill
// huge tiles. At scale 1 the rule is just "physical fit with a 1-cell
// breathing pad" so the smallest size always lights up if it fits at all.
const LABEL_MAX_TILE_FRAC_NUM: u16 = 3;
const LABEL_MAX_TILE_FRAC_DEN: u16 = 5; // 60 %

/// Per-frame upper bound on the bitmap scale, derived from the render
/// area. Returns `0` when even scale 1's minimum isn't met.
fn max_label_scale(area: Rect) -> u16 {
    (1..=bitmap_font::MAX_SCALE)
        .rev()
        .find(|&s| {
            let (w, h) = SCALE_MIN_AREA_CELLS[(s - 1) as usize];
            area.width >= w && area.height >= h
        })
        .unwrap_or(0)
}

// Sub-rows of pad inside the folder label band, above and below the glyph.
const NESTED_BAND_PAD_SUBROWS: u16 = 1;

/// Largest scale in `1..=max_scale` whose label fits the tile, or `None`
/// if nothing does. At scale 1 the rule is just physical fit with a
/// 1-cell breathing pad; above scale 1 the label additionally has to
/// stay inside `LABEL_MAX_TILE_FRAC_NUM/DEN` of the tile.
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

// Sub-cell padding the parent folder reserves around its squarified children.
// The folder colour shows through as a thin border, signalling containment.
const NESTED_INNER_PAD: u16 = 1;

struct NestedNode {
    rect: Rect, // sub-cell, post-gap shrink
    color: Color,
    target: TileTarget,
    name: String,
    subtitle1: String,
    subtitle2: String,
    is_folder: bool,
    // Plain-text fallback character rows when no bitmap label fits. For
    // files this caps how many subtitle lines we'll write.
    label_rows: u16,
    // Bitmap scale chosen for this node's label. `None` means "no
    // bitmap; fall back to plain text overlay if `label_rows > 0`". For
    // files we pick this dynamically each frame inside `render_nested`;
    // for folders it's set during `build_nested_at` because the band
    // reservation affects how children are laid out.
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

// ── top-level layout ────────────────────────────────────────────────────────

pub fn render(f: &mut Frame, app: &mut App) {
    let _g = crate::perf::begin("ui.render");
    // Drop expired pulses up front: keeps the per-tile lookup small
    // and avoids paying for stale animation state for the rest of
    // the session.
    app.gc_pulses();
    let area = f.area();
    let mut constraints = vec![Constraint::Length(2), Constraint::Min(0)];
    if app.bench.enabled {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    render_header(f, chunks[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(34)])
        .split(chunks[1]);

    render_treemap(f, body[0], app);
    render_legend(f, body[1], app);
    if app.bench.enabled {
        render_bench_hud(f, chunks[2], app);
        render_footer(f, chunks[3], app);
    } else {
        render_footer(f, chunks[2], app);
    }
}

// ── chrome (header / footer / bench HUD) ────────────────────────────────────

fn render_header(f: &mut Frame, area: Rect, app: &App) {
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
    // Width includes ` · ` (3 cells) between every adjacent pair so the
    // right-chunk reservation is exact.
    let stats_width = stat_items.iter().map(|s| s.chars().count()).sum::<usize>()
        + 3 * stat_items.len().saturating_sub(1);
    let (root, zoom) = app.breadcrumb_parts();
    let block = Block::default().borders(Borders::BOTTOM);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Reserve exactly the width the stats need on the right; the left
    // half flexes for the breadcrumb (which gets truncated by ratatui if
    // the terminal is too narrow).
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(stats_width as u16)])
        .split(inner);

    let left = Line::from(vec![
        Span::styled(
            status_text,
            Style::default()
                .bg(status_bg)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(root, Style::default().fg(Color::White)),
        Span::styled(zoom, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(left), chunks[0]);

    let mut stats_spans: Vec<Span> = Vec::with_capacity(stat_items.len() * 2);
    for (i, item) in stat_items.iter().enumerate() {
        if i > 0 {
            stats_spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        stats_spans.push(Span::styled(item.clone(), Style::default().fg(Color::Gray)));
    }
    f.render_widget(
        Paragraph::new(Line::from(stats_spans)).alignment(Alignment::Right),
        chunks[1],
    );
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let path = app
        .last_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let path = truncate_left(&path, area.width.saturating_sub(2) as usize);
    let line = Line::from(vec![
        Span::styled("hjkl/↑↓←→", Style::default().fg(Color::Yellow)),
        Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
        Span::styled("⏎", Style::default().fg(Color::Yellow)),
        Span::styled(" zoom  ", Style::default().fg(Color::DarkGray)),
        Span::styled("esc", Style::default().fg(Color::Yellow)),
        Span::styled(" up  ", Style::default().fg(Color::DarkGray)),
        Span::styled("tab", Style::default().fg(Color::Yellow)),
        Span::styled(" view  ", Style::default().fg(Color::DarkGray)),
        Span::styled("o", Style::default().fg(Color::Yellow)),
        Span::styled(" open  ", Style::default().fg(Color::DarkGray)),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::styled(" quit  ", Style::default().fg(Color::DarkGray)),
        Span::styled(path, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_bench_hud(f: &mut Frame, area: Rect, app: &App) {
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
    let line = Line::from(vec![
        Span::styled(
            "BENCH ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "frame {:.2}ms (p95 {:.2}ms, {:.0} fps)  ",
                frame_avg.as_secs_f64() * 1000.0,
                frame_p95.as_secs_f64() * 1000.0,
                fps,
            ),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!("input→draw {input_lat_ms:.2}ms  "),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(
                "layout {:.2}ms iters {} drawn {}  ",
                app.bench.last_treemap_layout.as_secs_f64() * 1000.0,
                app.bench.last_treemap_iters,
                app.bench.last_tiles_drawn,
            ),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(
                "hb {:.2}ms tx {:.2}ms  ",
                app.bench.last_halfblock.as_secs_f64() * 1000.0,
                app.bench.last_text_overlay.as_secs_f64() * 1000.0,
            ),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(
                "scan {:.0} f/s {:.1} M ln/s {:.1} MB/s  per-file {:.0}µs  ",
                files_per_s,
                lines_per_s / 1e6,
                mb_per_s,
                avg_count as f64 / 1000.0,
            ),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!(
                "alloc {}/{}",
                app.bench.last_frame_allocs,
                fmt_bytes_short(app.bench.last_frame_alloc_bytes),
            ),
            Style::default().fg(Color::Green),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ── treemap ─────────────────────────────────────────────────────────────────

fn render_treemap(f: &mut Frame, area: Rect, app: &mut App) {
    let _g = crate::perf::begin("ui.treemap");
    let title = match app.view {
        View::Tree => " tree ",
        View::Files => " files ",
        View::Nested => " nested ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::styled(title, Style::default().fg(Color::White)),
            Span::styled(
                "(area = lines, color = language)",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Clear, inner);
    app.last_tiles.clear();

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if matches!(app.view, View::Nested) {
        render_nested(f, inner, app);
    } else {
        render_flat(f, inner, app);
    }
}

/// Flat treemap path used by the Tree and Files views: every tile is a
/// sibling competing for the same rect.
fn render_flat(f: &mut Frame, inner: Rect, app: &mut App) {
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

        // 1. Layout fractional rectangles in sub-cell space, dropping items
        //    that would round below MIN_TILE_SUBCELLS in either dim.
        let tiles = layout_tiles(&buf, inner, app);
        if tiles.is_empty() {
            return;
        }

        // 2. Rasterize into a sub-cell colour grid (gap reserved on
        //    right + bottom of each tile).
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

                    // 3a. Paint scaled bitmap labels into the grid before
                    //     compositing. Glyph "on" pixels overwrite tile
                    //     sub-cells with the readable foreground; the half-
                    //     block emitter below renders them as text.
                    //     Skipped wholesale on small terminals via
                    //     `bitmap_enabled(inner)`.
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
                            let x0 = r.x as i32 + ((r.width.saturating_sub(label_w)) / 2) as i32;
                            let y0 = r.y as i32 + ((r.height.saturating_sub(label_h)) / 2) as i32;
                            bitmap_font::paint(
                                &mut grid, cols, sub_rows, x0, y0, &item.name, fg, scale,
                            );
                            scaled_painted[i] = true;
                        }
                    }
                    drop(_g);

                    // 3b. Emit the half-block characters for the colour grid.
                    composite_halfblocks(f.buffer_mut(), inner, &grid, cols);

                    // 4. Overlay text labels on tiles that didn't get a
                    //    bitmap label and still fit a character row.
                    overlay_labels(f.buffer_mut(), inner, &visible, &buf, app, &scaled_painted);
                });
            });
        });

        // 5. Save click targets keyed off the layout (not visible) rect so
        //    the gap area still lands on the nearest tile.
        record_hit_regions(&tiles, &buf, inner, &mut app.last_tiles);
    });
}

/// Nested treemap: every folder is a container box, its files and
/// subfolders are squarified inside it (recursively, no depth limit).
/// Parents paint first so children overlay on top, and hit regions are
/// recorded in the same order so deepest-tile wins on click.
fn render_nested(f: &mut Frame, inner: Rect, app: &mut App) {
    if app.current_folder().total_files == 0 {
        return;
    }
    let cols = inner.width as usize;
    let sub_rows = (inner.height as usize) * 2;
    let root_rect = Rect {
        x: 0,
        y: 0,
        width: inner.width,
        height: sub_rows as u16,
    };

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

        // Tracks which nodes got a scaled bitmap label so the plain-text
        // overlay loop knows to skip them. Reused across frames to avoid
        // a per-frame allocation when the tree is large.
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
                    let sx_end = (r.x + r.width).min(cols as u16);
                    let sy_end = (r.y + r.height).min(sub_rows as u16);
                    for sy in r.y..sy_end {
                        let row_base = sy as usize * cols;
                        for sx in r.x..sx_end {
                            grid[row_base + sx as usize] = Some(color);
                        }
                    }
                }
                // Scaled bitmap labels: paint glyph pixels into the same grid
                // before compositing. The half-block emitter below renders them
                // as text on top of the tile colour.
                //
                // - Files: pick the largest scale that fits the file's interior
                //   dynamically; centre the glyph block.
                // - Folders: use the scale chosen at build time, paint into the
                //   reserved band at the top of the folder rect. Children's
                //   rects start below the band so they don't disturb the
                //   glyph pixels.
                //
                // The whole loop is skipped on small terminals via the
                // `bitmap_on` gate computed once per frame.
                if bitmap_on {
                    let max_scale = max_label_scale(inner);
                    for (i, node) in nodes.iter().enumerate() {
                        let r = node.rect;
                        let selected = app.selected.as_ref() == Some(&node.target);
                        let pulse = app.tile_pulse(&node.target);
                        let bg = tile_color(node.color, selected, pulse);
                        let fg = readable_fg(bg);
                        // Folders: use the scale chosen at build time, paint into
                        //   the reserved band at the top of the rect.
                        // Files: pick the largest fitting scale per frame and
                        //   centre the glyph block.
                        let (scale, y0) = if node.is_folder {
                            let Some(s) = node.bitmap_scale else { continue };
                            (s, r.y as i32 + NESTED_BAND_PAD_SUBROWS as i32)
                        } else {
                            let Some(s) =
                                pick_label_scale(&node.name, r.width, r.height, max_scale)
                            else {
                                continue;
                            };
                            let label_h = bitmap_font::label_height(s);
                            (
                                s,
                                r.y as i32 + ((r.height.saturating_sub(label_h)) / 2) as i32,
                            )
                        };
                        let label_w = bitmap_font::label_width(&node.name, scale);
                        let x0 = r.x as i32 + ((r.width.saturating_sub(label_w)) / 2) as i32;
                        bitmap_font::paint(
                            &mut grid, cols, sub_rows, x0, y0, &node.name, fg, scale,
                        );
                        scaled_painted[i] = true;
                    }
                }
                composite_halfblocks(f.buffer_mut(), inner, &grid, cols);
            });

            // Labels: skip when there isn't a full character row inside the tile.
            let buf = f.buffer_mut();
            for (i, node) in nodes.iter().enumerate() {
                if scaled_painted[i] {
                    continue;
                }
                if node.label_rows == 0 {
                    continue;
                }
                let r = node.rect;
                let y_start = (r.y as i32 + 1) / 2;
                let y_end = (r.y as i32 + r.height as i32) / 2;
                if y_end <= y_start {
                    continue;
                }
                let abs_x = inner.x as i32 + r.x as i32;
                let abs_y = inner.y as i32 + y_start;
                let abs_w = r.width as i32;
                let abs_h = y_end - y_start;
                if abs_w < 3 || abs_h < 1 {
                    continue;
                }
                // For folders, label_rows is the cap that matches the reserved
                // band; for files, label_rows is just the maximum we'd ever
                // write. Both clamp to the actual character rows inside the
                // tile (`abs_h`) so we never overflow the visible area.
                let max_rows = (node.label_rows as i32).min(abs_h);
                if max_rows < 1 {
                    continue;
                }
                let selected = app.selected.as_ref() == Some(&node.target);
                let pulse = app.tile_pulse(&node.target);
                let bg = tile_color(node.color, selected, pulse);
                let fg = readable_fg(bg);
                let primary_mod = if node.is_folder {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                };
                write_row(
                    buf,
                    &truncate(&node.name, abs_w as usize),
                    fg,
                    primary_mod,
                    abs_x,
                    abs_y,
                    abs_w,
                );
                // Subtitle rows only render when the band reserved enough
                // space *and* the unmodified text fits the tile width.
                if max_rows >= 2 && (node.subtitle1.chars().count() as i32) <= abs_w {
                    write_row(
                        buf,
                        &node.subtitle1,
                        fg,
                        Modifier::empty(),
                        abs_x,
                        abs_y + 1,
                        abs_w,
                    );
                }
                if max_rows >= 3 && !node.subtitle2.is_empty() {
                    write_row(
                        buf,
                        &truncate(&node.subtitle2, abs_w as usize),
                        fg,
                        Modifier::DIM,
                        abs_x,
                        abs_y + 2,
                        abs_w,
                    );
                }
            }

            // Hit regions in node order: parents first, children last. App.hit()
            // iterates in reverse so the deepest tile under the cursor wins.
            for node in nodes.iter() {
                let r = node.rect;
                let cy0 = r.y as i32 / 2;
                let cy1 = (r.y as i32 + r.height as i32 + 1) / 2;
                let cell_rect = Rect {
                    x: (inner.x as i32 + r.x as i32).max(0) as u16,
                    y: (inner.y as i32 + cy0).max(0) as u16,
                    width: r.width,
                    height: (cy1 - cy0).max(0) as u16,
                };
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

/// Sub-rows the folder reserves at the bottom of its rect for children
/// to peek out from under the label band.
const FOLDER_BOTTOM_PAD: u16 = 1;

/// Build-time decision for a folder's label area: picks bitmap scale
/// (or `None` = no bitmap), how many plain-text rows to emit (0 = none,
/// 1 = name), and how tall the band reserved at the top of the folder
/// is in sub-rows. Children get whatever's left below `band_subrows`
/// (minus the bottom pad).
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
    // Plain text fallback: one character row (= 2 sub-rows) plus the
    // top sub-row pad and the bottom pad.
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
    // The outermost call (depth 0) doesn't draw a folder of its own — it
    // just hosts the squarified children — so it skips the pad/label band
    // entirely and uses the whole rect. For inner folders the band is
    // sized by the parent's `folder_label_layout` and passed in via
    // `band_subrows`.
    let (side_pad, top_band, bottom_pad) = if depth == 0 {
        (0, 0, 0)
    } else {
        (NESTED_INNER_PAD, band_subrows, 1)
    };
    if rect.width <= 2 * side_pad || rect.height <= top_band + bottom_pad {
        return;
    }
    let inner = Rect {
        x: rect.x + side_pad,
        y: rect.y + top_band,
        width: rect.width - 2 * side_pad,
        height: rect.height - top_band - bottom_pad,
    };

    // Children with non-zero size, sorted by lines desc for the squarifier.
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
    // Iterative squarify (same approach as the flat view): drop tiles that
    // round below MIN_TILE_SUBCELLS and re-squarify the survivors so they
    // expand into the freed space. Without this, dropped tiles leave large
    // empty patches showing the parent's colour.
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
                    // A file has no children, so any character row inside
                    // its rect is fair game for its label.
                    label_rows: 2,
                    // Files pick a bitmap scale dynamically per frame
                    // inside `render_nested` — leave `None` here.
                    bitmap_scale: None,
                });
            }
            Node::Folder(sub) => {
                let mut sub_path = base_path.to_vec();
                sub_path.push(name.to_string());
                let base = sub
                    .dominant_lang()
                    .map(lang::color)
                    .unwrap_or(Color::Rgb(120, 120, 120));
                // Darken folders so their leaf children pop against the
                // container, even when everything inside is the same
                // language. Deeper nesting darkens further (capped) so the
                // hierarchy reads as a depth gradient.
                let darken_amt = (0.35 + 0.10 * depth as f64).min(0.70);
                let color = darken(base, darken_amt);
                // Folders show only their name — size and child count are
                // already communicated visually by the tile area and the
                // children inside it.
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
                // Cascading cap: children may not pick a scale larger
                // than the parent's chosen scale, so a deeper subfolder
                // never out-shouts its parent visually. If the parent
                // fell back to plain text, disable bitmap labels on its
                // descendants entirely — even scale 1 is taller than a
                // single character row.
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

/// Iterative squarified layout. Re-runs after dropping any tile that rounds
/// below `MIN_TILE_SUBCELLS` so the remaining tiles redistribute the space.
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

    let layout_area = Rect {
        x: 0,
        y: 0,
        width: cols,
        height: sub_rows,
    };
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

/// Fill the colour grid for each tile, shrinking by GAP_SUBCELLS on right
/// and bottom. Returns the visible (post-gap) sub-cell rect per tile, used
/// for the text-overlay pass.
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

        let sx_end = (r.x + r.width).min(cols as u16);
        let sy_end = (r.y + r.height).min(sub_rows as u16);
        for sy in r.y..sy_end {
            for sx in r.x..sx_end {
                grid[sy as usize * cols + sx as usize] = Some(color);
            }
        }
        visible.push((r, tile.data));
    }
}

/// Render the colour grid as half-block characters: each character row pulls
/// its top half from sub-row `2*y` and its bottom half from sub-row `2*y+1`.
fn composite_halfblocks(buf: &mut Buffer, inner: Rect, grid: &[Option<Color>], cols: usize) {
    let _g = crate::perf::begin("ui.halfblock.fill");
    for cy in 0..(inner.height as usize) {
        for cx in 0..cols {
            let top = grid[(cy * 2) * cols + cx];
            let bot = grid[(cy * 2 + 1) * cols + cx];
            let abs_x = inner.x + cx as u16;
            let abs_y = inner.y + cy as u16;
            let Some(cell) = buf.cell_mut(Position::new(abs_x, abs_y)) else {
                continue;
            };
            match (top, bot) {
                (Some(t), Some(b)) if t == b => {
                    cell.set_symbol(" ").set_bg(t);
                }
                (Some(t), Some(b)) => {
                    cell.set_symbol("▀").set_fg(t).set_bg(b);
                }
                (Some(t), None) => {
                    cell.set_symbol("▀").set_fg(t).set_bg(Color::Reset);
                }
                (None, Some(b)) => {
                    cell.set_symbol("▄").set_fg(b).set_bg(Color::Reset);
                }
                (None, None) => {
                    cell.set_symbol(" ").set_bg(Color::Reset);
                }
            }
        }
    }
}

fn overlay_labels(
    buf: &mut Buffer,
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
        // Pixel rows fully inside the tile (both top and bottom sub-cells
        // belong to the tile, not to the gap).
        let y_start = (r.y as i32 + 1) / 2;
        let y_end = (r.y as i32 + r.height as i32) / 2;
        if y_end <= y_start {
            continue;
        }
        let abs_x0 = inner.x as i32 + r.x as i32;
        let abs_y0 = inner.y as i32 + y_start;
        let abs_w = r.width as i32;
        let abs_h = y_end - y_start;
        if abs_w <= 0 || abs_h <= 0 {
            continue;
        }
        let item = &items[*idx];
        let selected = app.selected.as_ref() == Some(&item.target);
        let pulse = app.tile_pulse(&item.target);
        let bg = tile_color(item.color, selected, pulse);
        write_label(buf, item, bg, abs_x0, abs_y0, abs_w, abs_h);
    }
    if app.bench.enabled {
        // Text overlay only — caller records the half-block fill duration
        // separately via the perf guard.
        // (last_text_overlay is captured by the outer measure_*.)
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
        let visible_r = Rect {
            x: r.x,
            y: r.y,
            width: r.width - GAP_SUBCELLS,
            height: r.height - GAP_SUBCELLS,
        };
        let cy0 = visible_r.y as i32 / 2;
        let cy1 = (visible_r.y as i32 + visible_r.height as i32 + 1) / 2;
        let cx0 = visible_r.x as i32;
        let cx1 = visible_r.x as i32 + visible_r.width as i32;
        let abs = Rect {
            x: (inner.x as i32 + cx0).max(0) as u16,
            y: (inner.y as i32 + cy0).max(0) as u16,
            width: (cx1 - cx0).max(0) as u16,
            height: (cy1 - cy0).max(0) as u16,
        };
        if abs.width == 0 || abs.height == 0 {
            continue;
        }
        out.push((abs, items[tile.data].target.clone()));
    }
}

fn write_label(buf: &mut Buffer, item: &TileItem, bg: Color, x: i32, y: i32, w: i32, h: i32) {
    if w < 3 || h < 1 {
        return;
    }
    let fg = readable_fg(bg);
    write_row(
        buf,
        &truncate(&item.name, w as usize),
        fg,
        Modifier::BOLD,
        x,
        y,
        w,
    );
    if h >= 2 && (item.subtitle1.chars().count() as i32) <= w {
        write_row(buf, &item.subtitle1, fg, Modifier::empty(), x, y + 1, w);
    }
    if h >= 3 && !item.subtitle2.is_empty() {
        let s = truncate_left(&item.subtitle2, w as usize);
        write_row(buf, &s, fg, Modifier::DIM, x, y + 2, w);
    }
}

fn write_row(buf: &mut Buffer, text: &str, fg: Color, modifier: Modifier, x: i32, y: i32, w: i32) {
    if y < 0 {
        return;
    }
    let mut col = x;
    for ch in text.chars() {
        if col >= x + w {
            break;
        }
        if col < 0 {
            col += 1;
            continue;
        }
        if let Some(cell) = buf.cell_mut(Position::new(col as u16, y as u16)) {
            cell.set_char(ch).set_fg(fg);
            if !modifier.is_empty() {
                cell.set_style(Style::default().add_modifier(modifier));
            }
        }
        col += 1;
    }
}

// ── item building (tree / files view) ───────────────────────────────────────

fn build_items_into(app: &App, out: &mut Vec<TileItem>) {
    let _g = crate::perf::begin("ui.build_items");
    let folder = app.current_folder();
    let total = folder.total_lines.max(1);
    match app.view {
        View::Tree => build_tree_items(app, folder, total, out),
        View::Files => build_files_items(app, folder, total, out),
        // Nested goes through its own pipeline; render_treemap branches
        // before calling build_items_into so this is unreachable.
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
                let color = sub
                    .dominant_lang()
                    .map(lang::color)
                    .unwrap_or(Color::Rgb(120, 120, 120));
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

// Cap how many file items we materialize for the Files view. The treemap
// can only render a few hundred tiles before they round below the minimum
// sub-cell size; building strings for the long tail is wasted work and
// dominates frame time on large trees.
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

fn render_legend(f: &mut Frame, area: Rect, app: &mut App) {
    let _g = crate::perf::begin("ui.legend");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(Span::styled(
            " languages ",
            Style::default().fg(Color::White),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
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

    let mut lines: Vec<Line> = Vec::with_capacity(body_capacity + 1);
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:<14}", "language"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:>6}", "files"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:>8}", "lines"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!("{:>5}", "%"), Style::default().fg(Color::DarkGray)),
    ]));

    for (lang_, stats) in ranked.iter().skip(scroll).take(body_capacity) {
        let pct = 100.0 * stats.lines as f64 / total as f64;
        let line = Line::from(vec![
            Span::styled("██ ", Style::default().fg(lang::color(*lang_))),
            Span::raw(format!("{:<11}", truncate(lang_.0, 11))),
            Span::raw(format!("{:>6}", fmt_compact(stats.files))),
            Span::raw(format!("{:>8}", fmt_compact(stats.lines))),
            Span::styled(
                format!("{:>5}", fmt_pct(pct)),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ── colour helpers ──────────────────────────────────────────────────────────

fn brighten(c: Color, amount: f64) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            let f = amount.clamp(0.0, 1.0);
            let nr = r as f64 + (255.0 - r as f64) * f;
            let ng = g as f64 + (255.0 - g as f64) * f;
            let nb = b as f64 + (255.0 - b as f64) * f;
            Color::Rgb(nr as u8, ng as u8, nb as u8)
        }
        _ => c,
    }
}

fn darken(c: Color, amount: f64) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            let f = (1.0 - amount).clamp(0.0, 1.0);
            Color::Rgb(
                (r as f64 * f) as u8,
                (g as f64 * f) as u8,
                (b as f64 * f) as u8,
            )
        }
        _ => c,
    }
}

fn readable_fg(bg: Color) -> Color {
    let (r, g, b) = match bg {
        Color::Rgb(r, g, b) => (r as f64, g as f64, b as f64),
        _ => return Color::White,
    };
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if lum > 140.0 {
        Color::Black
    } else {
        Color::White
    }
}
