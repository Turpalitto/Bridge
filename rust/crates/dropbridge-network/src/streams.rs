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
        if self.fill(hlen).await?.is_none() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{build_endpoint, RelayConfig};
    use bytes::Bytes;
    use dropbridge_identity::DeviceIdentity;
    use dropbridge_protocol::ALPN_TRANSFER;
    use dropbridge_transfer::{ChunkHeader, ChunkPayload, ChunkSink, ChunkSource};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_chunk_stream_roundtrip_and_exact_framing() {
        let id_a = DeviceIdentity::generate();
        let id_b = DeviceIdentity::generate();
        let ep_a = build_endpoint(
            &id_a,
            &RelayConfig::Disabled,
            vec![ALPN_TRANSFER.to_vec()],
            Some(47001),
        )
        .await
        .unwrap();
        let ep_b = build_endpoint(
            &id_b,
            &RelayConfig::Disabled,
            vec![ALPN_TRANSFER.to_vec()],
            Some(47002),
        )
        .await
        .unwrap();

        let hints_b = crate::hints::AddrHints::from_endpoint(&ep_b);
        let addr_b = hints_b.to_endpoint_addr(&id_b.device_id()).unwrap();

        let (conn_a_res, conn_b_res) = tokio::join!(ep_a.connect(addr_b, ALPN_TRANSFER), async {
            let incoming = ep_b.accept().await.unwrap();
            incoming.await.unwrap()
        });
        let conn_a = conn_a_res.unwrap();
        let conn_b = conn_b_res;

        let (send_a, _recv_a) = conn_a.open_bi().await.unwrap();

        let send_handle = tokio::spawn(async move {
            let mut sink = SendChunkStream::new(send_a);
            // Send a 0-length payload chunk
            sink.send_chunk(ChunkPayload {
                header: ChunkHeader::raw(1, 0, 0, 0, 0),
                data: Bytes::new(),
            })
            .await
            .unwrap();

            // Send a chunk with payload
            sink.send_chunk(ChunkPayload {
                header: ChunkHeader::raw(1, 0, 0, 0, 5),
                data: Bytes::from_static(b"hello"),
            })
            .await
            .unwrap();

            sink.finish().await.unwrap();
        });

        let (_send_b, recv_b) = conn_b.accept_bi().await.unwrap();
        let mut source = RecvChunkStream::new(recv_b);

        let c1 = source.next_chunk().await.unwrap().expect("first chunk");
        assert_eq!(c1.header.len, 0);
        assert_eq!(c1.data.len(), 0);

        let c2 = source.next_chunk().await.unwrap().expect("second chunk");
        assert_eq!(c2.header.len, 5);
        assert_eq!(&c2.data[..], b"hello");

        let c3 = source.next_chunk().await.unwrap();
        assert!(c3.is_none());

        send_handle.await.unwrap();
    }
}
