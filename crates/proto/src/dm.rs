//! Direct messages (family 3, Wave 2.2).
//!
//! 1:1 threads keyed by persona screen names, persisted server-side with
//! offline delivery. Attachments are blob refs (uploaded first via
//! `BlobPut`). Read receipts are per-account opt-out. Away users generate
//! a one-shot auto-response (the Hotline tradition).

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

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
        }
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
}

impl DmSend {
    pub fn new(to: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            to: to.into(),
            text: text.into(),
            quote_of: None,
            attachments: Vec::new(),
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
