//! ZMODEM header codec: frame types and the three header encodings.
//!
//! Every ZMODEM frame begins with a header carrying a frame type and four
//! bytes of flags or a 32-bit file position:
//!
//! ```text
//! hex header      [ * ][ * ][ ZDLE ][ 'B' ]
//!                 [ 14 lowercase hex digits: type p0 p1 p2 p3 crc16 ]
//!                 [ CR ][ LF ][ XON ]        (XON omitted for ZACK/ZFIN)
//!
//! binary 16       [ * ][ ZDLE ][ 'A' ]
//!                 [ type p0 p1 p2 p3 crc16 ]  each byte ZDLE-escaped
//!
//! binary 32       [ * ][ ZDLE ][ 'C' ]
//!                 [ type p0 p1 p2 p3 crc32 ]  each byte ZDLE-escaped
//! ```
//!
//! The CRC covers the five bytes `type p0 p1 p2 p3`. CRC-16 travels
//! big-endian, CRC-32 little-endian. Position values are little-endian
//! across `p0..p3` (`ZP0` is the least significant byte); flag bytes are
//! indexed from the other end (`ZF0` = `p3`, `ZF1` = `p2`, …), exactly as
//! in Forsberg's `zmodem.h`.
//!
//! Decoding accepts one *or more* leading `ZPAD`s for either format,
//! upper- or lowercase hex digits, and optional CR/LF/XON trailer bytes
//! (consumed opportunistically when present). Truncated input yields
//! [`HeaderError::Incomplete`] so a streaming caller can wait for more.

use thiserror::Error;

use crate::crc::{crc16_xmodem, crc32};
use crate::zdle::{decode_one, Escaper, WireItem, ZdleError};
use crate::{XON, ZBIN, ZBIN32, ZDLE, ZHEX, ZPAD};

/// `ZRINIT` ZF0 flag: receiver can handle full-duplex links.
pub const CANFDX: u8 = 0x01;
/// `ZRINIT` ZF0 flag: receiver can overlap I/O (streaming, no ACK-per-block).
pub const CANOVIO: u8 = 0x02;
/// `ZRINIT` ZF0 flag: receiver can check 32-bit CRC frames.
pub const CANFC32: u8 = 0x20;

/// The ZMODEM frame types (values 0–17 from the 1988 spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// Request receive init (sent by the sender to start a session).
    Zrqinit = 0,
    /// Receive init (receiver capabilities + buffer size).
    Zrinit = 1,
    /// Send init (sender options + attention string follows).
    Zsinit = 2,
    /// Acknowledgement, carrying a file position.
    Zack = 3,
    /// File name and info follow in a data subpacket.
    Zfile = 4,
    /// Receiver: skip this file.
    Zskip = 5,
    /// Last header/subpacket was garbled — negative acknowledgement.
    Znak = 6,
    /// Abort the batch.
    Zabort = 7,
    /// Finish the session.
    Zfin = 8,
    /// Resume transfer at this file position.
    Zrpos = 9,
    /// Data subpackets follow, starting at this file position.
    Zdata = 10,
    /// End of file at this position.
    Zeof = 11,
    /// Fatal read/write error at this position.
    Zferr = 12,
    /// Request (or answer with) the CRC of a file region.
    Zcrc = 13,
    /// Challenge the peer to echo this number (spoof detection).
    Zchallenge = 14,
    /// Request is complete.
    Zcompl = 15,
    /// Pseudo frame: peer struck five CANs.
    Zcan = 16,
    /// Request the receiver's free disk space.
    Zfreecnt = 17,
}

impl FrameType {
    /// All eighteen frame types, for exhaustive round-trip tests.
    pub const ALL: [FrameType; 18] = [
        FrameType::Zrqinit,
        FrameType::Zrinit,
        FrameType::Zsinit,
        FrameType::Zack,
        FrameType::Zfile,
        FrameType::Zskip,
        FrameType::Znak,
        FrameType::Zabort,
        FrameType::Zfin,
        FrameType::Zrpos,
        FrameType::Zdata,
        FrameType::Zeof,
        FrameType::Zferr,
        FrameType::Zcrc,
        FrameType::Zchallenge,
        FrameType::Zcompl,
        FrameType::Zcan,
        FrameType::Zfreecnt,
    ];

    /// Parse a wire byte into a frame type.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }
}

