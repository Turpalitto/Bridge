//! Node configuration.
use std::path::PathBuf;

use dropbridge_network::RelayConfig;
use dropbridge_protocol::DeviceKind;

#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Where keys, trust registry and the journal live.
    pub state_dir: PathBuf,
    /// Default receive root (spec §32: `%USERPROFILE%\DropBridge\From Phone`).
    pub receive_dir: PathBuf,
    /// Friendly device name (display only; identity is cryptographic).
    pub device_name: String,
    pub device_kind: DeviceKind,
    pub relay: RelayConfig,
    /// Rendezvous/control-plane base URL (Phase 3; optional).
    pub rendezvous_url: Option<String>,
    /// Auto-approve incoming offers from trusted devices with AUTO_RECEIVE.
    pub auto_receive: bool,
    /// Headless pairing approval (tests/CI). Default false: human confirms.
    pub pairing_auto_approve: bool,
    /// Whether to announce on the LAN (discovery).
    pub announce: bool,
    /// TEST-ONLY fault injection: receiver fails after ingesting N bytes.
    pub test_recv_fail_after: Option<u64>,
    /// Bind the endpoint to a fixed UDP port (stable hints, firewall-friendly).
    pub fixed_port: Option<u16>,
    /// Benchmark/advanced overrides for negotiated transfer parameters.
    pub override_chunk_size: Option<u64>,
    pub override_stream_count: Option<u32>,
    /// Hardware-unsealed identity seed passed directly from host Keystore / TEE.
    pub hardware_identity_seed: Option<[u8; 32]>,
}

impl NodeConfig {
    /// Reasonable defaults rooted at `base` (platform config dir).
    #[must_use]
    pub fn new(base: PathBuf, device_name: String, device_kind: DeviceKind) -> Self {
        Self {
            receive_dir: base.join("DropBridge").join("From Phone"),
            state_dir: base.join("DropBridge").join("state"),
            device_name,
            device_kind,
            relay: RelayConfig::N0Default,
            rendezvous_url: None,
            auto_receive: true,
            pairing_auto_approve: false,
            announce: true,
            test_recv_fail_after: None,
            fixed_port: None,
            override_chunk_size: None,
            override_stream_count: None,
            hardware_identity_seed: None,
        }
    }

    pub fn key_marker_path(&self) -> PathBuf {
        self.state_dir.join("key-marker.json")
    }
    pub fn trust_path(&self) -> PathBuf {
        self.state_dir.join("trust.json")
    }
    pub fn hints_path(&self) -> PathBuf {
        self.state_dir.join("hints.json")
    }
}
