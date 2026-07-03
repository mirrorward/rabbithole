//! E2EE prekey bundles (family 3 / DM, Wave 13).
//!
//! The key-distribution surface for opt-in end-to-end encrypted 1:1 DMs. A
//! client publishes its **public** X3DH key material once (identity key, a
//! signed prekey with an authenticating signature, and a batch of one-time
//! prekeys); a peer that wants to open an encrypted conversation fetches that
//! bundle, and the server **atomically consumes** one one-time prekey per fetch
//! (falling back to none once the batch is exhausted).
//!
//! These messages carry only public keys — the server never sees a private key
//! or any plaintext. They live in the DM family (their sole purpose is to boot
//! an encrypted DM session) on the previously-free type numbers 10–12, after
//! the plaintext DM messages 1–9 in [`crate::dm`].
//!
//! `identity_key` is the X25519 key mixed into the [X3DH] handshake;
//! `signing_key` is the Ed25519 verifying key used to check `signed_prekey_sig`
//! (an X25519 key cannot sign, so authenticity of the signed prekey rides a
//! dedicated signing key published alongside the identity).
//!
//! [X3DH]: https://signal.org/docs/specifications/x3dh/

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// Publish (or replace) this account's E2EE prekey bundle. → empty ack.
///
/// Republishing overwrites the stored identity/signed prekey and replaces the
/// account's one-time-prekey pool with `one_time_prekeys`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBundlePublish {
    /// X25519 identity public key (used directly in X3DH).
    pub identity_key: [u8; 32],
    /// Ed25519 verifying key that authenticates `signed_prekey_sig`.
    pub signing_key: [u8; 32],
    /// X25519 signed prekey public key.
    pub signed_prekey: [u8; 32],
    /// Ed25519 signature (64 bytes) over `signed_prekey`, by `signing_key`.
    pub signed_prekey_sig: Vec<u8>,
    /// A batch of X25519 one-time prekey public keys the server hands out one at
    /// a time. May be empty.
    pub one_time_prekeys: Vec<[u8; 32]>,
}

impl KeyBundlePublish {
    /// Construct a publish message.
    pub fn new(
        identity_key: [u8; 32],
        signing_key: [u8; 32],
        signed_prekey: [u8; 32],
        signed_prekey_sig: Vec<u8>,
        one_time_prekeys: Vec<[u8; 32]>,
    ) -> Self {
        Self {
            identity_key,
            signing_key,
            signed_prekey,
            signed_prekey_sig,
            one_time_prekeys,
        }
    }
}

impl Message for KeyBundlePublish {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 10;
}

/// Fetch `screen_name`'s prekey bundle to start an encrypted session.
/// → [`KeyBundle`] or `NotFound` (no such persona / no published bundle).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBundleRequest {
    /// The persona screen name whose bundle is wanted.
    pub screen_name: String,
}

impl KeyBundleRequest {
    /// Request the bundle for `screen_name`.
    pub fn new(screen_name: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
        }
    }
}

impl Message for KeyBundleRequest {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 11;
}

/// A fetched prekey bundle. `one_time_prekey` is the single OTP consumed for
/// this fetch, or `None` once the account's pool is exhausted (the initiator's
/// X3DH-lite handshake proceeds without it).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBundle {
    /// X25519 identity public key.
    pub identity_key: [u8; 32],
    /// Ed25519 verifying key that authenticates `signed_prekey_sig`.
    pub signing_key: [u8; 32],
    /// X25519 signed prekey public key.
    pub signed_prekey: [u8; 32],
    /// Ed25519 signature (64 bytes) over `signed_prekey`.
    pub signed_prekey_sig: Vec<u8>,
    /// One consumed one-time prekey, or `None` when the pool is empty.
    pub one_time_prekey: Option<[u8; 32]>,
}

impl KeyBundle {
    /// Construct a fetched bundle.
    pub fn new(
        identity_key: [u8; 32],
        signing_key: [u8; 32],
        signed_prekey: [u8; 32],
        signed_prekey_sig: Vec<u8>,
        one_time_prekey: Option<[u8; 32]>,
    ) -> Self {
        Self {
            identity_key,
            signing_key,
            signed_prekey,
            signed_prekey_sig,
            one_time_prekey,
        }
    }
}

impl Message for KeyBundle {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 12;
}
