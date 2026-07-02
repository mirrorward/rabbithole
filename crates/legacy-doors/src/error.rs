//! Error type for drop-file parsing.
//!
//! Readers are *total*: they never panic on malformed or truncated input.
//! The only hard error they raise is [`Error::Empty`] (nothing to parse);
//! everything else is handled best-effort, leaving unparseable fields at their
//! [`Default`](crate::DoorContext) values.

/// Errors that can arise while decoding a drop file back into a
/// [`DoorContext`](crate::DoorContext).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The supplied buffer had no content lines at all.
    #[error("drop file is empty")]
    Empty,
}
