//! Error type shared across the FTN codec.
//!
//! Decoding is *total*: every fallible read goes through [`crate::reader`],
//! which returns [`FtnError::Truncated`] instead of indexing out of bounds,
//! so feeding random or truncated bytes to any `decode` function can never
//! panic — it returns an `Err` at worst.

use std::fmt;

/// Errors produced while encoding or decoding FTN structures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FtnError {
    /// A read ran past the end of the input buffer.
    #[error("truncated input: needed {need} more byte(s) at offset {at}, buffer holds {len}")]
    Truncated {
        /// Offset the read started at.
        at: usize,
        /// Number of bytes the read required.
        need: usize,
        /// Total length of the buffer.
        len: usize,
    },

    /// A packet declared a version other than 2 in its type field (offset 18).
    #[error("unsupported packet type {0} (only FTS-0001 type-2 / FSC-0039 type-2+ are supported)")]
    PacketType(u16),

    /// A packed message record declared a leading type word that is neither 2
    /// (a message) nor 0 (the stream terminator).
    #[error("invalid packed-message type word {0:#06x} (expected 0x0002)")]
    MessageType(u16),

    /// An FTN address string did not match `zone:net/node[.point]`.
    #[error("invalid FTN address {input:?}: {reason}")]
    Address {
        /// The offending input.
        input: String,
        /// Why parsing failed.
        reason: AddressErrorKind,
    },
}

/// Why an [`FtnError::Address`] was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressErrorKind {
    /// The `zone:` separator was missing.
    MissingZone,
    /// The `/node` separator was missing.
    MissingNode,
    /// A numeric component was empty or not a valid `u16`.
    BadNumber,
    /// Trailing/garbage characters after the address.
    Trailing,
}

impl fmt::Display for AddressErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AddressErrorKind::MissingZone => "missing 'zone:' separator",
            AddressErrorKind::MissingNode => "missing '/node' separator",
            AddressErrorKind::BadNumber => "component is not a valid 16-bit number",
            AddressErrorKind::Trailing => "unexpected trailing characters",
        };
        f.write_str(s)
    }
}
