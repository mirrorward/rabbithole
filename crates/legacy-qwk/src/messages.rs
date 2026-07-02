//! `MESSAGES.DAT` codec: 128-byte records, `0xE3` line endings.
//!
//! `MESSAGES.DAT` is a flat sequence of **128-byte records**. The very first
//! record is a producer/ID header block ("Produced by ...", space-padded).
//! After it, each message is one **128-byte message header** followed by
//! `blocks - 1` body records, where `blocks` (inclusive of the header) is stored
//! in the header itself.
//!
//! ## Message header block (128 bytes, offsets are 0-based)
//!
//! ```text
//!  offset  len  field
//!  ------  ---  ------------------------------------------------------------
//!    0      1   status flag (ASCII: ' ' public/unread, '-' read, '*' private…)
//!    1      7   message number (ASCII, space-padded)
//!    8      8   date  "MM-DD-YY"
//!   16      5   time  "HH:MM"
//!   21     25   To    (space-padded, hard-truncated to 25 — QWKE lifts this)
//!   46     25   From
//!   71     25   Subject
//!   96     12   password
//!  108      8   reference (reply-to) message number (ASCII)
//!  116      6   number of 128-byte blocks, INCLUDING this header (ASCII)
//!  122      1   active flag: 0xE1 (225)=active, 0xE2 (226)=killed
//!  123      2   conference number, little-endian (low byte @123, high @124)
//!  125      2   logical message number within the packet (reader index)
//!  127      1   0x00 filler
//!  ------  ---  ------------------------------------------------------------
//!                                                          total = 128 bytes
//! ```
//!
//! ## Body encoding
//!
//! Body text is **not** CRLF-terminated: the end-of-line marker is the single
//! byte `0xE3` (227, "π" in CP437). The body is space-padded out to fill its
//! last 128-byte block. This codec converts `\n` <-> `0xE3` at the edge and
//! strips the trailing space/NUL padding on decode (so a body must not rely on
//! *trailing* whitespace surviving a round trip; interior whitespace is kept).

use crate::error::QwkError;
use crate::model::QwkMessage;
use crate::text::{read_field, write_field};

/// Fixed record / block size in bytes.
pub const BLOCK: usize = 128;

/// QWK body end-of-line marker (`0xE3`, "π" in CP437).
pub const EOL: u8 = 0xE3;

const ACTIVE: u8 = 0xE1;
const KILLED: u8 = 0xE2;

/// Default producer string written into the first record when none is supplied.
pub const DEFAULT_PRODUCER: &str = "Produced by RabbitHole QWK codec";

/// A decoded `MESSAGES.DAT`: the producer header line plus the message list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesDat {
    /// Text of the first 128-byte record (the "Produced by ..." block),
    /// trailing padding trimmed.
    pub producer: String,
    /// The messages in packet order.
    pub messages: Vec<QwkMessage>,
}

impl MessagesDat {
    /// Construct from a message list, using [`DEFAULT_PRODUCER`] for the header
    /// block.
    pub fn new(messages: Vec<QwkMessage>) -> Self {
        Self {
            producer: DEFAULT_PRODUCER.to_string(),
            messages,
        }
    }

    /// Encode the whole file to bytes: the producer block followed by every
    /// message. The output length is always a multiple of [`BLOCK`].
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![b' '; BLOCK];
        write_field(&mut out[..BLOCK], &self.producer);
        for (i, msg) in self.messages.iter().enumerate() {
            // Logical (reader) index is 1-based within the packet.
            encode_message(msg, (i + 1) as u16, &mut out);
        }
        out
    }

    /// Decode a whole `MESSAGES.DAT` byte stream.
    ///
    /// Returns [`QwkError::Truncated`] if the producer block or any message is
    /// incomplete, and [`QwkError::BadBlockCount`] if a header's block count is
    /// missing, non-numeric, or less than one. Never panics.
    pub fn decode(bytes: &[u8]) -> Result<Self, QwkError> {
        if bytes.len() < BLOCK {
            return Err(QwkError::Truncated {
                need: BLOCK,
                have: bytes.len(),
            });
        }
        let producer = read_field(&bytes[..BLOCK]);
        let mut messages = Vec::new();
        let mut pos = BLOCK;
        while pos < bytes.len() {
            let (msg, consumed) = decode_message(&bytes[pos..])?;
            messages.push(msg);
            pos += consumed;
        }
        Ok(Self { producer, messages })
    }
}

