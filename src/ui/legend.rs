use smelt_term::grid::{Color, GridSlice, Style};

use crate::app::App;
use crate::format::{fmt_compact, fmt_pct, truncate};
use crate::lang;

use super::put_str_clip;

pub(super) fn render_legend(slice: &mut GridSlice<'_>, app: &mut App) {
    let _g = smelt_perf::perf::begin("ui.legend");

    if slice.width() == 0 || slice.height() == 0 {
        return;
    }

    // Slice already lives inside the layout-tree-painted border; its
    // grid_rect is the inner area. Tracked on `App` so the host's
    // mouse handler can detect wheel events inside the legend.
    let inner = slice.grid_rect();
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
