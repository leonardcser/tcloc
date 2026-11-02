use smelt_term::grid::Color;

pub(super) fn brighten(c: Color, amount: f64) -> Color {
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

pub(super) fn darken(c: Color, amount: f64) -> Color {
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

pub(super) fn readable_fg(bg: Color) -> Color {
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
