//! Building manifests from local paths (spec §45–46).
use std::path::{Path, PathBuf};

use dropbridge_protocol::{FileEntry, Manifest};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("path is not valid utf-8: {0:?}")]
    NotUtf8(PathBuf),
    #[error("too many files (limit {0})")]
    TooManyFiles(usize),
    #[error("source not found: {0:?}")]
    NotFound(PathBuf),
    #[error("raw file descriptors are not supported on this platform (no /proc/self/fd): pass file paths instead")]
    UnsupportedFdSource,
}

/// Raw file descriptor source (e.g. from Android ContentResolver/ParcelFileDescriptor).
#[derive(Debug, Clone)]
pub struct FdSource {
    pub name: String,
    pub size: u64,
    pub mtime_secs: i64,
    pub fd: i32,
}

/// What the user asked to send.
#[derive(Debug, Clone)]
pub enum TransferSource {
    /// Explicit list of files/directories.
    Paths(Vec<PathBuf>),
    /// Raw file descriptors (e.g. Android ContentResolver).
    Fds(Vec<FdSource>),
}

/// Walk the sources and produce a manifest with `/`-separated relative paths.
///
/// Layout rules (spec §46): a single file sends as `file.ext`; a single
/// directory sends its contents under the directory name; multiple items send
/// under their own names.
pub fn build_manifest(src: &TransferSource) -> Result<(Manifest, Vec<PathBuf>), PlanError> {
    match src {
        TransferSource::Paths(paths) => {
            let mut entries: Vec<FileEntry> = Vec::new();
            let mut abs_paths: Vec<PathBuf> = Vec::new();
            let limit = dropbridge_protocol::limits::MAX_MANIFEST_ENTRIES;

            let items: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
            for p in &items {
                if !p.exists() {
                    return Err(PlanError::NotFound(p.to_path_buf()));
                }
            }

            let named_root: Option<&Path> = if items.len() == 1 {
                let p = items[0];
                if p.is_dir() {
                    // send "Dir/" itself, i.e. keep the directory name
                    p.parent()
                } else {
                    None
                }
            } else {
                None
            };

            for p in &items {
                walk(p, p, named_root, &mut entries, &mut abs_paths, limit)?;
            }

            // Assign file ids.
            for (i, e) in entries.iter_mut().enumerate() {
                e.file_id = i as u32;
            }
            Ok((Manifest::new(entries), abs_paths))
        }
        // Windows has no /proc/self/fd, and a CRT file descriptor cannot be
        // turned back into a path without a HANDLE, so refuse loudly instead of
        // producing a manifest that fails at the first read.
        #[cfg(windows)]
        TransferSource::Fds(_) => Err(PlanError::UnsupportedFdSource),
        #[cfg(not(windows))]
        TransferSource::Fds(fds) => {
            let limit = dropbridge_protocol::limits::MAX_MANIFEST_ENTRIES;
            if fds.len() > limit {
                return Err(PlanError::TooManyFiles(fds.len()));
            }
            let mut entries = Vec::with_capacity(fds.len());
            let mut abs_paths = Vec::with_capacity(fds.len());
            for (i, item) in fds.iter().enumerate() {
                #[cfg(target_os = "macos")]
                let path = PathBuf::from(format!("/dev/fd/{}", item.fd));
                #[cfg(not(target_os = "macos"))]
                let path = PathBuf::from(format!("/proc/self/fd/{}", item.fd));

                let safe_name = dropbridge_protocol::path_safety::sanitize_rel_path(&item.name)
                    .unwrap_or_else(|_| format!("file_{i}"));
                entries.push(FileEntry {
                    rel_path: safe_name,
                    size: item.size,
                    mtime_secs: item.mtime_secs,
                    file_id: i as u32,
                });
                abs_paths.push(path);
            }
            Ok((Manifest::new(entries), abs_paths))
        }
    }
}

fn walk(
    base: &Path,
    current: &Path,
    named_root: Option<&Path>,
    entries: &mut Vec<FileEntry>,
    abs_paths: &mut Vec<PathBuf>,
    limit: usize,
) -> Result<(), PlanError> {
    let meta = match std::fs::symlink_metadata(current) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.is_symlink() {
        return Ok(()); // symlinks/devices: skip (never follow out of the tree)
    }
    if meta.is_file() {
        let rel = rel_to(base, current, named_root)?;
        push_entry(current, &rel, entries, abs_paths, limit)?;
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(()); // special files: skip
    }
    let mut kids: Vec<_> = std::fs::read_dir(current)?.collect();
    // Deterministic order → stable manifests.
    kids.sort_by_key(|e| e.as_ref().map(|e| e.file_name()).unwrap_or_default());
    for kid in kids {
        let kid = kid?;
        let path = kid.path();
        if path.is_symlink() {
            continue;
        }
        walk(base, &path, named_root, entries, abs_paths, limit)?;
    }
    Ok(())
}

