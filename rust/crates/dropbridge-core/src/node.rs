//! The DropBridge node: identity, trust, endpoint, discovery, sessions.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dropbridge_discovery::{browse, DiscoveredPeer};
use dropbridge_identity::{
    DeviceId, DeviceIdentity, SecretProtector, TrustRegistry, TrustedDevice,
};
use dropbridge_network::{build_endpoint, hints::AddrHints};
use dropbridge_protocol::{Capabilities, PROTOCOL_VERSION};
use dropbridge_transfer::journal::Journal;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::Endpoint;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

use crate::config::NodeConfig;
use crate::events::NodeEvent;
use crate::pairing::{self, PairingState};
use crate::session;
use crate::CoreError;

pub struct Node {
    pub(crate) cfg: NodeConfig,
    pub(crate) identity: DeviceIdentity,
    pub(crate) trust: Mutex<TrustRegistry>,
    pub(crate) journal: Arc<Journal>,
    pub(crate) endpoint: Endpoint,
    pub(crate) events: broadcast::Sender<NodeEvent>,
    /// Last known addressing hints per device (persisted, LAN-first dialing).
    pub(crate) peer_hints: Mutex<HashMap<DeviceId, AddrHints>>,
    /// Pending pairing state (enroller side).
    pub(crate) pairing: Mutex<Option<PairingState>>,
    /// User decisions for incoming offers: session → approve.
    pub(crate) decisions: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>,
    /// Pairing attempt counter per remote id (rate limit, spec §53).
    pub(crate) pair_attempts: Mutex<HashMap<DeviceId, (i64, u32)>>,
    /// UI side of the pairing approval channel.
    pub(crate) pair_approval_sender: Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
    pub(crate) router: Mutex<Option<Router>>,
    protector_name: String,
}

impl Node {
    /// Start a node: load/create identity, open journal, bind endpoint,
    /// start accept loops and (optionally) LAN announcement.
    pub async fn start(cfg: NodeConfig) -> Result<Arc<Self>, CoreError> {
        std::fs::create_dir_all(&cfg.state_dir)?;

        // Identity with the platform protector: DirectSeedProtector when hardware
        // key is passed, DPAPI on Windows, chmod-0600 file elsewhere.
        let protector: Box<dyn SecretProtector> = if let Some(seed) = cfg.hardware_identity_seed {
            Box::new(dropbridge_identity::DirectSeedProtector::new(seed))
        } else {
            dropbridge_identity::protector::platform_protector(
                cfg.state_dir.join("keys/device.key"),
            )
        };
        let marker_path = cfg.key_marker_path();
        let existing = std::fs::read(&marker_path).ok();
        let (identity, marker) = dropbridge_identity::protector::load_or_create(
            protector.as_ref(),
            existing.as_deref(),
        )?;
        std::fs::write(&marker_path, &marker)?;

        let trust = TrustRegistry::open(cfg.trust_path())?;
        let journal = Arc::new(Journal::open_in(&cfg.state_dir)?);

        let endpoint = build_endpoint(
            &identity,
            &cfg.relay,
            dropbridge_network::endpoint::all_alpns(),
            cfg.fixed_port,
        )
        .await?;

        let (events, _) = broadcast::channel(256);

        let peer_hints = Self::load_hints(&cfg.hints_path());
        let announce = cfg.announce;
        let node = Arc::new(Self {
            protector_name: protector.name().to_string(),
            cfg,
            identity,
            trust: Mutex::new(trust),
            journal,
            endpoint,
            events,
            peer_hints: Mutex::new(peer_hints),
            pairing: Mutex::new(None),
            decisions: Mutex::new(HashMap::new()),
            pair_attempts: Mutex::new(HashMap::new()),
            pair_approval_sender: Mutex::new(None),
            router: Mutex::new(None),
        });

        // Router: transfer + pairing protocols by ALPN.
        let router = Router::builder(node.endpoint.clone())
            .accept(
                dropbridge_protocol::ALPN_TRANSFER,
                TransferHandler(Arc::clone(&node)),
            )
            .accept(
                dropbridge_protocol::ALPN_PAIRING,
                PairingHandler(Arc::clone(&node)),
            )
            .spawn();
        *node.router.lock().await = Some(router);

        if announce {
            let adv = dropbridge_discovery::SelfAdvertisement {
                identity: node.identity.clone(),
                name: node.cfg.device_name.clone(),
                kind: node.cfg.device_kind,
                transfer_port: node
                    .endpoint
                    .bound_sockets()
                    .first()
                    .map(|s| s.port())
                    .unwrap_or(0),
                pairing: false,
            };
            match dropbridge_discovery::announce(adv) {
                Ok(_h) => {
                    std::mem::forget(_h); // announce for process lifetime
                }
                Err(e) => warn!(error = %e, "LAN announcement unavailable"),
            }
        }

        info!(
            device_id = %node.identity.device_id_z32(),
            protector = %node.protector_name,
            "dropbridge node started"
        );
        Ok(node)
    }

    fn load_hints(path: &std::path::Path) -> HashMap<DeviceId, AddrHints> {
        std::fs::read(path)
            .ok()
            .and_then(|b| {
                serde_json::from_slice::<Vec<(DeviceId, AddrHints)>>(&b)
                    .ok()
                    .map(|v| v.into_iter().collect())
            })
            .unwrap_or_default()
    }

