//! ZMODEM data subpackets: payload + frame-end + CRC, ZDLE-escaped.
//!
//! After a `ZDATA`, `ZFILE`, or `ZSINIT` header, file/data bytes travel in
//! subpackets:
//!
//! ```text
//! [ payload bytes, ZDLE-escaped ]
//! [ ZDLE ][ end ]                       end in { ZCRCE ZCRCG ZCRCQ ZCRCW }
//! [ CRC bytes, ZDLE-escaped ]           CRC-16 big-endian, or CRC-32
//!                                       little-endian (matching the
//!                                       header format that opened the frame)
//! [ XON ]                               only after ZCRCW
//! ```
//!
//! The CRC covers the *unescaped* payload **plus the frame-end byte
//! itself**. The frame end tells the receiver what happens next:
//!
//! ```text
//! ZCRCG  more subpackets follow immediately (streaming)
//! ZCRCQ  more follow, but send me a ZACK first
//! ZCRCE  frame over, a header comes next
//! ZCRCW  frame over, send ZACK, a header comes next (used for ZFILE info)
//! ```
//!
//! Payloads are capped at [`MAX_PAYLOAD`] (1024, per the spec) on encode;
//! the decoder is liberal and accepts up to [`MAX_DECODE_PAYLOAD`] to
//! interoperate with 8 KiB "ZedZap" senders, and refuses to buffer beyond
//! that so hostile input cannot balloon memory.

use thiserror::Error;

use crate::crc::{crc16_xmodem, crc32};
use crate::zdle::{decode_one, Escaper, FrameEnd, WireItem, ZdleError};
use crate::XON;

/// Maximum payload length this encoder will produce (the spec's limit).
pub const MAX_PAYLOAD: usize = 1024;

/// Maximum payload length the decoder will accept (ZedZap-style 8 KiB).
pub const MAX_DECODE_PAYLOAD: usize = 8192;

/// Errors from subpacket coding. Never panics on any input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubpacketError {
    /// More bytes are needed to finish the subpacket.
    #[error("truncated subpacket: more bytes needed")]
    Incomplete,
    /// Encode was asked for a payload larger than [`MAX_PAYLOAD`].
    #[error("payload of {0} bytes exceeds the {MAX_PAYLOAD}-byte subpacket limit")]
    PayloadTooLarge(usize),
    /// The decoder saw more payload than [`MAX_DECODE_PAYLOAD`] with no
    /// frame end — corrupt or hostile input.
    #[error("subpacket exceeded {MAX_DECODE_PAYLOAD} bytes without a frame end")]
    Oversize,
    /// The subpacket CRC did not match.
    #[error("subpacket CRC mismatch: computed {computed:#010X}, received {received:#010X}")]
    BadCrc {
        /// CRC computed over payload + frame-end byte.
        computed: u32,
        /// CRC carried on the wire.
        received: u32,
    },
    /// The ZDLE layer rejected the bytes.
    #[error("ZDLE error in subpacket: {0}")]
    Zdle(ZdleError),
    /// A `CAN CAN` cancel sequence appeared inside the subpacket.
    #[error("cancel sequence inside subpacket")]
    Cancelled,
}

impl From<ZdleError> for SubpacketError {
    fn from(err: ZdleError) -> Self {
        match err {
            // Normalize: "ran out of bytes mid-escape" is just Incomplete.
            ZdleError::Incomplete => SubpacketError::Incomplete,
            other => SubpacketError::Zdle(other),
        }
    }
}

/// A successfully decoded subpacket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSubpacket {
    /// The unescaped payload bytes.
    pub payload: Vec<u8>,
    /// How the subpacket was terminated.
    pub end: FrameEnd,
    /// Total wire bytes consumed, including a trailing XON after ZCRCW
    /// when present.
    pub consumed: usize,
}

/// Encode one subpacket with a fresh ZDLE escaper.
///
/// `wide` selects the CRC: `false` = CRC-16 (frames opened by a `ZBIN`
/// header), `true` = CRC-32 (`ZBIN32`). A trailing XON is appended after
/// `ZCRCW`, as the classic implementations do.
pub fn encode_subpacket(
    payload: &[u8],
    end: FrameEnd,
    wide: bool,
) -> Result<Vec<u8>, SubpacketError> {
    encode_subpacket_with(payload, end, wide, &mut Escaper::new())
}