/// How a header travels on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFormat {
    /// `**<ZDLE>B` — printable hex, 16-bit CRC. Used for control frames.
    Hex,
    /// `*<ZDLE>A` — binary, 16-bit CRC.
    Bin16,
    /// `*<ZDLE>C` — binary, 32-bit CRC.
    Bin32,
}

/// A decoded/encodable ZMODEM header: a frame type plus four bytes that are
/// either flags (`ZF0..ZF3`) or a little-endian file position (`ZP0..ZP3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The frame type.
    pub frame_type: FrameType,
    /// The raw `p0 p1 p2 p3` bytes in transmit order.
    pub data: [u8; 4],
}

impl Header {
    /// A header with all four data bytes zero.
    pub fn new(frame_type: FrameType) -> Self {
        Header {
            frame_type,
            data: [0; 4],
        }
    }

    /// A header carrying a little-endian file position in `p0..p3`.
    pub fn with_pos(frame_type: FrameType, pos: u32) -> Self {
        Header {
            frame_type,
            data: pos.to_le_bytes(),
        }
    }

    /// A header carrying flag bytes. Note the reversed indexing: `ZF0` is
    /// transmitted last (`p3`), `ZF3` first (`p0`), per the spec.
    pub fn with_flags(frame_type: FrameType, zf3: u8, zf2: u8, zf1: u8, zf0: u8) -> Self {
        Header {
            frame_type,
            data: [zf3, zf2, zf1, zf0],
        }
    }

    /// The file position (little-endian across `p0..p3`).
    pub fn pos(&self) -> u32 {
        u32::from_le_bytes(self.data)
    }

    /// Flag byte `ZF0` (transmitted last).
    pub fn zf0(&self) -> u8 {
        self.data[3]
    }

    /// Flag byte `ZF1`.
    pub fn zf1(&self) -> u8 {
        self.data[2]
    }

    /// Flag byte `ZF2`.
    pub fn zf2(&self) -> u8 {
        self.data[1]
    }

    /// Flag byte `ZF3` (transmitted first).
    pub fn zf3(&self) -> u8 {
        self.data[0]
    }

    /// Encode in the given wire format.
    pub fn encode(&self, format: HeaderFormat) -> Vec<u8> {
        match format {
            HeaderFormat::Hex => self.encode_hex(),
            HeaderFormat::Bin16 => self.encode_bin16(),
            HeaderFormat::Bin32 => self.encode_bin32(),
        }
    }

    /// Encode as a hex header, including the CR LF (XON) trailer. Per the
    /// classic implementations, XON is omitted after `ZACK` and `ZFIN`.
    pub fn encode_hex(&self) -> Vec<u8> {
        let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        let body = self.body_bytes();
        let crc = crc16_xmodem(&body);
        for byte in body.iter().copied().chain([(crc >> 8) as u8, crc as u8]) {
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0F));
        }
        out.push(b'\r');
        out.push(b'\n');
        if !matches!(self.frame_type, FrameType::Zack | FrameType::Zfin) {
            out.push(XON);
        }
        out
    }

    /// Encode as a binary header with 16-bit CRC (ZDLE-escaped).
    pub fn encode_bin16(&self) -> Vec<u8> {
        let mut out = vec![ZPAD, ZDLE, ZBIN];
        let body = self.body_bytes();
        let crc = crc16_xmodem(&body);
        let mut escaper = Escaper::new();
        escaper.push_slice(&body, &mut out);
        escaper.push_slice(&[(crc >> 8) as u8, crc as u8], &mut out);
        out
    }

    /// Encode as a binary header with 32-bit CRC (ZDLE-escaped).
    pub fn encode_bin32(&self) -> Vec<u8> {
        let mut out = vec![ZPAD, ZDLE, ZBIN32];
        let body = self.body_bytes();
        let crc = crc32(&body);
        let mut escaper = Escaper::new();
        escaper.push_slice(&body, &mut out);
        escaper.push_slice(&crc.to_le_bytes(), &mut out);
        out
    }

    fn body_bytes(&self) -> [u8; 5] {
        [
            self.frame_type as u8,
            self.data[0],
            self.data[1],
            self.data[2],
            self.data[3],
        ]
    }
}

/// A successfully decoded header plus bookkeeping for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedHeader {
    /// The header itself.
    pub header: Header,
    /// The wire format it arrived in (tells the receiver whether the peer
    /// is using 32-bit CRCs for the data that follows).
    pub format: HeaderFormat,
    /// Total bytes consumed from the input, including pads and any hex
    /// trailer bytes present.
    pub consumed: usize,
}

