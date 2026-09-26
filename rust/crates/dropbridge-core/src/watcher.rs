//! Outbox watcher — the Windows "drop files into a folder" UX (spec §33–35).
//!
//! Watches `<base>/DropBridge/To Phone`. Items (files or directories) that are
//! *stable* — same size/mtime across two consecutive polls at least
//! `STABLE_GAP` apart — are queued for transfer to the selected phone.
//! Successful deliveries are moved to `To Phone/Sent/<YYYY-MM-DD>/`, never
//! deleted. Failures stay in place and are retried with backoff.
//!
//! No platform APIs needed: plain `std::fs` metadata polling works on
//! Windows, Linux and macOS.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::events::NodeEvent;
use crate::node::Node;
use crate::session;
use crate::CoreError;

const POLL_EVERY: Duration = Duration::from_secs(2);
const STABLE_GAP: Duration = Duration::from_secs(1);
const RETRY_BACKOFF: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    size: u64,
    mtime: SystemTime,
}

fn snapshot(path: &Path) -> Option<Snapshot> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.is_symlink() {
        return None;
    }
    Some(Snapshot {
        size: meta.len(),
        mtime: meta.modified().ok()?,
    })
}

fn dir_snapshot(path: &Path) -> Option<Snapshot> {
    // Stability of a directory = stability of (entry count, total size, max mtime).
    let mut size = 0u64;
    let mut mtime = SystemTime::UNIX_EPOCH;
    let mut stack = vec![path.to_path_buf()];
    let mut entries = 0u64;
    while let Some(p) = stack.pop() {
        let rd = std::fs::read_dir(&p).ok()?;
        for e in rd.flatten() {
            let ty = e.file_type().ok()?;
            if ty.is_symlink() {
                continue;
            }
            let meta = e.metadata().ok()?;
            entries += 1;
            if ty.is_dir() {
                stack.push(e.path());
            } else {
                size += meta.len();
            }
            if let Ok(m) = meta.modified() {
                if m > mtime {
                    mtime = m;
                }
            }
        }
    }
    Some(Snapshot {
        size: size.wrapping_add(entries.wrapping_mul(4093)),
        mtime,
    })
}

/// Check if a file is currently locked by an active write operation (e.g. Windows Explorer copy or download).
fn is_file_locked(path: &Path) -> bool {
    if path.is_file() {
        std::fs::OpenOptions::new().read(true).open(path).is_err()
    } else {
        false
    }
}

/// How to pick the destination phone.
#[derive(Debug, Clone)]
pub enum OutboxTarget {
    /// Trust the automatic choice: newest-seen trusted device that is not us.
    Auto,
    /// Fixed device id (z-base-32 prefix accepted).
    Device(String),
}

/// Handle returned by [`spawn_outbox_watcher`]; dropping/`stop()` ends it.
pub struct OutboxHandle {
    tx: watch::Sender<bool>,
}

impl OutboxHandle {
    pub fn stop(self) {
        let _ = self.tx.send(false);
    }
}

