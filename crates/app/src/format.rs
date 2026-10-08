//! Human-readable formatting helpers.

/// `1536` -> "1.5 KB". Binary units, one decimal above bytes.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// Bytes per second, e.g. "12.0 KB/s".
pub fn rate(bytes_per_sec: f64) -> String {
    format!("{}/s", bytes(bytes_per_sec.max(0.0).round() as u64))
}

/// Download progress: "12.3 MB of 40.1 MB (30%)", or "12.3 MB" without a known total.
pub fn progress(done: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => {
            let pct = (done.min(total) * 100) / total;
            format!("{} of {} ({pct}%)", bytes(done), bytes(total))
        }
        _ => bytes(done),
    }
}

/// This computer's name, tidied up for display: "Joaos-MacBook-Pro.local" -> "Joaos MacBook Pro".
pub fn device_name() -> String {
    let raw = gethostname::gethostname().to_string_lossy().into_owned();
    let name = raw
        .strip_suffix(".local")
        .unwrap_or(&raw)
        .replace(['-', '_'], " ");
    let name = name.trim();
    if name.is_empty() {
        "My computer".into()
    } else {
        name.to_string()
    }
}

/// "abcdef…wxyz" for long ids, unchanged for short ones.
pub fn short_id(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() > 12 {
        let head: String = chars[..6].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    } else {
        id.to_string()
    }
}

/// Port typed by the user: 1-65535.
pub fn parse_port(s: &str) -> Option<u16> {
    s.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

/// Time since an event: "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(d: std::time::Duration) -> String {
    let mins = d.as_secs() / 60;
    match mins {
        0 => "just now".into(),
        1..=59 => format!("{mins} min ago"),
        60..=1439 => format!("{} h ago", mins / 60),
        _ => format!("{} d ago", mins / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_progress() {
        assert_eq!(
            progress(3 * 1024 * 1024, Some(10 * 1024 * 1024)),
            "3.0 MB of 10.0 MB (30%)"
        );
        assert_eq!(progress(1536, None), "1.5 KB");
        assert_eq!(progress(0, Some(0)), "0 B");
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(rate(2048.0), "2.0 KB/s");
    }

    #[test]
    fn shortens_ids() {
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("0123456789abcdefXYZW"), "012345…XYZW");
    }

    #[test]
    fn ports() {
        assert_eq!(parse_port(" 25565 "), Some(25565));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("70000"), None);
        assert_eq!(parse_port("abc"), None);
    }

    #[test]
    fn ages() {
        use std::time::Duration;
        assert_eq!(ago(Duration::from_secs(30)), "just now");
        assert_eq!(ago(Duration::from_secs(5 * 60)), "5 min ago");
        assert_eq!(ago(Duration::from_secs(3 * 3600 + 10)), "3 h ago");
        assert_eq!(ago(Duration::from_secs(50 * 3600)), "2 d ago");
    }
}
