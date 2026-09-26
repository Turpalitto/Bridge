//! Endpoint construction.
use dropbridge_identity::DeviceIdentity;
use dropbridge_protocol::{ALPN_PAIRING, ALPN_PROBE, ALPN_TRANSFER};
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayMode};

use crate::NetworkError;

/// Relay strategy for this endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RelayConfig {
    /// Use the public n0 relays (default for consumer builds).
    N0Default,
    /// Self-hosted relays (spec §21) — a list of relay URLs.
    Custom(Vec<String>),
    /// LAN-only/offline mode: no relay at all.
    Disabled,
}

/// All DropBridge ALPNs.
pub fn all_alpns() -> Vec<Vec<u8>> {
    vec![
        ALPN_TRANSFER.to_vec(),
        ALPN_PAIRING.to_vec(),
        ALPN_PROBE.to_vec(),
    ]
}

/// Build a bound iroh endpoint for this device identity.
pub async fn build_endpoint(
    identity: &DeviceIdentity,
    relay: &RelayConfig,
    alpns: Vec<Vec<u8>>,
    fixed_port: Option<u16>,
) -> Result<Endpoint, NetworkError> {
    for attempt in 0..20 {
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(identity.secret_key().clone())
            .alpns(alpns.clone());
        if let Some(port) = fixed_port {
            builder = builder
                .bind_addr(("0.0.0.0", port))
                .map_err(|e| NetworkError::Endpoint(e.to_string()))?;
        }
        let builder = match relay {
            RelayConfig::N0Default => builder.relay_mode(RelayMode::Default),
            RelayConfig::Custom(urls) => {
                let mut parsed = Vec::with_capacity(urls.len());
                for u in urls {
                    parsed.push(
                        u.parse()
                            .map_err(|e| NetworkError::BadHint(format!("relay url {u}: {e}")))?,
                    );
                }
                builder.relay_mode(RelayMode::custom(parsed))
            }
            RelayConfig::Disabled => builder.relay_mode(RelayMode::Disabled),
        };
        match builder.bind().await {
            Ok(ep) => return Ok(ep),
            Err(e) if fixed_port.is_some() && attempt < 19 => {
                eprintln!("bind attempt {attempt} failed on port {fixed_port:?}: {e:?}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(e) => {
                eprintln!("final bind failed on port {fixed_port:?}: {e:?}");
                return Err(NetworkError::Endpoint(e.to_string()));
            }
        }
    }
    Err(NetworkError::Endpoint("Failed to bind sockets".into()))
}

/// Direct socket addresses this endpoint is listening on (LAN hints).
pub fn listening_sockets(ep: &Endpoint) -> Vec<std::net::SocketAddr> {
    ep.bound_sockets()
}
