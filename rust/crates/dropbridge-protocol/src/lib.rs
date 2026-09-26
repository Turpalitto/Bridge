//! DropBridge Protocol v1.
//!
//! Versioned, capability-negotiated message protocol spoken over QUIC streams
//! (provided by the transport layer, typically iroh). The protocol separates:
//!
//! * **control channel** — one bidirectional stream of length-prefixed
//!   [`Msg`] frames ([`encode_msg`] / [`FrameDecoder`]),
//! * **data streams** — cheap unidirectional streams carrying
//!   [`ChunkHeader`] + raw chunk bytes,
//! * **pairing channel** — dedicated ALPN with [`PairMsg`] frames.
//!
//! Design goals (spec §48–50): forward compatibility (unknown optional fields
//! are ignored via postcard/serde evolution rules), strict resource limits
//! (spec §53), zero trust in peer-supplied paths (spec §51).
#![forbid(unsafe_code)]

pub mod framing;
pub mod limits;
pub mod pairing;
pub mod path_safety;

pub use framing::{encode_msg, encode_raw, FrameDecoder};
pub use limits::*;
pub use pairing::PairMsg;

use serde::{Deserialize, Serialize};

/// ALPN for the DropBridge transfer protocol v1.
pub const ALPN_TRANSFER: &[u8] = b"dropbridge/transfer/1";
/// ALPN for device pairing.
pub const ALPN_PAIRING: &[u8] = b"dropbridge/pair/1";
/// ALPN for lightweight path probing (RTT/bandwidth estimates).
pub const ALPN_PROBE: &[u8] = b"dropbridge/probe/1";

/// Protocol version spoken by this implementation.
pub const PROTOCOL_VERSION: u32 = 1;

/// 32-byte device identity (Ed25519 public key of the iroh endpoint).
pub type DeviceId = [u8; 32];
/// BLAKE3 file hash.
pub type FileHash = [u8; 32];

/// Kinds of devices; used for UX only — never for authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceKind {
    Phone,
    Tablet,
    Laptop,
    Desktop,
    Other,
}

impl DeviceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceKind::Phone => "phone",
            DeviceKind::Tablet => "tablet",
            DeviceKind::Laptop => "laptop",
            DeviceKind::Desktop => "desktop",
            DeviceKind::Other => "device",
        }
    }
}

/// Capabilities advertised in [`Msg::Hello`]/[`Msg::HelloAck`] (spec §49).
///
/// Additional optional fields may be appended in future versions; older peers
/// ignore unknown trailing fields per the protocol evolution policy (§50).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Protocol versions this peer can speak, highest first.
    pub protocol_versions: Vec<u32>,
    pub supports_resume: bool,
    pub supports_folders: bool,
    pub supports_parallel_streams: bool,
    pub supports_compression: bool,
    /// Largest chunk the peer is willing to receive on one data stream.
    pub max_chunk_size: u64,
    /// Max parallel data streams the peer wants a sender to use.
    pub max_concurrency: u32,
    /// Human-friendly device name (spec §79; display only).
    pub device_name: String,
    pub device_kind: DeviceKind,
    /// Application version string, for diagnostics.
    pub app_version: String,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            protocol_versions: vec![PROTOCOL_VERSION],
            supports_resume: true,
            supports_folders: true,
            supports_parallel_streams: true,
            supports_compression: false,
            max_chunk_size: DEFAULT_CHUNK_SIZE,
            max_concurrency: DEFAULT_STREAM_COUNT,
            device_name: String::new(),
            device_kind: DeviceKind::Other,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// One file within a transfer session (spec §45, §46).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative, `/`-separated path inside the transfer. MUST be sanitized by
    /// the receiver via [`path_safety::sanitize_rel_path`] — never trusted raw.
    pub rel_path: String,
    pub size: u64,
    /// Modification time, unix seconds (0 = unknown).
    pub mtime_secs: i64,
    /// File id: index used on data streams.
    pub file_id: u32,
}

/// Manifest describing a (possibly multi-file / directory) transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: Vec<FileEntry>,
    /// Total size in bytes (sum of entries), for fast acceptance checks.
    pub total_bytes: u64,
}

impl Manifest {
    pub fn new(entries: Vec<FileEntry>) -> Self {
        let total_bytes = entries.iter().map(|e| e.size).sum();
        Self {
            entries,
            total_bytes,
        }
    }

    /// Validate structural limits (spec §53). Called by both sides.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(ProtocolError::ManifestTooLarge(self.entries.len()));
        }
        for e in &self.entries {
            if e.rel_path.len() > MAX_PATH_LEN {
                return Err(ProtocolError::PathTooLong);
            }
            path_safety::sanitize_rel_path(&e.rel_path)?;
        }
        Ok(())
    }
}

/// Transfer offer/accept session ids are u64 chosen by the sender.
pub type SessionId = u64;

