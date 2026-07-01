//! Length-delimited frame codec.
//!
//! On stream transports (QUIC streams, TCP/TLS) frames are delimited by a
//! 4-byte big-endian length prefix followed by the postcard-encoded
//! [`Frame`]. On message transports (WebSocket binary messages) the prefix
//! is omitted — one message = one encoded frame — so [`encode_frame`] /
//! [`decode_frame`] are exposed separately from the streaming [`FrameCodec`].

use bytes::{Buf, BufMut, BytesMut};

use crate::error::ProtoError;
use crate::frame::Frame;

/// Hard ceiling for a control-stream frame. Bulk data never rides the
/// control stream (it gets a transfer ticket and its own stream), so this
/// only needs to fit chunky metadata like directory listings.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

const LEN_PREFIX: usize = 4;

/// Encode a frame to bytes (no length prefix).
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, ProtoError> {
    let bytes = postcard::to_allocvec(frame)?;
    if bytes.len() > MAX_FRAME_SIZE {
        return Err(ProtoError::FrameTooLarge {
            size: bytes.len(),
            max: MAX_FRAME_SIZE,
        });
    }
    Ok(bytes)
}

/// Decode a frame from bytes (no length prefix).
pub fn decode_frame(bytes: &[u8]) -> Result<Frame, ProtoError> {
    if bytes.len() > MAX_FRAME_SIZE {
        return Err(ProtoError::FrameTooLarge {
            size: bytes.len(),
            max: MAX_FRAME_SIZE,
        });
    }
    Ok(postcard::from_bytes(bytes)?)
}

/// Streaming codec: 4-byte big-endian length prefix + frame bytes.
///
/// Transport-agnostic and sans-io: `net` adapts it to `tokio_util::codec`
/// where needed; the wasm client drives it directly.
#[derive(Debug, Default)]
pub struct FrameCodec {
    buf: BytesMut,
}

impl FrameCodec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append incoming bytes to the internal buffer.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Try to decode the next complete frame from the buffer.
    ///
    /// Returns `Ok(None)` when more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtoError> {
        if self.buf.len() < LEN_PREFIX {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME_SIZE {
            return Err(ProtoError::FrameTooLarge {
                size: len,
                max: MAX_FRAME_SIZE,
            });
        }
        if self.buf.len() < LEN_PREFIX + len {
            return Ok(None);
        }
        self.buf.advance(LEN_PREFIX);
        let frame_bytes = self.buf.split_to(len);
        Ok(Some(decode_frame(&frame_bytes)?))
    }

    /// Encode a frame with its length prefix, ready to write to a stream.
    pub fn encode(frame: &Frame) -> Result<Vec<u8>, ProtoError> {
        let body = encode_frame(frame)?;
        let mut out = Vec::with_capacity(LEN_PREFIX + body.len());
        out.put_u32(body.len() as u32);
        out.extend_from_slice(&body);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Family, Frame, FrameKind, Payload, RequestId};
    use crate::version::PROTOCOL_VERSION;

    fn test_frame(n: u16) -> Frame {
        Frame {
            version: PROTOCOL_VERSION,
            kind: FrameKind::Request,
            family: Family::SESSION,
            message_type: n,
            id: RequestId(u64::from(n)),
            error: None,
            payload: Payload(vec![1, 2, 3, n as u8]),
        }
    }

    #[test]
    fn roundtrip_single() {
        let frame = test_frame(1);
        let encoded = FrameCodec::encode(&frame).unwrap();
        let mut codec = FrameCodec::new();
        codec.feed(&encoded);
        assert_eq!(codec.next_frame().unwrap().unwrap(), frame);
        assert!(codec.next_frame().unwrap().is_none());
    }

    #[test]
    fn roundtrip_pipelined_and_fragmented() {
        let frames: Vec<Frame> = (0..5).map(test_frame).collect();
        let mut wire = Vec::new();
        for f in &frames {
            wire.extend(FrameCodec::encode(f).unwrap());
        }
        // Feed one byte at a time — decoder must handle arbitrary fragmentation.
        let mut codec = FrameCodec::new();
        let mut decoded = Vec::new();
        for byte in wire {
            codec.feed(&[byte]);
            while let Some(f) = codec.next_frame().unwrap() {
                decoded.push(f);
            }
        }
        assert_eq!(decoded, frames);
    }

    #[test]
    fn rejects_oversized_length_prefix() {
        let mut codec = FrameCodec::new();
        codec.feed(&(MAX_FRAME_SIZE as u32 + 1).to_be_bytes());
        codec.feed(&[0u8; 8]);
        assert!(matches!(
            codec.next_frame(),
            Err(ProtoError::FrameTooLarge { .. })
        ));
    }
}
