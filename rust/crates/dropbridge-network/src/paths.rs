//! Path Manager: scoring + hysteresis (spec §4–5).
//!
//! The actual route of a connection is negotiated by iroh (direct vs relay,
//! hole punching, upgrades). The Path Manager sits above it and answers:
//!
//! * which candidate kind should we *prefer* right now,
//! * should we switch, given hysteresis (no flapping on 1% differences),
//! * what to show in diagnostics (spec §61).
//!
//! Inputs are real measurements: probe RTTs (application-level ping/pong),
//! observed throughput from the transfer engine, metered status from the OS.
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PathKind {
    /// Dedicated Wi-Fi Direct P2P link (zero router congestion, lowest jitter).
    WifiDirectP2P,
    /// Local Mobile Hotspot link (e.g. phone tethering AP or Windows Mobile Hotspot).
    LocalHotspot,
    /// Direct connection on the local network (lowest latency, unmetered).
    LanDirect,
    /// Direct internet P2P (hole-punched or public endpoints).
    InternetDirect,
    /// Encrypted relay fallback.
    Relay,
}

impl PathKind {
    /// Base score before measurements. Encodes the conceptual preference:
    /// Wi-Fi Direct / Hotspot → LAN → Internet P2P → relay (spec §4).
    const fn base(self) -> f64 {
        match self {
            PathKind::WifiDirectP2P => 35.0,
            PathKind::LocalHotspot => 34.0,
            PathKind::LanDirect => 30.0,
            PathKind::InternetDirect => 26.0,
            PathKind::Relay => 10.0,
        }
    }
}

/// Measured/estimated properties of a candidate path.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct PathMetrics {
    /// Round-trip time in milliseconds (probe-measured).
    pub rtt_ms: f64,
    /// Measured throughput in Mbit/s, if we have a sample.
    pub measured_mbps: Option<f64>,
    /// Connection setup latency in milliseconds.
    pub setup_ms: f64,
    /// Observed packet loss percent (0–100).
    pub loss_pct: f64,
    /// The OS reports this network as metered (mobile data etc.).
    pub metered: bool,
    /// Candidate currently usable.
    pub available: bool,
}

impl PathMetrics {
    /// Convert path metrics into adaptive chunk sizing conditions.
    #[must_use]
    pub fn to_network_conditions(&self, kind: PathKind) -> dropbridge_transfer::NetworkConditions {
        let is_lan = matches!(
            kind,
            PathKind::WifiDirectP2P | PathKind::LocalHotspot | PathKind::LanDirect
        );
        dropbridge_transfer::NetworkConditions {
            rtt_ms: Some(self.rtt_ms),
            mbps: self.measured_mbps,
            loss_pct: Some(self.loss_pct),
            is_lan,
        }
    }
}

/// Score a candidate. Higher is better.
///
/// The shape deliberately saturates: a 10 Gbit link scores only marginally
/// better than 1 Gbit, while RTT and loss keep mattering — raw bandwidth is
/// not the whole story for interactive "drop" UX.
pub fn score(kind: PathKind, m: &PathMetrics) -> f64 {
    if !m.available {
        return f64::NEG_INFINITY;
    }
    let mut s = kind.base();
    if let Some(mbps) = m.measured_mbps {
        let capped = mbps.clamp(0.0, 2000.0);
        s += (capped / 2000.0) * 50.0;
    }
    s -= (m.rtt_ms.clamp(0.0, 500.0) / 500.0) * 30.0;
    s -= m.loss_pct.clamp(0.0, 100.0) * 1.5;
    s -= (m.setup_ms.clamp(0.0, 5000.0) / 5000.0) * 10.0;
    if m.metered {
        s -= 25.0;
    }
    s
}

/// Relative advantage needed before switching away from the current path
/// (spec §5 hysteresis).
pub const SWITCH_MARGIN: f64 = 1.15;

pub struct PathManager {
    metrics: HashMap<PathKind, PathMetrics>,
    current: Option<PathKind>,
}

