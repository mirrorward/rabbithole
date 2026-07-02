//! ZDLE escaping and unescaping of the raw ZMODEM byte stream.
//!
//! ZMODEM must pass binary data over links that eat control characters, so
//! sensitive bytes are escaped with `ZDLE` (0x18, the ASCII CAN character):
//!
//! ```text
//! plain byte b            ->  [ b ]
//! escaped byte b          ->  [ ZDLE ][ b ^ 0x40 ]
//! 0x7F (DEL)              ->  [ ZDLE ][ ZRUB0 = 'l' ]
//! 0xFF (DEL | 0x80)       ->  [ ZDLE ][ ZRUB1 = 'm' ]
//! frame end e             ->  [ ZDLE ][ e ]   e in { ZCRCE ZCRCG ZCRCQ ZCRCW }
//! ```
//!
//! Bytes that are **always** escaped: `ZDLE` itself (becoming `ZDLEE` =
//! 0x58), the flow-control set `0x10 0x11 0x13 0x90 0x91 0x93` (DLE, XON,
//! XOFF and their parity-set twins), and `0x7F`/`0xFF` via the ZRUB codes.
//! A carriage return (`0x0D` / `0x8D`) is escaped only when the previous
//! byte placed on the wire had low seven bits equal to `'@'` (0x40) — the
//! classic rule that keeps `@@CR` Telenet escape sequences from appearing
//! in the stream. (Escaping CR is only ever *extra* safety: the decoder
//! accepts it escaped or plain, so encoders may start from a fresh
//! [`Escaper`] at any frame boundary.)
//!
//! Decoding is the inverse: `ZDLE` followed by a byte whose bits satisfy
//! `b & 0x60 == 0x40` yields `b ^ 0x40`; `ZRUB0`/`ZRUB1` yield 0x7F/0xFF;
//! the four `ZCRC?` codes surface as [`WireItem::FrameEnd`]; `ZDLE ZDLE`
//! (two CANs) surfaces as [`WireItem::Cancel`] for the caller to count
//! (five consecutive CANs abort a session). Unescaped XON/XOFF noise
//! (`0x11 0x13 0x91 0x93`) is silently skipped, as the spec directs.

use thiserror::Error;

use crate::{XOFF, XON, ZDLE};

/// `ZCRCE` — frame end: end of frame, header follows (no ACK expected).
pub const ZCRCE: u8 = b'h';
/// `ZCRCG` — frame end: another subpacket follows, streaming (no ACK).
pub const ZCRCG: u8 = b'i';
/// `ZCRCQ` — frame end: another subpacket follows, ZACK expected.
pub const ZCRCQ: u8 = b'j';
/// `ZCRCW` — frame end: end of frame, ZACK expected, header follows.
pub const ZCRCW: u8 = b'k';
/// `ZRUB0` — escaped 0x7F (DEL).
pub const ZRUB0: u8 = b'l';
/// `ZRUB1` — escaped 0xFF.
pub const ZRUB1: u8 = b'm';

/// The four data-subpacket terminators, in the order they appear after
/// `ZDLE` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameEnd {
    /// End of frame; a header follows next. No response expected.
    Zcrce,
    /// Frame continues nonstop; another subpacket follows. No response.
    Zcrcg,
    /// Frame continues; a `ZACK` is expected before more data.
    Zcrcq,
    /// End of frame; a `ZACK` is expected, then a header.
    Zcrcw,
}

impl FrameEnd {
    /// The raw byte placed after `ZDLE` on the wire.
    pub fn to_byte(self) -> u8 {
        match self {
            FrameEnd::Zcrce => ZCRCE,
            FrameEnd::Zcrcg => ZCRCG,
            FrameEnd::Zcrcq => ZCRCQ,
            FrameEnd::Zcrcw => ZCRCW,
        }
    }

    /// Parse the byte that followed `ZDLE`, if it is a frame end.
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            ZCRCE => Some(FrameEnd::Zcrce),
            ZCRCG => Some(FrameEnd::Zcrcg),
            ZCRCQ => Some(FrameEnd::Zcrcq),
            ZCRCW => Some(FrameEnd::Zcrcw),
            _ => None,
        }
    }
}

/// Errors from the ZDLE layer. Never panics on any input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ZdleError {
    /// The input ended mid-sequence (e.g. a trailing lone `ZDLE`).
    /// Streaming callers should wait for more bytes.
    #[error("input ended in the middle of a ZDLE escape sequence")]
    Incomplete,
    /// `ZDLE` was followed by a byte that is not a valid escape code.
    #[error("invalid ZDLE escape byte 0x{0:02X}")]
    InvalidEscape(u8),
    /// A frame end appeared where only plain bytes are valid.
    #[error("unexpected frame end in escaped byte stream")]
    UnexpectedFrameEnd,
    /// `ZDLE ZDLE` (two CANs) appeared — the peer may be aborting.
    #[error("cancel (CAN CAN) sequence in escaped byte stream")]
    Cancelled,
}

