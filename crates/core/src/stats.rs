//! Rolling latency statistics and an append-only CSV log of ping samples.

use crate::{ConnState, NodeId};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
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

/// Appends one line per ping sample to `latency.csv`:
/// `unix_ms,peer_id,peer_name,path,rtt_ms`.
pub(crate) struct LatencyLog {
    file: Option<File>,
}

impl LatencyLog {
    pub fn open(path: &Path) -> Self {
        let is_new = !path.exists();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| {
                if is_new {
                    writeln!(f, "unix_ms,peer_id,peer_name,path,rtt_ms")?;
                }
                Ok(f)
            });
        match file {
            Ok(f) => {
                tracing::info!(path = %path.display(), "logging latency samples");
                Self { file: Some(f) }
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), "latency log disabled: {e}");
                Self { file: None }
            }
        }
    }

    pub fn record(&mut self, peer: &NodeId, name: Option<&str>, state: ConnState, rtt_ms: f32) {
        let Some(f) = self.file.as_mut() else { return };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let name = name.unwrap_or("").replace([',', '\n', '\r'], " ");
        let path = match state {
            ConnState::Direct => "direct",
            ConnState::Relayed => "relayed",
            ConnState::Connecting => "connecting",
            ConnState::Disconnected => "disconnected",
        };
        if let Err(e) = writeln!(f, "{ts},{peer},{name},{path},{rtt_ms:.2}") {
            tracing::warn!("latency log write failed, disabling: {e}");
            self.file = None;
        }
    }
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
}
