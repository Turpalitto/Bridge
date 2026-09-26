//! DropBridge UDP beacon: signed announcements + "who's there" probes.
//!
//! Wire format: JSON, ≤ 2 KB, on [`super::BEACON_PORT`].
//!
//! * `Adv` — periodic signed announcement from an announcer (sent to the
//!   broadcast address; also the reply to a `Who`).
//! * `Who` — browser probe, broadcast and/or unicast to known endpoints.
//!
//! The signature authenticates the *announcer's identity*: a LAN attacker can
//! claim to be "Home Laptop", but only the device holding the private key can
//! produce a valid signature for that device id. All further communication is
//! authenticated at the QUIC layer anyway; the beacon only decides *where* to
//! dial, never *who to trust*.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use dropbridge_identity::{DeviceId, DeviceIdentity};
use dropbridge_protocol::DeviceKind;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::{DiscoveredPeer, DiscoverySource, SelfAdvertisement, BEACON_PORT};

const MAX_BEACON_DATAGRAM: usize = 2048;
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Serialize, Deserialize)]
struct AdvPayload {
    proto: String,
    /// z-base-32 device id.
    id: String,
    name: String,
    kind: String,
    port: u16,
    pair: bool,
    /// unix ms, replay-window guard (informational).
    ts: i64,
    /// base64 signature over postcard(AdvSigned).
    sig: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AdvSigned {
    id: String,
    name: String,
    kind: String,
    port: u16,
    pair: bool,
    ts: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct WhoPayload {
    proto: String,
    /// Optional specific target (z32); empty = any DropBridge device.
    target: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum BeaconMsg {
    Adv(AdvPayload),
    Who(WhoPayload),
}

const PROTO_ADV: &str = "dropbridge/adv/1";
const PROTO_WHO: &str = "dropbridge/who/1";

fn kind_str(k: DeviceKind) -> &'static str {
    k.as_str()
}

fn parse_kind(s: &str) -> DeviceKind {
    match s {
        "phone" => DeviceKind::Phone,
        "tablet" => DeviceKind::Tablet,
        "laptop" => DeviceKind::Laptop,
        "desktop" => DeviceKind::Desktop,
        _ => DeviceKind::Other,
    }
}

fn build_adv(adv: &SelfAdvertisement, ts: i64) -> Option<Vec<u8>> {
    let id = adv.identity.device_id_z32();
    let signed = AdvSigned {
        id: id.clone(),
        name: adv.name.clone(),
        kind: kind_str(adv.kind).to_string(),
        port: adv.transfer_port,
        pair: adv.pairing,
        ts,
    };
    let canon = postcard_bytes(&signed)?;
    let sig = adv.identity.sign(&canon);
    let msg = AdvPayload {
        proto: PROTO_ADV.into(),
        id,
        name: adv.name.clone(),
        kind: kind_str(adv.kind).to_string(),
        port: adv.transfer_port,
        pair: adv.pairing,
        ts,
        sig: data_encoding::BASE64.encode(&sig),
    };
    serde_json::to_vec(&msg).ok()
}

fn postcard_bytes<T: serde::Serialize>(v: &T) -> Option<Vec<u8>> {
    postcard::to_allocvec(v).ok()
}

fn verify_adv(p: &AdvPayload) -> Option<DeviceId> {
    let signed = AdvSigned {
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        port: p.port,
        pair: p.pair,
        ts: p.ts,
    };
    let canon = postcard_bytes(&signed)?;
    let sig: [u8; 64] = data_encoding::BASE64
        .decode(p.sig.as_bytes())
        .ok()?
        .try_into()
        .ok()?;
    let id = DeviceIdentity::parse_z32(&p.id).ok()?;
    DeviceIdentity::verify(&id, &canon, &sig).ok()?;
    Some(id)
}

fn parse_adv(p: AdvPayload, from: SocketAddr) -> Option<DiscoveredPeer> {
    let device_id = verify_adv(&p)?;
    Some(DiscoveredPeer {
        device_id,
        name: p.name,
        kind: parse_kind(&p.kind),
        port: p.port,
        addr: SocketAddr::new(from.ip(), p.port),
        source: DiscoverySource::Beacon,
        pairing: p.pair,
    })
}

/// Announcer side: own BEACON_PORT, answer probes, periodic broadcast.
pub async fn announce_loop(adv: SelfAdvertisement, mut stop: tokio::sync::watch::Receiver<bool>) {
    let sock = match tokio::net::UdpSocket::bind(("0.0.0.0", BEACON_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "beacon: cannot bind, LAN announcement disabled");
            return;
        }
    };
    sock.set_broadcast(true).ok();
    let bcast: SocketAddr = (Ipv4Addr::BROADCAST, BEACON_PORT).into();
    let mut ticker = tokio::time::interval(ANNOUNCE_INTERVAL);
    let mut buf = vec![0u8; MAX_BEACON_DATAGRAM];

    loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() { return; }
            }
            _ = ticker.tick() => {
                let ts = now_ms();
                if let Some(pkt) = build_adv(&adv, ts) {
                    let _ = sock.send_to(&pkt, bcast).await;
                }
            }
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                handle_probe_packet(&adv, &sock, &buf[..n], from).await;
            }
        }
    }
}

async fn handle_probe_packet(
    adv: &SelfAdvertisement,
    sock: &tokio::net::UdpSocket,
    data: &[u8],
    from: SocketAddr,
) {
    let Ok(msg) = serde_json::from_slice::<BeaconMsg>(data) else {
        return;
    };
    let BeaconMsg::Who(w) = msg else { return };
    if w.proto != PROTO_WHO {
        return;
    }
    if !w.target.is_empty() && w.target != adv.identity.device_id_z32() {
        return;
    }
    if let Some(pkt) = build_adv(adv, now_ms()) {
        trace!(%from, "beacon: answering who");
        let _ = sock.send_to(&pkt, from).await;
    }
}

