//! BLAKE3 file hashing (spec §41).
//!
//! Integrity hashing is deliberately separate from transport encryption.
//! The sender hashes the whole file once, sequentially (pipelined with the
//! transfer in production; page cache makes this cheap); the receiver hashes
//! the fully-written file once at verification time. We never hash "first
//! and then read again for transfer" in the hot path of small transfers:
//! for single-file sends the hash pass overlaps sending.
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HashError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

const BUF: usize = 1024 * 1024;

/// Hash a file sequentially, streaming (bounded memory).
pub async fn hash_file(path: &Path) -> Result<[u8; 32], HashError> {
    let path = path.to_path_buf();
    // Blocking I/O on the dedicated blocking pool keeps the async runtime
    // responsive even with slow disks.
    tokio::task::spawn_blocking(move || hash_file_sync(&path))
        .await
        .map_err(|e| HashError::Io(std::io::Error::other(e.to_string())))?
}

pub fn hash_file_sync(path: &Path) -> Result<[u8; 32], HashError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

pub fn hash_to_hex(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_to_hash(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let h = [0xAB; 32];
        assert_eq!(hex_to_hash(&hash_to_hex(&h)).unwrap(), h);
        assert!(hex_to_hash("zz").is_none());
    }

    #[tokio::test]
    async fn hash_matches_sync() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.bin");
        std::fs::write(&f, vec![7u8; 3_000_000]).unwrap();
        let a = hash_file(&f).await.unwrap();
        let b = hash_file_sync(&f).unwrap();
        assert_eq!(a, b);
    }
}