/// Control-channel messages (spec §48).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    /// First message on a control channel.
    Hello {
        version: u32,
        caps: Capabilities,
    },
    /// Response to Hello. `ok=false` refuses the whole connection.
    HelloAck {
        version: u32,
        caps: Capabilities,
        ok: bool,
        reason: Option<String>,
    },
    /// Propose transferring the given manifest.
    TransferOffer {
        session: SessionId,
        manifest: Manifest,
        /// Optional free-text note shown to the receiver (bounded).
        note: Option<String>,
    },
    /// Accept an offer; negotiate chunking/concurrency (spec §44).
    TransferAccept {
        session: SessionId,
        chunk_size: u64,
        stream_count: u32,
        /// Byte ranges the receiver already has (resume, spec §39).
        /// Empty for a fresh transfer.
        have_ranges: Vec<FileRanges>,
    },
    TransferReject {
        session: SessionId,
        reason: RejectReason,
    },
    /// Mid-transfer re-sync after a reconnect: receiver tells sender which
    /// ranges it already has, so the sender resumes from there.
    SessionSync {
        session: SessionId,
        have_ranges: Vec<FileRanges>,
    },
    /// Sender→receiver: chunk streams for the session are done; verify.
    /// `hash` is the BLAKE3 hex digest over the canonical transfer content
    /// (manifest order: path byte-string, 0x1F, file bytes per file).
    Verify {
        session: SessionId,
        hash: String,
    },
    /// Receiver→sender: final per-file verification result.
    Complete {
        session: SessionId,
        ok: bool,
        detail: Option<String>,
    },
    Cancel {
        session: SessionId,
        reason: String,
    },
    /// Keepalive / RTT probe.
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// Application-level error.
    Error {
        code: u32,
        msg: String,
    },
}

/// Completed byte ranges for one file (resume protocol, spec §39–40).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FileRanges {
    pub file_id: u32,
    /// Inclusive-exclusive byte ranges already persisted+hashed by receiver.
    pub ranges: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    NotEnoughDiskSpace,
    NotTrusted,
    ReceivingPaused,
    TooManyTransfers,
    UnsupportedProtocol,
    UserRejected,
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("manifest too large: {0} entries")]
    ManifestTooLarge(usize),
    #[error("path too long")]
    PathTooLong,
    #[error("unsafe path rejected: {0}")]
    UnsafePath(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("encode error: {0}")]
    Encode(String),
}

/// Highest common protocol version, or None.
pub fn negotiate_version(local: &[u32], remote: &[u32]) -> Option<u32> {
    local.iter().filter(|v| remote.contains(v)).copied().max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let msg = Msg::Hello {
            version: PROTOCOL_VERSION,
            caps: Capabilities::default(),
        };
        let frame = encode_msg(&msg).unwrap();
        let mut dec = FrameDecoder::new();
        dec.feed(&frame).unwrap();
        let out = dec.next_frame().unwrap().unwrap();
        let back: Msg = postcard::from_bytes(&out).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn manifest_roundtrip_and_validate() {
        let m = Manifest::new(vec![
            FileEntry {
                rel_path: "Photos/2026/c.jpg".into(),
                size: 123,
                mtime_secs: 1750000000,
                file_id: 0,
            },
            FileEntry {
                rel_path: "a.txt".into(),
                size: 1,
                mtime_secs: 0,
                file_id: 1,
            },
        ]);
        m.validate().unwrap();
        let msg = Msg::TransferOffer {
            session: 42,
            manifest: m.clone(),
            note: None,
        };
        let frame = encode_msg(&msg).unwrap();
        let mut dec = FrameDecoder::new();
        dec.feed(&frame).unwrap();
        let back: Msg = postcard::from_bytes(&dec.next_frame().unwrap().unwrap()).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn malicious_paths_rejected() {
        for p in [
            "../etc/passwd",
            "..\\windows\\system32",
            "/etc/passwd",
            "C:\\evil.dll",
            "a/../../b",
            "CON",
            "nul.txt",
            "file:name",
            "\\\\server\\share",
            "a/\u{0000}b",
        ] {
            let m = Manifest::new(vec![FileEntry {
                rel_path: p.into(),
                size: 1,
                mtime_secs: 0,
                file_id: 0,
            }]);
            assert!(m.validate().is_err(), "path {p:?} must be rejected");
        }
    }

    #[test]
    fn version_negotiation() {
        assert_eq!(negotiate_version(&[1, 2], &[1]), Some(1));
        assert_eq!(negotiate_version(&[1], &[2]), None);
        assert_eq!(negotiate_version(&[3, 1], &[1, 3]), Some(3));
    }

    use proptest::prelude::*;
    proptest! {
        #[test]
        fn safe_paths_stay_safe(s in "[a-zA-Z0-9._ -]{1,32}(/[a-zA-Z0-9._ -]{1,32}){0,4}") {
            if let Ok(p) = path_safety::sanitize_rel_path(&s) {
                prop_assert!(!p.starts_with('/'));
                prop_assert!(!p.contains(".."));
            }
        }
    }
}
