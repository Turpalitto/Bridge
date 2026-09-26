//! Persistent transfer journal (spec §40) — SQLite, WAL, crash-safe.
//!
//! Stores transfer metadata and completed byte ranges so transfers survive
//! packet loss, app restarts and OS restarts.
use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ranges::RangeSet;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Send,
    Recv,
}

impl Role {
    fn as_str(&self) -> &'static str {
        match self {
            Role::Send => "send",
            Role::Recv => "recv",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "send" => Role::Send,
            _ => Role::Recv,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferState {
    Pending,
    Active,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl TransferState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferState::Pending => "pending",
            TransferState::Active => "active",
            TransferState::Paused => "paused",
            TransferState::Completed => "completed",
            TransferState::Failed => "failed",
            TransferState::Cancelled => "cancelled",
        }
    }
}

impl std::str::FromStr for TransferState {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "pending" => TransferState::Pending,
            "active" => TransferState::Active,
            "paused" => TransferState::Paused,
            "completed" => TransferState::Completed,
            "failed" => TransferState::Failed,
            _ => TransferState::Cancelled,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TransferRecord {
    pub id: String,
    pub peer_id: String,
    pub role: Role,
    pub state: TransferState,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub chunk_size: u64,
    pub stream_count: u32,
    pub manifest_json: String,
    pub root_path: String,
    /// BLAKE3 of the whole transfer payload (sender-computed), hex; empty if unknown yet.
    pub file_hash: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub retry_count: u32,
}

/// Journal handle. One per node; internally synchronized (SQLite is
/// single-writer; the mutex makes the handle `Send + Sync` for async tasks).
pub struct Journal {
    conn: std::sync::Mutex<Connection>,
}

impl Journal {
    /// Open (creating schema if needed) at `dir/journal.sqlite`.
    pub fn open_in(dir: &Path) -> Result<Self, JournalError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("journal.sqlite");
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS transfers(
               id TEXT PRIMARY KEY,
               peer_id TEXT NOT NULL,
               role TEXT NOT NULL,
               state TEXT NOT NULL,
               total_bytes INTEGER NOT NULL,
               done_bytes INTEGER NOT NULL DEFAULT 0,
               chunk_size INTEGER NOT NULL,
               stream_count INTEGER NOT NULL,
               manifest_json TEXT NOT NULL,
               root_path TEXT NOT NULL,
               file_hash TEXT NOT NULL DEFAULT '',
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               retry_count INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS ranges(
               transfer_id TEXT NOT NULL,
               file_id INTEGER NOT NULL,
               start INTEGER NOT NULL,
               end INTEGER NOT NULL,
               PRIMARY KEY(transfer_id, file_id, start)
             );",
        )?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    pub fn create_transfer(&self, rec: &TransferRecord) -> Result<(), JournalError> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO transfers VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                rec.id,
                rec.peer_id,
                rec.role.as_str(),
                rec.state.as_str(),
                rec.total_bytes as i64,
                rec.done_bytes as i64,
                rec.chunk_size as i64,
                rec.stream_count as i32,
                rec.manifest_json,
                rec.root_path,
                rec.file_hash,
                rec.created_at,
                rec.updated_at,
                rec.retry_count as i32,
            ],
        )?;
        Ok(())
    }

    pub fn set_state(&self, id: &str, state: TransferState) -> Result<(), JournalError> {
        self.lock().execute(
            "UPDATE transfers SET state=?2, updated_at=?3 WHERE id=?1",
            params![id, state.as_str(), Self::now()],
        )?;
        Ok(())
    }

    pub fn set_hash(&self, id: &str, hash_hex: &str) -> Result<(), JournalError> {
        self.lock().execute(
            "UPDATE transfers SET file_hash=?2, updated_at=?3 WHERE id=?1",
            params![id, hash_hex, Self::now()],
        )?;
        Ok(())
    }

    pub fn bump_retry(&self, id: &str) -> Result<(), JournalError> {
        self.lock().execute(
            "UPDATE transfers SET retry_count = retry_count + 1, updated_at=?2 WHERE id=?1",
            params![id, Self::now()],
        )?;
        Ok(())
    }

    /// Record a completed range for a file. Updates done_bytes incrementally.
    /// Idempotent: re-inserting an existing range does not double-count.
    pub fn add_range(
        &self,
        transfer_id: &str,
        file_id: u32,
        start: u64,
        end: u64,
    ) -> Result<(), JournalError> {
        let conn = self.lock();
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO ranges VALUES (?1,?2,?3,?4)",
            params![transfer_id, file_id as i32, start as i64, end as i64],
        )?;
        if inserted > 0 {
            conn.execute(
                "UPDATE transfers SET done_bytes = done_bytes + (?3 - ?2), updated_at=?4 WHERE id=?1",
                params![transfer_id, start as i64, end as i64, Self::now()],
            )?;
        }
        Ok(())
    }

    /// Load completed ranges for one file.
    pub fn ranges_for(&self, transfer_id: &str, file_id: u32) -> Result<RangeSet, JournalError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT start, end FROM ranges WHERE transfer_id=?1 AND file_id=?2 ORDER BY start",
        )?;
        let rows = stmt.query_map(params![transfer_id, file_id as i32], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
        })?;
        let mut set = RangeSet::new();
        for r in rows {
            let (a, b) = r?;
            set.add(a, b);
        }
        Ok(set)
    }

    pub fn get(&self, id: &str) -> Result<Option<TransferRecord>, JournalError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT * FROM transfers WHERE id=?1")?;
        let mut rows = stmt.query_map(params![id], Self::row_to_record)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Recent transfers for history UI (spec §75).
    pub fn recent(&self, limit: u32) -> Result<Vec<TransferRecord>, JournalError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT * FROM transfers ORDER BY updated_at DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], Self::row_to_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn remove(&self, id: &str) -> Result<(), JournalError> {
        let conn = self.lock();
        conn.execute("DELETE FROM ranges WHERE transfer_id=?1", params![id])?;
        conn.execute("DELETE FROM transfers WHERE id=?1", params![id])?;
        Ok(())
    }

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransferRecord> {
        let role_str: String = row.get(2)?;
        let state_str: String = row.get(3)?;
        Ok(TransferRecord {
            id: row.get(0)?,
            peer_id: row.get(1)?,
            role: role_str.parse().unwrap(),
            state: state_str.parse().unwrap(),
            total_bytes: row.get::<_, i64>(4)? as u64,
            done_bytes: row.get::<_, i64>(5)? as u64,
            chunk_size: row.get::<_, i64>(6)? as u64,
            stream_count: row.get::<_, i32>(7)? as u32,
            manifest_json: row.get(8)?,
            root_path: row.get(9)?,
            file_hash: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            retry_count: row.get(13)?,
        })
    }
}

