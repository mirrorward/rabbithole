//! End-to-end encryption core for RabbitHole private messaging (PLAN §8, Wave 13).
//!
//! This crate is the **cryptographic core only** for opt-in E2EE DMs and private
//! rooms. It contains no wire/proto types, no server wiring, and no persistence —
//! only the primitives a client needs to establish a forward-secret session and
//! encrypt/decrypt messages. It performs **no I/O** and pulls in no async runtime
//! or filesystem, so it is usable from a browser (wasm) as well as native clients.
//!
//! # Protocols
//!
//! The design follows the Signal protocol family:
//!
//! - [`keys`] — X25519 ([RFC 7748]) key agreement. [`keys::IdentityKeyPair`] is a
//!   long-term key; [`keys::PreKeyPair`] is a (semi-)ephemeral prekey published so
//!   peers can start a session asynchronously.
//! - [`x3dh`] — an **X3DH-lite** asynchronous handshake ([Signal X3DH spec]) that
//!   derives an initial shared secret from three Diffie–Hellman operations over the
//!   parties' identity and prekeys. The exact DH combination is documented on
//!   [`x3dh::initiator_shared_secret`].
//! - [`ratchet`] — the **Double Ratchet** ([Signal Double Ratchet spec]) providing
//!   forward secrecy and post-compromise security for 1:1 conversations, with
//!   out-of-order and skipped-message handling (bounded to resist DoS).
//! - [`sealed`] — a **sealed-sender** envelope: encrypt-to-a-public-key using a
//!   fresh ephemeral X25519 key so the transport never sees the sender's identity.
//!
//! # KDF choice
//!
//! To minimise the dependency surface we use **BLAKE3 in key-derivation mode**
//! ([`blake3::Hasher::new_derive_key`] / [`blake3::derive_key`]) rather than
//! HKDF-SHA256. BLAKE3's derive-key mode is a purpose-built KDF that takes an
//! application-specific, hardcoded context string providing domain separation;
//! its extendable output (XOF) lets us produce the 64 bytes a root-key step needs
//! in one pass. Every distinct KDF use in this crate has its own context string
//! (see [`kdf`]), which is exactly the domain separation HKDF's `info` parameter
//! would otherwise give us.
//!
//! # AEAD
//!
//! Message confidentiality/integrity uses ChaCha20-Poly1305 ([RFC 8439]). Because
//! every message key is unique (derived once from the ratchet), the AEAD key and
//! nonce are derived deterministically from the message key, so no nonce needs to
//! be transmitted or randomly generated. See [`aead`].
//!
//! [RFC 7748]: https://www.rfc-editor.org/rfc/rfc7748
//! [RFC 8439]: https://www.rfc-editor.org/rfc/rfc8439
//! [Signal X3DH spec]: https://signal.org/docs/specifications/x3dh/
//! [Signal Double Ratchet spec]: https://signal.org/docs/specifications/doubleratchet/

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod aead;
pub mod kdf;
pub mod keys;
pub mod ratchet;
pub mod sealed;
pub mod x3dh;

pub use keys::{IdentityKeyPair, PreKeyPair, PublicKey};
pub use ratchet::{Header, Message, Session};
pub use sealed::{sealed_open, sealed_seal, SealedEnvelope};
pub use x3dh::{initiator_shared_secret, responder_shared_secret, SharedSecret};

/// Errors produced by the E2EE core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// AEAD decryption failed: wrong key, corrupted, or tampered ciphertext.
    #[error("authentication failed (tampered or wrong key)")]
    Decrypt,
    /// The number of skipped messages exceeded the safety bound.
    ///
    /// This bounds attacker-controlled work/memory (a peer could otherwise claim
    /// a huge message number to force us to derive and store unbounded keys).
    #[error("too many skipped messages (max {max})")]
    TooManySkipped {
        /// The configured maximum.
        max: u32,
    },
    /// A message was received before the session had a receiving chain.
    #[error("session has no receiving chain yet")]
    NoReceivingChain,
    /// Tried to encrypt before the session had a sending chain.
    ///
    /// A responder cannot send until it has decrypted the initiator's first
    /// message (that is what establishes its sending chain).
    #[error("session has no sending chain yet")]
    NoSendingChain,
}

/// Convenience alias for results in this crate.
pub type Result<T> = core::result::Result<T, Error>;
