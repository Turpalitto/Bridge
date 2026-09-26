//! DropBridge core orchestration.
//!
//! Ties identity + trust, discovery, transport and the transfer engine into
//! one [`Node`] with a small event surface that UI layers (Flutter FFI, CLI,
//! Windows tray) consume:
//!
//! * accountless QR pairing (both roles),
//! * incoming/outgoing transfer sessions with trust checks,
//! * LAN discovery merge + addressing hints,
//! * resume via the persistent journal.
#![forbid(unsafe_code)]

pub mod config;
pub mod events;
pub mod node;
pub mod pairing;
pub mod session;
pub mod sync;
pub mod watcher;

pub use config::NodeConfig;
pub use events::NodeEvent;
pub use node::Node;

/// Error type used across the core.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("identity: {0}")]
    Identity(#[from] dropbridge_identity::IdentityError),
    #[error("network: {0}")]
    Network(#[from] dropbridge_network::NetworkError),
    #[error("journal: {0}")]
    Journal(#[from] dropbridge_transfer::journal::JournalError),
    #[error("plan: {0}")]
    Plan(#[from] dropbridge_transfer::plan::PlanError),
    #[error("recv: {0}")]
    Recv(#[from] dropbridge_transfer::recv::RecvError),
    #[error("transport: {0}")]
    Transport(#[from] dropbridge_transfer::transport::TransportError),
    #[error("protocol: {0}")]
    Protocol(#[from] dropbridge_protocol::ProtocolError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not trusted")]
    NotTrusted,
    #[error("peer rejected: {0}")]
    Rejected(String),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("timeout")]
    Timeout,
    #[error("{0}")]
    Other(String),
}
