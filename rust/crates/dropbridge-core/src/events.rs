//! User-facing events emitted by the node.
use std::path::PathBuf;

use dropbridge_discovery::DiscoveredPeer;
use dropbridge_identity::DeviceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeEvent {
    /// A paired device was seen on the LAN.
    DeviceDiscovered(DiscoveredPeer),
    /// Someone wants to send us files.
    IncomingOffer {
        peer: DeviceId,
        peer_name: String,
        session: u64,
        files: usize,
        total_bytes: u64,
    },
    TransferProgress {
        session: u64,
        /// Bytes transferred since the previous event.
        bytes_delta: u64,
        /// Total bytes known so far (0 until accepted).
        total_bytes: u64,
    },
    TransferCompleted {
        session: u64,
        ok: bool,
        /// Final file locations (receive side).
        files: Vec<PathBuf>,
        detail: String,
    },
    /// Pairing flow needs the user (enroller side).
    PairingChallenge {
        device_name: String,
        device_kind: String,
        auth_code: u32,
    },
    /// A send attempt failed (outbox retries, network errors).
    TransferFailed { session: u64, reason: String },
    /// A device was added to (or removed from) the trust store.
    TrustChanged {
        device_id: DeviceId,
        name: String,
        trusted: bool,
    },
}