impl Default for PathManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PathManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            metrics: HashMap::new(),
            current: None,
        }
    }

    /// Feed a fresh measurement for a candidate.
    pub fn observe(&mut self, kind: PathKind, m: PathMetrics) {
        self.metrics.insert(kind, m);
    }

    /// Mark a candidate as down (e.g. connection died).
    pub fn mark_down(&mut self, kind: PathKind) {
        if let Some(m) = self.metrics.get_mut(&kind) {
            m.available = false;
        }
        if self.current == Some(kind) {
            self.current = None;
        }
    }

    pub fn metrics(&self, kind: PathKind) -> Option<&PathMetrics> {
        self.metrics.get(&kind)
    }

    pub fn current(&self) -> Option<PathKind> {
        self.current
    }

    /// Choose the path to prefer. Sticky: keeps the current path unless a
    /// candidate beats it by [`SWITCH_MARGIN`] or the current path is down.
    pub fn choose(&mut self) -> Option<PathKind> {
        let mut best: Option<(PathKind, f64)> = None;
        for (&kind, m) in &self.metrics {
            let s = score(kind, m);
            if best.is_none_or(|(_, best_score)| s > best_score) {
                best = Some((kind, s));
            }
        }
        let (best_kind, best_score) = best?;
        if best_score == f64::NEG_INFINITY {
            self.current = None;
            return None;
        }

        if let Some(cur) = self.current {
            let cur_score = self
                .metrics
                .get(&cur)
                .map(|m| score(cur, m))
                .unwrap_or(f64::NEG_INFINITY);
            // Keep current unless clearly beaten.
            if cur_score != f64::NEG_INFINITY && best_score < cur_score * SWITCH_MARGIN {
                return Some(cur);
            }
        }
        self.current = Some(best_kind);
        Some(best_kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(rtt: f64, mbps: Option<f64>, available: bool) -> PathMetrics {
        PathMetrics {
            rtt_ms: rtt,
            measured_mbps: mbps,
            available,
            ..Default::default()
        }
    }

    #[test]
    fn lan_beats_relay_on_equal_metrics() {
        let mut pm = PathManager::new();
        pm.observe(PathKind::LanDirect, m(3.0, Some(900.0), true));
        pm.observe(PathKind::Relay, m(30.0, Some(200.0), true));
        assert_eq!(pm.choose(), Some(PathKind::LanDirect));
    }

    #[test]
    fn relay_used_when_lan_down() {
        let mut pm = PathManager::new();
        pm.observe(PathKind::LanDirect, m(3.0, Some(900.0), true));
        pm.observe(PathKind::Relay, m(40.0, Some(100.0), true));
        assert_eq!(pm.choose(), Some(PathKind::LanDirect));
        pm.mark_down(PathKind::LanDirect);
        assert_eq!(pm.choose(), Some(PathKind::Relay));
    }

    #[test]
    fn no_flapping_on_small_difference() {
        let mut pm = PathManager::new();
        pm.observe(PathKind::LanDirect, m(5.0, Some(800.0), true));
        pm.observe(PathKind::InternetDirect, m(45.0, Some(780.0), true));
        assert_eq!(pm.choose(), Some(PathKind::LanDirect));
        // Internet improves slightly: still no switch (within margin).
        pm.observe(PathKind::InternetDirect, m(40.0, Some(820.0), true));
        assert_eq!(pm.choose(), Some(PathKind::LanDirect));
        // Internet becomes clearly better: switch happens.
        pm.observe(PathKind::InternetDirect, m(10.0, Some(1500.0), true));
        assert_eq!(pm.choose(), Some(PathKind::InternetDirect));
    }

    #[test]
    fn metered_penalty() {
        let mut pm = PathManager::new();
        let mut lan = m(5.0, Some(500.0), true);
        lan.metered = true;
        pm.observe(PathKind::LanDirect, lan);
        pm.observe(PathKind::InternetDirect, m(40.0, Some(500.0), true));
        assert_eq!(pm.choose(), Some(PathKind::InternetDirect));
    }

    #[test]
    fn unavailable_scores_neg_inf() {
        assert_eq!(
            score(PathKind::LanDirect, &m(1.0, Some(9000.0), false)),
            f64::NEG_INFINITY
        );
    }

    #[test]
    fn wifi_direct_beats_lan_on_equal_metrics() {
        let mut pm = PathManager::new();
        pm.observe(PathKind::LanDirect, m(2.0, Some(900.0), true));
        pm.observe(PathKind::WifiDirectP2P, m(2.0, Some(900.0), true));
        assert_eq!(pm.choose(), Some(PathKind::WifiDirectP2P));
    }
}
