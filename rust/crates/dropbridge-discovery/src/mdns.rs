//! mDNS/DNS-SD adapter: `_dropbridge._udp` service type (spec §14).
//!
//! Best-effort by design: some APs mangle multicast, some platforms restrict
//! it — failures here are logged and swallowed; the beacon path still works.
use std::net::SocketAddr;
use std::time::Duration;

use dropbridge_identity::DeviceIdentity;
use dropbridge_protocol::DeviceKind;
use tracing::{debug, warn};

use crate::{DiscoveredPeer, DiscoverySource, SelfAdvertisement};

pub const SERVICE_TYPE: &str = "_dropbridge._udp.local.";

fn parse_kind(s: &str) -> DeviceKind {
    match s {
        "phone" => DeviceKind::Phone,
        "tablet" => DeviceKind::Tablet,
        "laptop" => DeviceKind::Laptop,
        "desktop" => DeviceKind::Desktop,
        _ => DeviceKind::Other,
    }
}

/// Register a DNS-SD service for ourselves until `stop` flips true.
pub async fn announce_loop(adv: SelfAdvertisement, mut stop: tokio::sync::watch::Receiver<bool>) {
    let result = tokio::task::spawn_blocking(move || register_blocking(adv)).await;
    match result {
        Ok(Ok(md)) => {
            // Stay registered until stopped.
            let _ = stop.wait_for(|v| *v).await;
            let _ = md.shutdown();
        }
        Ok(Err(e)) => debug!(error = %e, "mdns: registration unavailable"),
        Err(_) => {}
    }
}

fn register_blocking(adv: SelfAdvertisement) -> Result<mdns_sd::ServiceDaemon, String> {
    let md = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
    let id = adv.identity.device_id_z32();
    let short = &id[..id.len().min(12)];
    let instance = sanitize_instance(&adv.name);
    let host = format!("dropbridge-{short}.local.");
    let kind = adv.kind.as_str().to_string();
    let pair = if adv.pairing { "1" } else { "0" }.to_string();
    let props: &[(&str, &str)] = &[("id", &id), ("kind", &kind), ("pair", &pair), ("ver", "1")];
    let ip = crate::beacon::first_local_ipv4()
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let service =
        mdns_sd::ServiceInfo::new(SERVICE_TYPE, &instance, &host, ip, adv.transfer_port, props)
            .map_err(|e| e.to_string())?
            .enable_addr_auto();
    md.register(service).map_err(|e| e.to_string())?;
    Ok(md)
}

fn sanitize_instance(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if cleaned.trim().is_empty() {
        "DropBridge".into()
    } else {
        cleaned
    }
}

/// Browse for the service for a bounded window.
pub async fn browse_loop(window: Duration, tx: tokio::sync::mpsc::UnboundedSender<DiscoveredPeer>) {
    let deadline = tokio::time::Instant::now() + window;
    let rx = match tokio::task::spawn_blocking(|| {
        mdns_sd::ServiceDaemon::new().and_then(|md| md.browse(SERVICE_TYPE))
    })
    .await
    {
        Ok(Ok(rx)) => rx,
        Ok(Err(e)) => {
            debug!(error = %e, "mdns: browse unavailable");
            return;
        }
        Err(_) => return,
    };

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let item = match tokio::time::timeout(remaining, rx.recv_async()).await {
            Ok(item) => item,
            Err(_) => break, // deadline
        };
        let Ok(event) = item else { break };
        if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
            let props = info.get_properties();
            let id_z32 = props.get("id").map(|v| v.val_str().to_string());
            let kind = props
                .get("kind")
                .map(|v| parse_kind(v.val_str()))
                .unwrap_or(DeviceKind::Other);
            let pairing = props.get("pair").is_some_and(|v| v.val_str() == "1");
            let Some(id_z32) = id_z32 else { continue };
            let Ok(device_id) = DeviceIdentity::parse_z32(&id_z32) else {
                warn!("mdns: bad device id txt");
                continue;
            };
            let port = info.get_port();
            for ip in info.get_addresses() {
                let addr = SocketAddr::new(ip.to_ip_addr(), port);
                let _ = tx.send(DiscoveredPeer {
                    device_id,
                    name: info
                        .get_fullname()
                        .split('.')
                        .next()
                        .unwrap_or("DropBridge")
                        .into(),
                    kind,
                    port,
                    addr,
                    source: DiscoverySource::Mdns,
                    pairing,
                });
            }
        }
    }
}
