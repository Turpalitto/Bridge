//! Node configuration.
use std::path::PathBuf;

use dropbridge_network::RelayConfig;
use dropbridge_protocol::DeviceKind;

#[derive(Clone)]
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

impl std::fmt::Debug for NodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeConfig")
            .field("state_dir", &self.state_dir)
            .field("receive_dir", &self.receive_dir)
            .field("device_name", &self.device_name)
            .field("device_kind", &self.device_kind)
            .field("relay", &self.relay)
            .field("rendezvous_url", &self.rendezvous_url)
            .field("auto_receive", &self.auto_receive)
            .field("pairing_auto_approve", &self.pairing_auto_approve)
            .field("announce", &self.announce)
            .field("test_recv_fail_after", &self.test_recv_fail_after)
            .field("fixed_port", &self.fixed_port)
            .field("override_chunk_size", &self.override_chunk_size)
            .field("override_stream_count", &self.override_stream_count)
            .field(
                "hardware_identity_seed",
                &self.hardware_identity_seed.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_config_debug_redacts_seed() {
        let mut cfg = NodeConfig::new(PathBuf::from("/tmp"), "test".into(), DeviceKind::Laptop);
        cfg.hardware_identity_seed = Some([42u8; 32]);
        let debug_str = format!("{cfg:?}");
        assert!(debug_str.contains("[REDACTED]"));
        assert!(!debug_str.contains("42"));
    }
}
