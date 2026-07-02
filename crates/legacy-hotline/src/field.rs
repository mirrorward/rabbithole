//! Transaction parameter fields (Hotline's TLV encoding).
//!
//! A transaction body is a *parameter list*: a 2-byte count followed by that
//! many fields. Each field is a classic type-length-value triple.
//!
//! ## One field
//!
//! ```text
//! offset  size  field      notes
//! ------  ----  ---------  ---------------------------------------------
//!   0      2    field id   u16 big-endian (see `constants`)
//!   2      2    size       u16 big-endian, byte length of the value
//!   4    size   data       raw bytes (text, integer, or binary blob)
//! ```
//!
//! ## A parameter list (a transaction body)
//!
//! ```text
//! offset  size  field      notes
//! ------  ----  ---------  ---------------------------------------------
//!   0      2    count      u16 big-endian, number of fields that follow
//!   2      *    fields     `count` back-to-back fields as above
//! ```
//!
//! ## Integer fields are size-dependent
//!
//! Hotline has no fixed integer width. A numeric field is whatever width the
//! sender chose, and the receiver infers the width from the field `size`:
//! a 1-, 2-, or 4-byte big-endian integer (an empty value means zero). When we
//! *write* an integer we pick the **minimal** width that holds it (2 bytes for
//! anything up to `0xFFFF`, otherwise 4) — matching what classic servers emit.

use crate::error::HotlineError;

/// Ceiling on a single field's declared value size (16 MiB).
///
/// The size field is only a `u16` on the wire (max 65 535), so this ceiling is
/// really a belt-and-suspenders guard for the typed constructors.
pub const MAX_FIELD_SIZE: usize = 16 * 1024 * 1024;

/// A single TLV parameter field: a 16-bit id and an opaque byte value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field identifier (see the `constants` module for well-known ids).
    pub id: u16,
    /// Raw value bytes; interpretation depends on the field id.
    pub data: Vec<u8>,
}

impl Field {
    /// A field with an arbitrary byte payload.
    pub fn new(id: u16, data: impl Into<Vec<u8>>) -> Self {
        Self {
            id,
            data: data.into(),
        }
    }

    /// A text field (UTF-8 / MacRoman bytes stored verbatim, no terminator).
    pub fn text(id: u16, text: &str) -> Self {
        Self::new(id, text.as_bytes().to_vec())
    }

    /// An integer field encoded in the **minimal** big-endian width.
    ///
    /// Values up to `0xFFFF` take 2 bytes; larger values take 4. This mirrors
    /// classic Hotline servers, which never emit a 1-byte or 3-byte integer.
    pub fn int(id: u16, value: u32) -> Self {
        Self::new(id, min_int_bytes(value))
    }

    /// Interpret this field's value as a big-endian integer.
    ///
    /// See [`read_int`] for the accepted widths.
    pub fn as_int(&self) -> Result<u32, HotlineError> {
        read_int(&self.data)
    }

    /// Interpret this field's value as UTF-8 text (lossily).
    pub fn as_text_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.data)
    }

    /// A credential field (LOGIN / PASSWORD): text stored [`obfuscate`]d, as
    /// classic clients send it in Login and the account-admin flows.
    pub fn credential(id: u16, text: &str) -> Self {
        Self::new(id, obfuscate(text.as_bytes()))
    }

    /// Interpret this field's value as an obfuscated credential, returning
    /// the clear text (lossily decoded as UTF-8).
    pub fn as_credential_text_lossy(&self) -> String {
        String::from_utf8_lossy(&deobfuscate(&self.data)).into_owned()
    }

    /// Encoded length of this field on the wire (`4 + data.len()`).
    pub fn encoded_len(&self) -> usize {
        4 + self.data.len()
    }

    /// Append this field's wire form to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_be_bytes());
        // Value length is clamped by construction elsewhere; a value longer
        // than u16::MAX would truncate here, so callers must respect the wire
        // limit. Typed constructors keep values small; raw `new` is caller's
        // responsibility (documented on `MAX_FIELD_SIZE`).
        let len = self.data.len().min(u16::MAX as usize) as u16;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.data[..len as usize]);
    }

    /// Decode a single field from the front of `bytes`.
    ///
    /// Returns the field and the number of bytes consumed.
    pub fn decode(bytes: &[u8]) -> Result<(Field, usize), HotlineError> {
        if bytes.len() < 4 {
            return Err(HotlineError::Truncated {
                need: 4 - bytes.len(),
                have: bytes.len(),
            });
        }
        let id = u16::from_be_bytes([bytes[0], bytes[1]]);
        let size = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        let end = 4 + size;
        if bytes.len() < end {
            return Err(HotlineError::Truncated {
                need: end - bytes.len(),
                have: bytes.len(),
            });
        }
        Ok((
            Field {
                id,
                data: bytes[4..end].to_vec(),
            },
            end,
        ))
    }
}

