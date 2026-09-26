//! Frame codec for control channels.
//!
//! Wire format: `[u32 BE length][postcard payload]`. The decoder is
//! transport-agnostic: feed it bytes from any stream, pull complete frames.

use crate::limits::MAX_FRAME_BYTES;
use crate::ProtocolError;
use serde::Serialize;

/// Encode any serde message into a length-prefixed frame (control channel).
pub fn encode_raw<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtocolError> {
    let payload = postcard::to_allocvec(msg).map_err(|e| ProtocolError::Encode(e.to_string()))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Alias used by the public API.
pub fn encode_msg<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtocolError> {
    encode_raw(msg)
}

/// Incremental frame decoder. Feed bytes, drain frames.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// Hard cap on buffered bytes, guards against a lying length prefix.
    cap: usize,
}

impl FrameDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::with_cap(MAX_FRAME_BYTES)
    }

    #[must_use]
    pub fn with_cap(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    /// Append bytes received from the wire. Returns an error if the peer is
    /// buffering more than the frame cap (malicious length prefix).
    pub fn feed(&mut self, data: &[u8]) -> Result<(), ProtocolError> {
        if self.buf.len() + data.len() > self.cap + 4 {
            return Err(ProtocolError::FrameTooLarge(self.buf.len() + data.len()));
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }

    /// Pop the next complete frame payload, if any.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, ProtocolError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buf[..4].try_into().expect("len checked")) as usize;
        if len > self.cap {
            return Err(ProtocolError::FrameTooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let payload = self.buf[4..4 + len].to_vec();
        self.buf.drain(..4 + len);
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_feeding() {
        let frame = encode_raw(&vec![1u8, 2, 3]).unwrap();
        let mut dec = FrameDecoder::new();
        assert!(dec.next_frame().unwrap().is_none());
        dec.feed(&frame[..2]).unwrap();
        assert!(dec.next_frame().unwrap().is_none());
        dec.feed(&frame[2..]).unwrap();
        let payload = dec.next_frame().unwrap().unwrap();
        let v: Vec<u8> = postcard::from_bytes(&payload).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
        assert!(dec.next_frame().unwrap().is_none());
    }

    #[test]
    fn two_frames_in_one_feed() {
        let a = encode_raw(&7u64).unwrap();
        let b = encode_raw(&9u64).unwrap();
        let mut both = a;
        both.extend_from_slice(&b);
        let mut dec = FrameDecoder::new();
        dec.feed(&both).unwrap();
        let x: u64 = postcard::from_bytes(&dec.next_frame().unwrap().unwrap()).unwrap();
        let y: u64 = postcard::from_bytes(&dec.next_frame().unwrap().unwrap()).unwrap();
        assert_eq!((x, y), (7, 9));
    }

    #[test]
    fn lying_length_prefix_rejected() {
        let mut dec = FrameDecoder::new();
        dec.feed(&u32::MAX.to_be_bytes()).unwrap();
        dec.feed(&[0u8; 8]).unwrap();
        assert!(matches!(
            dec.next_frame(),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }
}
