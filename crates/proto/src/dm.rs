//! Direct messages (family 3, Wave 2.2).
//!
//! 1:1 threads keyed by persona screen names, persisted server-side with
//! offline delivery. Attachments are blob refs (uploaded first via
//! `BlobPut`). Read receipts are per-account opt-out. Away users generate
//! a one-shot auto-response (the Hotline tradition).

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// The X3DH-lite prologue an initiator attaches to the **first** encrypted
/// message so the responder can derive the same shared secret without a prior
/// exchange. Absent on every subsequent (already-ratcheting) message.
///
/// All fields are public X25519 keys — the server relays them opaquely.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrekeyPrologue {
    /// The initiator's X25519 identity public key (`IK_A`).
    pub identity_key: [u8; 32],
    /// The initiator's fresh X25519 ephemeral public key (`EK_A`).
    pub ephemeral_key: [u8; 32],
    /// The one-time prekey consumed from the responder's bundle, if any. Carried
    /// for forward-compatibility; the X3DH-lite (3-DH) handshake does not fold it
    /// into the shared secret, so its absence never blocks session setup.
    pub one_time_prekey: Option<[u8; 32]>,
}

impl PrekeyPrologue {
    /// Construct a prologue.
    pub fn new(
        identity_key: [u8; 32],
        ephemeral_key: [u8; 32],
        one_time_prekey: Option<[u8; 32]>,
    ) -> Self {
        Self {
            identity_key,
            ephemeral_key,
            one_time_prekey,
        }
    }
}

/// The opaque E2EE carriage for one DM: the Double Ratchet header, the AEAD
/// ciphertext, and (first message only) the X3DH prologue.
///
/// The server stores and relays this verbatim; it never decodes the ratchet
/// header or decrypts the ciphertext. Mirrors what the `rabbithole-e2ee`
/// ratchet `Message` serializes to (`header` is the postcard-encoded ratchet
/// `Header`; `ciphertext` is the ChaCha20-Poly1305 output with its tag).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedPayload {
    /// X3DH prologue, present only on the session-establishing first message.
    pub prekey: Option<PrekeyPrologue>,
    /// postcard-serialized ratchet `Header` (opaque to the server).
    pub header: Vec<u8>,
    /// AEAD ciphertext with appended tag (opaque to the server).
    pub ciphertext: Vec<u8>,
}

impl EncryptedPayload {
    /// Construct an encrypted payload.
    pub fn new(prekey: Option<PrekeyPrologue>, header: Vec<u8>, ciphertext: Vec<u8>) -> Self {
        Self {
            prekey,
            header,
            ciphertext,
        }
    }
}

/// One direct message on the wire.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmMessage {
    pub id: i64,
    pub from: String,
    pub to: String,
    pub text: String,
    /// Message id being quoted, if any.
    pub quote_of: Option<i64>,
    /// blake3 blob ids fetched via `BlobGet`.
    pub attachments: Vec<[u8; 32]>,
    pub at_unix_ms: i64,
    /// True for server-generated away auto-responses.
    pub is_auto: bool,
    /// Opt-in E2EE carriage. `Some` = end-to-end encrypted (then `text` is
    /// empty and the server holds only opaque ciphertext); `None` = the
    /// unchanged plaintext path. Serde-additive: defaults to `None`.
    #[serde(default)]
    pub encrypted: Option<EncryptedPayload>,
}

impl DmMessage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: i64,
        from: impl Into<String>,
        to: impl Into<String>,
        text: impl Into<String>,
        quote_of: Option<i64>,
        attachments: Vec<[u8; 32]>,
        at_unix_ms: i64,
        is_auto: bool,
    ) -> Self {
        Self {
            id,
            from: from.into(),
            to: to.into(),
            text: text.into(),
            quote_of,
            attachments,
            at_unix_ms,
            is_auto,
            encrypted: None,
        }
    }

    /// Attach an E2EE payload (builder). The plaintext `text` should be empty
    /// on an encrypted message.
    pub fn with_encrypted(mut self, encrypted: EncryptedPayload) -> Self {
        self.encrypted = Some(encrypted);
        self
    }
}

