//! DropBridge transfer engine.
//!
//! Streaming, chunked, resumable file movement (spec §36–47):
//!
//! * never loads whole files in RAM (bounded buffers),
//! * BLAKE3 integrity (distinct from transport encryption),
//! * persistent SQLite journal of completed ranges → resume across crashes,
//!   restarts and network changes,
//! * receiver writes `.dropbridge-part` files and atomically renames after
//!   verification,
//! * path sanitization happens at the protocol layer; the receiver still
//!   verifies canonical paths stay under the receive root.
//!
//! The engine is transport-agnostic: it produces/consumes chunk jobs over the
//! [`transport`] traits so it can run over iroh QUIC streams in production
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(windows, deny(unsafe_code))]

pub mod adaptive;
pub mod compression;
pub mod hash;
pub mod journal;
pub mod plan;
pub mod progress;
pub mod ranges;
pub mod recv;
pub mod send;
pub mod transport;

pub use adaptive::{compute_adaptive_chunk_size, NetworkConditions};
pub use plan::{build_manifest, FdSource, TransferSource};
pub use progress::ProgressEstimator;
pub use recv::{CollisionPolicy, ReceiverEngine};
pub use send::{run_sender, SendFile, SendPlan};
pub use transport::{ChunkPayload, ChunkSink, ChunkSource};

/// Chunk data stream header (serialized with postcard at stream start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChunkHeader {
    pub session: u64,
    pub file_id: u32,
    pub chunk_idx: u64,
    pub offset: u64,
    pub len: u32,
    pub is_compressed: bool,
    pub uncompressed_len: u32,
}

impl ChunkHeader {
    #[must_use]
    pub fn raw(session: u64, file_id: u32, chunk_idx: u64, offset: u64, len: u32) -> Self {
        Self {
            session,
            file_id,
            chunk_idx,
            offset,
            len,
            is_compressed: false,
            uncompressed_len: len,
        }
    }
}
