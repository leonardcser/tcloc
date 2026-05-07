/// Comma-separated decimal: 1234567 -> "1,234,567".
pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Long byte format: "1.23 MB". Two decimals once past 1 KiB.
pub fn fmt_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let (v, u) = scale_1024(n);
    if u == 0 {
        format!("{} B", n)
    } else {
        format!("{:.2} {}", v, UNITS[u])
    }
}

/// Compact byte format: "1.2M". One decimal, no space, single-letter unit.
pub fn fmt_bytes_short(n: u64) -> String {
    if n < 1024 {
        return format!("{}B", n);
    }
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let (v, u) = scale_1024(n);
    format!("{:.1}{}", v, UNITS[u])
}

/// Compact decimal for legend: "8,742", "12.3K", "6.99M".
pub fn fmt_compact(n: u64) -> String {
    if n < 10_000 {
        return fmt_int(n);
    }
    const UNITS: &[(u64, &str)] = &[
        (1_000_000_000_000, "T"),
        (1_000_000_000, "B"),
        (1_000_000, "M"),
        (1_000, "K"),
    ];
    for &(div, suffix) in UNITS {
        if n >= div {
            let v = n as f64 / div as f64;
            return if v >= 100.0 {
                format!("{:.0}{}", v, suffix)
            } else if v >= 10.0 {
                format!("{:.1}{}", v, suffix)
            } else {
                format!("{:.2}{}", v, suffix)
            };
        }
    }
    fmt_int(n)
}

/// Percentage rounded to fit in 5 chars.
pub fn fmt_pct(p: f64) -> String {
    if p >= 100.0 {
        "100%".to_string()
    } else if p >= 10.0 {
        format!("{:.0}%", p)
    } else {
        format!("{:.1}%", p)
    }
}

/// Truncate to `max` chars, append `…` when shortened.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Truncate from the left, prepending `…` when shortened.
pub fn truncate_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s
            .chars()
            .rev()
            .take(max)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }
    let skip = count - (max - 1);
    let mut out = String::from("…");
    out.extend(s.chars().skip(skip));
    out
}

fn scale_1024(n: u64) -> (f64, usize) {
    let mut v = n as f64;
    let mut u = 0usize;
    while v >= 1024.0 && u < 4 {
        v /= 1024.0;
        u += 1;
    }
    (v, u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_int_basics() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(42), "42");
        assert_eq!(fmt_int(1_000), "1,000");
        assert_eq!(fmt_int(1_234_567), "1,234,567");
        assert_eq!(fmt_int(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2 * 1024), "2.00 KB");
        assert_eq!(fmt_bytes(1024 * 1024 + 512 * 1024), "1.50 MB");
    }

    #[test]
    fn fmt_bytes_short_units() {
        assert_eq!(fmt_bytes_short(0), "0B");
        assert_eq!(fmt_bytes_short(2048), "2.0K");
        assert_eq!(fmt_bytes_short(3 * 1024 * 1024), "3.0M");
    }

    #[test]
    fn fmt_compact_thresholds() {
        assert_eq!(fmt_compact(0), "0");
        assert_eq!(fmt_compact(9_999), "9,999");
        assert_eq!(fmt_compact(10_000), "10.0K");
        assert_eq!(fmt_compact(123_456), "123K");
        assert_eq!(fmt_compact(6_991_170), "6.99M");
    }

    #[test]
    fn fmt_pct_widths() {
        assert_eq!(fmt_pct(100.0), "100%");
        assert_eq!(fmt_pct(50.0), "50%");
        assert_eq!(fmt_pct(9.5), "9.5%");
        assert_eq!(fmt_pct(0.1), "0.1%");
    }

    #[test]
    fn truncate_basic() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("hello", 1), "h");
        assert_eq!(truncate("héllo", 4), "hél…");
    }

    #[test]
    fn truncate_left_basic() {
        assert_eq!(truncate_left("hello", 10), "hello");
        assert_eq!(truncate_left("/very/long/path", 7), "…g/path");
        assert_eq!(truncate_left("abcdef", 4), "…def");
    }
}
