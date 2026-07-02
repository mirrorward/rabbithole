//! Chat family (2): rooms.
//!
//! Wave 1 scope: the single public lobby (room name `"lobby"`) with an
//! in-memory scrollback. Multiple/ad-hoc/private rooms arrive in Wave 2 on
//! the same messages.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// Say something in a room. → empty ack, then the line comes back to all
/// members (including the sender) as a [`ChatMessage`] push.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSend {
    pub room: String,
    pub text: String,
}

impl ChatSend {
    pub fn new(room: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            text: text.into(),
        }
    }
}

impl Message for ChatSend {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 1;
}

/// Push: a chat line.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub room: String,
    pub from: String,
    pub text: String,
    /// Server timestamp, unix milliseconds.
    pub at_unix_ms: i64,
}

impl ChatMessage {
    pub fn new(
        room: impl Into<String>,
        from: impl Into<String>,
        text: impl Into<String>,
        at_unix_ms: i64,
    ) -> Self {
        Self {
            room: room.into(),
            from: from.into(),
            text: text.into(),
            at_unix_ms,
        }
    }
}

impl Message for ChatMessage {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 2;
}

/// Fetch recent scrollback for a room. → [`ChatHistory`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryRequest {
    pub room: String,
    /// Maximum lines, newest last; the server may return fewer.
    pub limit: u32,
}

impl ChatHistoryRequest {
    pub fn new(room: impl Into<String>, limit: u32) -> Self {
        Self {
            room: room.into(),
            limit,
        }
    }
}

impl Message for ChatHistoryRequest {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 3;
}

/// Reply to [`ChatHistoryRequest`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatHistory {
    pub messages: Vec<ChatMessage>,
}

impl ChatHistory {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self { messages }
    }
}

impl Message for ChatHistory {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 4;
}