/// Convert a normalized (`\n`) body into on-disk bytes (`0xE3` line endings).
fn encode_body(body: &str) -> Vec<u8> {
    body.chars()
        .map(|c| match c {
            '\n' => EOL,
            c if (c as u32) <= 0xFF => c as u8,
            _ => b'?',
        })
        .collect()
}

/// Convert an on-disk body block (`0xE3` line endings, space/NUL padded) back
/// into normalized (`\n`) text, trimming trailing padding.
fn decode_body(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|&b| b != b' ' && b != 0)
        .map_or(0, |i| i + 1);
    bytes[..end]
        .iter()
        .map(|&b| if b == EOL { '\n' } else { b as char })
        .collect()
}

/// Append one message (header block + padded body blocks) to `out`.
fn encode_message(msg: &QwkMessage, logical: u16, out: &mut Vec<u8>) {
    let body = encode_body(&msg.body);
    // Body occupies at least one block even when empty.
    let body_blocks = body.len().div_ceil(BLOCK).max(1);
    let total_blocks = 1 + body_blocks;

    let start = out.len();
    out.resize(start + total_blocks * BLOCK, b' ');
    let rec = &mut out[start..];

    rec[0] = msg.status;
    write_field(&mut rec[1..8], &msg.number.to_string());
    write_field(&mut rec[8..16], &msg.date);
    write_field(&mut rec[16..21], &msg.time);
    write_field(&mut rec[21..46], &msg.to);
    write_field(&mut rec[46..71], &msg.from);
    write_field(&mut rec[71..96], &msg.subject);
    write_field(&mut rec[96..108], &msg.password);
    let reference = if msg.reference == 0 {
        String::new()
    } else {
        msg.reference.to_string()
    };
    write_field(&mut rec[108..116], &reference);
    write_field(&mut rec[116..122], &total_blocks.to_string());
    rec[122] = if msg.active { ACTIVE } else { KILLED };
    rec[123..125].copy_from_slice(&msg.conference.to_le_bytes());
    rec[125..127].copy_from_slice(&logical.to_le_bytes());
    rec[127] = 0x00;

    rec[BLOCK..BLOCK + body.len()].copy_from_slice(&body);
    // Remaining body bytes are already the space fill from `resize`.
}

