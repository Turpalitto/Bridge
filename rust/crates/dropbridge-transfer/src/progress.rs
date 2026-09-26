//! Smoothed speed and ETA calculation (spec §62, derived from V1 transfer engine).
use std::time::Instant;

/// Moving-average speed (bytes/sec) and ETA calculator.
#[derive(Debug, Clone)]
pub struct ProgressEstimator {
    last_sample_time: Option<Instant>,
    last_sample_bytes: u64,
    speed_bps: f64,
}

impl Default for ProgressEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressEstimator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_sample_time: None,
            last_sample_bytes: 0,
            speed_bps: 0.0,
        }
    }

    /// Feed the current cumulative transferred bytes and total bytes.
    /// Returns `(smoothed_bytes_per_sec, Option<eta_seconds>)`.
    pub fn update(&mut self, done_bytes: u64, total_bytes: u64) -> (f64, Option<u64>) {
        let now = Instant::now();
        match self.last_sample_time {
            None => {
                self.last_sample_time = Some(now);
                self.last_sample_bytes = done_bytes;
            }
            Some(t) => {
                let elapsed = now.duration_since(t).as_secs_f64();
                if elapsed >= 0.25 {
                    let inst = (done_bytes.saturating_sub(self.last_sample_bytes) as f64) / elapsed;
                    if self.speed_bps == 0.0 {
                        self.speed_bps = inst;
                    } else {
                        // V1 exponential smoothing: speed = speed * 0.6 + inst * 0.4
                        self.speed_bps = self.speed_bps * 0.6 + inst * 0.4;
                    }
                    self.last_sample_bytes = done_bytes;
                    self.last_sample_time = Some(now);
                }
            }
        }

        let remaining = total_bytes.saturating_sub(done_bytes);
        let eta_sec = if self.speed_bps > 1024.0 && remaining > 0 {
            Some((remaining as f64 / self.speed_bps).ceil() as u64)
        } else {
            None
        };
        (self.speed_bps, eta_sec)
    }

    /// Current smoothed speed in bytes/second.
    #[must_use]
    pub fn speed_bps(&self) -> f64 {
        self.speed_bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn estimator_computes_speed_and_eta() {
        let mut est = ProgressEstimator::new();
        let (speed, eta) = est.update(0, 10_000_000);
        assert_eq!(speed, 0.0);
        assert_eq!(eta, None);

        // Manually simulate a 1-second sample
        est.last_sample_time = Some(Instant::now() - Duration::from_secs(1));
        let (speed, eta) = est.update(1_000_000, 10_000_000);
        assert!(speed > 900_000.0 && speed < 1_100_000.0);
        assert!(eta.is_some());
        let eta_val = eta.unwrap();
        assert!((8..=10).contains(&eta_val));
    }
}