/// Errors from header decoding. Never panics on any input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    /// More bytes are needed to finish (or classify) the header.
    #[error("truncated header: more bytes needed")]
    Incomplete,
    /// The input does not start with a `ZPAD` (`*`).
    #[error("expected ZPAD '*' at start of header, found 0x{0:02X}")]
    MissingPad(u8),
    /// `ZPAD`s were not followed by `ZDLE`.
    #[error("expected ZDLE after header padding, found 0x{0:02X}")]
    MissingZdle(u8),
    /// The format byte after `ZDLE` was not `A`, `B`, or `C`.
    #[error("unknown header format byte 0x{0:02X}")]
    UnknownFormat(u8),
    /// A hex header contained a non-hex digit.
    #[error("invalid hex digit 0x{0:02X} in hex header")]
    InvalidHexDigit(u8),
    /// The header CRC did not match.
    #[error("header CRC mismatch: computed {computed:#06X}, received {received:#06X}")]
    BadCrc {
        /// CRC computed over the received header body.
        computed: u32,
        /// CRC carried on the wire.
        received: u32,
    },
    /// The frame-type byte is not one of the eighteen known types.
    #[error("unknown frame type {0}")]
    UnknownFrameType(u8),
    /// The ZDLE layer rejected the binary header bytes.
    #[error("ZDLE error in binary header: {0}")]
    Zdle(ZdleError),
    /// A frame end appeared inside a binary header.
    #[error("unexpected frame end inside binary header")]
    UnexpectedFrameEnd,
    /// A `CAN CAN` cancel sequence appeared inside the header.
    #[error("cancel sequence inside header")]
    Cancelled,
}

impl From<ZdleError> for HeaderError {
    fn from(err: ZdleError) -> Self {
        match err {
            // Normalize: "ran out of bytes mid-escape" is just Incomplete.
            ZdleError::Incomplete => HeaderError::Incomplete,
            other => HeaderError::Zdle(other),
        }
    }
}

/// Decode one header from the front of `input`.
///
/// `input` must begin at the header's first `ZPAD`; any number of pads is
/// accepted. Returns [`HeaderError::Incomplete`] when more bytes are
/// needed. Never panics.
pub fn decode_header(input: &[u8]) -> Result<DecodedHeader, HeaderError> {
    let mut i = 0;
    match input.first() {
        None => return Err(HeaderError::Incomplete),
        Some(&ZPAD) => {}
        Some(&b) => return Err(HeaderError::MissingPad(b)),
    }
    while input.get(i) == Some(&ZPAD) {
        i += 1;
    }
    match input.get(i) {
        None => return Err(HeaderError::Incomplete),
        Some(&ZDLE) => i += 1,
        Some(&b) => return Err(HeaderError::MissingZdle(b)),
    }
    let format = match input.get(i) {
        None => return Err(HeaderError::Incomplete),
        Some(&ZHEX) => HeaderFormat::Hex,
        Some(&ZBIN) => HeaderFormat::Bin16,
        Some(&ZBIN32) => HeaderFormat::Bin32,
        Some(&b) => return Err(HeaderError::UnknownFormat(b)),
    };
    i += 1;
    match format {
        HeaderFormat::Hex => decode_hex_body(input, i),
        HeaderFormat::Bin16 => decode_bin_body(input, i, false),
        HeaderFormat::Bin32 => decode_bin_body(input, i, true),
    }
}

fn decode_hex_body(input: &[u8], mut i: usize) -> Result<DecodedHeader, HeaderError> {
    // 14 hex digits: type(2) p0..p3(8) crc16(4).
    let mut raw = [0u8; 7];
    for slot in raw.iter_mut() {
        let hi = hex_value(*input.get(i).ok_or(HeaderError::Incomplete)?)?;
        let lo = hex_value(*input.get(i + 1).ok_or(HeaderError::Incomplete)?)?;
        *slot = (hi << 4) | lo;
        i += 2;
    }
    let body = &raw[..5];
    let received = u16::from(raw[5]) << 8 | u16::from(raw[6]);
    let computed = crc16_xmodem(body);
    if computed != received {
        return Err(HeaderError::BadCrc {
            computed: u32::from(computed),
            received: u32::from(received),
        });
    }
    // Opportunistically consume the CR LF (XON) trailer, tolerating the
    // parity-set variants (0x8D, 0x8A) some senders emit.
    for expected in [&[0x0D, 0x8D][..], &[0x0A, 0x8A][..], &[XON][..]] {
        if let Some(&b) = input.get(i) {
            if expected.contains(&b) {
                i += 1;
            }
        }
    }
    finish_header(
        raw[0],
        [raw[1], raw[2], raw[3], raw[4]],
        HeaderFormat::Hex,
        i,
    )
}