/// Send a DM. → [`DmSent`] or `NotFound` (no such persona) /
/// `Forbidden` (blocked, or you lack DM_SEND).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmSend {
    pub to: String,
    pub text: String,
    pub quote_of: Option<i64>,
    pub attachments: Vec<[u8; 32]>,
    /// Opt-in E2EE carriage. `Some` = the server stores/relays the ciphertext
    /// opaquely and ignores `text`; `None` = the unchanged plaintext path.
    /// Serde-additive: defaults to `None`.
    #[serde(default)]
    pub encrypted: Option<EncryptedPayload>,
}

impl DmSend {
    pub fn new(to: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            to: to.into(),
            text: text.into(),
            quote_of: None,
            attachments: Vec::new(),
            encrypted: None,
        }
    }

    /// Build an end-to-end encrypted send to `to`, carrying `encrypted` and no
    /// plaintext.
    pub fn new_encrypted(to: impl Into<String>, encrypted: EncryptedPayload) -> Self {
        Self {
            to: to.into(),
            text: String::new(),
            quote_of: None,
            attachments: Vec::new(),
            encrypted: Some(encrypted),
        }
    }
}

impl Message for DmSend {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 1;
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmSent {
    pub id: i64,
    pub at_unix_ms: i64,
}

impl DmSent {
    pub fn new(id: i64, at_unix_ms: i64) -> Self {
        Self { id, at_unix_ms }
    }
}

impl Message for DmSent {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 2;
}

/// Push: a DM arrived (live, or on login for offline-queued mail).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmReceived {
    pub message: DmMessage,
}

impl DmReceived {
    pub fn new(message: DmMessage) -> Self {
        Self { message }
    }
}

impl Message for DmReceived {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 3;
}

/// Page through a thread (messages with `with`), newest last.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmHistoryRequest {
    pub with: String,
    /// Return messages with id < before (0 = from the newest).
    pub before_id: i64,
    pub limit: u32,
}

impl DmHistoryRequest {
    pub fn new(with: impl Into<String>, before_id: i64, limit: u32) -> Self {
        Self {
            with: with.into(),
            before_id,
            limit,
        }
    }
}

impl Message for DmHistoryRequest {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 4;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DmHistory {
    pub messages: Vec<DmMessage>,
}

impl DmHistory {
    pub fn new(messages: Vec<DmMessage>) -> Self {
        Self { messages }
    }
}

impl Message for DmHistory {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 5;
}

/// List my conversations with unread counts. → [`DmThreads`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DmThreadsRequest;

impl Message for DmThreadsRequest {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 6;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmThreadSummary {
    pub with: String,
    pub last_text: String,
    pub last_at_unix_ms: i64,
    pub unread: u64,
}

impl DmThreadSummary {
    pub fn new(
        with: impl Into<String>,
        last_text: impl Into<String>,
        last_at_unix_ms: i64,
        unread: u64,
    ) -> Self {
        Self {
            with: with.into(),
            last_text: last_text.into(),
            last_at_unix_ms,
            unread,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DmThreads {
    pub threads: Vec<DmThreadSummary>,
}

impl DmThreads {
    pub fn new(threads: Vec<DmThreadSummary>) -> Self {
        Self { threads }
    }
}

impl Message for DmThreads {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 7;
}

/// Mark a thread read up to a message id. → empty ack; the other side
/// gets a [`DmReadReceipt`] push if your account sends receipts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmMarkRead {
    pub with: String,
    pub up_to_id: i64,
}

impl DmMarkRead {
    pub fn new(with: impl Into<String>, up_to_id: i64) -> Self {
        Self {
            with: with.into(),
            up_to_id,
        }
    }
}

impl Message for DmMarkRead {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 8;
}

/// Push: your messages up to `up_to_id` were read by `by`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmReadReceipt {
    pub by: String,
    pub up_to_id: i64,
}

impl DmReadReceipt {
    pub fn new(by: impl Into<String>, up_to_id: i64) -> Self {
        Self {
            by: by.into(),
            up_to_id,
        }
    }
}

impl Message for DmReadReceipt {
    const FAMILY: Family = Family::DM;
    const MESSAGE_TYPE: u16 = 9;
}