fn push_entry(
    abs: &Path,
    rel: &str,
    entries: &mut Vec<FileEntry>,
    abs_paths: &mut Vec<PathBuf>,
    limit: usize,
) -> Result<(), PlanError> {
    if entries.len() >= limit {
        return Err(PlanError::TooManyFiles(limit));
    }
    let meta = std::fs::metadata(abs)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    entries.push(FileEntry {
        rel_path: rel.to_string(),
        size: meta.len(),
        mtime_secs: mtime,
        file_id: entries.len() as u32,
    });
    abs_paths.push(abs.to_path_buf());
    Ok(())
}

fn rel_to(base: &Path, current: &Path, named_root: Option<&Path>) -> Result<String, PlanError> {
    // For a single-directory transfer we keep the directory name as prefix.
    let root = match named_root {
        Some(r) if base.starts_with(r) => r,
        _ => base.parent().unwrap_or(base),
    };
    let rel = if named_root.is_some() {
        current.strip_prefix(root).unwrap_or(current)
    } else if base.is_file() {
        // single file → just its name
        Path::new(base.file_name().unwrap_or_default())
    } else {
        current.strip_prefix(root).unwrap_or(current)
    };
    let mut s = String::new();
    for c in rel.components() {
        let part = c
            .as_os_str()
            .to_str()
            .ok_or_else(|| PlanError::NotUtf8(current.to_path_buf()))?;
        if !s.is_empty() {
            s.push('/');
        }
        s.push_str(part);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn single_file_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.jpg");
        fs::write(&f, b"12345").unwrap();
        let (m, paths) = build_manifest(&TransferSource::Paths(vec![f.clone()])).unwrap();
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].rel_path, "a.jpg");
        assert_eq!(m.entries[0].size, 5);
        assert_eq!(m.total_bytes, 5);
        assert_eq!(paths, vec![f]);
    }

    #[test]
    fn directory_manifest_preserves_structure() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Photos");
        fs::create_dir_all(root.join("2026")).unwrap();
        fs::write(root.join("2026/c.jpg"), b"abc").unwrap();
        fs::write(root.join("a.jpg"), b"1").unwrap();
        let (m, _) = build_manifest(&TransferSource::Paths(vec![root])).unwrap();
        let mut rels: Vec<&str> = m.entries.iter().map(|e| e.rel_path.as_str()).collect();
        rels.sort();
        assert_eq!(rels, vec!["Photos/2026/c.jpg", "Photos/a.jpg"]);
        assert_eq!(m.total_bytes, 4);
    }

    #[test]
    fn multiple_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"x").unwrap();
        fs::write(&b, b"yz").unwrap();
        let (m, _) = build_manifest(&TransferSource::Paths(vec![a, b])).unwrap();
        assert_eq!(m.total_bytes, 3);
        assert_eq!(m.entries.len(), 2);
    }

    #[test]
    fn missing_source_errors() {
        let r = build_manifest(&TransferSource::Paths(vec!["/no/such".into()]));
        assert!(matches!(r, Err(PlanError::NotFound(_))));
    }

    #[test]
    #[cfg(not(windows))]
    fn fd_manifest_streaming() {
        let fds = vec![
            FdSource {
                name: "video.mp4".into(),
                size: 1_048_576,
                mtime_secs: 1_700_000_000,
                fd: 3,
            },
            FdSource {
                name: "notes.txt".into(),
                size: 256,
                mtime_secs: 1_700_000_001,
                fd: 4,
            },
        ];
        let (m, abs) = build_manifest(&TransferSource::Fds(fds)).unwrap();
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.total_bytes, 1_048_576 + 256);
        assert_eq!(m.entries[0].rel_path, "video.mp4");
        assert_eq!(m.entries[1].rel_path, "notes.txt");
        assert_eq!(abs.len(), 2);
    }

    #[test]
    #[cfg(windows)]
    fn fd_manifest_is_refused_on_windows() {
        let fds = vec![FdSource {
            name: "a.txt".into(),
            size: 1,
            mtime_secs: 0,
            fd: 3,
        }];
        assert!(matches!(
            build_manifest(&TransferSource::Fds(fds)),
            Err(PlanError::UnsupportedFdSource)
        ));
    }

    #[test]
    #[cfg(unix)]
    fn top_level_symlink_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("secret.txt");
        std::fs::write(&target, b"secret").unwrap();
        let symlink_path = tmp.path().join("link_to_secret.txt");
        std::os::unix::fs::symlink(&target, &symlink_path).unwrap();

        let (m, abs) = build_manifest(&TransferSource::Paths(vec![symlink_path])).unwrap();
        assert_eq!(m.entries.len(), 0);
        assert_eq!(abs.len(), 0);
    }
}
