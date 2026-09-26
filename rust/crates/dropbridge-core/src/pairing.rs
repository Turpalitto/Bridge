//! Accountless QR pairing — enroller and joiner roles (spec §9, §12).
//!
//! Security notes are in docs/THREAT_MODEL.md. Key properties here:
//!
//! * the QUIC handshake authenticates both device keys (MITM-safe);
//! * the one-time token proves possession of the QR (TTL ≤ 120 s);
//! * the 6-digit auth code + enroller confirmation stops a token thief from
//!   silently pairing (the human sees an unknown device name);
//! * token is consumed on success → no replay.
use std::time::Duration;

use dropbridge_identity::TrustedDevice;
use dropbridge_protocol::encode_raw;
use dropbridge_protocol::limits::{
    MAX_PAIR_FRAME_BYTES, PAIRING_TTL_SECS, PAIR_RATE_LIMIT_PER_HOUR,
};
use dropbridge_protocol::pairing::{PairInvitation, PairMsg};
use iroh::endpoint::Connection;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::events::NodeEvent;
use crate::node::Node;
use crate::session::CtrlStream;
use crate::CoreError;

/// Live pairing state on the enroller side.
pub struct PairingState {
    pub invitation: PairInvitation,
    /// UI confirms the joiner through this channel.
    pub approval: Option<oneshot::Receiver<bool>>,
}

impl Node {
    /// Enroller: create a fresh one-time invitation (QR payload).
    pub async fn create_pair_invitation(&self) -> Result<PairInvitation, CoreError> {
        let token: [u8; 32] = rand::random();
        let now = chrono::Utc::now().timestamp();
        let hints = dropbridge_network::AddrHints::from_endpoint(&self.endpoint);
        let inv = PairInvitation::new(
            self.identity.device_id(),
            self.cfg.device_name.clone(),
            self.cfg.device_kind,
            token,
            now,
            hints.relay_urls,
            hints.direct,
        );
        let (tx, rx) = oneshot::channel();
        *self.pairing.lock().await = Some(PairingState {
            invitation: inv.clone(),
            approval: Some(rx),
        });
        // The UI retrieves the sender half when it shows the QR.
        *self.pair_approval_sender.lock().await = Some(tx);
        Ok(inv)
    }

    /// UI hook: approve/deny the joiner currently on screen.
    pub async fn approve_pairing(&self, approve: bool) {
        if let Some(tx) = self.pair_approval_sender.lock().await.take() {
            let _ = tx.send(approve);
        }
    }

    /// Joiner: scan result → pair with the enroller.
    pub async fn join_pairing(&self, inv: PairInvitation) -> Result<(), CoreError> {
        let now = chrono::Utc::now().timestamp();
        if inv.expired(now) {
            return Err(CoreError::Pairing("invitation expired".into()));
        }
        let hints = dropbridge_network::AddrHints {
            relay_urls: inv.relay_hints.clone(),
            direct: inv.direct_hints.clone(),
        };
        let addr = hints.to_endpoint_addr(&inv.device_id)?;
        let conn = tokio::time::timeout(
            Duration::from_secs(20),
            self.endpoint
                .connect(addr, dropbridge_protocol::ALPN_PAIRING),
        )
        .await
        .map_err(|_| CoreError::Timeout)?
        .map_err(|e| CoreError::Pairing(e.to_string()))?;

        // TLS already authenticated the enroller key; assert QR agreement.
        let remote = conn.remote_id();
        if remote.as_bytes() != &inv.device_id {
            return Err(CoreError::Pairing(
                "QR does not match the device we reached".into(),
            ));
        }

        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| CoreError::Pairing(e.to_string()))?;
        let mut ctrl = CtrlStream::new(send, recv);

        let req = PairMsg::Request {
            token: inv.pairing_token,
            device_name: self.cfg.device_name.clone(),
            device_kind: self.cfg.device_kind,
        };
        ctrl.send_raw(encode_raw(&req)?).await?;

        // Enroller shows our name + a code; mirror it for our user.
        let frame = ctrl
            .next_raw(Duration::from_secs(30))
            .await?
            .ok_or_else(|| CoreError::Pairing("no challenge".into()))?;
        let PairMsg::Challenge { auth_code } = decode_pair(&frame)? else {
            return Err(CoreError::Pairing("expected challenge".into()));
        };
        info!(auth_code, "pairing: confirm this code on the other device");
        // MVP CLI/daemon policy: the joiner confirmed by initiating; the
        // enroller-side human is the approval gate (see THREAT_MODEL).
        let _ = auth_code;

        ctrl.send_raw(encode_raw(&PairMsg::Confirm { auth_code })?)
            .await?;
        let frame = ctrl
            .next_raw(Duration::from_secs(120))
            .await?
            .ok_or_else(|| CoreError::Pairing("pairing ended early".into()))?;
        match decode_pair(&frame)? {
            PairMsg::Done => {}
            PairMsg::Fail { reason } => return Err(CoreError::Pairing(reason)),
            other => return Err(CoreError::Pairing(format!("unexpected {other:?}"))),
        }

        // Persist the enroller as trusted.
        self.upsert_trusted(TrustedDevice {
            device_id: inv.device_id,
            name: inv.device_name.clone(),
            kind: inv.device_kind.as_str().to_string(),
            permissions: dropbridge_identity::Permission::default_pairing().bits(),
            paired_at: now,
            last_seen: now,
        })
        .await?;
        // Remember how we reached them.
        {
            let mut h = self.peer_hints.lock().await;
            h.entry(inv.device_id)
                .and_modify(|e| {
                    let mut merged = hints.clone();
                    merged.merge(std::mem::take(e));
                    *e = merged;
                })
                .or_insert_with(|| hints.clone());
        }
        self.persist_hints().await;
        let _ = ctrl.finish();
        conn.close(0u32.into(), b"paired");
        Ok(())
    }
}

