//! Sender side of the transfer engine.
//!
//! Design (spec §36–44):
//! * chunk jobs are generated from the manifest minus already-acked ranges,
//! * `stream_count` worker tasks pull jobs from a shared queue and push
//!   chunk payloads through their [`ChunkSink`] (one QUIC stream each),
//! * file reads are seeked partial reads — the file is never fully buffered,
//! * bounded memory: at most `stream_count` chunk buffers in flight.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use dropbridge_protocol::{FileEntry, Manifest};

use crate::journal::Journal;
use crate::ranges::RangeSet;
use crate::transport::{ChunkPayload, ChunkSink, TransportError};
use crate::ChunkHeader;

/// One file to send.
#[derive(Debug, Clone)]
pub struct SendFile {
    pub entry: FileEntry,
    pub path: PathBuf,
    /// Ranges the receiver already has (resume).
    pub have: RangeSet,
}

/// Negotiated send plan.
#[derive(Debug, Clone)]
pub struct SendPlan {
    pub session: u64,
    pub chunk_size: u64,
    pub stream_count: u32,
    pub files: Vec<SendFile>,
    pub manifest: Manifest,
}

impl SendPlan {
    /// Total bytes still to transfer.
    pub fn remaining_bytes(&self) -> u64 {
        self.files
            .iter()
            .map(|f| f.entry.size - f.have.covered().min(f.entry.size))
            .sum()
    }
}

#[derive(Debug, Clone, Copy)]
struct ChunkJob {
    file_idx: u32,
    chunk_idx: u64,
    offset: u64,
    len: u32,
}

/// Progress notification (bytes transferred since last update).
pub type ProgressTx = tokio::sync::mpsc::UnboundedSender<u64>;

/// Run the sender with `stream_count` sinks. Returns when every job is done
/// or a sink fails (caller decides about retry/resume).
pub async fn run_sender<S: ChunkSink + Send + 'static>(
    plan: Arc<SendPlan>,
    journal: Arc<Journal>,
    transfer_id: String,
    sinks: Vec<S>,
    progress: Option<ProgressTx>,
) -> Result<u64, TransportError> {
    let queue = build_job_queue(&plan);
    let queue = Arc::new(Mutex::new(VecDeque::from(queue)));

    let mut handles = Vec::new();
    for sink in sinks {
        let q = Arc::clone(&queue);
        let p = Arc::clone(&plan);
        let j = Arc::clone(&journal);
        let tid = transfer_id.clone();
        let prog = progress.clone();
        handles.push(tokio::spawn(async move {
            worker(sink, q, p, j, tid, prog).await
        }));
    }

    let mut first_err: Option<TransportError> = None;
    let mut sent = 0u64;
    for h in handles {
        match h.await {
            Ok(Ok(n)) => sent += n,
            Ok(Err(e)) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(TransportError::Read(e.to_string()));
                }
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(sent),
    }
}

fn build_job_queue(plan: &SendPlan) -> Vec<ChunkJob> {
    let mut jobs = Vec::new();
    let cs = plan.chunk_size;
    for (file_idx, f) in plan.files.iter().enumerate() {
        let size = f.entry.size;
        if size == 0 {
            continue;
        }
        for (a, b) in f.have.missing(size) {
            let mut off = a;
            while off < b {
                let len = cs.min(b - off);
                jobs.push(ChunkJob {
                    file_idx: file_idx as u32,
                    chunk_idx: off / cs,
                    offset: off,
                    len: len as u32,
                });
                off += len;
            }
        }
    }
    jobs
}