/// Read a size-dependent Hotline integer from a big-endian byte slice.
///
/// Accepted widths:
/// - `0` bytes → `0` (an empty numeric field means zero)
/// - `1` byte  → `u8`  widened
/// - `2` bytes → `u16` big-endian widened
/// - `4` bytes → `u32` big-endian
///
/// Any other width returns [`HotlineError::BadIntWidth`].
pub fn read_int(bytes: &[u8]) -> Result<u32, HotlineError> {
    match bytes.len() {
        0 => Ok(0),
        1 => Ok(u32::from(bytes[0])),
        2 => Ok(u32::from(u16::from_be_bytes([bytes[0], bytes[1]]))),
        4 => Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        other => Err(HotlineError::BadIntWidth(other)),
    }
}

/// Encode an integer in the minimal Hotline width: 2 bytes if it fits in a
/// `u16`, otherwise 4 bytes. Always big-endian.
pub fn min_int_bytes(value: u32) -> Vec<u8> {
    if value <= u32::from(u16::MAX) {
        (value as u16).to_be_bytes().to_vec()
    } else {
        value.to_be_bytes().to_vec()
    }
}

/// Obfuscate credential bytes the classic Hotline way: bitwise-complement
/// every byte (`255 - b`, i.e. `!b`).
///
/// LOGIN (105) and PASSWORD (106) travel obfuscated in the Login transaction
/// and in the account-admin flows (NewUser / GetUser / SetUser). This is
/// obfuscation, not encryption — it only keeps credentials out of casual
/// packet dumps.
pub fn obfuscate(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|b| !b).collect()
}

/// De-obfuscate credential bytes (see [`obfuscate`]).
///
/// The complement transform is an involution, so this is the same operation;
/// the separate name keeps call sites self-describing.
pub fn deobfuscate(bytes: &[u8]) -> Vec<u8> {
    obfuscate(bytes)
}

/// Encode a parameter list: 2-byte count followed by every field.
pub fn encode_params(fields: &[Field]) -> Vec<u8> {
    let count = fields.len().min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(2 + fields.iter().map(Field::encoded_len).sum::<usize>());
    out.extend_from_slice(&count.to_be_bytes());
    for f in fields.iter().take(count as usize) {
        f.encode_into(&mut out);
    }
    out
}

