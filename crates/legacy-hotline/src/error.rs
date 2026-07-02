//! Errors produced while decoding/encoding the Hotline wire format.
//!
//! Every decoder in this crate is total: it returns a [`HotlineError`] rather
//! than panicking on malformed, truncated, or hostile input. This is a
//! deliberate hardening property — the codec sits directly on a socket facing
//! untrusted legacy clients, so a bad byte must never bring the server down.

/// A decode/encode failure in the Hotline codec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HotlineError {
    /// The input ended before a fixed- or declared-length field was complete.
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

    /// The 4-byte protocol identifier was not the expected magic (`TRTP`).
    #[error("bad protocol id: expected {expected:02X?}, got {got:02X?}")]
    BadProtocolId {
        /// The magic we required.
        expected: [u8; 4],
        /// What was on the wire.
        got: [u8; 4],
    },

    /// The 4-byte sub-protocol identifier was not the expected magic (`HOTL`).
    #[error("bad sub-protocol id: expected {expected:02X?}, got {got:02X?}")]
    BadSubProtocolId {
        /// The magic we required.
        expected: [u8; 4],
        /// What was on the wire.
        got: [u8; 4],
    },

    /// An integer parameter field had a byte width the reader cannot interpret.
    ///
    /// Hotline integer fields are 1, 2, or 4 bytes wide (an empty field decodes
    /// as zero). Any other declared size is rejected here.
    #[error("integer field has unsupported width {0} (expected 0, 1, 2, or 4)")]
    BadIntWidth(usize),

    /// A length field described a value larger than the enforced ceiling.
    #[error("value too large: {size} exceeds maximum {max}")]
    TooLarge {
        /// The declared/observed size.
        size: usize,
        /// The maximum this decoder accepts.
        max: usize,
    },

    /// A decoder that expected to consume its whole input found extra bytes.
    ///
    /// Emitted by strict, self-delimiting decoders (e.g. the parameter list)
    /// so that golden round-trips are exact and silent corruption is caught.
    #[error("trailing bytes after decode: {0} byte(s) remain")]
    TrailingBytes(usize),

    /// Continuation fragments of a transaction disagreed about `total_size`.
    #[error("fragment total_size mismatch for txn {id}: {first} then {next}")]
    FragmentMismatch {
        /// Transaction id the fragments claimed to belong to.
        id: u32,
        /// `total_size` seen on the first fragment.
        first: u32,
        /// `total_size` seen on the conflicting fragment.
        next: u32,
    },

    /// A fragment pushed more body bytes than the transaction's `total_size`.
    #[error("fragment overflow for txn {id}: {have} bytes exceeds total_size {total}")]
    FragmentOverflow {
        /// Transaction id.
        id: u32,
        /// Bytes accumulated so far including this fragment.
        have: usize,
        /// Declared total body size.
        total: u32,
    },
}