/// Encode one subpacket, threading an existing [`Escaper`] so the
/// `@`-then-CR rule holds across subpacket boundaries in a stream.
pub fn encode_subpacket_with(
    payload: &[u8],
    end: FrameEnd,
    wide: bool,
    escaper: &mut Escaper,
) -> Result<Vec<u8>, SubpacketError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(SubpacketError::PayloadTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(payload.len() + payload.len() / 8 + 12);
    escaper.push_slice(payload, &mut out);
    escaper.push_frame_end(end, &mut out);
    if wide {
        let crc = crc32_with_trailer(payload, end.to_byte());
        escaper.push_slice(&crc.to_le_bytes(), &mut out);
    } else {
        let crc = crc16_with_trailer(payload, end.to_byte());
        escaper.push_slice(&[(crc >> 8) as u8, crc as u8], &mut out);
    }
    if end == FrameEnd::Zcrcw {
        out.push(XON);
    }
    Ok(out)
}

fn crc16_with_trailer(payload: &[u8], trailer: u8) -> u16 {
    let mut buf = Vec::with_capacity(payload.len() + 1);
    buf.extend_from_slice(payload);
    buf.push(trailer);
    crc16_xmodem(&buf)
}

fn crc32_with_trailer(payload: &[u8], trailer: u8) -> u32 {
    let mut buf = Vec::with_capacity(payload.len() + 1);
    buf.extend_from_slice(payload);
    buf.push(trailer);
    crc32(&buf)
}