    pub(crate) async fn persist_hints(&self) {
        let hints = self.peer_hints.lock().await;
        let v: Vec<(DeviceId, AddrHints)> = hints.iter().map(|(k, v)| (*k, v.clone())).collect();
        if let Ok(b) = serde_json::to_vec(&v) {
            let _ = std::fs::write(self.cfg.hints_path(), b);
        }
    }

    pub fn device_id(&self) -> DeviceId {
        self.identity.device_id()
    }

    pub fn device_id_z32(&self) -> String {
        self.identity.device_id_z32()
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub async fn stop(&self) {
        if let Some(router) = self.router.lock().await.take() {
            let _ = router.shutdown().await;
        }
        self.endpoint.close().await;
    }

    pub fn events(&self) -> broadcast::Receiver<NodeEvent> {
        self.events.subscribe()
    }

    pub(crate) fn emit(&self, ev: NodeEvent) {
        let _ = self.events.send(ev);
    }

    pub fn config(&self) -> &NodeConfig {
        &self.cfg
    }

    /// Our capabilities for Hello exchanges (spec §49).
    pub fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities {
            device_name: self.cfg.device_name.clone(),
            device_kind: self.cfg.device_kind,
            protocol_versions: vec![PROTOCOL_VERSION],
            ..Default::default()
        };
        if let Some(cs) = self.cfg.override_chunk_size {
            caps.max_chunk_size = cs;
        }
        if let Some(sc) = self.cfg.override_stream_count {
            caps.max_concurrency = sc;
        }
        caps
    }

    /// Remember a LAN sighting (feeds LAN-first dialing).
    pub async fn note_discovery(&self, peer: DiscoveredPeer) {
        let mut hints = self.peer_hints.lock().await;
        let entry = hints.entry(peer.device_id).or_default();
        let direct = SocketAddr::new(peer.addr.ip(), peer.port);
        entry.add_direct(direct);
        drop(hints);
        self.persist_hints().await;
        self.emit(NodeEvent::DeviceDiscovered(peer));
    }

    /// Browse the LAN for DropBridge devices (bounded window, spec §55).
    pub async fn discover(&self, window: Duration) -> Vec<DiscoveredPeer> {
        let known: Vec<SocketAddr> = {
            let hints = self.peer_hints.lock().await;
            hints
                .values()
                .flat_map(|h| h.direct.iter().filter_map(|d| d.parse().ok()))
                .collect()
        };
        let found = browse(window, known).await;
        for p in &found {
            self.note_discovery(p.clone()).await;
        }
        found
    }

    /// Best known address for a device: stored hints (LAN sightings + relay).
    pub async fn hints_for(&self, id: &DeviceId) -> Option<AddrHints> {
        let mut h = self.peer_hints.lock().await.get(id).cloned()?;
        // ensure at least relay hints from the endpoint when using defaults
        if h.relay_urls.is_empty() && h.direct.is_empty() {
            return None;
        }
        if h.relay_urls.is_empty() {
            if let Some(mine) = AddrHints::from_endpoint(&self.endpoint).relay_urls.first() {
                h.relay_urls.push(mine.clone());
            }
        }
        Some(h)
    }

    pub async fn trusted_devices(&self) -> Vec<TrustedDevice> {
        self.trust.lock().await.devices().to_vec()
    }

    pub async fn revoke_device(&self, id: &DeviceId) -> Result<bool, CoreError> {
        let mut t = self.trust.lock().await;
        let removed = t.revoke(id)?;
        if removed {
            self.emit(NodeEvent::TrustChanged {
                device_id: *id,
                name: String::new(),
                trusted: false,
            });
        }
        Ok(removed)
    }

    /// Approve/reject a pending incoming offer (UI hook).
    pub async fn decide(&self, session: u64, approve: bool) {
        if let Some(tx) = self.decisions.lock().await.remove(&session) {
            let _ = tx.send(approve);
        }
    }

    pub async fn pair_state_set(&self, st: Option<PairingState>) {
        *self.pairing.lock().await = st;
    }

    pub async fn upsert_trusted(&self, d: TrustedDevice) -> Result<(), CoreError> {
        self.trust.lock().await.upsert(d.clone())?;
        self.emit(NodeEvent::TrustChanged {
            device_id: d.device_id,
            name: d.name,
            trusted: true,
        });
        Ok(())
    }
}

#[derive(Clone)]
struct TransferHandler(Arc<Node>);

impl std::fmt::Debug for TransferHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferHandler").finish()
    }
}

impl ProtocolHandler for TransferHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let node = Arc::clone(&self.0);
        tokio::spawn(async move {
            if let Err(e) = session::handle_incoming_transfer(&node, connection).await {
                debug!(error = %e, "incoming transfer session ended");
            }
        });
        Ok(())
    }
}

#[derive(Clone)]
struct PairingHandler(Arc<Node>);

impl std::fmt::Debug for PairingHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingHandler").finish()
    }
}

impl ProtocolHandler for PairingHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let node = Arc::clone(&self.0);
        tokio::spawn(async move {
            if let Err(e) = pairing::handle_incoming_pairing(&node, connection).await {
                debug!(error = %e, "pairing session ended");
            }
        });
        Ok(())
    }
}
