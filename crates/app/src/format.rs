//! Human-readable formatting helpers.

use std::time::Duration;

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

/// Coarse duration for "connected for": "<1 min", "12 min", "2 h 5 min", "3 d 4 h".
pub fn duration(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    if mins == 0 {
        "<1 min".into()
    } else if mins < 60 {
        format!("{mins} min")
    } else if mins < 60 * 24 {
        format!("{} h {} min", mins / 60, mins % 60)
    } else {
        format!("{} d {} h", mins / (60 * 24), (mins / 60) % 24)
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

/// Host part of a relay URL: "https://use1-1.relay.n0.iroh.link./" -> "use1-1.relay.n0.iroh.link".
pub fn relay_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?']).next().unwrap_or(rest);
    host.trim_end_matches('.').to_string()
}

/// Port typed by the user: 1-65535.
pub fn parse_port(s: &str) -> Option<u16> {
    s.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn formats_durations() {
        assert_eq!(duration(Duration::from_secs(30)), "<1 min");
        assert_eq!(duration(Duration::from_secs(12 * 60 + 5)), "12 min");
        assert_eq!(duration(Duration::from_secs(125 * 60)), "2 h 5 min");
        assert_eq!(
            duration(Duration::from_secs((24 * 3 + 4) * 3600)),
            "3 d 4 h"
        );
    }

    #[test]
    fn shortens_ids() {
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("0123456789abcdefXYZW"), "012345…XYZW");
    }

    #[test]
    fn relay_hosts() {
        assert_eq!(
            relay_host("https://use1-1.relay.n0.iroh.link./"),
            "use1-1.relay.n0.iroh.link"
        );
        assert_eq!(relay_host("relay.example.com"), "relay.example.com");
    }

    #[test]
    fn ports() {
        assert_eq!(parse_port(" 25565 "), Some(25565));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("70000"), None);
        assert_eq!(parse_port("abc"), None);
    }
}