/// Decode one subpacket from the front of `input`.
///
/// `wide` must match the CRC width of the frame's opening header. Returns
/// [`SubpacketError::Incomplete`] when more bytes are needed. Never
/// panics.
pub fn decode_subpacket(input: &[u8], wide: bool) -> Result<DecodedSubpacket, SubpacketError> {
    let mut payload = Vec::new();
    let mut i = 0;
    let end = loop {
        let (item, used) = decode_one(&input[i..])?;
        i += used;
        match item {
            WireItem::Byte(b) => {
                if payload.len() >= MAX_DECODE_PAYLOAD {
                    return Err(SubpacketError::Oversize);
                }
                payload.push(b);
            }
            WireItem::FrameEnd(end) => break end,
            WireItem::Cancel => return Err(SubpacketError::Cancelled),
        }
    };
    let crc_len = if wide { 4 } else { 2 };
    let mut crc_bytes = [0u8; 4];
    for slot in crc_bytes.iter_mut().take(crc_len) {
        let (item, used) = decode_one(&input[i..])?;
        i += used;
        match item {
            WireItem::Byte(b) => *slot = b,
            WireItem::FrameEnd(_) => {
                return Err(SubpacketError::Zdle(ZdleError::UnexpectedFrameEnd))
            }
            WireItem::Cancel => return Err(SubpacketError::Cancelled),
        }
    }
    let (computed, received) = if wide {
        (
            crc32_with_trailer(&payload, end.to_byte()),
            u32::from_le_bytes(crc_bytes),
        )
    } else {
        let received = u32::from(crc_bytes[0]) << 8 | u32::from(crc_bytes[1]);
        (
            u32::from(crc16_with_trailer(&payload, end.to_byte())),
            received,
        )
    };
    if computed != received {
        return Err(SubpacketError::BadCrc { computed, received });
    }
    // Opportunistically consume the XON that follows ZCRCW.
    if end == FrameEnd::Zcrcw && input.get(i) == Some(&XON) {
        i += 1;
    }
    Ok(DecodedSubpacket {
        payload,
        end,
        consumed: i,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDS: [FrameEnd; 4] = [
        FrameEnd::Zcrce,
        FrameEnd::Zcrcg,
        FrameEnd::Zcrcq,
        FrameEnd::Zcrcw,
    ];

    #[test]
    fn round_trip_all_ends_both_widths() {
        let payload: Vec<u8> = (0..=255u8).collect();
        for wide in [false, true] {
            for end in ENDS {
                let wire = encode_subpacket(&payload, end, wide).expect("encode");
                let decoded = decode_subpacket(&wire, wide).expect("decode");
                assert_eq!(decoded.payload, payload);
                assert_eq!(decoded.end, end);
                assert_eq!(decoded.consumed, wire.len());
            }
        }
    }

    #[test]
    fn empty_payload_round_trips() {
        for wide in [false, true] {
            let wire = encode_subpacket(&[], FrameEnd::Zcrce, wide).expect("encode");
            let decoded = decode_subpacket(&wire, wide).expect("decode");
            assert!(decoded.payload.is_empty());
        }
    }

    #[test]
    fn zcrcw_gets_trailing_xon() {
        let wire = encode_subpacket(b"x", FrameEnd::Zcrcw, true).expect("encode");
        assert_eq!(wire.last(), Some(&XON));
        // ...and the decoder consumes it.
        let decoded = decode_subpacket(&wire, true).expect("decode");
        assert_eq!(decoded.consumed, wire.len());
    }

    #[test]
    fn adversarial_payloads_round_trip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0x18; 128],
            vec![
                b'@', 0x0D, 0x8D, 0x7F, 0xFF, 0x11, 0x13, 0x91, 0x93, 0x10, 0x90,
            ],
            vec![b'h', b'i', b'j', b'k', b'l', b'm'], // frame-end lookalikes
            vec![0x00; 1024],
        ];
        for case in cases {
            for wide in [false, true] {
                let wire = encode_subpacket(&case, FrameEnd::Zcrcg, wide).expect("encode");
                let decoded = decode_subpacket(&wire, wide).expect("decode");
                assert_eq!(decoded.payload, case);
            }
        }
    }

    #[test]
    fn oversized_encode_is_refused() {
        let big = vec![0u8; MAX_PAYLOAD + 1];
        assert_eq!(
            encode_subpacket(&big, FrameEnd::Zcrcg, true),
            Err(SubpacketError::PayloadTooLarge(MAX_PAYLOAD + 1))
        );
    }

    #[test]
    fn runaway_decode_is_bounded() {
        // A long stream of plain bytes with no frame end must be rejected,
        // not buffered forever.
        let junk = vec![b'a'; MAX_DECODE_PAYLOAD + 16];
        assert_eq!(
            decode_subpacket(&junk, false),
            Err(SubpacketError::Oversize)
        );
    }

    #[test]
    fn corrupted_crc_is_rejected() {
        for wide in [false, true] {
            let mut wire = encode_subpacket(b"data!", FrameEnd::Zcrce, wide).expect("encode");
            // Corrupt a payload byte that travels unescaped, so the ZDLE
            // layer stays valid and the CRC check must catch it.
            assert_eq!(wire[0], b'd');
            wire[0] = b'X';
            assert!(matches!(
                decode_subpacket(&wire, wide),
                Err(SubpacketError::BadCrc { .. })
            ));
        }
    }

    #[test]
    fn wrong_width_is_rejected() {
        let wire = encode_subpacket(b"payload", FrameEnd::Zcrce, true).expect("encode");
        // Reading a CRC-32 subpacket as CRC-16 must fail the checksum.
        assert!(matches!(
            decode_subpacket(&wire, false),
            Err(SubpacketError::BadCrc { .. })
        ));
    }

    #[test]
    fn truncation_yields_incomplete_at_every_prefix() {
        let wire = encode_subpacket(&[0x18, 0x11, b'Z'], FrameEnd::Zcrcq, true).expect("encode");
        for cut in 0..wire.len() {
            assert_eq!(
                decode_subpacket(&wire[..cut], true),
                Err(SubpacketError::Incomplete),
                "prefix of {cut} bytes"
            );
        }
    }

    #[test]
    fn cancel_inside_subpacket_is_surfaced() {
        let wire = [b'a', b'b', 0x18, 0x18];
        assert_eq!(
            decode_subpacket(&wire, false),
            Err(SubpacketError::Cancelled)
        );
    }

    #[test]
    fn streamed_subpackets_share_escaper_state() {
        // The @-then-CR rule depends on the last wire byte; threading one
        // escaper across subpackets must still round-trip cleanly.
        let mut escaper = Escaper::new();
        let one = encode_subpacket_with(b"end@", FrameEnd::Zcrcg, true, &mut escaper).unwrap();
        let two = encode_subpacket_with(b"\rnext", FrameEnd::Zcrce, true, &mut escaper).unwrap();
        let d1 = decode_subpacket(&one, true).unwrap();
        let d2 = decode_subpacket(&two, true).unwrap();
        assert_eq!(d1.payload, b"end@");
        assert_eq!(d2.payload, b"\rnext");
    }
}