/// Start watching `outbox` and delivering stable items to `target`.
pub fn spawn_outbox_watcher(
    node: Arc<Node>,
    outbox: PathBuf,
    target: OutboxTarget,
) -> Result<OutboxHandle, CoreError> {
    std::fs::create_dir_all(&outbox).map_err(CoreError::Io)?;
    let (tx, mut rx) = watch::channel(true);

    tokio::spawn(async move {
        let mut seen: HashMap<PathBuf, (Snapshot, Option<Instant>)> = HashMap::new();
        let mut failed_at: HashMap<PathBuf, Instant> = HashMap::new();
        let mut backoff: HashMap<PathBuf, Duration> = HashMap::new();

        while *rx.borrow_and_update() {
            tokio::select! {
                r = rx.changed() => {
                    // Err = sender dropped (stop requested via Drop).
                    if r.is_err() || !*rx.borrow() { break; }
                }
                _ = tokio::time::sleep(POLL_EVERY) => {}
            }

            let items = match std::fs::read_dir(&outbox) {
                Ok(rd) => rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        // Skip our own bookkeeping + temp/part files.
                        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                            return false;
                        };
                        if name == "Sent"
                            || name.starts_with('.')
                            || name.ends_with(".dropbridge-part")
                        {
                            return false;
                        }
                        match std::fs::symlink_metadata(p) {
                            Ok(m) => !m.is_symlink(),
                            Err(_) => false,
                        }
                    })
                    .collect::<Vec<_>>(),
                Err(e) => {
                    warn!(?e, "outbox read failed");
                    continue;
                }
            };

            // Drop vanished entries.
            seen.retain(|k, _| items.contains(k));

            for item in items {
                let snap = if item.is_dir() {
                    dir_snapshot(&item)
                } else {
                    snapshot(&item)
                };
                let Some(snap) = snap else { continue };

                // Backoff after a failed attempt.
                if let Some(at) = failed_at.get(&item) {
                    let wait = backoff.get(&item).copied().unwrap_or(RETRY_BACKOFF);
                    if at.elapsed() < wait {
                        continue;
                    }
                }

                let stable = matches!(seen.get(&item), Some((prev, _)) if *prev == snap);
                let first_seen_old =
                    matches!(seen.get(&item), Some((_, Some(t))) if t.elapsed() >= STABLE_GAP);

                if !stable {
                    seen.insert(
                        item.clone(),
                        (
                            snap,
                            seen.get(&item).and_then(|s| s.1).or(Some(Instant::now())),
                        ),
                    );
                    continue;
                }
                if !first_seen_old {
                    continue;
                }
                if is_file_locked(&item) {
                    debug!(path = %item.display(), "outbox item still being written to (locked); deferring transfer");
                    continue;
                }

                info!(path = %item.display(), "outbox item stable — sending");
                let res = deliver(&node, &item, &target).await;
                match res {
                    Ok(()) => {
                        if let Err(e) = move_to_sent(&outbox, &item) {
                            warn!(?e, "delivery ok but move-to-sent failed");
                        }
                        seen.remove(&item);
                        failed_at.remove(&item);
                        backoff.remove(&item);
                    }
                    Err(e) => {
                        warn!(?e, path = %item.display(), "outbox delivery failed; will retry");
                        let next = backoff
                            .get(&item)
                            .map(|d| (*d * 2).min(MAX_BACKOFF))
                            .unwrap_or(RETRY_BACKOFF);
                        backoff.insert(item.clone(), next);
                        failed_at.insert(item.clone(), Instant::now());
                        node.emit(NodeEvent::TransferFailed {
                            session: 0,
                            reason: format!("outbox: {e}"),
                        });
                    }
                }
            }
        }
        debug!("outbox watcher stopped");
    });

    Ok(OutboxHandle { tx })
}

async fn deliver(node: &Arc<Node>, item: &Path, target: &OutboxTarget) -> Result<(), CoreError> {
    let peer = match target {
        OutboxTarget::Device(q) => {
            let devices = node.trusted_devices().await;
            devices
                .into_iter()
                .find(|d| {
                    d.name.to_lowercase().contains(&q.to_lowercase())
                        || dropbridge_network::hints::id_z32(&d.device_id)
                            .to_lowercase()
                            .starts_with(&q.to_lowercase())
                })
                .map(|d| d.device_id)
                .ok_or_else(|| CoreError::Other(format!("no trusted device matches {q:?}")))?
        }
        OutboxTarget::Auto => {
            let devices = node.trusted_devices().await;
            // Prefer phones, then most recently seen.
            devices
                .into_iter()
                .max_by_key(|d| (d.kind == "phone", d.last_seen))
                .map(|d| d.device_id)
                .ok_or_else(|| CoreError::Other("no trusted devices paired yet".into()))?
        }
    };

    let res = session::send_files(
        node,
        &peer,
        dropbridge_transfer::TransferSource::Paths(vec![item.to_path_buf()]),
    )
    .await?;
    if !res.ok {
        return Err(CoreError::Other(format!(
            "peer reported failure: {}",
            res.detail
        )));
    }
    Ok(())
}

fn move_to_sent(outbox: &Path, item: &Path) -> std::io::Result<()> {
    let date = chrono_today();
    let sent = outbox.join("Sent").join(date);
    std::fs::create_dir_all(&sent)?;
    let mut dest = sent.join(item.file_name().unwrap_or_default());
    let mut n = 1;
    while dest.exists() {
        let stem = item.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let ext = item.extension().and_then(|s| s.to_str());
        dest = match ext {
            Some(e) => sent.join(format!("{stem}-{n}.{e}")),
            None => sent.join(format!("{stem}-{n}")),
        };
        n += 1;
    }
    std::fs::rename(item, dest)
}

fn chrono_today() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let days = now.as_secs() / 86_400;
    // Civil-from-days (Howard Hinnant's algorithm) — no chrono dep needed.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_format_is_iso() {
        let d = chrono_today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }

    #[test]
    fn dir_snapshot_changes_with_content() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, b"hello").unwrap();
        let s1 = dir_snapshot(tmp.path()).unwrap();
        std::fs::write(&a, b"hello world").unwrap();
        let s2 = dir_snapshot(tmp.path()).unwrap();
        assert_ne!(s1.size, s2.size);
    }
}
