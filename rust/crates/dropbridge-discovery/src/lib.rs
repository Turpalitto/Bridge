//! LAN discovery for DropBridge (spec §14, §16, §55).
//!
//! Two independent mechanisms, because routers love breaking multicast:
//!
//! 1. **DropBridge beacon** — small signed UDP announcements + "who's there"
//!    probes on a fixed port. Works with broadcast and with unicast probes to
//!    remembered endpoints. Pure tokio/UDP: robust and dependency-light.
//! 2. **mDNS/DNS-SD** (`_dropbridge._udp`) — the standard path; best-effort.
//!
//! Discovery is *event-driven and bounded*: callers browse for a window of
//! time (e.g. when the app opens or a share intent arrives), never with a
//! permanent aggressive scan (spec §55 battery).
#![forbid(unsafe_code)]

pub mod beacon;
pub mod mdns;

use dropbridge_identity::{DeviceId, DeviceIdentity};
use dropbridge_protocol::DeviceKind;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

/// Port used by the DropBridge UDP beacon.
pub const BEACON_PORT: u16 = 47901;

/// Default browse window for interactive discovery.
pub const DEFAULT_BROWSE_WINDOW: Duration = Duration::from_secs(6);

/// How we saw a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoverySource {
    Mdns,
    Beacon,
    /// A previously remembered endpoint answered a unicast probe.
    KnownEndpoint,
}

/// A device seen on the LAN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredPeer {
    pub device_id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    /// The iroh transfer endpoint port the peer listens on.
    pub port: u16,
    /// Address we observed the peer at.
    pub addr: SocketAddr,
    pub source: DiscoverySource,
    /// Peer is currently showing a pairing QR.
    pub pairing: bool,
}

impl DiscoveredPeer {
    /// Merge another sighting of the same device (prefer richer sources).
    pub fn merge(&mut self, other: DiscoveredPeer) {
        self.pairing |= other.pairing;
        if self.port == 0 {
            self.port = other.port;
        }
        // Prefer the freshest address; keep source if mDNS (stable).
        if self.source != DiscoverySource::Mdns {
            self.addr = other.addr;
            self.source = other.source;
        }
    }
}

/// Identity information we advertise about ourselves.
#[derive(Debug, Clone)]
pub struct SelfAdvertisement {
    pub identity: DeviceIdentity,
    pub name: String,
    pub kind: DeviceKind,
    /// UDP port of our iroh endpoint (direct-connect hint).
    pub transfer_port: u16,
    /// Whether we currently show a pairing QR (enroller side).
    pub pairing: bool,
}

/// Announce ourselves on the LAN (beacon + mDNS) until `stop` is dropped.
///
/// Returns a handle; dropping the handle stops announcement.
pub fn announce(adv: SelfAdvertisement) -> std::io::Result<AnnouncerHandle> {
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let beacon_task = tokio::spawn(beacon::announce_loop(adv.clone(), stop_rx.clone()));
    let mdns_task = tokio::spawn(mdns::announce_loop(adv, stop_rx));
    Ok(AnnouncerHandle {
        _beacon: beacon_task,
        _mdns: mdns_task,
        stop: stop_tx,
    })
}

pub struct AnnouncerHandle {
    _beacon: tokio::task::JoinHandle<()>,
    _mdns: tokio::task::JoinHandle<()>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl AnnouncerHandle {
    pub async fn stop(self) {
        let _ = self.stop.send(true);
    }
}

/// Browse for DropBridge peers for a bounded window.
///
/// `known_endpoints` are previous addresses of peers we already paired with;
/// they get direct unicast probes (spec §16 fallback chain).
pub async fn browse(window: Duration, known_endpoints: Vec<SocketAddr>) -> Vec<DiscoveredPeer> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DiscoveredPeer>();

    let b = tokio::spawn(beacon::browse_loop(window, known_endpoints, tx.clone()));
    let m = tokio::spawn(mdns::browse_loop(window, tx));

    let deadline = tokio::time::Instant::now() + window;
    let mut found: Vec<DiscoveredPeer> = Vec::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            peer = rx.recv() => {
                let Some(peer) = peer else { break };
                match found.iter_mut().find(|p| p.device_id == peer.device_id) {
                    Some(existing) => existing.merge(peer),
                    None => found.push(peer),
                }
            }
        }
    }
    b.abort();
    m.abort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prefers() {
        let a = DiscoveredPeer {
            device_id: [1; 32],
            name: "x".into(),
            kind: DeviceKind::Phone,
            port: 0,
            addr: "10.0.0.2:1".parse().unwrap(),
            source: DiscoverySource::Beacon,
            pairing: false,
        };
        let b = DiscoveredPeer {
            port: 4242,
            pairing: true,
            addr: "10.0.0.3:2".parse().unwrap(),
            source: DiscoverySource::Mdns,
            ..a.clone()
        };
        let mut m = a;
        m.merge(b);
        assert_eq!(m.port, 4242);
        assert!(m.pairing);
        assert_eq!(m.source, DiscoverySource::Mdns);
    }
}
