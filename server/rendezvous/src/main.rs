//! DropBridge rendezvous — the minimal control plane (spec §17–18).
//!
//! Responsibilities, and ONLY these:
//! * signed presence: "is my paired device online, and where can I reach it?"
//! * opaque, signed endpoint-hint blobs (we cannot read or forge them),
//! * wake hints: "your laptop wants to send you something" (spec §57) —
//!   delivered to the OS push channel by clients, never carrying file data.
//!
//! Explicit non-goals: file storage, file metadata, names of files, account
//! management. The relay is blind and so are we (spec §8).
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tower::limit::ConcurrencyLimitLayer;

const MAX_BLOB_BYTES: usize = 8 * 1024;
const MAX_DEVICES: usize = 1_000_000;
/// Presence older than this is "offline".
const PRESENCE_TTL_SECS: i64 = 120;

#[derive(Parser)]
#[command(
    name = "dropbridge-rendezvous",
    version,
    about = "DropBridge minimal control plane"
)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:8090")]
    bind: SocketAddr,
    /// SQLite database path ("memory" for ephemeral).
    #[arg(long, default_value = "rendezvous.sqlite")]
    db: String,
}

struct AppState {
    db: Mutex<Connection>,
}

#[derive(Deserialize)]
struct PresenceReq {
    /// z-base-32 device id.
    device: String,
    /// unix seconds.
    ts: i64,
    /// base64 (standard) opaque endpoint hints blob.
    blob: String,
    /// base64 Ed25519 signature over the canonical message.
    sig: String,
}

#[derive(Serialize)]
struct PresenceResp {
    online: bool,
    last_seen: i64,
    blob: String,
}

#[derive(Deserialize)]
struct WakeReq {
    from: String,
    to: String,
    ts: i64,
    sig: String,
}

#[derive(Serialize)]
struct WakeItem {
    from: String,
    ts: i64,
}

fn canonical(device: &str, ts: i64, blob: &[u8]) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(b"dropbridge-rendezvous/1/");
    m.extend_from_slice(device.as_bytes());
    m.extend_from_slice(b"/");
    m.extend_from_slice(&ts.to_be_bytes());
    m.extend_from_slice(b"/");
    m.extend_from_slice(blob);
    m
}

fn verify_sig(device_z32: &str, ts: i64, blob: &[u8], sig_b64: &str) -> bool {
    let Ok(pk) = iroh_base::PublicKey::from_z32(device_z32) else {
        return false;
    };
    let Ok(sig_bytes) = data_encoding::BASE64.decode(sig_b64.as_bytes()) else {
        return false;
    };
    let Ok(sig) = sig_bytes
        .as_slice()
        .try_into()
        .map(iroh_base::Signature::from_bytes)
    else {
        return false;
    };
    pk.verify(&canonical(device_z32, ts, blob), &sig).is_ok()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn put_presence(
    State(st): State<Arc<AppState>>,
    Json(req): Json<PresenceReq>,
) -> impl IntoResponse {
    if req.blob.len() > MAX_BLOB_BYTES * 4 / 3 + 64 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "blob too large".to_string());
    }
    let Ok(blob) = data_encoding::BASE64.decode(req.blob.as_bytes()) else {
        return (StatusCode::BAD_REQUEST, "blob not base64".to_string());
    };
    if blob.len() > MAX_BLOB_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "blob too large".to_string());
    }
    if (req.ts - now()).abs() > 300 {
        return (StatusCode::BAD_REQUEST, "timestamp skew".to_string());
    }
    if !verify_sig(&req.device, req.ts, &blob, &req.sig) {
        return (StatusCode::UNAUTHORIZED, "bad signature".to_string());
    }
    let db = match st.db.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let exists: bool = db
        .query_row(
            "SELECT 1 FROM presence WHERE device=?1",
            params![req.device],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM presence", [], |r| r.get(0))
            .unwrap_or(0);
        if count >= MAX_DEVICES as i64 {
            return (StatusCode::SERVICE_UNAVAILABLE, "registry full".to_string());
        }
    }
    if let Err(e) = db.execute(
        "INSERT INTO presence(device, blob, last_seen) VALUES (?1, ?2, ?3)
         ON CONFLICT(device) DO UPDATE SET blob=?2, last_seen=?3",
        params![req.device, req.blob, now()],
    ) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("db: {e}"));
    }
    (StatusCode::NO_CONTENT, String::new())
}

