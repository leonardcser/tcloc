//! Scaled bitmap labels rendered into the half-block sub-cell colour grid.
//!
//! Each label uses `font8x8` with every "on" pixel expanded to
//! `scale × scale` sub-cells. Painting into the same
//! `Vec<Option<Color>>` the half-block compositor reads from lets every
//! "on" sub-cell become either the top or bottom half of a `▀`/`▄`,
//! mixing naturally with the tile colour.
//!
//! Coordinates are in sub-cells, matching the rest of the renderer.
use smelt_term::grid::Color;

use font8x8::legacy::BASIC_LEGACY;

/// Largest scale the picker may pick. Every scale in `1..=MAX_SCALE` is a
/// valid label size; bigger = more legible but needs more room.
pub const MAX_SCALE: u16 = 3;

const GLYPH_PX: u16 = 8;
const GLYPH_SPACING_PX: u16 = 1;

/// Width in sub-cells `text` would occupy at `scale`.
pub fn label_width(text: &str, scale: u16) -> u16 {
    let n = text.chars().count() as u16;
    if n == 0 || scale == 0 {
        return 0;
    }
    n * GLYPH_PX * scale + (n - 1) * GLYPH_SPACING_PX * scale
}

/// Height in sub-cells a label at `scale` occupies.
pub fn label_height(scale: u16) -> u16 {
    GLYPH_PX * scale
}

/// Paint `text` into `grid` at sub-cell origin `(x, y)`, scaled by
/// `scale`. Pixels outside the grid are clipped.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    grid: &mut [Option<Color>],
    cols: usize,
    sub_rows: usize,
    x: i32,
    y: i32,
    text: &str,
    fg: Color,
    scale: u16,
) {
    if text.is_empty() || scale == 0 {
        return;
    }
    let s = scale as i32;
    let advance = (GLYPH_PX + GLYPH_SPACING_PX) as i32 * s;
    let cols_i = cols as i32;
    let rows_i = sub_rows as i32;
    let mut pen_x = x;
    for ch in text.chars() {
        let idx = ch as usize;
        // Outside ASCII range: render as a question mark so we don't
        // silently swallow missing chars in long file names.
        let glyph = BASIC_LEGACY
            .get(idx)
            .unwrap_or(&BASIC_LEGACY[b'?' as usize]);
        for (row_idx, row) in glyph.iter().enumerate() {
            for col in 0..GLYPH_PX as usize {
                if (row >> col) & 1 == 0 {
                    continue;
                }
                let px0 = pen_x + col as i32 * s;
                let py0 = y + row_idx as i32 * s;
                let cx0 = px0.max(0);
                let cy0 = py0.max(0);
                let cx1 = (px0 + s).min(cols_i);
                let cy1 = (py0 + s).min(rows_i);
                if cx0 >= cx1 || cy0 >= cy1 {
                    continue;
                }
                for sy in cy0..cy1 {
                    let row_base = sy as usize * cols;
                    for sx in cx0..cx1 {
                        grid[row_base + sx as usize] = Some(fg);
                    }
                }
            }
        }
        pen_x += advance;
        if pen_x >= cols_i {
            break;
        }
    }
}