/// Enroller side of a pairing connection (joiner dialed us).
pub async fn handle_incoming_pairing(node: &Node, conn: Connection) -> Result<(), CoreError> {
    let remote_id: dropbridge_identity::DeviceId = *conn.remote_id().as_bytes();

    // Rate limit untrusted pairing attempts (spec §53).
    {
        let now = chrono::Utc::now().timestamp();
        let mut att = node.pair_attempts.lock().await;
        let entry = att.entry(remote_id).or_insert((now, 0));
        if now - entry.0 > 3600 {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > PAIR_RATE_LIMIT_PER_HOUR {
            warn!("pairing rate limit exceeded");
            return Err(CoreError::Pairing("rate limited".into()));
        }
    }

    let (send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| CoreError::Pairing(e.to_string()))?;
    let mut ctrl = CtrlStream::new(send, recv);

    let frame = ctrl
        .next_raw(Duration::from_secs(30))
        .await?
        .ok_or_else(|| CoreError::Pairing("no request".into()))?;
    let PairMsg::Request {
        token,
        device_name,
        device_kind,
    } = decode_pair(&frame)?
    else {
        return Err(CoreError::Pairing("expected request".into()));
    };

    // Validate one-time token against the active invitation and consume it atomically.
    let (code, approval_rx) = {
        let mut guard = node.pairing.lock().await;
        let Some(mut state) = guard.take() else {
            fail(&mut ctrl, "no active pairing").await;
            return Err(CoreError::Pairing("no active pairing".into()));
        };
        let inv = &state.invitation;
        let now = chrono::Utc::now().timestamp();
        let token_matches = inv
            .pairing_token
            .iter()
            .zip(token.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
        if inv.expired(now) || !token_matches {
            fail(&mut ctrl, "invitation expired or invalid").await;
            return Err(CoreError::Pairing("bad token".into()));
        }
        let code = inv.auth_code(&remote_id);
        let rx = state.approval.take();
        (code, rx)
    };

    ctrl.send_raw(encode_raw(&PairMsg::Challenge { auth_code: code })?)
        .await?;
    node.emit(NodeEvent::PairingChallenge {
        device_name: device_name.clone(),
        device_kind: device_kind.as_str().to_string(),
        auth_code: code,
    });

    // Human approval gate (or configured auto-approve for headless setups).
    let approved = if node.config().pairing_auto_approve {
        true
    } else {
        match approval_rx {
            Some(rx) => matches!(
                tokio::time::timeout(Duration::from_secs(PAIRING_TTL_SECS.max(0) as u64), rx).await,
                Ok(Ok(true))
            ),
            None => false,
        }
    };
    if !approved {
        fail(&mut ctrl, "pairing rejected by user").await;
        return Err(CoreError::Pairing("rejected".into()));
    }

    // Confirm joiner echoed the code (they saw it on their screen).
    let frame = ctrl
        .next_raw(Duration::from_secs(60))
        .await?
        .ok_or_else(|| CoreError::Pairing("no confirm".into()))?;
    let PairMsg::Confirm { auth_code } = decode_pair(&frame)? else {
        fail(&mut ctrl, "expected confirm").await;
        return Err(CoreError::Pairing("expected confirm".into()));
    };
    if auth_code != code {
        fail(&mut ctrl, "auth code mismatch").await;
        return Err(CoreError::Pairing("code mismatch".into()));
    }

    ctrl.send_raw(encode_raw(&PairMsg::Done)?).await?;
    let _ = ctrl.finish();

    // Trust the joiner and consume the token.
    let now = chrono::Utc::now().timestamp();
    node.upsert_trusted(TrustedDevice {
        device_id: remote_id,
        name: device_name.clone(),
        kind: device_kind.as_str().to_string(),
        permissions: dropbridge_identity::Permission::default_pairing().bits(),
        paired_at: now,
        last_seen: now,
    })
    .await?;
    *node.pairing.lock().await = None; // one-time token consumed
    info!(name = %device_name, "device paired");

    // Wait for the joiner to receive Done and close the connection cleanly
    let _ = tokio::time::timeout(Duration::from_millis(500), conn.closed()).await;
    conn.close(0u32.into(), b"paired");
    Ok(())
}

async fn fail(ctrl: &mut CtrlStream, reason: &str) {
    if let Ok(f) = encode_raw(&PairMsg::Fail {
        reason: reason.to_string(),
    }) {
        let _ = ctrl.send_raw(f).await;
    }
}

fn decode_pair(frame: &[u8]) -> Result<PairMsg, CoreError> {
    if frame.len() > MAX_PAIR_FRAME_BYTES {
        return Err(CoreError::Pairing("frame too large".into()));
    }
    postcard::from_bytes(frame).map_err(|e| CoreError::Pairing(e.to_string()))
}

/// Convenience for UIs: build the QR string for an invitation.
pub fn invitation_qr_string(inv: &PairInvitation) -> Result<String, CoreError> {
    inv.to_qr_string().map_err(CoreError::Other)
}

/// Parse a scanned QR string.
pub fn parse_qr(s: &str) -> Result<PairInvitation, CoreError> {
    PairInvitation::from_qr_string(s).map_err(CoreError::Pairing)
}
