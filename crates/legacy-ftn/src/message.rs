//! Packed message records (FTS-0001 "stored message").
//!
//! Within a packet, each message is a fixed 14-byte header followed by four
//! text fields and the body, all little-endian:
//!
//! ```text
//!  off  sz  field           notes
//!   0    2  messageType     u16  == 2 (a 0x0000 word here ends the message run)
//!   2    2  origNode        u16
//!   4    2  destNode        u16
//!   6    2  origNet         u16
//!   8    2  destNet         u16
//!  10    2  attribute       u16  (bit flags: private, crash, ...)
//!  12    2  cost            u16
//!  14   20  DateTime        fixed 20-byte field, NUL-terminated ("02 Jul 26  13:30:45")
//!   *        To             NUL-terminated, <= 36 bytes
//!   *        From           NUL-terminated, <= 36 bytes
//!   *        Subject        NUL-terminated, <= 72 bytes
//!   *        Body           NUL-terminated (control/kludge lines + visible text)
//! ```
//!
//! A run of packed messages is terminated by a two-byte `0x0000` word where the
//! next `messageType` would be. [`encode_messages`] appends that terminator;
//! [`decode_messages`] stops at it (or at end-of-buffer).
//!
//! The **body is kept as raw bytes** ([`PackedMessage::body`]) so CP437 content
//! survives the round-trip losslessly; use [`PackedMessage::body_text`] to
//! decode it at the edge. Name/subject fields are CP437 too and round-trip
//! losslessly through [`crate::cp437`] (every non-zero byte has a unique glyph).

use crate::cp437;
use crate::error::FtnError;
use crate::reader::Reader;

/// The `messageType` value that introduces a packed message.
pub const MESSAGE_TYPE_2: u16 = 2;

/// Fixed width of the packed-message binary header.
pub const MSG_HEADER_LEN: usize = 14;

/// Fixed width of the `DateTime` field.
pub const DATETIME_LEN: usize = 20;

/// Maximum length (excluding the NUL) of the `To` and `From` fields.
pub const NAME_MAX: usize = 35;

/// Maximum length (excluding the NUL) of the `Subject` field.
pub const SUBJECT_MAX: usize = 71;

/// One packed message record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackedMessage {
    /// Origin node number.
    pub orig_node: u16,
    /// Destination node number.
    pub dest_node: u16,
    /// Origin net number.
    pub orig_net: u16,
    /// Destination net number.
    pub dest_net: u16,
    /// Attribute flag word.
    pub attribute: u16,
    /// Cost (in the origin's currency units).
    pub cost: u16,
    /// Free-form date/time string (max 19 chars on the wire).
    pub date_time: String,
    /// Recipient name (max 35 chars on the wire).
    pub to: String,
    /// Sender name (max 35 chars on the wire).
    pub from: String,
    /// Subject line (max 71 chars on the wire).
    pub subject: String,
    /// Raw message body bytes (CP437; kludges + visible text, no trailing NUL).
    pub body: Vec<u8>,
}

impl PackedMessage {
    /// Decode the raw body bytes to a Unicode string via CP437.
    pub fn body_text(&self) -> String {
        cp437::decode(&self.body)
    }

    /// Parse the raw body into a [`crate::kludge::Message`] (control lines +
    /// visible text).
    pub fn parse_body(&self) -> crate::kludge::Message {
        crate::kludge::Message::parse(&self.body)
    }

    /// Set the raw body from a [`crate::kludge::Message`] by serializing it.
    pub fn set_body(&mut self, message: &crate::kludge::Message) {
        self.body = message.serialize();
    }

    /// Encode this single record (without the trailing stream terminator).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MSG_HEADER_LEN + DATETIME_LEN + self.body.len() + 64);
        out.extend_from_slice(&MESSAGE_TYPE_2.to_le_bytes());
        out.extend_from_slice(&self.orig_node.to_le_bytes());
        out.extend_from_slice(&self.dest_node.to_le_bytes());
        out.extend_from_slice(&self.orig_net.to_le_bytes());
        out.extend_from_slice(&self.dest_net.to_le_bytes());
        out.extend_from_slice(&self.attribute.to_le_bytes());
        out.extend_from_slice(&self.cost.to_le_bytes());

        // DateTime: fixed 20-byte field, content truncated to 19 + NUL pad.
        let mut dt = [0u8; DATETIME_LEN];
        let dt_bytes = cp437::encode_lossy(&self.date_time);
        let n = dt_bytes.len().min(DATETIME_LEN - 1);
        dt[..n].copy_from_slice(&dt_bytes[..n]);
        out.extend_from_slice(&dt);

        push_cstr(&mut out, &self.to, NAME_MAX);
        push_cstr(&mut out, &self.from, NAME_MAX);
        push_cstr(&mut out, &self.subject, SUBJECT_MAX);

