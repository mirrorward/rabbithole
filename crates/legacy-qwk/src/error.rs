//! Errors produced while decoding/encoding QWK/QWKE packet files.
//!
//! Every decoder in this crate is **total**: it returns a [`QwkError`] rather
//! than panicking on malformed, truncated, or hostile input. QWK/REP packets
//! arrive from untrusted offline readers, so a bad byte must never bring a
//! surrounding service down. Text fields are decoded as Latin-1 (lossless for
//! all 256 byte values) and therefore never fail; the fallible surface is the
//! fixed-length structure (block counts, record lengths, and the small set of
//! integer fields in `CONTROL.DAT`).

/// A decode/encode failure in the QWK/QWKE codec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum QwkError {
    /// The input ended before a fixed- or declared-length region was complete.
    ///
    /// `need` is how many more bytes were required; `have` is how many were
    /// actually available at the point the decoder gave up.
    #[error("truncated input: need {need} byte(s), have {have}")]
    Truncated {
        /// Bytes required to make progress.
        need: usize,
        /// Bytes actually available.
        have: usize,
    },

    /// The `MESSAGES.DAT` "number of 128-byte blocks" header field (offsets
    /// 116..122) was empty, non-numeric, or claimed fewer than one block.
    ///
    /// The count is inclusive of the header block, so a valid message always
    /// reports at least `1`.
    #[error("invalid block-count field {field:?}: {reason}")]
    BadBlockCount {
        /// The raw six-byte field, trimmed and Latin-1 decoded.
        field: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A byte stream whose length is not a whole number of fixed-size records.
    ///
    /// Emitted by the `.NDX` decoder (5-byte records); `record_len` is the
    /// record size and `remainder` the leftover byte count.
    #[error(
        "stream is not a whole number of {record_len}-byte records: {remainder} trailing byte(s)"
    )]
    PartialRecord {
        /// The fixed record length the decoder expected.
        record_len: usize,
        /// Leftover bytes after the last whole record.
        remainder: usize,
    },

    /// `CONTROL.DAT` ended before a required line was present.
    #[error("CONTROL.DAT ended early: missing {field}")]
    ControlTruncated {
        /// Human-readable name of the missing line.
        field: &'static str,
    },

    /// A `CONTROL.DAT` line that must be numeric could not be parsed.
    #[error("CONTROL.DAT has a malformed {field}: {value:?}")]
    ControlNotNumeric {
        /// Human-readable name of the offending line.
        field: &'static str,
        /// The raw line contents.
        value: String,
    },
}
