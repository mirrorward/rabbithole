//! The Hotline transaction: a 20-byte header plus a parameter-list body.
//!
//! Every message after the handshake is a transaction. It is one uniform
//! request / reply / server-push shape, distinguished by `is_reply` and the
//! `type` field.
//!
//! ## 20-byte header (all multi-byte fields big-endian)
//!
//! ```text
//! offset  size  field        notes
//! ------  ----  -----------  --------------------------------------------
//!   0      1    flags        reserved; normally 0
//!   1      1    is_reply     0 = request / push, 1 = reply
//!   2      2    type         transaction type (see `constants`)
//!   4      4    id           transaction id, echoed in the matching reply
//!   8      4    error        error code (replies only; 0 = success)
//!  12      4    total_size   total body size across all fragments
//!  16      4    data_size    body bytes carried in *this* frame
//! ```
//!
//! ## Body
//!
//! The body is a [`crate::field`] parameter list (2-byte count + fields).
//! `total_size` is the full body length. When `data_size < total_size` the
//! body is split across frames — see [`crate::reassembly`].
//!
//! ## Frame on the wire
//!
//! ```text
//! [ 20-byte header ][ data_size bytes of body ]
//! ```

use crate::error::HotlineError;
use crate::field::{decode_params, encode_params, Field};

/// The parsed 20-byte transaction header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionHeader {
    /// Reserved flags byte; normally `0`.
    pub flags: u8,
    /// `0` for a request or server push, `1` for a reply.
    pub is_reply: u8,
    /// Transaction type (see the `constants` module).
    pub type_: u16,
    /// Transaction id; a reply echoes the request's id.
    pub id: u32,
    /// Error code carried by replies (`0` = success).
    pub error: u32,
    /// Total body size summed across every fragment.
    pub total_size: u32,
    /// Body bytes present in this particular frame.
    pub data_size: u32,
}

impl TransactionHeader {
    /// Wire length of the header, in bytes.
    pub const LEN: usize = 20;

    /// Whether this frame carries only part of the body (more fragments follow).
    pub fn is_fragmented(&self) -> bool {
        self.data_size < self.total_size
    }

    /// Serialize to the fixed 20-byte wire form.
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0] = self.flags;
        out[1] = self.is_reply;
        out[2..4].copy_from_slice(&self.type_.to_be_bytes());
        out[4..8].copy_from_slice(&self.id.to_be_bytes());
        out[8..12].copy_from_slice(&self.error.to_be_bytes());
        out[12..16].copy_from_slice(&self.total_size.to_be_bytes());
        out[16..20].copy_from_slice(&self.data_size.to_be_bytes());
        out
    }

    /// Parse a 20-byte header from the front of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < Self::LEN {
            return Err(HotlineError::Truncated {
                need: Self::LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        Ok(Self {
            flags: bytes[0],
            is_reply: bytes[1],
            type_: u16::from_be_bytes([bytes[2], bytes[3]]),
            id: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            error: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            total_size: u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            data_size: u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
        })
    }
}

/// A complete, single-frame transaction: header plus decoded parameter fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    /// The 20-byte header.
    pub header: TransactionHeader,
    /// The decoded body fields.
    pub fields: Vec<Field>,
}

impl Transaction {
    /// Build a request transaction (`is_reply = 0`, `error = 0`).
    ///
    /// `total_size` and `data_size` are computed from the fields; the frame is
    /// not fragmented.
    pub fn request(type_: u16, id: u32, fields: Vec<Field>) -> Self {
        Self::with(type_, id, 0, 0, fields)
    }

    /// Build a reply transaction (`is_reply = 1`) with an error code.
    pub fn reply(type_: u16, id: u32, error: u32, fields: Vec<Field>) -> Self {
        Self::with(type_, id, 1, error, fields)
    }

    fn with(type_: u16, id: u32, is_reply: u8, error: u32, fields: Vec<Field>) -> Self {
        let body_len = body_len(&fields) as u32;
        Self {
            header: TransactionHeader {
                flags: 0,
                is_reply,
                type_,
                id,
                error,
                total_size: body_len,
                data_size: body_len,
            },
            fields,
        }
    }

