//! Transfer session orchestration over the DropBridge protocol.
//!
//! Sender flow:  Hello → TransferOffer → TransferAccept(have_ranges) →
//!               N chunk streams → Verify(hash) → Complete.
//! Receiver flow mirrors it with trust checks, disk-space gating, journal
//! resume and atomic finalize.
use std::sync::Arc;
use std::time::Duration;

use dropbridge_identity::{DeviceId, Permission};
use dropbridge_network::streams::{RecvChunkStream, SendChunkStream};
use dropbridge_protocol::limits::{
    clamp_chunk_size, clamp_stream_count, CONTROL_IDLE_TIMEOUT_SECS, MAX_CONCURRENT_TRANSFERS,
};
use dropbridge_protocol::{encode_msg, FileRanges, FrameDecoder, Manifest, Msg, PROTOCOL_VERSION};
use dropbridge_transfer::journal::{new_transfer_id, Role, TransferRecord, TransferState};
use dropbridge_transfer::recv::{CollisionPolicy, ReceiverEngine};
use dropbridge_transfer::send::{SendFile, SendPlan};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::events::NodeEvent;
use crate::node::Node;
use crate::CoreError;

const CTRL_TIMEOUT: Duration = Duration::from_secs(CONTROL_IDLE_TIMEOUT_SECS);

/// Framed bidirectional control stream.
pub struct CtrlStream {
    send: SendStream,
    recv: RecvStream,
    dec: FrameDecoder,
}

impl CtrlStream {
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            dec: FrameDecoder::new(),
        }
    }

    pub async fn send_raw(&mut self, frame: Vec<u8>) -> Result<(), CoreError> {
        self.send
            .write_all(&frame)
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), CoreError> {
        self.send
            .finish()
            .map_err(|e| CoreError::Other(e.to_string()))?;
        Ok(())
    }

    /// Read the next complete frame payload, or None on clean close.
    pub async fn next_raw(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, CoreError> {
        loop {
            if let Some(f) = self.dec.next_frame()? {
                return Ok(Some(f));
            }
            let mut buf = [0u8; 8192];
            let res = tokio::time::timeout(timeout, self.recv.read(&mut buf)).await;
            let n = match res {
                Err(_) => return Err(CoreError::Timeout),
                Ok(Err(e)) => return Err(CoreError::Other(e.to_string())),
                Ok(Ok(None)) => return Ok(None),
                Ok(Ok(Some(n))) => n,
            };
            self.dec.feed(&buf[..n])?;
        }
    }

    pub async fn send_msg(&mut self, msg: &Msg) -> Result<(), CoreError> {
        self.send_raw(encode_msg(msg)?).await
    }

    pub async fn next_msg(&mut self, timeout: Duration) -> Result<Option<Msg>, CoreError> {
        let Some(frame) = self.next_raw(timeout).await? else {
            return Ok(None);
        };
        let msg: Msg = postcard::from_bytes(&frame)
            .map_err(|e| CoreError::Other(format!("bad message: {e}")))?;
        Ok(Some(msg))
    }
}

/// Result of a completed send.
#[derive(Debug, Clone)]
pub struct SendResult {
    pub session: u64,
    pub ok: bool,
    pub bytes: u64,
    pub detail: String,
}

/// Sender-side high level API: send paths to a trusted device.
pub async fn send_files(
    node: &Arc<Node>,
    peer: &DeviceId,
    source: dropbridge_transfer::TransferSource,
) -> Result<SendResult, CoreError> {
    send_files_with_session(node, peer, source, None).await
}