/// New random transfer id.
pub fn new_transfer_id() -> String {
    let b: [u8; 16] = rand::random();
    data_encoding::HEXLOWER.encode(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_roundtrip_and_resume() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open_in(dir.path()).unwrap();
        let id = new_transfer_id();
        let rec = TransferRecord {
            id: id.clone(),
            peer_id: "peer".into(),
            role: Role::Recv,
            state: TransferState::Active,
            total_bytes: 1000,
            done_bytes: 0,
            chunk_size: 100,
            stream_count: 4,
            manifest_json: "{}".into(),
            root_path: "/tmp/recv".into(),
            file_hash: String::new(),
            created_at: 1,
            updated_at: 1,
            retry_count: 0,
        };
        j.create_transfer(&rec).unwrap();
        j.add_range(&id, 0, 0, 100).unwrap();
        j.add_range(&id, 0, 100, 200).unwrap();
        j.add_range(&id, 1, 0, 50).unwrap();

        // duplicate range is ignored (idempotent reconnect)
        j.add_range(&id, 0, 0, 100).unwrap();

        let r0 = j.ranges_for(&id, 0).unwrap();
        assert_eq!(r0.as_slice(), &[(0, 200)]);
        let r1 = j.ranges_for(&id, 1).unwrap();
        assert_eq!(r1.as_slice(), &[(0, 50)]);

        let got = j.get(&id).unwrap().unwrap();
        assert_eq!(got.done_bytes, 250); // duplicate not double-counted? see below
        let recent = j.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
    }
}