fn decode_bin_body(input: &[u8], mut i: usize, wide: bool) -> Result<DecodedHeader, HeaderError> {
    let crc_len = if wide { 4 } else { 2 };
    let mut raw = [0u8; 9];
    for slot in raw.iter_mut().take(5 + crc_len) {
        let (item, used) = decode_one(&input[i..])?;
        i += used;
        match item {
            WireItem::Byte(b) => *slot = b,
            WireItem::FrameEnd(_) => return Err(HeaderError::UnexpectedFrameEnd),
            WireItem::Cancel => return Err(HeaderError::Cancelled),
        }
    }
    let body = &raw[..5];
    let (computed, received) = if wide {
        let received = u32::from_le_bytes([raw[5], raw[6], raw[7], raw[8]]);
        (crc32(body), received)
    } else {
        let received = u32::from(raw[5]) << 8 | u32::from(raw[6]);
        (u32::from(crc16_xmodem(body)), received)
    };
    if computed != received {
        return Err(HeaderError::BadCrc { computed, received });
    }
    let format = if wide {
        HeaderFormat::Bin32
    } else {
        HeaderFormat::Bin16
    };
    finish_header(raw[0], [raw[1], raw[2], raw[3], raw[4]], format, i)
}

fn finish_header(
    type_byte: u8,
    data: [u8; 4],
    format: HeaderFormat,
    consumed: usize,
) -> Result<DecodedHeader, HeaderError> {
    let frame_type =
        FrameType::from_byte(type_byte).ok_or(HeaderError::UnknownFrameType(type_byte))?;
    Ok(DecodedHeader {
        header: Header { frame_type, data },
        format,
        consumed,
    })
}

fn hex_digit(nibble: u8) -> u8 {
    b"0123456789abcdef"[usize::from(nibble & 0x0F)]
}

