//! Transport abstraction: the engine moves chunk payloads through anything
//! implementing these traits. In production that is an iroh/QUIC stream
//! adapter (see `dropbridge-network`); in tests, in-memory pipes.
use bytes::Bytes;
use thiserror::Error;

use crate::ChunkHeader;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport write failed: {0}")]
    Write(String),
    #[error("transport read failed: {0}")]
    Read(String),
    #[error("transport closed")]
    Closed,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One chunk of payload ready to go on the wire.
#[derive(Debug, Clone)]
pub struct ChunkPayload {
    pub header: ChunkHeader,
    pub data: Bytes,
}

/// Something a sender can push chunks into (one QUIC send stream per sink).
pub trait ChunkSink {
    /// Write header+bytes. Implementations should avoid extra copies
    /// (spec §68): bytes go disk → buffer → network.
    fn send_chunk(
        &mut self,
        chunk: ChunkPayload,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    /// Flush/finish the underlying stream.
    fn finish(&mut self) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;
}

/// Something a receiver reads chunks from (one QUIC recv stream per source).
pub trait ChunkSource {
    /// Read the next chunk; `None` when the stream ends cleanly.
    fn next_chunk(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<ChunkPayload>, TransportError>> + Send;
}

/// Channel-based sink: lets us decouple engine tasks from the wire in tests
/// and in the network adapter (bounded backpressure).
impl ChunkSink for tokio::sync::mpsc::Sender<ChunkPayload> {
    async fn send_chunk(&mut self, chunk: ChunkPayload) -> Result<(), TransportError> {
        self.send(chunk).await.map_err(|_| TransportError::Closed)
    }
    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

impl ChunkSource for tokio::sync::mpsc::Receiver<ChunkPayload> {
    async fn next_chunk(&mut self) -> Result<Option<ChunkPayload>, TransportError> {
        Ok(self.recv().await)
    }
}