/// Decode one message starting at the front of `bytes`; returns the message and
/// the number of bytes it consumed.
fn decode_message(bytes: &[u8]) -> Result<(QwkMessage, usize), QwkError> {
    if bytes.len() < BLOCK {
        return Err(QwkError::Truncated {
            need: BLOCK,
            have: bytes.len(),
        });
    }
    let hdr = &bytes[..BLOCK];

    let block_field = read_field(&hdr[116..122]);
    let trimmed = block_field.trim();
    if trimmed.is_empty() {
        return Err(QwkError::BadBlockCount {
            field: block_field,
            reason: "empty",
        });
    }
    let total_blocks: usize = trimmed.parse().map_err(|_| QwkError::BadBlockCount {
        field: block_field.clone(),
        reason: "not a base-10 integer",
    })?;
    if total_blocks < 1 {
        return Err(QwkError::BadBlockCount {
            field: block_field,
            reason: "must count at least the header block",
        });
    }

    let total_len = total_blocks * BLOCK;
    if bytes.len() < total_len {
        return Err(QwkError::Truncated {
            need: total_len,
            have: bytes.len(),
        });
    }

    let number = read_field(&hdr[1..8]).trim().parse().unwrap_or(0);
    let reference = read_field(&hdr[108..116]).trim().parse().unwrap_or(0);
    let conference = u16::from_le_bytes([hdr[123], hdr[124]]);
    let body = decode_body(&bytes[BLOCK..total_len]);

    let msg = QwkMessage {
        status: hdr[0],
        number,
        conference,
        date: read_field(&hdr[8..16]),
        time: read_field(&hdr[16..21]),
        to: read_field(&hdr[21..46]),
        from: read_field(&hdr[46..71]),
        subject: read_field(&hdr[71..96]),
        password: read_field(&hdr[96..108]),
        reference,
        active: hdr[122] == ACTIVE,
        body,
    };
    Ok((msg, total_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<QwkMessage> {
        vec![
            QwkMessage {
                status: b'*',
                number: 1,
                conference: 5,
                date: "07-02-26".into(),
                time: "13:45".into(),
                to: "SYSOP".into(),
                from: "KEVIN".into(),
                subject: "Hello there".into(),
                password: String::new(),
                reference: 0,
                active: true,
                body: "First line\nSecond line\nThird".into(),
            },
            QwkMessage::new(
                300,
                2,
                "ALL",
                "KEVIN",
                "Re: Hello",
                "A reply body\nwith two lines",
            ),
        ]
    }

    #[test]
    fn round_trip_message_list() {
        let dat = MessagesDat::new(sample());
        let bytes = dat.encode();
        assert_eq!(bytes.len() % BLOCK, 0);
        let back = MessagesDat::decode(&bytes).unwrap();
        assert_eq!(back.messages, dat.messages);
        assert_eq!(back.producer, dat.producer);
    }

    #[test]
    fn conference_number_is_little_endian_at_123() {
        let mut msg = QwkMessage::new(0, 1, "A", "B", "S", "hi");
        msg.conference = 0x0102; // low=0x02 @123, high=0x01 @124
        let bytes = MessagesDat::new(vec![msg]).encode();
        assert_eq!(bytes[BLOCK + 123], 0x02);
        assert_eq!(bytes[BLOCK + 124], 0x01);
    }

    #[test]
    fn body_uses_e3_line_endings_not_crlf() {
        let msg = QwkMessage::new(0, 1, "A", "B", "S", "one\ntwo");
        let bytes = MessagesDat::new(vec![msg]).encode();
        let body = &bytes[BLOCK * 2..];
        assert!(body.contains(&EOL), "0xE3 EOL marker must be present");
        assert!(!body.windows(2).any(|w| w == b"\r\n"), "no CRLF in body");
    }

    #[test]
    fn block_count_includes_header_and_is_ascii() {
        let msg = QwkMessage::new(0, 1, "A", "B", "S", "short");
        let bytes = MessagesDat::new(vec![msg]).encode();
        // header + 1 body block = 2
        assert_eq!(read_field(&bytes[BLOCK + 116..BLOCK + 122]).trim(), "2");
    }

    #[test]
    fn empty_body_round_trips() {
        let msg = QwkMessage::new(0, 7, "A", "B", "S", "");
        let dat = MessagesDat::new(vec![msg]);
        let back = MessagesDat::decode(&dat.encode()).unwrap();
        assert_eq!(back.messages[0].body, "");
        assert_eq!(back.messages[0].number, 7);
    }

    #[test]
    fn active_flag_maps_to_e1_e2() {
        let mut msg = QwkMessage::new(0, 1, "A", "B", "S", "x");
        msg.active = false;
        let bytes = MessagesDat::new(vec![msg]).encode();
        assert_eq!(bytes[BLOCK + 122], KILLED);
        let back = MessagesDat::decode(&bytes).unwrap();
        assert!(!back.messages[0].active);
    }

    #[test]
    fn decode_truncated_never_panics() {
        // Every truncation length must yield Err, never a panic.
        let full = MessagesDat::new(sample()).encode();
        for n in 0..full.len() {
            let _ = MessagesDat::decode(&full[..n]);
        }
    }

    #[test]
    fn decode_random_bytes_never_panics() {
        let mut seed = 0x1234_5678u32;
        for len in [0usize, 1, 63, 128, 200, 256, 400] {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            let _ = MessagesDat::decode(&bytes);
        }
    }

    #[test]
    fn bad_block_count_is_reported() {
        let mut bytes = MessagesDat::new(vec![QwkMessage::new(0, 1, "A", "B", "S", "x")]).encode();
        // Corrupt the block-count field of the first message header to letters.
        for b in &mut bytes[BLOCK + 116..BLOCK + 122] {
            *b = b'Z';
        }
        assert!(matches!(
            MessagesDat::decode(&bytes),
            Err(QwkError::BadBlockCount { .. })
        ));
    }
}
