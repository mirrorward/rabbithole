//! The Hotline connection handshake.
//!
//! Before any transactions flow, the client sends a fixed 12-byte handshake
//! and the server answers with a fixed 8-byte reply. Both begin with the ASCII
//! protocol magic `TRTP` ("Tada Retro Transfer Protocol", the framing layer
//! Hotline rides on).
//!
//! ## Client handshake (12 bytes, all multi-byte fields big-endian)
//!
//! ```text
//! offset  size  field         value
//! ------  ----  ------------  ----------------------------------------
//!   0      4    protocol id   'T' 'R' 'T' 'P'   (0x54 52 54 50)
//!   4      4    sub-protocol  'H' 'O' 'T' 'L'   (0x48 4F 54 4C)
//!   8      2    version       0x0001
//!  10      2    sub-version   client-specific (often 0x0000 / 0x0002)
//! ```
//!
//! ## Server reply (8 bytes)
//!
//! ```text
//! offset  size  field         value
//! ------  ----  ------------  ----------------------------------------
//!   0      4    protocol id   'T' 'R' 'T' 'P'
//!   4      4    error code    u32; 0 = success, non-zero = refuse
//! ```
//!
//! A non-zero error code tells the client the server declined the connection;
//! the socket is then closed.

use crate::error::HotlineError;

/// The `TRTP` framing-protocol magic that opens every handshake and reply.
pub const PROTOCOL_ID: [u8; 4] = *b"TRTP";

/// The `HOTL` sub-protocol magic identifying Hotline (vs. tracker/transfer).
pub const SUB_PROTOCOL_HOTL: [u8; 4] = *b"HOTL";

/// The protocol version modern and classic clients send: `1`.
pub const HANDSHAKE_VERSION: u16 = 1;

/// The 12-byte client handshake frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handshake {
    /// Protocol version; classic clients send [`HANDSHAKE_VERSION`] (`1`).
    pub version: u16,
    /// Sub-version; client-specific, commonly `0` or `2`.
    pub sub_version: u16,
}

impl Handshake {
    /// Wire length of an encoded handshake, in bytes.
    pub const LEN: usize = 12;

    /// A handshake with the canonical version and a zero sub-version.
    pub fn hotl() -> Self {
        Self {
            version: HANDSHAKE_VERSION,
            sub_version: 0,
        }
    }

    /// Serialize to the fixed 12-byte wire form.
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&PROTOCOL_ID);
        out[4..8].copy_from_slice(&SUB_PROTOCOL_HOTL);
        out[8..10].copy_from_slice(&self.version.to_be_bytes());
        out[10..12].copy_from_slice(&self.sub_version.to_be_bytes());
        out
    }

    /// Parse a 12-byte handshake, validating both magic fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < Self::LEN {
            return Err(HotlineError::Truncated {
                need: Self::LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        let proto: [u8; 4] = bytes[0..4].try_into().expect("slice is 4 bytes");
        if proto != PROTOCOL_ID {
            return Err(HotlineError::BadProtocolId {
                expected: PROTOCOL_ID,
                got: proto,
            });
        }
        let sub: [u8; 4] = bytes[4..8].try_into().expect("slice is 4 bytes");
        if sub != SUB_PROTOCOL_HOTL {
            return Err(HotlineError::BadSubProtocolId {
                expected: SUB_PROTOCOL_HOTL,
                got: sub,
            });
        }
        let version = u16::from_be_bytes([bytes[8], bytes[9]]);
        let sub_version = u16::from_be_bytes([bytes[10], bytes[11]]);
        Ok(Self {
            version,
            sub_version,
        })
    }
}

/// The 8-byte server handshake reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandshakeReply {
    /// `0` = success (proceed with transactions); non-zero = refused.
    pub error: u32,
}

impl HandshakeReply {
    /// Wire length of an encoded reply, in bytes.
    pub const LEN: usize = 8;

    /// A success reply (`error == 0`).
    pub fn ok() -> Self {
        Self { error: 0 }
    }

    /// Whether this reply accepts the connection.
    pub fn is_ok(&self) -> bool {
        self.error == 0
    }

    /// Serialize to the fixed 8-byte wire form.
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&PROTOCOL_ID);
        out[4..8].copy_from_slice(&self.error.to_be_bytes());
        out
    }

    /// Parse an 8-byte reply, validating the protocol magic.
    pub fn decode(bytes: &[u8]) -> Result<Self, HotlineError> {
        if bytes.len() < Self::LEN {
            return Err(HotlineError::Truncated {
                need: Self::LEN - bytes.len(),
                have: bytes.len(),
            });
        }
        let proto: [u8; 4] = bytes[0..4].try_into().expect("slice is 4 bytes");
        if proto != PROTOCOL_ID {
            return Err(HotlineError::BadProtocolId {
                expected: PROTOCOL_ID,
                got: proto,
            });
        }
        let error = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        Ok(Self { error })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_golden_bytes() {
        // Real classic-client handshake: TRTP HOTL, version 1, sub-version 2.
        let expected = [
            0x54, 0x52, 0x54, 0x50, // "TRTP"
            0x48, 0x4F, 0x54, 0x4C, // "HOTL"
            0x00, 0x01, // version 1
            0x00, 0x02, // sub-version 2
        ];
        let hs = Handshake {
            version: 1,
            sub_version: 2,
        };
        assert_eq!(hs.encode(), expected);
        assert_eq!(Handshake::decode(&expected).unwrap(), hs);
    }

    #[test]
    fn reply_golden_bytes() {
        let expected = [0x54, 0x52, 0x54, 0x50, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(HandshakeReply::ok().encode(), expected);
        let r = HandshakeReply::decode(&expected).unwrap();
        assert!(r.is_ok());
    }

    #[test]
    fn reply_error_roundtrip() {
        let r = HandshakeReply { error: 5 };
        let bytes = r.encode();
        let back = HandshakeReply::decode(&bytes).unwrap();
        assert_eq!(back, r);
        assert!(!back.is_ok());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = Handshake::hotl().encode();
        bytes[0] = b'X';
        assert!(matches!(
            Handshake::decode(&bytes),
            Err(HotlineError::BadProtocolId { .. })
        ));
    }

    #[test]
    fn rejects_bad_sub_protocol() {
        let mut bytes = Handshake::hotl().encode();
        bytes[4] = b'X';
        assert!(matches!(
            Handshake::decode(&bytes),
            Err(HotlineError::BadSubProtocolId { .. })
        ));
    }

    #[test]
    fn rejects_short_input() {
        assert!(matches!(
            Handshake::decode(&[0u8; 4]),
            Err(HotlineError::Truncated { .. })
        ));
        assert!(matches!(
            HandshakeReply::decode(&[0u8; 4]),
            Err(HotlineError::Truncated { .. })
        ));
    }
}