    /// Serialize header + body to a single (unfragmented) frame.
    pub fn encode(&self) -> Vec<u8> {
        let body = encode_params(&self.fields);
        let mut header = self.header;
        header.total_size = body.len() as u32;
        header.data_size = body.len() as u32;
        let mut out = Vec::with_capacity(TransactionHeader::LEN + body.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(&body);
        out
    }

    /// Decode a complete single-frame transaction from `bytes`.
    ///
    /// The frame must not be fragmented (`data_size == total_size`); use
    /// [`crate::reassembly::Reassembler`] for multi-frame bodies. The body is
    /// parsed strictly, so trailing bytes after the parameter list are an error.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        let header = TransactionHeader::decode(bytes)?;
        let body_start = TransactionHeader::LEN;
        let data_size = header.data_size as usize;
        let end = body_start + data_size;
        if bytes.len() < end {
            return Err(HotlineError::Truncated {
                need: end - bytes.len(),
                have: bytes.len(),
            });
        }
        if header.is_fragmented() {
            return Err(HotlineError::Truncated {
                need: (header.total_size - header.data_size) as usize,
                have: data_size,
            });
        }
        let fields = decode_body(&header, &bytes[body_start..end])?;
        Ok(Self { header, fields })
    }
}

/// Parse a fully-reassembled body (`total_size` bytes) into fields.
pub(crate) fn decode_body(
    _header: &TransactionHeader,
    body: &[u8],
) -> Result<Vec<Field>, HotlineError> {
    // An empty body (no parameter list at all) is legal for bare
    // acknowledgements; treat it as zero fields.
    if body.is_empty() {
        return Ok(Vec::new());
    }
    decode_params(body)
}

/// Byte length of the parameter list a set of fields will encode to.
fn body_len(fields: &[Field]) -> usize {
    2 + fields.iter().map(Field::encoded_len).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{field, transaction};

    #[test]
    fn header_roundtrip() {
        let h = TransactionHeader {
            flags: 0,
            is_reply: 1,
            type_: transaction::LOGIN,
            id: 0x0102_0304,
            error: 0,
            total_size: 40,
            data_size: 20,
        };
        let bytes = h.encode();
        assert_eq!(TransactionHeader::decode(&bytes).unwrap(), h);
        assert!(h.is_fragmented());
    }

    #[test]
    fn login_golden_transaction() {
        // A login request: type 107, id 1, two fields (login="guest", empty
        // password). Hand-encoded byte-for-byte.
        let expected = [
            // ---- 20-byte header ----
            0x00, // flags
            0x00, // is_reply = request
            0x00, 0x6B, // type = 107 (login)
            0x00, 0x00, 0x00, 0x01, // id = 1
            0x00, 0x00, 0x00, 0x00, // error = 0
            0x00, 0x00, 0x00, 0x0F, // total_size = 15
            0x00, 0x00, 0x00, 0x0F, // data_size = 15
            // ---- body: parameter list ----
            0x00, 0x02, // field count = 2
            0x00, 0x69, 0x00, 0x05, b'g', b'u', b'e', b's', b't', // login(105)="guest"
            0x00, 0x6A, 0x00, 0x00, // password(106)=""
        ];
        let txn = Transaction::request(
            transaction::LOGIN,
            1,
            vec![
                Field::text(field::LOGIN, "guest"),
                Field::new(field::PASSWORD, Vec::new()),
            ],
        );
        assert_eq!(txn.encode(), expected);
        let back = Transaction::decode(&expected).unwrap();
        assert_eq!(back, txn);
    }

    #[test]
    fn get_user_list_golden() {
        // Empty-body request: get user name list (300), id 7.
        let expected = [
            0x00, 0x00, // flags, is_reply
            0x01, 0x2C, // type = 300
            0x00, 0x00, 0x00, 0x07, // id = 7
            0x00, 0x00, 0x00, 0x00, // error
            0x00, 0x00, 0x00, 0x02, // total_size = 2 (empty param list)
            0x00, 0x00, 0x00, 0x02, // data_size = 2
            0x00, 0x00, // field count = 0
        ];
        let txn = Transaction::request(transaction::GET_USER_NAME_LIST, 7, vec![]);
        assert_eq!(txn.encode(), expected);
        assert_eq!(Transaction::decode(&expected).unwrap(), txn);
    }

    #[test]
    fn reply_carries_error() {
        let txn = Transaction::reply(
            transaction::LOGIN,
            1,
            5,
            vec![Field::text(field::ERROR_TEXT, "denied")],
        );
        let bytes = txn.encode();
        let back = Transaction::decode(&bytes).unwrap();
        assert_eq!(back.header.is_reply, 1);
        assert_eq!(back.header.error, 5);
        assert_eq!(back, txn);
    }

    #[test]
    fn decode_rejects_fragment() {
        // total_size > data_size: not a complete single frame.
        let mut bytes = Transaction::request(transaction::LOGIN, 1, vec![]).encode();
        bytes[12..16].copy_from_slice(&100u32.to_be_bytes()); // bump total_size
        assert!(matches!(
            Transaction::decode(&bytes),
            Err(HotlineError::Truncated { .. })
        ));
    }
}