fn hex_value(byte: u8) -> Result<u8, HeaderError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HeaderError::InvalidHexDigit(byte)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zrqinit_hex_is_the_canonical_string() {
        // The famous "rz\r**\x18B00..." opener, minus the "rz\r" part.
        let wire = Header::new(FrameType::Zrqinit).encode_hex();
        assert_eq!(wire, b"**\x18B00000000000000\r\n\x11".to_vec());
    }

    #[test]
    fn zfin_and_zack_hex_omit_xon() {
        let fin = Header::new(FrameType::Zfin).encode_hex();
        assert_eq!(fin.last(), Some(&b'\n'));
        let ack = Header::with_pos(FrameType::Zack, 42).encode_hex();
        assert_eq!(ack.last(), Some(&b'\n'));
        let rinit = Header::new(FrameType::Zrinit).encode_hex();
        assert_eq!(rinit.last(), Some(&XON));
    }

    #[test]
    fn hex_round_trip_all_types() {
        for frame_type in FrameType::ALL {
            let header = Header::with_pos(frame_type, 0xDEAD_BEEF);
            let wire = header.encode_hex();
            let decoded = decode_header(&wire).expect("decode");
            assert_eq!(decoded.header, header);
            assert_eq!(decoded.format, HeaderFormat::Hex);
            assert_eq!(decoded.consumed, wire.len());
        }
    }

    #[test]
    fn hex_decode_accepts_uppercase_digits() {
        let mut wire = Header::with_pos(FrameType::Zrpos, 0xABCDEF01).encode_hex();
        wire.iter_mut().for_each(|b| *b = b.to_ascii_uppercase());
        // Uppercasing also hits 'b'->'B' (the ZHEX byte is already 'B').
        let decoded = decode_header(&wire).expect("decode uppercase");
        assert_eq!(decoded.header.pos(), 0xABCDEF01);
    }

    #[test]
    fn bin16_round_trip_all_types() {
        for frame_type in FrameType::ALL {
            // Adversarial data bytes that force ZDLE escaping.
            for data in [[0u8; 4], [0x18, 0x11, 0x7F, 0xFF], [0x40, 0x0D, 0x8D, 0x93]] {
                let header = Header { frame_type, data };
                let wire = header.encode_bin16();
                let decoded = decode_header(&wire).expect("decode");
                assert_eq!(decoded.header, header);
                assert_eq!(decoded.format, HeaderFormat::Bin16);
                assert_eq!(decoded.consumed, wire.len());
            }
        }
    }

    #[test]
    fn bin32_round_trip_all_types() {
        for frame_type in FrameType::ALL {
            let header = Header::with_pos(frame_type, u32::MAX);
            let wire = header.encode_bin32();
            let decoded = decode_header(&wire).expect("decode");
            assert_eq!(decoded.header, header);
            assert_eq!(decoded.format, HeaderFormat::Bin32);
            assert_eq!(decoded.consumed, wire.len());
        }
    }

    #[test]
    fn extra_pads_are_tolerated() {
        let mut wire = vec![ZPAD; 5];
        wire.extend(
            Header::new(FrameType::Zrinit)
                .encode_bin16()
                .into_iter()
                .skip(1),
        );
        let decoded = decode_header(&wire).expect("decode");
        assert_eq!(decoded.header.frame_type, FrameType::Zrinit);
    }

    #[test]
    fn corrupted_crc_is_rejected() {
        let mut wire = Header::with_pos(FrameType::Zdata, 1234).encode_bin32();
        // Corrupt a data byte that travels unescaped (p0 = 0xD2), so the
        // ZDLE layer stays valid and the CRC check must catch it.
        assert_eq!(wire[4], 0xD2);
        wire[4] = b'x';
        assert!(matches!(
            decode_header(&wire),
            Err(HeaderError::BadCrc { .. })
        ));

        let mut hexwire = Header::new(FrameType::Zrinit).encode_hex();
        assert_eq!(hexwire[6], b'0');
        hexwire[6] = b'1'; // flip a data nibble (p0 high nibble)
        assert!(matches!(
            decode_header(&hexwire),
            Err(HeaderError::BadCrc { .. })
        ));
    }

    #[test]
    fn truncation_yields_incomplete_at_every_prefix() {
        for header in [
            Header::new(FrameType::Zrqinit).encode_hex(),
            Header::with_pos(FrameType::Zrpos, 0x18181818).encode_bin16(),
            Header::with_pos(FrameType::Zeof, 0xFFFFFFFF).encode_bin32(),
        ] {
            // Hex trailer bytes are optional, so stop before them for hex.
            let hard_end = header
                .iter()
                .position(|&b| b == b'\r')
                .unwrap_or(header.len());
            for cut in 0..hard_end {
                assert_eq!(
                    decode_header(&header[..cut]),
                    Err(HeaderError::Incomplete),
                    "prefix of {cut} bytes"
                );
            }
        }
    }

    #[test]
    fn garbage_start_is_rejected_not_panicking() {
        assert_eq!(decode_header(b"hello"), Err(HeaderError::MissingPad(b'h')));
        assert_eq!(
            decode_header(&[ZPAD, b'x']),
            Err(HeaderError::MissingZdle(b'x'))
        );
        assert_eq!(
            decode_header(&[ZPAD, ZDLE, b'Z']),
            Err(HeaderError::UnknownFormat(b'Z'))
        );
    }

    #[test]
    fn unknown_frame_type_is_rejected() {
        // Hand-build a bin16 header with type byte 200.
        let body = [200u8, 0, 0, 0, 0];
        let crc = crate::crc::crc16_xmodem(&body);
        let mut wire = vec![ZPAD, ZDLE, ZBIN];
        let mut esc = Escaper::new();
        esc.push_slice(&body, &mut wire);
        esc.push_slice(&[(crc >> 8) as u8, crc as u8], &mut wire);
        assert_eq!(
            decode_header(&wire),
            Err(HeaderError::UnknownFrameType(200))
        );
    }

    #[test]
    fn flags_and_pos_accessors() {
        let header = Header::with_flags(FrameType::Zrinit, 0xA1, 0xB2, 0xC3, 0xD4);
        assert_eq!(header.zf3(), 0xA1);
        assert_eq!(header.zf2(), 0xB2);
        assert_eq!(header.zf1(), 0xC3);
        assert_eq!(header.zf0(), 0xD4);
        // ZF0 travels last (p3).
        assert_eq!(header.data, [0xA1, 0xB2, 0xC3, 0xD4]);

        let pos_header = Header::with_pos(FrameType::Zrpos, 0x0102_0304);
        // Little-endian: p0 is the least significant byte.
        assert_eq!(pos_header.data, [0x04, 0x03, 0x02, 0x01]);
        assert_eq!(pos_header.pos(), 0x0102_0304);
    }
}