/// Like [`send_files`] but reusing a previous session id — this is what
/// makes retries *resume* instead of restart (spec §39): the receiver's
/// journal is keyed by session, so the same session yields have_ranges.
pub async fn send_files_with_session(
    node: &Arc<Node>,
    peer: &DeviceId,
    source: dropbridge_transfer::TransferSource,
    session_override: Option<u64>,
) -> Result<SendResult, CoreError> {
    // Trust gate: we must trust the peer to receive.
    if !node
        .trust
        .lock()
        .await
        .is_trusted(peer, Permission::RECEIVE_FILES)
    {
        return Err(CoreError::NotTrusted);
    }
    let hints = node
        .hints_for(peer)
        .await
        .ok_or_else(|| CoreError::Other("no known route to peer (discover first)".into()))?;
    let addr = hints.to_endpoint_addr(peer)?;

    let conn = tokio::time::timeout(Duration::from_secs(30), async {
        node.endpoint
            .connect(addr, dropbridge_protocol::ALPN_TRANSFER)
            .await
    })
    .await
    .map_err(|_| CoreError::Timeout)?
    .map_err(|e| CoreError::Network(dropbridge_network::NetworkError::Connect(e.to_string())))?;

    let remote = *conn.remote_id().as_bytes();
    if &remote != peer {
        return Err(CoreError::Other("dialed wrong device".into()));
    }

    // Build the manifest from local paths.
    let (manifest, abs_paths) = dropbridge_transfer::build_manifest(&source)?;
    manifest.validate()?;

    let session: u64 = session_override.unwrap_or_else(rand_session);
    let transfer_id = new_transfer_id();

    // Journal the outgoing transfer.
    node.journal.create_transfer(&TransferRecord {
        id: transfer_id.clone(),
        peer_id: hex_of(peer),
        role: Role::Send,
        state: TransferState::Active,
        total_bytes: manifest.total_bytes,
        done_bytes: 0,
        chunk_size: dropbridge_protocol::limits::DEFAULT_CHUNK_SIZE,
        stream_count: dropbridge_protocol::limits::DEFAULT_STREAM_COUNT,
        manifest_json: serde_json::to_string(&manifest)
            .map_err(|e| CoreError::Other(e.to_string()))?,
        root_path: String::new(),
        file_hash: String::new(),
        created_at: chrono::Utc::now().timestamp(),
        updated_at: chrono::Utc::now().timestamp(),
        retry_count: 0,
    })?;

    // Control handshake.
    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?;
    let mut ctrl = CtrlStream::new(send, recv);
    let my_caps = node.capabilities();
    ctrl.send_msg(&Msg::Hello {
        version: PROTOCOL_VERSION,
        caps: my_caps.clone(),
    })
    .await?;
    let ack = expect_msg(&mut ctrl).await?;
    let peer_caps = match ack {
        Msg::HelloAck { ok: true, caps, .. } => caps,
        Msg::HelloAck { reason, .. } => {
            node.journal
                .set_state(&transfer_id, TransferState::Failed)?;
            return Err(CoreError::Rejected(reason.unwrap_or_default()));
        }
        other => return Err(CoreError::Other(format!("unexpected {other:?}"))),
    };

    ctrl.send_msg(&Msg::TransferOffer {
        session,
        manifest: manifest.clone(),
        note: None,
    })
    .await?;
    let accept = expect_msg(&mut ctrl).await?;
    let (chunk_size, stream_count, have_ranges) = match accept {
        Msg::TransferAccept {
            chunk_size,
            stream_count,
            have_ranges,
            ..
        } => (
            clamp_chunk_size(chunk_size.min(my_caps.max_chunk_size)),
            clamp_stream_count(
                stream_count
                    .min(peer_caps.max_concurrency)
                    .min(my_caps.max_concurrency),
            ),
            have_ranges,
        ),
        Msg::TransferReject { reason, .. } => {
            node.journal
                .set_state(&transfer_id, TransferState::Failed)?;
            return Err(CoreError::Rejected(format!("{reason:?}")));
        }
        other => return Err(CoreError::Other(format!("unexpected {other:?}"))),
    };

    // Build the plan, applying the receiver's resume ranges.
    let files: Vec<SendFile> = manifest
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let have = have_ranges
                .iter()
                .find(|r| r.file_id == e.file_id)
                .map(|r| dropbridge_transfer::ranges::RangeSet::from_sorted(r.ranges.clone()))
                .unwrap_or_default();
            SendFile {
                entry: e.clone(),
                path: abs_paths[i].clone(),
                have,
            }
        })
        .collect();
    let plan = Arc::new(SendPlan {
        session,
        chunk_size,
        stream_count,
        files,
        manifest: manifest.clone(),
    });

    // Hash pass (pipelined with sending in the background).
    let paths_for_hash = abs_paths.clone();
    let manifest_for_hash = manifest.clone();
    let hash_task =
        tokio::task::spawn_blocking(move || sender_digest(&manifest_for_hash, &paths_for_hash));

    // Open chunk streams.
    let mut sinks = Vec::with_capacity(stream_count as usize);
    for _ in 0..stream_count {
        let s = conn
            .open_uni()
            .await
            .map_err(|e| CoreError::Other(e.to_string()))?;
        sinks.push(SendChunkStream::new(s));
    }

    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<u64>();
    let progress_node = Arc::clone(node);
    let progress_task = tokio::spawn(async move {
        while let Some(d) = prx.recv().await {
            progress_node.emit(NodeEvent::TransferProgress {
                session,
                bytes_delta: d,
                total_bytes: manifest.total_bytes,
            });
        }
    });

    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    node.active_transfers.lock().await.insert(session, cancel_tx);

    let journal = Arc::clone(&node.journal);
    let send_outcome = tokio::select! {
        res = dropbridge_transfer::send::run_sender(
            Arc::clone(&plan),
            journal,
            transfer_id.clone(),
            sinks,
            Some(ptx),
        ) => res,
        _ = cancel_rx.changed() => {
            conn.close(2u32.into(), b"cancelled by user");
            node.journal
                .set_state(&transfer_id, TransferState::Cancelled)?;
            node.active_transfers.lock().await.remove(&session);
            node.emit(NodeEvent::TransferCompleted {
                session,
                ok: false,
                files: vec![],
                detail: "cancelled by user".into(),
            });
            return Err(CoreError::Other("cancelled by user".into()));
        }
    };
    node.active_transfers.lock().await.remove(&session);
    drop(progress_task);

    let sent = match send_outcome {
        Ok(n) => n,
        Err(e) => {
            conn.close(1u32.into(), b"send error");
            node.journal
                .set_state(&transfer_id, TransferState::Failed)?;
            node.journal.bump_retry(&transfer_id)?;
            return Err(CoreError::Transport(e));
        }
    };

    let digest = hash_task
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
        .map_err(|e| CoreError::Other(e.to_string()))?;
    let hash_hex = dropbridge_transfer::hash::hash_to_hex(&digest);
    node.journal.set_hash(&transfer_id, &hash_hex)?;

    ctrl.send_msg(&Msg::Verify {
        session,
        hash: hash_hex,
    })
    .await?;
    let done = expect_msg(&mut ctrl).await?;
    let ok = match done {
        Msg::Complete { ok, detail, .. } => {
            if ok {
                node.journal
                    .set_state(&transfer_id, TransferState::Completed)?;
                node.emit(NodeEvent::TransferCompleted {
                    session,
                    ok: true,
                    files: vec![],
                    detail: detail.clone().unwrap_or_default(),
                });
                ok
            } else {
                node.journal
                    .set_state(&transfer_id, TransferState::Failed)?;
                node.journal.bump_retry(&transfer_id)?;
                node.emit(NodeEvent::TransferCompleted {
                    session,
                    ok: false,
                    files: vec![],
                    detail: detail.clone().unwrap_or_default(),
                });
                conn.close(0u32.into(), b"failed");
                return Err(CoreError::Other(format!(
                    "transfer failed on receiver: {}",
                    detail.unwrap_or_default()
                )));
            }
        }
        other => {
            node.journal
                .set_state(&transfer_id, TransferState::Failed)?;
            return Err(CoreError::Other(format!("unexpected {other:?}")));
        }
    };
    conn.close(0u32.into(), b"done");
    Ok(SendResult {
        session,
        ok,
        bytes: sent,
        detail: String::new(),
    })
}

