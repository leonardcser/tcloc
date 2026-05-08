use smelt_term::Rect;

#[derive(Debug, Clone)]
pub struct Item<T> {
    pub value: f64,
    pub data: T,
}

#[derive(Debug, Clone)]
pub struct Tile<T> {
    pub rect: Rect,
    pub data: T,
}

pub fn squarify_into<T: Clone>(items: &[Item<T>], area: Rect, out: &mut Vec<Tile<T>>) {
    out.clear();
    if items.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }

    let total_value: f64 = items.iter().map(|i| i.value).sum();
    if total_value <= 0.0 {
        return;
    }
    let total_area = (area.width as f64) * (area.height as f64);
    let scale = total_area / total_value;

    let mut sorted: Vec<(f64, T)> = items
        .iter()
        .map(|i| (i.value * scale, i.data.clone()))
        .collect();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    out.reserve(items.len());
    let bounds = FRect {
        x: area.left as f64,
        y: area.top as f64,
        w: area.width as f64,
        h: area.height as f64,
    };
    layout(&sorted, bounds, out);
}

#[derive(Clone, Copy)]
struct FRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn layout<T: Clone>(items: &[(f64, T)], mut bounds: FRect, out: &mut Vec<Tile<T>>) {
    let mut i = 0;
    while i < items.len() {
        let short = bounds.w.min(bounds.h);
        if short <= 0.0 {
            return;
        }
        let mut row_end = i + 1;
        let mut best_ratio = worst_ratio(&items[i..row_end], short);
        while row_end < items.len() {
            let cand = worst_ratio(&items[i..row_end + 1], short);
            if cand <= best_ratio {
                best_ratio = cand;
                row_end += 1;
            } else {
                break;
            }
        }
        place_row(&items[i..row_end], &mut bounds, out);
        i = row_end;
    }
}

fn worst_ratio<T>(row: &[(f64, T)], short: f64) -> f64 {
    let sum: f64 = row.iter().map(|(v, _)| *v).sum();
    if sum <= 0.0 {
        return f64::INFINITY;
    }
    let mut max = f64::NEG_INFINITY;
    let mut min = f64::INFINITY;
    for (v, _) in row {
        if *v > max {
            max = *v;
        }
        if *v < min {
            min = *v;
        }
    }
    let s2 = short * short;
    let sum2 = sum * sum;
    (s2 * max / sum2).max(sum2 / (s2 * min))
}

fn place_row<T: Clone>(row: &[(f64, T)], bounds: &mut FRect, out: &mut Vec<Tile<T>>) {
    let sum: f64 = row.iter().map(|(v, _)| *v).sum();
    if sum <= 0.0 {
        return;
    }
    let horizontal = bounds.w <= bounds.h;
    if horizontal {
        let row_h = (sum / bounds.w).min(bounds.h);
        let mut x = bounds.x;
        for (v, data) in row {
            let w = v / row_h;
            push_tile(out, x, bounds.y, w, row_h, data.clone());
            x += w;
        }
        bounds.y += row_h;
        bounds.h -= row_h;
    } else {
        let row_w = (sum / bounds.h).min(bounds.w);
        let mut y = bounds.y;
        for (v, data) in row {
            let h = v / row_w;
            push_tile(out, bounds.x, y, row_w, h, data.clone());
            y += h;
        }
        bounds.x += row_w;
        bounds.w -= row_w;
    }
}

fn push_tile<T>(out: &mut Vec<Tile<T>>, x: f64, y: f64, w: f64, h: f64, data: T) {
    let xi = x.round() as i32;
    let yi = y.round() as i32;
    let x2 = (x + w).round() as i32;
    let y2 = (y + h).round() as i32;
    let width = (x2 - xi).max(0) as u16;
    let height = (y2 - yi).max(0) as u16;
    if width == 0 || height == 0 {
        return;
    }
    out.push(Tile {
        rect: Rect {
            top: yi.max(0) as u16,
            left: xi.max(0) as u16,
            width,
            height,
        },
        data,
    });
}