        out.extend_from_slice(&self.body);
        out.push(0); // body NUL terminator
        out
    }

    /// Decode one record from a reader positioned at its `messageType` word.
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, FtnError> {
        let msg_type = r.u16_le()?;
        if msg_type != MESSAGE_TYPE_2 {
            return Err(FtnError::MessageType(msg_type));
        }
        let orig_node = r.u16_le()?;
        let dest_node = r.u16_le()?;
        let orig_net = r.u16_le()?;
        let dest_net = r.u16_le()?;
        let attribute = r.u16_le()?;
        let cost = r.u16_le()?;

        let dt_field = r.array::<DATETIME_LEN>()?;
        let dt_len = dt_field
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(DATETIME_LEN);
        let date_time = cp437::decode(&dt_field[..dt_len]);

        let to = cp437::decode(r.cstr());
        let from = cp437::decode(r.cstr());
        let subject = cp437::decode(r.cstr());
        let body = r.cstr().to_vec();

        Ok(PackedMessage {
            orig_node,
            dest_node,
            orig_net,
            dest_net,
            attribute,
            cost,
            date_time,
            to,
            from,
            subject,
            body,
        })
    }
}

fn push_cstr(out: &mut Vec<u8>, s: &str, max: usize) {
    let bytes = cp437::encode_lossy(s);
    let n = bytes.len().min(max);
    out.extend_from_slice(&bytes[..n]);
    out.push(0);
}

/// Encode a list of messages, appending the `0x0000` stream terminator.
pub fn encode_messages(messages: &[PackedMessage]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in messages {
        out.extend_from_slice(&m.encode());
    }
    out.extend_from_slice(&0u16.to_le_bytes()); // terminator
    out
}

/// Decode a run of packed messages. Stops at the `0x0000` terminator or when
/// the buffer is exhausted. Never panics on truncated or malformed input.
pub fn decode_messages(buf: &[u8]) -> Result<Vec<PackedMessage>, FtnError> {
    let mut r = Reader::new(buf);
    let mut out = Vec::new();
    loop {
        match r.peek_u16_le() {
            // End of stream: explicit terminator or no room for another word.
            None => break,
            Some(0) => break,
            Some(MESSAGE_TYPE_2) => {
                out.push(PackedMessage::decode_from(&mut r)?);
            }
            Some(other) => return Err(FtnError::MessageType(other)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PackedMessage {
        PackedMessage {
            orig_node: 464,
            dest_node: 1,
            orig_net: 280,
            dest_net: 104,
            attribute: 0x0001,
            cost: 0,
            date_time: "02 Jul 26  13:30:45".to_string(),
            to: "Sysop".to_string(),
            from: "Kevin".to_string(),
            subject: "Hello, FidoNet".to_string(),
            body: b"This is the body.\rWith two lines.".to_vec(),
        }
    }

    #[test]
    fn single_roundtrip() {
        let m = sample();
        let bytes = m.encode();
        let mut r = Reader::new(&bytes);
        let decoded = PackedMessage::decode_from(&mut r).unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn header_is_14_bytes_and_datetime_20() {
        let bytes = sample().encode();
        // messageType at 0..2
        assert_eq!(&bytes[0..2], &[0x02, 0x00]);
        // DateTime field occupies exactly 20 bytes at offset 14.
        let dt = &bytes[MSG_HEADER_LEN..MSG_HEADER_LEN + DATETIME_LEN];
        assert_eq!(&dt[..19], b"02 Jul 26  13:30:45");
        assert_eq!(dt[19], 0); // NUL pad
    }

    #[test]
    fn list_roundtrip_with_terminator() {
        let a = sample();
        let mut b = sample();
        b.subject = "Second".to_string();
        b.body = b"Body two".to_vec();
        let msgs = vec![a, b];
        let bytes = encode_messages(&msgs);
        // ends with the 0x0000 terminator
        assert_eq!(&bytes[bytes.len() - 2..], &[0x00, 0x00]);
        let decoded = decode_messages(&bytes).unwrap();
        assert_eq!(decoded, msgs);
    }

    #[test]
    fn empty_list_is_just_terminator() {
        assert_eq!(encode_messages(&[]), vec![0x00, 0x00]);
        assert_eq!(decode_messages(&[0x00, 0x00]).unwrap(), Vec::new());
        assert_eq!(decode_messages(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn names_truncated_to_limits() {
        let mut m = sample();
        m.to = "x".repeat(100);
        let bytes = m.encode();
        let decoded = decode_messages(&bytes).unwrap();
        assert_eq!(decoded[0].to.len(), NAME_MAX);
    }

    #[test]
    fn body_bytes_are_lossless() {
        let mut m = sample();
        m.body = (1u8..=255).collect(); // every non-zero CP437 byte
        let decoded = decode_messages(&m.encode()).unwrap();
        assert_eq!(decoded[0].body, m.body);
    }

    #[test]
    fn truncated_stream_never_panics() {
        let bytes = encode_messages(&[sample()]);
        for n in 0..bytes.len() {
            let _ = decode_messages(&bytes[..n]);
        }
    }
}