/// One decoded item from the escaped wire stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireItem {
    /// A literal data byte (after unescaping).
    Byte(u8),
    /// `ZDLE` + one of `ZCRCE/G/Q/W` — a data-subpacket terminator.
    FrameEnd(FrameEnd),
    /// `ZDLE ZDLE` — the peer is striking CANs. Session-level abort is five
    /// consecutive CANs; the caller counts.
    Cancel,
}

/// Stateful ZDLE escaper.
///
/// Tracks the last byte placed on the wire so the `@`-then-CR rule can be
/// applied across calls. A fresh escaper (last byte 0) is always safe to
/// start at a frame boundary.
#[derive(Debug, Clone)]
pub struct Escaper {
    last_sent: u8,
}

impl Default for Escaper {
    fn default() -> Self {
        Self::new()
    }
}

impl Escaper {
    /// A fresh escaper with no wire history.
    pub fn new() -> Self {
        Escaper { last_sent: 0 }
    }

    /// Escape one byte onto `out`.
    pub fn push_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        let escaped = match byte {
            ZDLE => true,
            0x10 | 0x90 | XON | 0x91 | XOFF | 0x93 => true,
            0x0D | 0x8D => self.last_sent & 0x7F == b'@',
            0x7F => {
                out.push(ZDLE);
                out.push(ZRUB0);
                self.last_sent = ZRUB0;
                return;
            }
            0xFF => {
                out.push(ZDLE);
                out.push(ZRUB1);
                self.last_sent = ZRUB1;
                return;
            }
            _ => false,
        };
        if escaped {
            out.push(ZDLE);
            let wire = byte ^ 0x40;
            out.push(wire);
            self.last_sent = wire;
        } else {
            out.push(byte);
            self.last_sent = byte;
        }
    }

    /// Escape a whole slice onto `out`.
    pub fn push_slice(&mut self, data: &[u8], out: &mut Vec<u8>) {
        for &byte in data {
            self.push_byte(byte, out);
        }
    }

    /// Emit `ZDLE` + the frame-end code (frame ends are never escaped).
    pub fn push_frame_end(&mut self, end: FrameEnd, out: &mut Vec<u8>) {
        out.push(ZDLE);
        let byte = end.to_byte();
        out.push(byte);
        self.last_sent = byte;
    }
}

/// Escape `data` with a fresh [`Escaper`] and return the wire bytes.
pub fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 4);
    Escaper::new().push_slice(data, &mut out);
    out
}

/// Decode one wire item from the front of `input`.
///
/// Returns the item and the number of input bytes consumed (which includes
/// any skipped unescaped XON/XOFF noise). Never panics.
pub fn decode_one(input: &[u8]) -> Result<(WireItem, usize), ZdleError> {
    let mut i = 0;
    // Skip unescaped flow-control noise, per the spec.
    while let Some(&b) = input.get(i) {
        if matches!(b, XON | XOFF | 0x91 | 0x93) {
            i += 1;
        } else {
            break;
        }
    }
    match input.get(i) {
        None => Err(ZdleError::Incomplete),
        Some(&ZDLE) => match input.get(i + 1) {
            None => Err(ZdleError::Incomplete),
            Some(&ZDLE) => Ok((WireItem::Cancel, i + 2)),
            Some(&ZRUB0) => Ok((WireItem::Byte(0x7F), i + 2)),
            Some(&ZRUB1) => Ok((WireItem::Byte(0xFF), i + 2)),
            Some(&b) => {
                if let Some(end) = FrameEnd::from_byte(b) {
                    Ok((WireItem::FrameEnd(end), i + 2))
                } else if b & 0x60 == 0x40 {
                    Ok((WireItem::Byte(b ^ 0x40), i + 2))
                } else {
                    Err(ZdleError::InvalidEscape(b))
                }
            }
        },
        Some(&b) => Ok((WireItem::Byte(b), i + 1)),
    }
}