async fn get_presence(
    State(st): State<Arc<AppState>>,
    Path(device): Path<String>,
) -> impl IntoResponse {
    let db = match st.db.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let row: Result<(String, i64), _> = db.query_row(
        "SELECT blob, last_seen FROM presence WHERE device=?1",
        params![device],
        |r| Ok((r.get(0)?, r.get(1)?)),
    );
    match row {
        Ok((blob, last_seen)) => (
            StatusCode::OK,
            Json(PresenceResp {
                online: now() - last_seen < PRESENCE_TTL_SECS,
                last_seen,
                blob,
            }),
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "unknown device").into_response(),
    }
}

async fn post_wake(State(st): State<Arc<AppState>>, Json(req): Json<WakeReq>) -> impl IntoResponse {
    // The sender signs: canonical(from, ts, to-as-blob).
    if !verify_sig(&req.from, req.ts, req.to.as_bytes(), &req.sig) {
        return (StatusCode::UNAUTHORIZED, "bad signature".to_string());
    }
    if (req.ts - now()).abs() > 300 {
        return (StatusCode::BAD_REQUEST, "timestamp skew".to_string());
    }
    let db = match st.db.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM wakes WHERE recipient=?1",
            params![req.to],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if count >= 25 {
        return (StatusCode::TOO_MANY_REQUESTS, "too many pending wakes".to_string());
    }
    let _ = db.execute(
        "INSERT INTO wakes(recipient, sender, ts) VALUES (?1, ?2, ?3)",
        params![req.to, req.from, req.ts],
    );
    (StatusCode::NO_CONTENT, String::new())
}

async fn get_wakes(
    State(st): State<Arc<AppState>>,
    Path(device): Path<String>,
) -> impl IntoResponse {
    let db = match st.db.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let mut stmt = match db
        .prepare("SELECT sender, ts FROM wakes WHERE recipient=?1 AND ts > ?2 ORDER BY ts LIMIT 100")
    {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };
    let since = now() - 3600;
    let items: Vec<WakeItem> = stmt
        .query_map(params![device, since], |r| {
            Ok(WakeItem {
                from: r.get(0)?,
                ts: r.get(1)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    let _ = db.execute(
        "DELETE FROM wakes WHERE recipient=?1 AND ts <= ?2",
        params![device, since],
    );
    (StatusCode::OK, Json(items)).into_response()
}

async fn healthz() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();

    let db = if cli.db == "memory" {
        Connection::open_in_memory()?
    } else {
        Connection::open(&cli.db)?
    };
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS presence(
            device TEXT PRIMARY KEY,
            blob TEXT NOT NULL,
            last_seen INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS wakes(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            recipient TEXT NOT NULL,
            sender TEXT NOT NULL,
            ts INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS wakes_by_recipient ON wakes(recipient, ts);",
    )?;

    let state = Arc::new(AppState { db: Mutex::new(db) });

    // Periodic housekeeping: purge wakes older than 24h and presence older than 7d
    let cleanup_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            interval.tick().await;
            let db = match cleanup_state.db.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let cutoff_wakes = now() - 86400;
            let cutoff_presence = now() - (7 * 86400);
            let _ = db.execute("DELETE FROM wakes WHERE ts < ?1", params![cutoff_wakes]);
            let _ = db.execute("DELETE FROM presence WHERE last_seen < ?1", params![cutoff_presence]);
        }
    });

    let app = Router::new()
        .route("/v1/presence", post(put_presence))
        .route("/v1/presence/{device}", get(get_presence))
        .route("/v1/wake", post(post_wake))
        .route("/v1/wake/{device}", get(get_wakes))
        // Concurrency limit: max 50 in-flight requests.
        // Protects the single-writer SQLite mutex from thread starvation
        // under load. Signature verification on write endpoints already
        // prevents unauthenticated abuse.
        .layer(ConcurrencyLimitLayer::new(50))
        .route("/healthz", get(healthz))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    tracing::info!(bind = %cli.bind, "rendezvous listening (no file data ever passes here)");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_is_stable() {
        let a = canonical("dev", 5, b"x");
        let b = canonical("dev", 5, b"x");
        assert_eq!(a, b);
        assert_ne!(a, canonical("dev", 6, b"x"));
    }

    #[test]
    fn signature_roundtrip() {
        let sk = iroh_base::SecretKey::generate();
        let id = sk.public().to_z32();
        let blob = b"some opaque hints";
        let ts = now();
        let sig = sk.sign(&canonical(&id, ts, blob));
        let sig_b64 = data_encoding::BASE64.encode(&sig.to_bytes());
        assert!(verify_sig(&id, ts, blob, &sig_b64));
        assert!(!verify_sig(&id, ts, b"tampered", &sig_b64));
    }
}
