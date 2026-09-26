//! Serializable addressing hints (QR payloads, rendezvous records,
//! discovery TXT records). Everything here is public by design (it is how
//! to *reach* a device, not who to trust).
use std::net::SocketAddr;

use dropbridge_identity::{DeviceId, DeviceIdentity};
use iroh::{EndpointAddr, RelayUrl, TransportAddr};
use serde::{Deserialize, Serialize};

use crate::NetworkError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddrHints {
    /// Relay URLs (strings) where the device can be reached.
    pub relay_urls: Vec<String>,
    /// Direct "ip:port" candidates (typically LAN).
    pub direct: Vec<String>,
}

impl AddrHints {
    /// Collect hints from a live endpoint.
    pub fn from_endpoint(ep: &iroh::Endpoint) -> Self {
        let addr = ep.addr();
        let mut out = Self::default();
        for r in addr.relay_urls() {
            out.relay_urls.push(r.to_string());
        }
        for a in addr.ip_addrs() {
            out.direct.push(a.to_string());
        }
        for s in ep.bound_sockets() {
            if s.ip().is_unspecified() {
                let loopback = format!("127.0.0.1:{}", s.port());
                if !out.direct.contains(&loopback) {
                    out.direct.push(loopback);
                }
            } else {
                let s_str = s.to_string();
                if !out.direct.contains(&s_str) {
                    out.direct.push(s_str);
                }
            }
        }
        out
    }

    pub fn merge(&mut self, other: AddrHints) {
        for r in other.relay_urls {
            if !self.relay_urls.contains(&r) {
                self.relay_urls.push(r);
            }
        }
        for d in other.direct {
            if !self.direct.contains(&d) {
                self.direct.push(d);
            }
        }
    }

    /// Add a discovered LAN socket address.
    pub fn add_direct(&mut self, addr: SocketAddr) {
        let s = addr.to_string();
        if !self.direct.contains(&s) {
            self.direct.push(s);
        }
    }

    /// Build an iroh address for dialing.
    pub fn to_endpoint_addr(&self, device: &DeviceId) -> Result<EndpointAddr, NetworkError> {
        let id = iroh::EndpointId::from_bytes(device)
            .map_err(|e| NetworkError::BadHint(e.to_string()))?;
        let mut addrs: Vec<TransportAddr> = Vec::new();
        for d in &self.direct {
            if let Ok(sa) = d.parse::<SocketAddr>() {
                addrs.push(TransportAddr::Ip(sa));
            }
        }
        for r in &self.relay_urls {
            if let Ok(url) = r.parse::<RelayUrl>() {
                addrs.push(TransportAddr::Relay(url));
            }
        }
        if addrs.is_empty() {
            return Err(NetworkError::BadHint("no usable hints".into()));
        }
        Ok(EndpointAddr::from_parts(id, addrs))
    }

    /// Extract hints from a live connection address.
    pub fn from_endpoint_addr(addr: &EndpointAddr) -> Self {
        let mut out = Self::default();
        for r in addr.relay_urls() {
            out.relay_urls.push(r.to_string());
        }
        for a in addr.ip_addrs() {
            out.direct.push(a.to_string());
        }
        out
    }
}

/// Helper: z32 id string for hints in text records.
pub fn id_z32(id: &DeviceId) -> String {
    // Cheap re-encoding without constructing an identity.
    iroh::EndpointId::from_bytes(id)
        .map(|k| k.to_z32())
        .unwrap_or_default()
}

pub fn parse_z32(s: &str) -> Result<DeviceId, NetworkError> {
    DeviceIdentity::parse_z32(s).map_err(|e| NetworkError::BadHint(e.to_string()))
}
