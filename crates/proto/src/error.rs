//! Protocol-level error codes and codec errors.

use serde::{Deserialize, Serialize};

/// Application error codes carried in reply frames.
///
/// `#[non_exhaustive]`: new codes may be added without a protocol version
/// bump; unknown codes decode as [`ErrorCode::Other`] via the explicit
/// `Other(u16)` escape hatch rather than failing.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Malformed or semantically invalid request.
    BadRequest,
    /// Authentication required or failed.
    Unauthenticated,
    /// Authenticated but not permitted (capability bit / ACL denied).
    Forbidden,
    /// Referenced object does not exist (or is hidden from this principal).
    NotFound,
    /// Something with this identity already exists.
    AlreadyExists,
    /// Rate limit exceeded; retry later.
    RateLimited,
    /// Server-side failure.
    Internal,
    /// The peer speaks a protocol version we cannot serve.
    VersionMismatch,
    /// Feature not enabled or not supported by this server.
    Unsupported,
    /// Payload exceeded a configured limit (e.g. attachment max size).
    TooLarge,
    /// Session expired or was revoked.
    SessionExpired,
    /// The server is shutting down or the surface is disabled.
    Unavailable,
    /// Escape hatch for codes this build doesn't know.
    Other(u16),
}

/// Errors produced while encoding/decoding frames.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("frame exceeds maximum size ({size} > {max})")]
    FrameTooLarge { size: usize, max: usize },
    #[error("truncated frame: need {needed} more bytes")]
    Truncated { needed: usize },
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
}
