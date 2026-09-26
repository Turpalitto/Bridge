//! QUIC stream ↔ chunk adapter (spec §68 low-copy path).
//!
//! Wire format on a data stream:
//!
//! ```text
//! [u32 BE header_len][postcard ChunkHeader][raw chunk bytes][...next chunk]
//! ```
//!
//! Sending uses `write_chunk(Bytes)` so payload bytes are not copied again
//! after leaving the disk-read buffer.
use bytes::{Bytes, BytesMut};
use dropbridge_transfer::transport::{ChunkPayload, ChunkSink, ChunkSource, TransportError};
use dropbridge_transfer::ChunkHeader;

const MAX_HEADER_LEN: usize = 4096;
const MAX_CHUNK_LEN: usize = 16 * 1024 * 1024 + 4096;

pub struct SendChunkStream {
    inner: iroh::endpoint::SendStream,
}

impl SendChunkStream {
    #[must_use]
    pub fn new(inner: iroh::endpoint::SendStream) -> Self {
        Self { inner }
    }

    /// Access the raw stream (e.g. to set priority).
    pub fn inner(&mut self) -> &mut iroh::endpoint::SendStream {
        &mut self.inner
    }
}

impl ChunkSink for SendChunkStream {
    async fn send_chunk(&mut self, chunk: ChunkPayload) -> Result<(), TransportError> {
        let header = postcard::to_allocvec(&chunk.header)
            .map_err(|e| TransportError::Write(e.to_string()))?;
        let mut frame = Vec::with_capacity(4 + header.len());
        frame.extend_from_slice(&(header.len() as u32).to_be_bytes());
        frame.extend_from_slice(&header);
        self.inner
            .write_all(&frame)
            .await
            .map_err(|e| TransportError::Write(e.to_string()))?;
        self.inner
            .write_chunk(chunk.data)
            .await
            .map_err(|e| TransportError::Write(e.to_string()))?;
        Ok(())
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        self.inner
            .finish()
            .map_err(|e| TransportError::Write(e.to_string()))
    }
}

pub struct RecvChunkStream {
    inner: iroh::endpoint::RecvStream,
    buf: BytesMut,
}

impl RecvChunkStream {
    #[must_use]
    pub fn new(inner: iroh::endpoint::RecvStream) -> Self {
        Self {
            inner,
            buf: BytesMut::new(),
        }
    }

    /// Fill the internal buffer until it holds at least `n` bytes.
    /// Returns Ok(None) on a clean end-of-stream at an empty boundary.
    async fn fill(&mut self, n: usize) -> Result<Option<()>, TransportError> {
        while self.buf.len() < n {
            let want = n - self.buf.len();
            let Some(chunk) = self
                .inner
                .read_chunk(want)
                .await
                .map_err(|e| TransportError::Read(e.to_string()))?
            else {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(TransportError::Closed);
            };
            self.buf.extend_from_slice(&chunk);
        }
        Ok(Some(()))
    }
}

impl ChunkSource for RecvChunkStream {
    async fn next_chunk(&mut self) -> Result<Option<ChunkPayload>, TransportError> {
        if self.fill(4).await?.is_none() {
            return Ok(None);
        }
        let tmp = self.buf.split_to(4);
        let hlen = u32::from_be_bytes([tmp[0], tmp[1], tmp[2], tmp[3]]) as usize;
        if hlen == 0 || hlen > MAX_HEADER_LEN {
            return Err(TransportError::Read(format!("bad header len {hlen}")));
        }
        if self.fill(4 + hlen).await?.is_none() {
            return Err(TransportError::Closed);
        }
        let header_bytes = self.buf.split_to(hlen);
        let header: ChunkHeader = postcard::from_bytes(&header_bytes)
            .map_err(|e| TransportError::Read(format!("header decode: {e}")))?;
        let dlen = header.len as usize;
        if dlen > MAX_CHUNK_LEN {
            return Err(TransportError::Read(format!("chunk too large: {dlen}")));
        }
        if self.fill(dlen).await?.is_none() {
            return Err(TransportError::Closed);
        }
        let data: Bytes = self.buf.split_to(dlen).freeze();
        Ok(Some(ChunkPayload { header, data }))
    }
}