/// Decode a complete parameter list, consuming exactly `bytes`.
///
/// Strict: leftover bytes after the declared field count are reported as
/// [`HotlineError::TrailingBytes`], so a valid transaction body round-trips
/// byte-for-byte.
pub fn decode_params(bytes: &[u8]) -> Result<Vec<Field>, HotlineError> {
    if bytes.len() < 2 {
        return Err(HotlineError::Truncated {
            need: 2 - bytes.len(),
            have: bytes.len(),
        });
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut pos = 2;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let (field, used) = Field::decode(&bytes[pos..])?;
        pos += used;
        fields.push(field);
    }
    if pos != bytes.len() {
        return Err(HotlineError::TrailingBytes(bytes.len() - pos));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_roundtrip() {
        let f = Field::text(105, "guest");
        let mut buf = Vec::new();
        f.encode_into(&mut buf);
        assert_eq!(
            buf,
            vec![0x00, 0x69, 0x00, 0x05, b'g', b'u', b'e', b's', b't']
        );
        let (back, used) = Field::decode(&buf).unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(back, f);
    }

    #[test]
    fn read_int_widths() {
        assert_eq!(read_int(&[]).unwrap(), 0);
        assert_eq!(read_int(&[0x2A]).unwrap(), 42);
        assert_eq!(read_int(&[0x01, 0x00]).unwrap(), 256);
        assert_eq!(read_int(&[0x00, 0x01, 0x00, 0x00]).unwrap(), 65_536);
        assert!(matches!(
            read_int(&[0, 0, 0]),
            Err(HotlineError::BadIntWidth(3))
        ));
    }

    #[test]
    fn min_width_picks_two_or_four() {
        assert_eq!(min_int_bytes(0), vec![0x00, 0x00]);
        assert_eq!(min_int_bytes(0xFFFF), vec![0xFF, 0xFF]);
        assert_eq!(min_int_bytes(0x1_0000), vec![0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn int_field_roundtrips_value() {
        for v in [0u32, 1, 255, 256, 65_535, 65_536, u32::MAX] {
            let f = Field::int(9, v);
            assert_eq!(f.as_int().unwrap(), v);
        }
    }

    #[test]
    fn obfuscation_is_a_complement_involution() {
        // 'p' = 0x70 -> 0x8F, 'a' = 0x61 -> 0x9E, 's' = 0x73 -> 0x8C.
        assert_eq!(obfuscate(b"pass"), vec![0x8F, 0x9E, 0x8C, 0x8C]);
        assert_eq!(deobfuscate(&[0x8F, 0x9E, 0x8C, 0x8C]), b"pass".to_vec());
        assert_eq!(deobfuscate(&obfuscate(b"gu\xC3\xA9st")), b"gu\xC3\xA9st");
        assert_eq!(obfuscate(&[]), Vec::<u8>::new());
        // `!b` is exactly `255 - b`.
        for b in [0u8, 1, 0x7F, 0xFE, 0xFF] {
            assert_eq!(obfuscate(&[b]), vec![255 - b]);
        }
    }

    #[test]
    fn credential_field_roundtrip() {
        let f = Field::credential(106, "s3cret");
        assert_ne!(f.data, b"s3cret".to_vec());
        assert_eq!(f.as_credential_text_lossy(), "s3cret");
        // Wire round-trip preserves the obfuscated bytes exactly.
        let mut buf = Vec::new();
        f.encode_into(&mut buf);
        let (back, _) = Field::decode(&buf).unwrap();
        assert_eq!(back.as_credential_text_lossy(), "s3cret");
    }

    #[test]
    fn params_roundtrip() {
        let fields = vec![Field::text(105, "guest"), Field::int(103, 42)];
        let bytes = encode_params(&fields);
        assert_eq!(&bytes[0..2], &[0x00, 0x02]);
        assert_eq!(decode_params(&bytes).unwrap(), fields);
    }

    #[test]
    fn empty_params_roundtrip() {
        let bytes = encode_params(&[]);
        assert_eq!(bytes, vec![0x00, 0x00]);
        assert_eq!(decode_params(&bytes).unwrap(), Vec::<Field>::new());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut bytes = encode_params(&[Field::int(1, 7)]);
        bytes.push(0xEE);
        assert!(matches!(
            decode_params(&bytes),
            Err(HotlineError::TrailingBytes(1))
        ));
    }

    #[test]
    fn truncated_field_size() {
        // count=1, one field claims size 8 but only 2 bytes follow.
        let bytes = [0x00, 0x01, 0x00, 0x0A, 0x00, 0x08, 0xAA, 0xBB];
        assert!(matches!(
            decode_params(&bytes),
            Err(HotlineError::Truncated { .. })
        ));
    }
}