/// Standard gateways and directed broadcasts for Android tethering, Wi-Fi Direct GO, and Windows Hotspot.
pub const HOTSPOT_GATEWAYS: &[&str] = &[
    "192.168.43.1",
    "192.168.49.1",
    "192.168.137.1",
    "192.168.43.255",
    "192.168.49.255",
    "192.168.137.255",
];

/// Browser side: ephemeral socket, broadcast + unicast probes, collect Adv.
pub async fn browse_loop(
    window: Duration,
    known_endpoints: Vec<SocketAddr>,
    tx: tokio::sync::mpsc::UnboundedSender<DiscoveredPeer>,
) {
    let sock = match tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "beacon: browse bind failed");
            return;
        }
    };
    sock.set_broadcast(true).ok();
    let me = adv_local_identity_probe_target();
    let bcast: SocketAddr = (Ipv4Addr::BROADCAST, BEACON_PORT).into();
    let mut probe_targets = known_endpoints;
    for gw in HOTSPOT_GATEWAYS {
        if let Ok(ip) = gw.parse::<std::net::IpAddr>() {
            probe_targets.push(SocketAddr::new(ip, BEACON_PORT));
        }
    }
    let deadline = tokio::time::Instant::now() + window;
    let mut ticker = tokio::time::interval(PROBE_INTERVAL);
    let mut buf = vec![0u8; MAX_BEACON_DATAGRAM];

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return,
            _ = ticker.tick() => {
                let who = serde_json::to_vec(&WhoPayload { proto: PROTO_WHO.into(), target: me.clone() }).unwrap_or_default();
                let _ = sock.send_to(&who, bcast).await;
                for target in &probe_targets {
                    let _ = sock.send_to(&who, target).await;
                }
            }
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                if let Ok(BeaconMsg::Adv(adv)) = serde_json::from_slice::<BeaconMsg>(&buf[..n]) {
                    if adv.proto == PROTO_ADV {
                        if let Some(peer) = parse_adv(adv, from) {
                            let _ = tx.send(peer);
                        }
                    }
                }
            }
        }
    }
}

/// No specific target by default.
fn adv_local_identity_probe_target() -> String {
    String::new()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Best-effort: first non-loopback IPv4 (used only as a hint in logs/tests).
#[allow(dead_code)]
pub fn first_local_ipv4() -> Option<IpAddr> {
    // Avoid heavy deps: try a UDP connect to a public address to select the
    // egress interface; no packets are sent for an unconnected UDP socket.
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("198.18.0.1:80").ok()?;
    let addr = sock.local_addr().ok()?;
    match addr.ip() {
        IpAddr::V4(v4) => Some(IpAddr::V4(v4)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dropbridge_protocol::DeviceKind;

    fn self_adv() -> SelfAdvertisement {
        SelfAdvertisement {
            identity: DeviceIdentity::generate(),
            name: "Test Laptop".into(),
            kind: DeviceKind::Laptop,
            transfer_port: 12345,
            pairing: true,
        }
    }

    #[test]
    fn adv_signs_and_verifies() {
        let adv = self_adv();
        let pkt = build_adv(&adv, now_ms()).unwrap();
        let parsed: BeaconMsg = serde_json::from_slice(&pkt).unwrap();
        let BeaconMsg::Adv(p) = parsed else { panic!() };
        let peer = parse_adv(p, "10.0.0.5:47901".parse().unwrap()).unwrap();
        assert_eq!(peer.device_id, adv.identity.device_id());
        assert!(peer.pairing);
        assert_eq!(peer.port, 12345);
    }

    #[test]
    fn forged_adv_rejected() {
        let adv = self_adv();
        let pkt = build_adv(&adv, now_ms()).unwrap();
        let mut parsed: AdvPayload = serde_json::from_slice(&pkt).unwrap();
        parsed.name = "Evil Laptop".into(); // tamper
        assert!(parse_adv(parsed, "10.0.0.9:1".parse().unwrap()).is_none());
    }

    #[tokio::test]
    async fn probe_and_answer_on_loopback() {
        // Announcer bound to BEACON_PORT on loopback-only address.
        let sock = tokio::net::UdpSocket::bind(("127.0.0.1", BEACON_PORT))
            .await
            .unwrap();
        let adv = self_adv();
        let pkt = build_adv(&adv, now_ms()).unwrap();

        // Browser socket.
        let browser = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let who = serde_json::to_vec(&WhoPayload {
            proto: PROTO_WHO.into(),
            target: String::new(),
        })
        .unwrap();
        browser
            .send_to(&who, ("127.0.0.1", BEACON_PORT))
            .await
            .unwrap();

        let mut buf = [0u8; MAX_BEACON_DATAGRAM];
        let (n, from) = sock.recv_from(&mut buf).await.unwrap();
        assert!(serde_json::from_slice::<BeaconMsg>(&buf[..n]).is_ok());
        sock.send_to(&pkt, from).await.unwrap();

        let (n2, from2) = browser.recv_from(&mut buf).await.unwrap();
        let BeaconMsg::Adv(p) = serde_json::from_slice::<BeaconMsg>(&buf[..n2]).unwrap() else {
            panic!()
        };
        let peer = parse_adv(p, from2).unwrap();
        assert_eq!(peer.device_id, adv.identity.device_id());
    }
}