async fn worker<S: ChunkSink + Send>(
    mut sink: S,
    queue: Arc<Mutex<VecDeque<ChunkJob>>>,
    plan: Arc<SendPlan>,
    journal: Arc<Journal>,
    transfer_id: String,
    progress: Option<ProgressTx>,
) -> Result<u64, TransportError> {
    let mut sent = 0u64;
    // Reusable read buffer: one chunk per worker keeps memory bounded.
    let mut buf: Vec<u8> = vec![0; plan.chunk_size as usize];
    let mut open_file: Option<(u32, tokio::fs::File)> = None;

    loop {
        let job = {
            let mut q = queue.lock().await;
            q.pop_front()
        };
        let Some(job) = job else { break };
        let file = &plan.files[job.file_idx as usize];

        // Keep one file handle hot per worker; reopen on file change.
        let need_open = match &open_file {
            Some((idx, _)) => *idx != job.file_idx,
            None => true,
        };
        if need_open {
            let f = tokio::fs::File::open(&file.path)
                .await
                .map_err(|e| TransportError::Read(e.to_string()))?;
            open_file = Some((job.file_idx, f));
        }
        let (_, f) = open_file.as_mut().expect("opened above");

        f.seek(std::io::SeekFrom::Start(job.offset))
            .await
            .map_err(|e| TransportError::Read(e.to_string()))?;
        let need = job.len as usize;
        let got = read_full(f, &mut buf[..need])
            .await
            .map_err(|e| TransportError::Read(e.to_string()))?;
        if got != need {
            return Err(TransportError::Read(format!(
                "short read on {:?} at {}",
                file.path, job.offset
            )));
        }

        let (data, is_compressed) = if crate::compression::is_compressible_path(&file.path) {
            match crate::compression::compress_chunk(&buf[..need], 1) {
                Some(compressed) => (Bytes::from(compressed), true),
                None => (Bytes::copy_from_slice(&buf[..need]), false),
            }
        } else {
            (Bytes::copy_from_slice(&buf[..need]), false)
        };

        let payload = ChunkPayload {
            header: ChunkHeader {
                session: plan.session,
                file_id: file.entry.file_id,
                chunk_idx: job.chunk_idx,
                offset: job.offset,
                len: data.len() as u32,
                is_compressed,
                uncompressed_len: job.len,
            },
            data,
        };
        sink.send_chunk(payload).await?;

        // Journaling per chunk is the crash-consistency backbone of resume.
        // For small-file batches this is amortized by SQLite batching.
        journal
            .add_range(
                &transfer_id,
                file.entry.file_id,
                job.offset,
                job.offset + job.len as u64,
            )
            .map_err(|e| TransportError::Write(e.to_string()))?;

        sent += job.len as u64;
        if let Some(p) = &progress {
            let _ = p.send(job.len as u64);
        }
    }

    sink.finish().await?;
    Ok(sent)
}

async fn read_full(f: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = f.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_queue_respects_have_ranges() {
        let mut have = RangeSet::new();
        have.add(0, 1024); // first 1 KiB already there
        let plan = SendPlan {
            session: 1,
            chunk_size: 1024,
            stream_count: 2,
            files: vec![SendFile {
                entry: FileEntry {
                    rel_path: "f".into(),
                    size: 3072,
                    mtime_secs: 0,
                    file_id: 0,
                },
                path: PathBuf::from("/tmp/f"),
                have,
            }],
            manifest: Manifest::new(vec![]),
        };
        let jobs = build_job_queue(&plan);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].offset, 1024);
        assert_eq!(jobs[1].offset, 2048);
        assert_eq!(plan.remaining_bytes(), 2048);
    }

    #[test]
    fn receiver_rewind_forces_sender_retransmission() {
        // If receiver had 2048 bytes but rewind truncated it back to 512 bytes:
        let mut have = RangeSet::new();
        have.add(0, 512); // receiver only acknowledges 0..512
        let plan = SendPlan {
            session: 2,
            chunk_size: 512,
            stream_count: 1,
            files: vec![SendFile {
                entry: FileEntry {
                    rel_path: "f2".into(),
                    size: 2048,
                    mtime_secs: 0,
                    file_id: 0,
                },
                path: PathBuf::from("/tmp/f2"),
                have,
            }],
            manifest: Manifest::new(vec![]),
        };
        let jobs = build_job_queue(&plan);
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].offset, 512);
        assert_eq!(jobs[1].offset, 1024);
        assert_eq!(jobs[2].offset, 1536);
        assert_eq!(plan.remaining_bytes(), 1536);
    }
}
