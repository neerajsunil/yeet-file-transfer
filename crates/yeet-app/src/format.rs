//! Human-friendly numbers, Apple style (decimal units).

/// A size, e.g. "2.3 MB".
pub fn bytes(n: u64) -> String {
    let n = n as f64;
    if n < 1e3 {
        format!("{n:.0} bytes")
    } else if n < 1e6 {
        format!("{:.0} KB", n / 1e3)
    } else if n < 1e9 {
        format!("{:.1} MB", n / 1e6)
    } else {
        format!("{:.2} GB", n / 1e9)
    }
}

/// A speed, e.g. "112 MB/s".
pub fn rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1e9 {
        format!("{:.2} GB/s", bytes_per_sec / 1e9)
    } else if bytes_per_sec >= 1e6 {
        format!("{:.0} MB/s", bytes_per_sec / 1e6)
    } else {
        format!("{:.0} KB/s", bytes_per_sec / 1e3)
    }
}

/// A rough duration, e.g. "2 min 5 s".
pub fn duration(secs: f64) -> String {
    let s = secs.round().max(1.0) as u64;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{} h {} min", s / 3600, s % 3600 / 60)
    }
}

/// 66077 -> "66,077".
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1 file", "1,200 files".
pub fn files(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", count(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(bytes(999), "999 bytes");
        assert_eq!(bytes(1_500), "2 KB");
        assert_eq!(bytes(2_340_000), "2.3 MB");
        assert_eq!(bytes(1_250_000_000), "1.25 GB");
        assert_eq!(rate(112e6), "112 MB/s");
    }

    #[test]
    fn durations_round_and_never_show_zero() {
        assert_eq!(duration(0.2), "1 s");
        assert_eq!(duration(125.0), "2 min 5 s");
        assert_eq!(duration(3_700.0), "1 h 1 min");
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(count(66_077), "66,077");
        assert_eq!(count(1_000_000), "1,000,000");
        assert_eq!(files(1), "1 file");
        assert_eq!(files(1_200), "1,200 files");
    }
}
