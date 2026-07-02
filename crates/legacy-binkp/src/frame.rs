//! binkp block framing: the 2-byte big-endian block header.
//!
//! Every binkp exchange is a stream of *blocks*. Each block is prefixed by a
//! 16-bit big-endian header whose most-significant bit selects the block kind
//! and whose low 15 bits give the payload length:
//!
//! ```text
//!   byte 0            byte 1
//!   ┌─┬─────────────┐ ┌─────────────────┐
//!   │T│  len[14:8]   │ │     len[7:0]    │   header = (T<<15) | len
//!   └┬┴─────────────┘ └─────────────────┘
//!    │
//!    └─ T = 1 → command block    T = 0 → data block
//!
//!   command block body:  [ id ][ args … ]      (len = 1 + args.len())
//!   data    block body:  [ payload … ]         (len = payload.len())
//! ```
//!
//! Because the length is only 15 bits, a single block carries at most
//! [`BLOCK_MAX`] (32767) bytes. Decoding is total: truncated or garbage input
//! yields [`FrameError`] rather than a panic, and [`FrameError::Incomplete`]
//! is distinguished so a streaming caller can wait for more bytes.

use thiserror::Error;

/// Bit set in the block header to mark a *command* block.
pub const COMMAND_BIT: u16 = 0x8000;

/// Maximum payload of a single block: the 15-bit length field caps it here.
pub const BLOCK_MAX: usize = 0x7FFF;

/// Errors from block framing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    /// Not enough bytes yet to decode a whole block; try again with more.
    #[error("incomplete block: need more bytes")]
    Incomplete,
    /// A command block declared length 0, so it has no command id byte.
    #[error("command block is empty (no command id)")]
    EmptyCommand,
    /// A payload exceeds the 15-bit block length limit ([`BLOCK_MAX`]).
    #[error("block payload too large: {len} > {max}", max = BLOCK_MAX)]
    TooLarge {
        /// The offending payload length.
        len: usize,
    },
}

/// A decoded binkp block: either a command block or a raw data block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawBlock {
    /// A command block: a 1-byte command id followed by its raw argument bytes.
    Command {
        /// The command id byte (see [`crate::command::CommandId`]).
        id: u8,
        /// The raw (still-unparsed) argument bytes.
        args: Vec<u8>,
    },
    /// A data block carrying opaque file bytes.
    Data(Vec<u8>),
}

impl RawBlock {
    /// Serialize this block, header included.
    ///
    /// Fails with [`FrameError::TooLarge`] if the body would not fit in the
    /// 15-bit length field.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        match self {
            RawBlock::Command { id, args } => {
                let len = args.len() + 1;
                if len > BLOCK_MAX {
                    return Err(FrameError::TooLarge { len });
                }
                let header = COMMAND_BIT | (len as u16);
                let mut out = Vec::with_capacity(2 + len);
                out.extend_from_slice(&header.to_be_bytes());
                out.push(*id);
                out.extend_from_slice(args);
                Ok(out)
            }
            RawBlock::Data(payload) => {
                let len = payload.len();
                if len > BLOCK_MAX {
                    return Err(FrameError::TooLarge { len });
                }
                // Top bit already clear because len <= 0x7FFF.
                let header = len as u16;
                let mut out = Vec::with_capacity(2 + len);
                out.extend_from_slice(&header.to_be_bytes());
                out.extend_from_slice(payload);
                Ok(out)
            }
        }
    }
}

/// Decode a single block from the front of `input`.
///
/// On success returns the block and the number of bytes consumed (header +
/// body), so a streaming caller can advance its buffer. Returns
/// [`FrameError::Incomplete`] when `input` does not yet hold the whole block.
pub fn decode_block(input: &[u8]) -> Result<(RawBlock, usize), FrameError> {
    if input.len() < 2 {
        return Err(FrameError::Incomplete);
    }
    let header = u16::from_be_bytes([input[0], input[1]]);
    let is_command = header & COMMAND_BIT != 0;
    let len = (header & !COMMAND_BIT) as usize;
    let total = 2 + len;
    if input.len() < total {
        return Err(FrameError::Incomplete);
    }
    let body = &input[2..total];
    let block = if is_command {
        let (id, args) = body.split_first().ok_or(FrameError::EmptyCommand)?;
        RawBlock::Command {
            id: *id,
            args: args.to_vec(),
        }
    } else {
        RawBlock::Data(body.to_vec())
    };
    Ok((block, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_block_round_trips() {
        let block = RawBlock::Data(b"hello".to_vec());
        let bytes = block.encode().unwrap();
        // header 0x0005, top bit clear
        assert_eq!(&bytes[..2], &[0x00, 0x05]);
        let (decoded, used) = decode_block(&bytes).unwrap();
        assert_eq!(decoded, block);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn command_block_round_trips() {
        let block = RawBlock::Command {
            id: 3,
            args: b"file.zip 10 0 0".to_vec(),
        };
        let bytes = block.encode().unwrap();
        // len = args + 1 = 16, top bit set
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), COMMAND_BIT | 16);
        let (decoded, used) = decode_block(&bytes).unwrap();
        assert_eq!(decoded, block);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn empty_command_body_is_rejected() {
        // header says command, length 0 → no id byte.
        let bytes = [0x80, 0x00];
        assert_eq!(decode_block(&bytes), Err(FrameError::EmptyCommand));
    }

    #[test]
    fn truncated_header_and_body_are_incomplete() {
        assert_eq!(decode_block(&[]), Err(FrameError::Incomplete));
        assert_eq!(decode_block(&[0x80]), Err(FrameError::Incomplete));
        // header claims 5 bytes but only 3 present
        assert_eq!(
            decode_block(&[0x00, 0x05, 0x01, 0x02, 0x03]),
            Err(FrameError::Incomplete)
        );
    }

    #[test]
    fn decode_leaves_trailing_bytes_for_next_block() {
        let first = RawBlock::Data(b"ab".to_vec()).encode().unwrap();
        let second = RawBlock::Command {
            id: 5,
            args: vec![],
        }
        .encode()
        .unwrap();
        let mut stream = first.clone();
        stream.extend_from_slice(&second);
        let (b1, used) = decode_block(&stream).unwrap();
        assert_eq!(b1, RawBlock::Data(b"ab".to_vec()));
        let (b2, _) = decode_block(&stream[used..]).unwrap();
        assert_eq!(
            b2,
            RawBlock::Command {
                id: 5,
                args: vec![]
            }
        );
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let block = RawBlock::Data(vec![0u8; BLOCK_MAX + 1]);
        assert_eq!(
            block.encode(),
            Err(FrameError::TooLarge { len: BLOCK_MAX + 1 })
        );
    }

    #[test]
    fn max_length_block_is_accepted() {
        let block = RawBlock::Data(vec![7u8; BLOCK_MAX]);
        let bytes = block.encode().unwrap();
        let (decoded, used) = decode_block(&bytes).unwrap();
        assert_eq!(decoded, block);
        assert_eq!(used, bytes.len());
    }
}
