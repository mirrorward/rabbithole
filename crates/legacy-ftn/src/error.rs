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

    /// A St. Louis nodelist line could not be parsed into an entry.
    #[error("invalid nodelist line: {reason}")]
    Nodelist {
        /// Why parsing failed.
        reason: NodelistErrorKind,
    },

    /// A NODEDIFF could not be applied to a base nodelist.
    #[error("invalid NODEDIFF: {reason}")]
    Nodediff {
        /// Why application failed.
        reason: NodediffErrorKind,
    },

    /// A nodelist's declared header CRC did not match the computed value.
    #[error("nodelist CRC mismatch: header declares {declared:#06x}, computed {computed:#06x}")]
    Crc {
        /// CRC value declared on the `;A … : NNNNN` header line.
        declared: u16,
        /// CRC value computed over the rest of the file.
        computed: u16,
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

/// Why an [`FtnError::Nodelist`] was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodelistErrorKind {
    /// The line had fewer than the seven mandatory comma-separated fields.
    TooFewFields {
        /// Number of comma-separated fields actually found.
        found: usize,
    },
    /// The leading keyword was not one of the recognized St. Louis keywords.
    UnknownKeyword,
    /// The `Number` field was empty or not a valid `u16`.
    BadNumber,
    /// The `;A … : NNNNN` header line carried no parseable trailing CRC value.
    MissingHeaderCrc,
}

impl fmt::Display for NodelistErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodelistErrorKind::TooFewFields { found } => {
                write!(f, "expected at least 7 fields, found {found}")
            }
            NodelistErrorKind::UnknownKeyword => f.write_str("unrecognized leading keyword"),
            NodelistErrorKind::BadNumber => {
                f.write_str("number field is not a valid 16-bit number")
            }
            NodelistErrorKind::MissingHeaderCrc => {
                f.write_str("header line has no parseable trailing CRC")
            }
        }
    }
}

/// Why an [`FtnError::Nodediff`] was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodediffErrorKind {
    /// A command line did not match `<A|C|D><decimal>`.
    BadCommand {
        /// 1-based line number within the diff.
        line: usize,
    },
    /// A `Copy`/`Delete` command referenced more base lines than remain.
    Underflow {
        /// 1-based line number within the diff.
        line: usize,
    },
    /// An `Add` command promised more data lines than the diff contained.
    MissingAddLines {
        /// 1-based line number within the diff.
        line: usize,
    },
}

impl fmt::Display for NodediffErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodediffErrorKind::BadCommand { line } => {
                write!(f, "malformed command on diff line {line}")
            }
            NodediffErrorKind::Underflow { line } => {
                write!(
                    f,
                    "copy/delete past end of base nodelist at diff line {line}"
                )
            }
            NodediffErrorKind::MissingAddLines { line } => {
                write!(f, "add command missing data lines at diff line {line}")
            }
        }
    }
}
