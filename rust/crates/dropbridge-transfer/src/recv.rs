//! Receiver side: part files, range tracking, verification, atomic rename.
//!
//! Guarantees (spec §43, §51, §54, §74):
//! * incomplete data lives only in `*.dropbridge-part` files,
//! * no file may escape the receive root (defense in depth on top of the
//!   protocol-layer sanitizer),
//! * free space is checked before a transfer starts,
//! * collisions never silently overwrite (default: rename),
//! * rename to the final name happens only after BLAKE3 verification.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use dropbridge_protocol::path_safety::{is_within_root, sanitize_rel_path};
use dropbridge_protocol::{FileEntry, Manifest};

use crate::journal::Journal;
use crate::ranges::RangeSet;
use crate::transport::{ChunkSource, TransportError};

pub const PART_SUFFIX: &str = ".dropbridge-part";

#[derive(Debug, Error)]
pub enum RecvError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsafe path from peer: {0}")]
    UnsafePath(String),
    #[error("not enough disk space: need {needed} bytes, have {available}")]
    NotEnoughSpace { needed: u64, available: u64 },
    #[error("chunk out of bounds: file {file} offset {offset} len {len} size {size}")]
    OutOfBounds {
        file: u32,
        offset: u64,
        len: u64,
        size: u64,
    },
    #[error("hash mismatch")]
    HashMismatch,
    #[error("journal: {0}")]
    Journal(#[from] crate::journal::JournalError),
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionPolicy {
    #[default]
    Rename,
    Replace,
    Skip,
}

struct RecvFile {
    entry: FileEntry,
    part_path: PathBuf,
    final_path: PathBuf,
    ranges: RangeSet,
}

/// Stateful receiver for one transfer session.
pub struct ReceiverEngine {
    root: PathBuf,
    files: HashMap<u32, RecvFile>,
    /// file_id → entry order, kept for deterministic finalize/hash order.
    order: Vec<u32>,
    /// Cached part-file write handles.
    handles: HashMap<u32, tokio::fs::File>,
}

impl ReceiverEngine {
    /// Prepare to receive `manifest` into `root`.
    ///
    /// * sanitizes every path and verifies it stays inside `root`,
    /// * checks free disk space,
    /// * creates part files (pre-created so resume can seek),
    /// * loads already-persisted ranges from the journal.
    pub fn prepare(
        root: &Path,
        manifest: &Manifest,
        journal: &Journal,
        transfer_id: &str,
    ) -> Result<Self, RecvError> {
        manifest
            .validate()
            .map_err(|e| RecvError::UnsafePath(e.to_string()))?;

        // Disk space check BEFORE creating anything (spec §54).
        let available = free_space(root)?;
        if manifest.total_bytes > available {
            return Err(RecvError::NotEnoughSpace {
                needed: manifest.total_bytes,
                available,
            });
        }

        std::fs::create_dir_all(root)?;
        let canonical_root = root.canonicalize()?;

        let mut files = HashMap::new();
        let mut order = Vec::new();
        for entry in &manifest.entries {
            let rel = sanitize_rel_path(&entry.rel_path)
                .map_err(|e| RecvError::UnsafePath(e.to_string()))?;
            let final_path = canonical_root.join(&rel);
            // Defense in depth: canonical candidate must remain inside root.
            let candidate_parent = final_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| canonical_root.clone());
            std::fs::create_dir_all(&candidate_parent)?;
            if !is_within_root(&canonical_root, &candidate_parent) {
                return Err(RecvError::UnsafePath(rel));
            }
            let part_path = part_path_for(&final_path);
            let ranges = journal.ranges_for(transfer_id, entry.file_id)?;
            files.insert(
                entry.file_id,
                RecvFile {
                    entry: entry.clone(),
                    part_path,
                    final_path,
                    ranges,
                },
            );
            order.push(entry.file_id);
        }

        Ok(Self {
            root: canonical_root,
            files,
            order,
            handles: HashMap::new(),
        })
    }

    /// Resume information to send back to the sender (spec §39).
    pub fn have_ranges(&self) -> Vec<dropbridge_protocol::FileRanges> {
        let mut out = Vec::new();
        for id in &self.order {
            let f = &self.files[id];
            if f.ranges.covered() > 0 {
                out.push(dropbridge_protocol::FileRanges {
                    file_id: *id,
                    ranges: f.ranges.as_slice().to_vec(),
                });
            }
        }
        out
    }

    pub fn total_received(&self) -> u64 {
        self.files.values().map(|f| f.ranges.covered()).sum()
    }

    /// Consume chunks from one source until it ends. Persists ranges.
    ///
    /// `fail_after` is a test/chaos hook: error out once that many new bytes
    /// have been written (deterministic fault injection, spec §83).
    pub async fn ingest<S: ChunkSource>(
        &mut self,
        source: &mut S,
        journal: &Journal,
        transfer_id: &str,
        progress: Option<tokio::sync::mpsc::UnboundedSender<u64>>,
        fail_after: Option<u64>,
    ) -> Result<u64, RecvError> {
        let mut written = 0u64;
        while let Some(chunk) = source.next_chunk().await? {
            let h = chunk.header;
            if h.len as usize != chunk.data.len() {
                return Err(RecvError::OutOfBounds {
                    file: h.file_id,
                    offset: h.offset,
                    len: h.len as u64,
                    size: 0,
                });
            }

            let (size, part_path) = {
                let Some(f) = self.files.get(&h.file_id) else {
                    return Err(RecvError::OutOfBounds {
                        file: h.file_id,
                        offset: h.offset,
                        len: h.len as u64,
                        size: 0,
                    });
                };
                (f.entry.size, f.part_path.clone())
            };

            let expected_payload_len = if h.is_compressed {
                if h.uncompressed_len == 0
                    || h.uncompressed_len as usize > crate::compression::MAX_DECOMPRESSED_CHUNK_SIZE
                {
                    return Err(RecvError::OutOfBounds {
                        file: h.file_id,
                        offset: h.offset,
                        len: h.uncompressed_len as u64,
                        size,
                    });
                }
                h.uncompressed_len as usize
            } else {
                chunk.data.len()
            };

            let end = h.offset.checked_add(expected_payload_len as u64).ok_or(
                RecvError::OutOfBounds {
                    file: h.file_id,
                    offset: h.offset,
                    len: expected_payload_len as u64,
                    size,
                },
            )?;
            if end > size {
                return Err(RecvError::OutOfBounds {
                    file: h.file_id,
                    offset: h.offset,
                    len: expected_payload_len as u64,
                    size,
                });
            }

            let payload: std::borrow::Cow<'_, [u8]> = if h.is_compressed {
                let decompressed =
                    crate::compression::decompress_chunk(&chunk.data, expected_payload_len)
                        .map_err(|e| {
                            RecvError::Io(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                e.to_string(),
                            ))
                        })?;
                std::borrow::Cow::Owned(decompressed)
            } else {
                std::borrow::Cow::Borrowed(&chunk.data[..])
            };

            // Cached part-file handle (one per file id).
            if let std::collections::hash_map::Entry::Vacant(e) = self.handles.entry(h.file_id) {
                let file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(false)
                    .open(&part_path)
                    .await?;
                e.insert(file);
            }
            let file = self.handles.get_mut(&h.file_id).expect("inserted above");
            // Idempotency (spec §82): rewriting an existing range converges
            // to the same bytes; range accounting below stays correct.
            file.seek(std::io::SeekFrom::Start(h.offset)).await?;
            file.write_all(&payload).await?;

            let f = self.files.get_mut(&h.file_id).expect("checked above");
            if !f.ranges.contains_offset(h.offset) {
                f.ranges.add(h.offset, end);
                journal.add_range(transfer_id, h.file_id, h.offset, end)?;
                written += payload.len() as u64;
                if let Some(p) = &progress {
                    let _ = p.send(payload.len() as u64);
                }
                if let Some(limit) = fail_after {
                    if written >= limit {
                        return Err(RecvError::Io(std::io::Error::other(
                            "injected fault (test hook)",
                        )));
                    }
                }
            }
        }
        Ok(written)
    }

    /// True when every file is fully covered.
    pub fn complete(&self) -> bool {
        self.files.values().all(|f| f.ranges.complete(f.entry.size))
    }

    /// Verify the assembled content against the sender's BLAKE3 digest.
    pub async fn verify(&mut self, expected_hex: &str) -> Result<bool, RecvError> {
        self.handles.clear();
        let Some(expected) = crate::hash::hex_to_hash(expected_hex) else {
            return Ok(false);
        };
        let files: Vec<(String, PathBuf, u64)> = self
            .order
            .iter()
            .map(|id| {
                let f = &self.files[id];
                (f.entry.rel_path.clone(), f.part_path.clone(), f.entry.size)
            })
            .collect();
        let actual = tokio::task::spawn_blocking(move || {
            let mut hasher = blake3::Hasher::new();
            let mut buf = vec![0u8; 1024 * 1024];
            for (rel, path, size) in files {
                hasher.update(rel.as_bytes());
                hasher.update(&[0x1F]);
                hash_stream_into(&mut hasher, &path, size, &mut buf)?;
            }
            Ok::<[u8; 32], std::io::Error>(*hasher.finalize().as_bytes())
        })
        .await
        .map_err(|e| RecvError::Io(std::io::Error::other(e.to_string())))??;
        Ok(actual == expected)
    }

    /// Compute the transfer digest (sender side uses the source files).
    pub async fn compute_digest(&mut self) -> Result<[u8; 32], RecvError> {
        self.handles.clear();
        let files: Vec<(String, PathBuf, u64)> = self
            .order
            .iter()
            .map(|id| {
                let f = &self.files[id];
                (f.entry.rel_path.clone(), f.part_path.clone(), f.entry.size)
            })
            .collect();
        tokio::task::spawn_blocking(move || {
            let mut hasher = blake3::Hasher::new();
            let mut buf = vec![0u8; 1024 * 1024];
            for (rel, path, size) in files {
                hasher.update(rel.as_bytes());
                hasher.update(&[0x1F]);
                hash_stream_into(&mut hasher, &path, size, &mut buf)?;
            }
            Ok::<[u8; 32], std::io::Error>(*hasher.finalize().as_bytes())
        })
        .await
        .map_err(|e| RecvError::Io(std::io::Error::other(e.to_string())))?
        .map_err(RecvError::Io)
    }

    /// Atomically move part files to final names (spec §43, §74).
    /// Returns the list of final paths actually materialized.
    pub fn finalize(&mut self, policy: CollisionPolicy) -> Result<Vec<PathBuf>, RecvError> {
        self.handles.clear(); // release handles before rename
        if !self.complete() {
            return Err(RecvError::Io(std::io::Error::other(
                "finalize called before transfer complete",
            )));
        }
        let mut out = Vec::new();
        for id in &self.order {
            let f = &self.files[id];
            if f.entry.size == 0 {
                // zero-byte file: ensure existence
                if let Some(parent) = f.final_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(false)
                    .open(&f.final_path)?;
                out.push(f.final_path.clone());
                continue;
            }
            let target = match (f.final_path.exists(), policy) {
                (false, _) => f.final_path.clone(),
                (true, CollisionPolicy::Replace) => f.final_path.clone(),
                (true, CollisionPolicy::Skip) => continue,
                (true, CollisionPolicy::Rename) => find_unused_name(&f.final_path)?,
            };
            std::fs::rename(&f.part_path, &target)?;
            out.push(target);
        }
        Ok(out)
    }

    /// Remove part files (cancel/cleanup).
    pub fn cleanup(&self) {
        for id in &self.order {
            let f = &self.files[id];
            let _ = std::fs::remove_file(&f.part_path);
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

pub fn part_path_for(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_os_string();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

fn hash_stream_into(
    hasher: &mut blake3::Hasher,
    path: &Path,
    size: u64,
    buf: &mut [u8],
) -> std::io::Result<()> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut remaining = size;
    while remaining > 0 {
        let want = buf.len().min(remaining as usize);
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::Error::other("unexpected EOF during verify"));
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

/// `photo.jpg → photo (1).jpg → photo (2).jpg` (spec §74).
pub fn find_unused_name(path: &Path) -> Result<PathBuf, RecvError> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned());
    let dir = path.parent().unwrap_or(Path::new(""));
    for i in 1u64..1_000_000 {
        let name = match &ext {
            Some(e) if !e.is_empty() => format!("{stem} ({i}).{e}"),
            _ => format!("{stem} ({i})"),
        };
        let cand = dir.join(name);
        if !cand.exists() {
            return Ok(cand);
        }
    }
    Err(RecvError::Io(std::io::Error::other(
        "too many name collisions",
    )))
}

/// Free disk space for the filesystem containing `path` (best effort).
pub fn free_space(path: &Path) -> Result<u64, RecvError> {
    let probe = if path.exists() {
        path.to_path_buf()
    } else {
        // walk up to an existing ancestor
        let mut p = path.to_path_buf();
        loop {
            if p.exists() {
                break p;
            }
            if !p.pop() {
                break PathBuf::from(".");
            }
        }
    };
    #[cfg(unix)]
    {
        let st = nix::sys::statvfs::statvfs(&probe)
            .map_err(|e| RecvError::Io(std::io::Error::other(e.to_string())))?;
        Ok(st.blocks_available() as u64 * st.fragment_size() as u64)
    }
    #[cfg(windows)]
    #[allow(unsafe_code)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut path_wide: Vec<u16> = probe.as_os_str().encode_wide().collect();
        path_wide.push(0);
        let mut free_bytes_available: u64 = 0;
        let mut total_number_of_bytes: u64 = 0;
        let mut total_number_of_free_bytes: u64 = 0;
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                lpDirectoryName: *const u16,
                lpFreeBytesAvailableToCaller: *mut u64,
                lpTotalNumberOfBytes: *mut u64,
                lpTotalNumberOfFreeBytes: *mut u64,
            ) -> i32;
        }
        let ret = unsafe {
            GetDiskFreeSpaceExW(
                path_wide.as_ptr(),
                &mut free_bytes_available,
                &mut total_number_of_bytes,
                &mut total_number_of_free_bytes,
            )
        };
        if ret != 0 {
            Ok(free_bytes_available)
        } else {
            Ok(u64::MAX)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = probe;
        Ok(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ChunkPayload;
    use bytes::Bytes;
    use dropbridge_protocol::FileEntry;

    fn manifest_of(entries: Vec<(&str, u64)>) -> Manifest {
        Manifest::new(
            entries
                .into_iter()
                .enumerate()
                .map(|(i, (p, s))| FileEntry {
                    rel_path: p.into(),
                    size: s,
                    mtime_secs: 0,
                    file_id: i as u32,
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn receive_small_file_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("recv");
        let jdir = dir.path().join("j");
        let journal = Journal::open_in(&jdir).unwrap();
        let m = manifest_of(vec![("hello.txt", 11)]);
        let mut eng = ReceiverEngine::prepare(&root, &m, &journal, "t1").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChunkPayload>(4);
        tx.send(ChunkPayload {
            header: crate::ChunkHeader::raw(1, 0, 0, 0, 11),
            data: Bytes::from_static(b"hello world"),
        })
        .await
        .unwrap();
        drop(tx);
        eng.ingest(&mut rx, &journal, "t1", None, None)
            .await
            .unwrap();
        assert!(eng.complete());

        // digest over "hello.txt" + 0x1F + content
        let digest = eng.compute_digest().await.unwrap();
        assert!(eng
            .verify(&crate::hash::hash_to_hex(&digest))
            .await
            .unwrap());
        assert!(!eng
            .verify(&crate::hash::hash_to_hex(&[0u8; 32]))
            .await
            .unwrap());

        let finals = eng.finalize(CollisionPolicy::Rename).unwrap();
        assert_eq!(finals.len(), 1);
        assert_eq!(std::fs::read(&finals[0]).unwrap(), b"hello world");
        // no part files left
        assert!(!finals[0].with_extension("txt.dropbridge-part").exists());
    }

    #[tokio::test]
    async fn receive_compressed_chunk_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("recv");
        let jdir = dir.path().join("j");
        let journal = Journal::open_in(&jdir).unwrap();
        let original_data = b"dropbridge repetitive text for 2026 compression testing ".repeat(20);
        let m = manifest_of(vec![("data.txt", original_data.len() as u64)]);
        let mut eng = ReceiverEngine::prepare(&root, &m, &journal, "t_comp").unwrap();

        let compressed = zstd::bulk::compress(&original_data, 1).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChunkPayload>(4);
        tx.send(ChunkPayload {
            header: crate::ChunkHeader {
                session: 1,
                file_id: 0,
                chunk_idx: 0,
                offset: 0,
                len: compressed.len() as u32,
                is_compressed: true,
                uncompressed_len: original_data.len() as u32,
            },
            data: Bytes::from(compressed),
        })
        .await
        .unwrap();
        drop(tx);
        eng.ingest(&mut rx, &journal, "t_comp", None, None)
            .await
            .unwrap();
        assert!(eng.complete());

        let digest = eng.compute_digest().await.unwrap();
        assert!(eng
            .verify(&crate::hash::hash_to_hex(&digest))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn resume_ranges_survive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("recv");
        let jdir = dir.path().join("j");
        let journal = Journal::open_in(&jdir).unwrap();
        let m = manifest_of(vec![("big.bin", 8)]);
        {
            let mut eng = ReceiverEngine::prepare(&root, &m, &journal, "t2").unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            tx.send(ChunkPayload {
                header: crate::ChunkHeader::raw(1, 0, 0, 0, 4),
                data: Bytes::from_static(&[1, 2, 3, 4]),
            })
            .await
            .unwrap();
            drop(tx);
            eng.ingest(&mut rx, &journal, "t2", None, None)
                .await
                .unwrap();
        }
        // "restart": fresh engine, same journal
        let eng2 = ReceiverEngine::prepare(&root, &m, &journal, "t2").unwrap();
        let have = eng2.have_ranges();
        assert_eq!(have.len(), 1);
        assert_eq!(have[0].ranges, vec![(0, 4)]);
    }

    #[tokio::test]
    async fn path_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("recv");
        let jdir = dir.path().join("j");
        let journal = Journal::open_in(&jdir).unwrap();
        let m = manifest_of(vec![("../evil.txt", 3)]);
        let r = ReceiverEngine::prepare(&root, &m, &journal, "t3");
        assert!(matches!(r, Err(RecvError::UnsafePath(_))));
    }

    #[test]
    fn collision_renaming() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("photo.jpg");
        std::fs::write(&p, b"x").unwrap();
        let a = find_unused_name(&p).unwrap();
        assert_eq!(a.file_name().unwrap(), "photo (1).jpg");
        std::fs::write(&a, b"y").unwrap();
        let b = find_unused_name(&p).unwrap();
        assert_eq!(b.file_name().unwrap(), "photo (2).jpg");
    }

    #[tokio::test]
    async fn out_of_bounds_chunk_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("recv");
        let jdir = dir.path().join("j");
        let journal = Journal::open_in(&jdir).unwrap();
        let m = manifest_of(vec![("f.bin", 4)]);
        let mut eng = ReceiverEngine::prepare(&root, &m, &journal, "t4").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send(ChunkPayload {
            header: crate::ChunkHeader::raw(1, 0, 0, 2, 4),
            data: Bytes::from_static(&[0, 0, 0, 0]),
        })
        .await
        .unwrap();
        drop(tx);
        let r = eng.ingest(&mut rx, &journal, "t4", None, None).await;
        assert!(matches!(r, Err(RecvError::OutOfBounds { .. })));
    }
}
