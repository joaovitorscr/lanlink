//! Rolling latency statistics and an optional, daily rotated CSV log of ping samples.

use crate::{ConnState, NodeId};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Number of recent samples the rolling stats cover. With 1s pings this is the last minute.
pub const WINDOW: usize = 60;

/// Latency summary over the last `WINDOW` ping samples. All values in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LatencyStats {
    pub last_ms: f32,
    pub min_ms: f32,
    pub avg_ms: f32,
    pub max_ms: f32,
    /// Mean absolute difference between consecutive samples.
    pub jitter_ms: f32,
    /// Samples currently in the window.
    pub samples: u32,
    /// Total samples received on this connection.
    pub total: u64,
}

#[derive(Debug, Default)]
pub(crate) struct LatencyWindow {
    samples: VecDeque<f32>,
    total: u64,
}

impl LatencyWindow {
    pub fn push(&mut self, ms: f32) -> LatencyStats {
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
        self.total += 1;
        self.stats()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.total = 0;
    }

    pub fn stats(&self) -> LatencyStats {
        let n = self.samples.len();
        if n == 0 {
            return LatencyStats::default();
        }
        let (mut min, mut max, mut sum) = (f32::MAX, 0f32, 0f32);
        for &s in &self.samples {
            min = min.min(s);
            max = max.max(s);
            sum += s;
        }
        let jitter = if n > 1 {
            let diffs: f32 = self
                .samples
                .iter()
                .zip(self.samples.iter().skip(1))
                .map(|(a, b)| (b - a).abs())
                .sum();
            diffs / (n - 1) as f32
        } else {
            0.0
        };
        LatencyStats {
            last_ms: *self.samples.back().unwrap(),
            min_ms: min,
            avg_ms: sum / n as f32,
            max_ms: max,
            jitter_ms: jitter,
            samples: n as u32,
            total: self.total,
        }
    }
}

/// Daily latency files kept in the logs dir, like `init_logging` keeps for log files.
const LATENCY_FILES_KEPT: usize = 7;
const LATENCY_PREFIX: &str = "latency.";
const LATENCY_SUFFIX: &str = ".csv";
const DAY_MS: u128 = 24 * 60 * 60 * 1000;

/// Appends one line per ping sample to `<dir>/latency.<YYYY-MM-DD>.csv` (UTC day):
/// `unix_ms,peer_id,peer_name,path,rtt_ms`. Off unless enabled; keeps the newest 7 files.
pub(crate) struct LatencyLog {
    dir: PathBuf,
    enabled: bool,
    /// Open file and the day (since the unix epoch) it is for.
    file: Option<(u64, File)>,
}

impl LatencyLog {
    pub fn new(dir: PathBuf, enabled: bool) -> Self {
        Self {
            dir,
            enabled,
            file: None,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.file = None;
        }
    }

    pub fn record(&mut self, peer: &NodeId, name: Option<&str>, state: ConnState, rtt_ms: f32) {
        if !self.enabled {
            return;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let day = (ts / DAY_MS) as u64;
        if self.file.as_ref().is_none_or(|(d, _)| *d != day) {
            match self.open_day(day) {
                Ok(f) => self.file = Some((day, f)),
                Err(e) => {
                    tracing::warn!(dir = %self.dir.display(), "latency log disabled: {e}");
                    self.set_enabled(false);
                    return;
                }
            }
        }
        let Some((_, f)) = self.file.as_mut() else {
            return;
        };
        let name = name.unwrap_or("").replace([',', '\n', '\r'], " ");
        let path = match state {
            ConnState::Direct => "direct",
            ConnState::Relayed => "relayed",
            ConnState::Connecting => "connecting",
            ConnState::Disconnected => "disconnected",
        };
        if let Err(e) = writeln!(f, "{ts},{peer},{name},{path},{rtt_ms:.2}") {
            tracing::warn!("latency log write failed, disabling: {e}");
            self.set_enabled(false);
        }
    }

    /// Open (appending) the file for `day`, writing the header if it is new, and prune old files.
    fn open_day(&self, day: u64) -> std::io::Result<File> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!(
            "{LATENCY_PREFIX}{}{LATENCY_SUFFIX}",
            civil_date(day)
        ));
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        if f.metadata()?.len() == 0 {
            writeln!(f, "unix_ms,peer_id,peer_name,path,rtt_ms")?;
        }
        tracing::info!(path = %path.display(), "logging latency samples");
        prune_latency_files(&self.dir, LATENCY_FILES_KEPT);
        Ok(f)
    }
}

/// Delete all but the newest `keep` latency files in `dir`. Best effort.
fn prune_latency_files(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            n.strip_prefix(LATENCY_PREFIX)
                .and_then(|n| n.strip_suffix(LATENCY_SUFFIX))
                .is_some_and(|d| d.len() == 10)
        })
        .collect();
    // YYYY-MM-DD names sort chronologically.
    names.sort();
    let excess = names.len().saturating_sub(keep);
    for n in &names[..excess] {
        if let Err(e) = std::fs::remove_file(dir.join(n)) {
            tracing::debug!("removing old latency log {n}: {e}");
        }
    }
}

/// `YYYY-MM-DD` for a day count since 1970-01-01 (proleptic Gregorian, UTC).
fn civil_date(day: u64) -> String {
    // Howard Hinnant's days_from_civil inverse.
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_stats() {
        let mut w = LatencyWindow::default();
        for ms in [10.0, 20.0, 10.0, 40.0] {
            w.push(ms);
        }
        let s = w.stats();
        assert_eq!(
            (s.min_ms, s.max_ms, s.avg_ms, s.last_ms),
            (10.0, 40.0, 20.0, 40.0)
        );
        assert_eq!(s.jitter_ms, 50.0 / 3.0);
        assert_eq!((s.samples, s.total), (4, 4));
    }

    #[test]
    fn window_rolls() {
        let mut w = LatencyWindow::default();
        for i in 0..(WINDOW + 10) {
            w.push(i as f32);
        }
        let s = w.stats();
        assert_eq!(s.samples as usize, WINDOW);
        assert_eq!(s.total as usize, WINDOW + 10);
        assert_eq!(s.min_ms, 10.0);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_734), "2026-10-08");
    }

    #[test]
    fn prunes_to_newest() {
        let dir = std::env::temp_dir().join(format!("lanlink-latency-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for d in 1..=9 {
            File::create(dir.join(format!("latency.2026-10-{d:02}.csv"))).unwrap();
        }
        File::create(dir.join("app.2026-10-01.log")).unwrap();
        prune_latency_files(&dir, 7);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), 8);
        assert_eq!(left[0], "app.2026-10-01.log");
        assert_eq!(left[1], "latency.2026-10-03.csv");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_off_by_default_and_writes_header_when_on() {
        let dir = std::env::temp_dir().join(format!("lanlink-latlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let peer = iroh::SecretKey::generate().public();
        let mut log = LatencyLog::new(dir.clone(), false);
        log.record(&peer, Some("a"), ConnState::Direct, 1.0);
        assert!(!dir.exists());
        log.set_enabled(true);
        log.record(&peer, Some("a,b"), ConnState::Relayed, 2.5);
        log.record(&peer, None, ConnState::Direct, 3.0);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "unix_ms,peer_id,peer_name,path,rtt_ms");
        assert_eq!(lines.len(), 3);
        assert!(lines[1].ends_with(&format!(",{peer},a b,relayed,2.50")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