/// Receiver side: handle one incoming transfer connection.
pub async fn handle_incoming_transfer(node: &Arc<Node>, conn: Connection) -> Result<(), CoreError> {
    let peer_id: DeviceId = *conn.remote_id().as_bytes();

    // Trust gate BEFORE any work (spec §13).
    let trusted_send = node
        .trust
        .lock()
        .await
        .is_trusted(&peer_id, Permission::SEND_FILES);

    let (send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?;
    let mut ctrl = CtrlStream::new(send, recv);

    let hello = expect_msg(&mut ctrl).await?;
    let peer_caps = match hello {
        Msg::Hello { caps, version } => {
            if version != PROTOCOL_VERSION {
                // Forward compatibility (§50): unknown higher versions are
                // refused politely; older ones could be bridged here later.
                ctrl.send_msg(&Msg::HelloAck {
                    version: PROTOCOL_VERSION,
                    caps: node.capabilities(),
                    ok: false,
                    reason: Some("unsupported protocol version".into()),
                })
                .await?;
                return Err(CoreError::Other("version mismatch".into()));
            }
            caps
        }
        other => return Err(CoreError::Other(format!("expected Hello, got {other:?}"))),
    };

    if !trusted_send {
        ctrl.send_msg(&Msg::HelloAck {
            version: PROTOCOL_VERSION,
            caps: node.capabilities(),
            ok: false,
            reason: Some("not trusted".into()),
        })
        .await?;
        return Err(CoreError::NotTrusted);
    }

    ctrl.send_msg(&Msg::HelloAck {
        version: PROTOCOL_VERSION,
        caps: node.capabilities(),
        ok: true,
        reason: None,
    })
    .await?;

    let offer = expect_msg(&mut ctrl).await?;
    let (session, manifest) = match offer {
        Msg::TransferOffer {
            session, manifest, ..
        } => (session, manifest),
        other => return Err(CoreError::Other(format!("expected offer, got {other:?}"))),
    };
    manifest.validate()?;

    let transfer_id = format!("recv-{session:016x}-{}", hex_of(&peer_id));

    // Concurrency cap (spec §53).
    let active = active_transfer_count(node).await;
    if active >= MAX_CONCURRENT_TRANSFERS {
        ctrl.send_msg(&Msg::TransferReject {
            session,
            reason: dropbridge_protocol::RejectReason::TooManyTransfers,
        })
        .await?;
        return Err(CoreError::Other("too many concurrent transfers".into()));
    }

    // User approval unless auto-receive is granted to this peer.
    let auto = node
        .trust
        .lock()
        .await
        .get(&peer_id)
        .is_some_and(|d| d.has(Permission::AUTO_RECEIVE))
        && node.config().auto_receive;
    if !auto {
        node.emit(NodeEvent::IncomingOffer {
            peer: peer_id,
            peer_name: peer_caps.device_name.clone(),
            session,
            files: manifest.entries.len(),
            total_bytes: manifest.total_bytes,
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        node.decisions.lock().await.insert(session, tx);
        let approved = matches!(
            tokio::time::timeout(Duration::from_secs(300), rx).await,
            Ok(Ok(true))
        );
        if !approved {
            ctrl.send_msg(&Msg::TransferReject {
                session,
                reason: dropbridge_protocol::RejectReason::UserRejected,
            })
            .await?;
            return Err(CoreError::Other("user rejected".into()));
        }
    }

    // Prepare receiver (sanitizes paths, checks disk space, loads resume).
    let engine = match ReceiverEngine::prepare(
        &node.cfg.receive_dir,
        &manifest,
        &node.journal,
        &transfer_id,
    ) {
        Ok(e) => e,
        Err(dropbridge_transfer::recv::RecvError::NotEnoughSpace { .. }) => {
            ctrl.send_msg(&Msg::TransferReject {
                session,
                reason: dropbridge_protocol::RejectReason::NotEnoughDiskSpace,
            })
            .await?;
            return Err(CoreError::Other("not enough disk space".into()));
        }
        Err(e) => return Err(e.into()),
    };

    // Journal the incoming transfer.
    node.journal.create_transfer(&TransferRecord {
        id: transfer_id.clone(),
        peer_id: hex_of(&peer_id),
        role: Role::Recv,
        state: TransferState::Active,
        total_bytes: manifest.total_bytes,
        done_bytes: engine.total_received(),
        chunk_size: dropbridge_protocol::limits::DEFAULT_CHUNK_SIZE,
        stream_count: dropbridge_protocol::limits::DEFAULT_STREAM_COUNT,
        manifest_json: serde_json::to_string(&manifest)
            .map_err(|e| CoreError::Other(e.to_string()))?,
        root_path: node.cfg.receive_dir.to_string_lossy().into_owned(),
        file_hash: String::new(),
        created_at: chrono::Utc::now().timestamp(),
        updated_at: chrono::Utc::now().timestamp(),
        retry_count: 0,
    })?;

    let engine = Arc::new(Mutex::new(engine));
    let have_ranges: Vec<FileRanges> = engine.lock().await.have_ranges();

    let my_caps = node.capabilities();
    let chunk_size = clamp_chunk_size(
        my_caps.max_chunk_size.min(
            peer_caps
                .max_chunk_size
                .max(dropbridge_protocol::limits::MIN_CHUNK_SIZE),
        ),
    );
    let stream_count = clamp_stream_count(peer_caps.max_concurrency.min(my_caps.max_concurrency));
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    node.active_transfers.lock().await.insert(session, cancel_tx);

    ctrl.send_msg(&Msg::TransferAccept {
        session,
        chunk_size,
        stream_count,
        have_ranges,
    })
    .await?;

    // Ingest chunk streams until the sender stops opening them.
    let mut ingest_tasks = Vec::new();
    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<u64>();
    let progress_node = Arc::clone(node);
    let progress_task = tokio::spawn(async move {
        while let Some(d) = prx.recv().await {
            progress_node.emit(NodeEvent::TransferProgress {
                session,
                bytes_delta: d,
                total_bytes: manifest.total_bytes,
            });
        }
    });

    // The sender opens exactly `stream_count` streams; accept them, then the
    // Verify message arrives on the control stream.
    for _ in 0..stream_count {
        let stream = match conn.accept_uni().await {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "sender stopped opening streams");
                break;
            }
        };
        let eng = Arc::clone(&engine);
        let journal = Arc::clone(&node.journal);
        let tid = transfer_id.clone();
        let p = ptx.clone();
        let fail_after = node.cfg.test_recv_fail_after;
        ingest_tasks.push(tokio::spawn(async move {
            let mut src = RecvChunkStream::new(stream);
            let mut guard = eng.lock().await;
            let out = guard
                .ingest(&mut src, &journal, &tid, Some(p), fail_after)
                .await;
            drop(guard);
            out
        }));
    }
    drop(ptx);
    drop(progress_task);

    // All ingest streams must finish before expecting the Verify message.
    let ingest_res = tokio::select! {
        res = async {
            for t in ingest_tasks {
                match t.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => return Err(e.to_string()),
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(())
        } => res,
        _ = cancel_rx.changed() => {
            conn.close(2u32.into(), b"cancelled by user");
            node.journal
                .set_state(&transfer_id, TransferState::Cancelled)?;
            node.active_transfers.lock().await.remove(&session);
            node.emit(NodeEvent::TransferCompleted {
                session,
                ok: false,
                files: vec![],
                detail: "cancelled by user".into(),
            });
            return Err(CoreError::Other("cancelled by user".into()));
        }
    };
    node.active_transfers.lock().await.remove(&session);
    if let Err(e) = ingest_res {
        node.journal
            .set_state(&transfer_id, TransferState::Failed)?;
        node.emit(NodeEvent::TransferCompleted {
            session,
            ok: false,
            files: vec![],
            detail: e.clone(),
        });
        let _ = ctrl
            .send_msg(&Msg::Complete {
                session,
                ok: false,
                detail: Some(e),
            })
            .await;
        conn.close(1u32.into(), b"ingest error");
        return Err(CoreError::Other("ingest failed".into()));
    }

    // Wait for the Verify message now that all ingest streams are done.
    let verify = expect_msg(&mut ctrl).await?;
    let expected_hash = match verify {
        Msg::Verify { hash, .. } => hash,
        other => {
            node.journal
                .set_state(&transfer_id, TransferState::Failed)?;
            conn.close(1u32.into(), b"expected verify");
            return Err(CoreError::Other(format!("expected Verify, got {other:?}")));
        }
    };

    let mut guard = engine.lock().await;
    if !guard.complete() {
        node.journal
            .set_state(&transfer_id, TransferState::Failed)?;
        ctrl.send_msg(&Msg::Complete {
            session,
            ok: false,
            detail: Some("missing ranges".into()),
        })
        .await?;
        return Err(CoreError::Other("transfer incomplete".into()));
    }
    let (ok, files, detail) = if guard.verify(&expected_hash).await.unwrap_or(false) {
        match guard.finalize(CollisionPolicy::Rename) {
            Ok(files) => (true, files, "verified".to_string()),
            Err(e) => {
                guard.cleanup();
                (false, Vec::new(), format!("finalize failed: {e}"))
            }
        }
    } else {
        guard.cleanup();
        (false, Vec::new(), "hash mismatch".to_string())
    };
    drop(guard);

    node.journal.set_state(
        &transfer_id,
        if ok {
            TransferState::Completed
        } else {
            TransferState::Failed
        },
    )?;
    node.journal.set_hash(&transfer_id, &expected_hash)?;
    node.emit(NodeEvent::TransferCompleted {
        session,
        ok,
        files: files.clone(),
        detail: detail.clone(),
    });
    ctrl.send_msg(&Msg::Complete {
        session,
        ok,
        detail: if ok { None } else { Some(detail.clone()) },
    })
    .await?;
    info!(session, ok, files = files.len(), detail = %detail, "transfer finished");
    conn.closed().await;
    if !ok {
        return Err(CoreError::Other(detail));
    }
    Ok(())
}

async fn expect_msg(ctrl: &mut CtrlStream) -> Result<Msg, CoreError> {
    ctrl.next_msg(CTRL_TIMEOUT)
        .await?
        .ok_or(CoreError::Other("control stream closed".into()))
}

async fn active_transfer_count(node: &Node) -> usize {
    node.journal
        .recent(64)
        .map(|rs| {
            rs.iter()
                .filter(|r| r.state == TransferState::Active)
                .count()
        })
        .unwrap_or(0)
}

fn rand_session() -> u64 {
    rand::random()
}

fn hex_of(id: &DeviceId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sender-side transfer digest: BLAKE3 over (rel_path, 0x1F, bytes) in
/// manifest order — must match ReceiverEngine::verify.
fn sender_digest(
    manifest: &Manifest,
    paths: &[std::path::PathBuf],
) -> Result<[u8; 32], std::io::Error> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    for (i, entry) in manifest.entries.iter().enumerate() {
        hasher.update(entry.rel_path.as_bytes());
        hasher.update(&[0x1F]);
        use std::io::Read;
        let mut f = std::fs::File::open(&paths[i])?;
        let mut remaining = entry.size;
        while remaining > 0 {
            let want = buf.len().min(remaining as usize);
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                return Err(std::io::Error::other("file shrank during send"));
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
    }
    Ok(*hasher.finalize().as_bytes())
}