/// Unescape a complete escaped byte stream that contains only plain data
/// (no frame ends, no cancels). Useful for tests and simple callers;
/// framing-aware decoding lives in [`crate::header`] and
/// [`crate::subpacket`].
pub fn unescape(input: &[u8]) -> Result<Vec<u8>, ZdleError> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let (item, used) = decode_one(&input[i..])?;
        i += used;
        match item {
            WireItem::Byte(b) => out.push(b),
            WireItem::FrameEnd(_) => return Err(ZdleError::UnexpectedFrameEnd),
            WireItem::Cancel => return Err(ZdleError::Cancelled),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_zdle_and_flow_control() {
        assert_eq!(escape(&[ZDLE]), vec![ZDLE, 0x58]);
        assert_eq!(escape(&[0x10]), vec![ZDLE, 0x50]);
        assert_eq!(escape(&[XON]), vec![ZDLE, 0x51]);
        assert_eq!(escape(&[XOFF]), vec![ZDLE, 0x53]);
        assert_eq!(escape(&[0x90]), vec![ZDLE, 0xD0]);
        assert_eq!(escape(&[0x91]), vec![ZDLE, 0xD1]);
        assert_eq!(escape(&[0x93]), vec![ZDLE, 0xD3]);
    }

    #[test]
    fn escapes_rubouts() {
        assert_eq!(escape(&[0x7F]), vec![ZDLE, ZRUB0]);
        assert_eq!(escape(&[0xFF]), vec![ZDLE, ZRUB1]);
    }

    #[test]
    fn cr_after_at_rule() {
        // CR alone is not escaped...
        assert_eq!(escape(&[0x0D]), vec![0x0D]);
        // ...but after '@' (or any byte with low bits 0x40) it is.
        assert_eq!(escape(&[b'@', 0x0D]), vec![b'@', ZDLE, 0x4D]);
        assert_eq!(escape(&[b'@', 0x8D]), vec![b'@', ZDLE, 0xCD]);
        assert_eq!(escape(&[0xC0, 0x0D]), vec![0xC0, ZDLE, 0x4D]);
        // An escaped byte whose wire form is '@' (0x00 is never escaped by
        // this encoder, but a rub code is not '@') — plain 'A' resets it.
        assert_eq!(escape(&[b'A', 0x0D]), vec![b'A', 0x0D]);
    }

    #[test]
    fn plain_bytes_pass_through() {
        assert_eq!(escape(b"hello"), b"hello".to_vec());
        assert_eq!(escape(&[0x00, 0x01, 0x1F]), vec![0x00, 0x01, 0x1F]);
    }

    #[test]
    fn round_trip_all_bytes() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(unescape(&escape(&all)).unwrap(), all);
    }

    #[test]
    fn round_trip_adversarial() {
        let cases: Vec<Vec<u8>> = vec![
            vec![ZDLE; 32],
            vec![ZDLE, b'@', 0x0D, ZDLE, 0x8D, 0xFF, 0x7F],
            vec![b'@', 0x0D, b'@', 0x8D, 0xC0, 0x0D],
            vec![XON, XOFF, 0x91, 0x93, 0x10, 0x90],
            vec![ZCRCE, ZCRCG, ZCRCQ, ZCRCW, ZRUB0, ZRUB1],
            (0..=255u8).rev().collect(),
            vec![0x8D; 100],
        ];
        for case in cases {
            assert_eq!(unescape(&escape(&case)).unwrap(), case, "case {case:02X?}");
        }
    }

    #[test]
    fn round_trip_lcg_random() {
        // Deterministic pseudo-random buffers, no external crates.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        for len in [0usize, 1, 7, 64, 1024] {
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                buf.push((state >> 33) as u8);
            }
            assert_eq!(unescape(&escape(&buf)).unwrap(), buf);
        }
    }

    #[test]
    fn decode_frame_ends_and_cancel() {
        assert_eq!(
            decode_one(&[ZDLE, ZCRCW]).unwrap(),
            (WireItem::FrameEnd(FrameEnd::Zcrcw), 2)
        );
        assert_eq!(decode_one(&[ZDLE, ZDLE]).unwrap(), (WireItem::Cancel, 2));
    }

    #[test]
    fn decode_skips_flow_control_noise() {
        assert_eq!(
            decode_one(&[XON, XOFF, b'x']).unwrap(),
            (WireItem::Byte(b'x'), 3)
        );
    }

    #[test]
    fn decode_truncated_is_incomplete_not_panic() {
        assert_eq!(decode_one(&[]), Err(ZdleError::Incomplete));
        assert_eq!(decode_one(&[ZDLE]), Err(ZdleError::Incomplete));
        assert_eq!(decode_one(&[XON]), Err(ZdleError::Incomplete));
    }

    #[test]
    fn decode_invalid_escape() {
        // 0x20 ('space') after ZDLE: 0x20 & 0x60 == 0x20, not a valid escape.
        assert_eq!(
            decode_one(&[ZDLE, 0x20]),
            Err(ZdleError::InvalidEscape(0x20))
        );
    }

    #[test]
    fn unescape_rejects_embedded_frame_end_and_cancel() {
        assert_eq!(
            unescape(&[b'a', ZDLE, ZCRCE]),
            Err(ZdleError::UnexpectedFrameEnd)
        );
        assert_eq!(unescape(&[b'a', ZDLE, ZDLE]), Err(ZdleError::Cancelled));
    }
}
