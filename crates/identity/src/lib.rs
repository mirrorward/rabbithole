//! Identity and authentication primitives for RabbitHole.
//!
//! - [`keys`]: Ed25519 identity keys — the portable root of user and server
//!   identity across federation (PLAN §7).
//! - [`password`]: Argon2id hashing with the OWASP profile, PHC-encoded,
//!   with transparent rehash-on-login when parameters are raised.
//! - [`token`]: opaque, high-entropy session tokens (server-stored,
//!   revocable — deliberately not JWTs).
//! - [`totp`]: RFC 6238 TOTP enrollment/verification + hashed recovery codes.
//!
//! Nothing in this crate does I/O; persistence and transport live elsewhere.

#![forbid(unsafe_code)]

pub mod keys;
pub mod password;
pub mod token;
pub mod totp;

pub use keys::{IdentityKey, PublicKey, Signature};
pub use password::{hash_password, needs_rehash, verify_password, PasswordError};
pub use token::SessionToken;
