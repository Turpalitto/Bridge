//! Transport layer over iroh + the DropBridge Path Manager.
//!
//! * [`endpoint`] — builds configured iroh endpoints from a device identity
//!   (default n0 relays, self-hosted relays, or relay-disabled LAN mode),
//! * [`streams`] — adapts QUIC streams to the transfer engine's chunk traits,
//! * [`paths`] — scores candidate routes (LAN direct / internet direct /
//!   relay) with hysteresis so we don't flap between nearly-equal paths
//!   (spec §5),
//! * [`hints`] — serializable addressing hints for QR codes, rendezvous and
//!   discovery.
//!
//! Routing reality: iroh itself picks direct vs relay per connection and
//! upgrades relay→direct automatically. The Path Manager adds (a) LAN-first
//! bias using discovered direct addresses, (b) metered-network policy, and
//! (c) the diagnostics users see. We never claim QUIC migration is magic:
//! if a path dies mid-transfer, the transfer layer resumes (spec §6).
#![forbid(unsafe_code)]

pub mod endpoint;
pub mod hints;
pub mod paths;
pub mod streams;

pub use endpoint::{build_endpoint, RelayConfig};
pub use hints::AddrHints;
pub use paths::{PathKind, PathManager, PathMetrics};
pub use streams::{RecvChunkStream, SendChunkStream};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("endpoint: {0}")]
    Endpoint(String),
    #[error("connect: {0}")]
    Connect(String),
    #[error("bad hint: {0}")]
    BadHint(String),
    #[error("timeout")]
    Timeout,
}
