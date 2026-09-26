//! Adaptive chunk sizing and congestion pacing for 2026-era networks.
//!
//! Dynamically scales chunk size between 256 KiB (lossy, high-latency, or metered
//! links) and 8 MiB (ultra-fast Wi-Fi 6/7, 2.5GbE/10GbE LAN) to minimize syscalls,
//! journal transactions, and QUIC stream overhead.

/// Minimum chunk size (256 KiB): used for lossy links or high RTT.
pub const MIN_ADAPTIVE_CHUNK_SIZE: u64 = 256 * 1024;
/// Standard default chunk size (1 MiB).
pub const DEFAULT_ADAPTIVE_CHUNK_SIZE: u64 = 1024 * 1024;
/// High-speed LAN chunk size (4 MiB): for multi-hundred megabit LAN transfers.
pub const FAST_LAN_CHUNK_SIZE: u64 = 4 * 1024 * 1024;
/// Extreme throughput chunk size (8 MiB): for multi-gigabit Wi-Fi 7 / 2.5GbE.
pub const MAX_ADAPTIVE_CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// Network measurements passed into chunk size planning.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkConditions {
    pub rtt_ms: Option<f64>,
    pub mbps: Option<f64>,
    pub loss_pct: Option<f64>,
    pub is_lan: bool,
}

impl NetworkConditions {
    #[must_use]
    pub fn lan(rtt_ms: f64, mbps: f64) -> Self {
        Self {
            rtt_ms: Some(rtt_ms),
            mbps: Some(mbps),
            loss_pct: Some(0.0),
            is_lan: true,
        }
    }

    #[must_use]
    pub fn wan(rtt_ms: f64, mbps: f64, loss_pct: f64) -> Self {
        Self {
            rtt_ms: Some(rtt_ms),
            mbps: Some(mbps),
            loss_pct: Some(loss_pct),
            is_lan: false,
        }
    }
}

/// Compute optimal chunk size based on network metrics and file volume.
#[must_use]
pub fn compute_adaptive_chunk_size(
    conditions: Option<&NetworkConditions>,
    total_file_bytes: u64,
) -> u64 {
    // For small transfers (< 1 MiB), keep chunks small to avoid memory overhead
    if total_file_bytes < 1024 * 1024 {
        return MIN_ADAPTIVE_CHUNK_SIZE.min(total_file_bytes.max(64 * 1024));
    }

    let Some(net) = conditions else {
        return DEFAULT_ADAPTIVE_CHUNK_SIZE;
    };

    let loss = net.loss_pct.unwrap_or(0.0);
    let rtt = net.rtt_ms.unwrap_or(30.0);
    let mbps = net.mbps.unwrap_or(50.0);

    // High loss (> 2.5%) or high latency (> 100ms): downscale to avoid massive retransmissions
    if loss > 2.5 || rtt > 100.0 || mbps < 25.0 {
        return MIN_ADAPTIVE_CHUNK_SIZE;
    }

    // High throughput LAN / Wi-Fi 6/7: scale up to 4–8 MiB
    if (net.is_lan || rtt < 12.0) && loss < 0.5 {
        if mbps > 800.0 && total_file_bytes >= 64 * 1024 * 1024 {
            return MAX_ADAPTIVE_CHUNK_SIZE;
        }
        if mbps > 200.0 && total_file_bytes >= 16 * 1024 * 1024 {
            return FAST_LAN_CHUNK_SIZE;
        }
    }

    // Standard high-speed internet / moderate LAN
    if mbps > 100.0 && rtt < 45.0 {
        return 2 * 1024 * 1024;
    }

    DEFAULT_ADAPTIVE_CHUNK_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_file_uses_bounded_chunk() {
        let size = compute_adaptive_chunk_size(None, 500 * 1024);
        assert_eq!(size, 256 * 1024);
    }

    #[test]
    fn lossy_link_uses_min_chunk() {
        let cond = NetworkConditions::wan(40.0, 100.0, 5.0);
        let size = compute_adaptive_chunk_size(Some(&cond), 50 * 1024 * 1024);
        assert_eq!(size, MIN_ADAPTIVE_CHUNK_SIZE);
    }

    #[test]
    fn high_rtt_link_uses_min_chunk() {
        let cond = NetworkConditions::wan(150.0, 200.0, 0.1);
        let size = compute_adaptive_chunk_size(Some(&cond), 50 * 1024 * 1024);
        assert_eq!(size, MIN_ADAPTIVE_CHUNK_SIZE);
    }

    #[test]
    fn gigabit_lan_large_file_uses_max_chunk() {
        let cond = NetworkConditions::lan(2.0, 1200.0);
        let size = compute_adaptive_chunk_size(Some(&cond), 100 * 1024 * 1024);
        assert_eq!(size, MAX_ADAPTIVE_CHUNK_SIZE);
    }

    #[test]
    fn moderate_lan_uses_4mib_chunk() {
        let cond = NetworkConditions::lan(5.0, 400.0);
        let size = compute_adaptive_chunk_size(Some(&cond), 32 * 1024 * 1024);
        assert_eq!(size, FAST_LAN_CHUNK_SIZE);
    }
}
