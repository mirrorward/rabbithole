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

// ---- Rooms (Wave 2.2) ----------------------------------------------------

/// A room as the wire sees it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub name: String,
    pub category: String,
    pub topic: String,
    pub private: bool,
    pub member_count: u32,
    pub created_by: String,
}

impl RoomInfo {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            category: String::new(),
            topic: String::new(),
            private: false,
            member_count: 0,
            created_by: String::new(),
        }
    }
}

/// List rooms: public ones plus private rooms you belong to. → [`RoomList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RoomListRequest;

impl Message for RoomListRequest {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 10;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RoomList {
    pub rooms: Vec<RoomInfo>,
}

impl RoomList {
    pub fn new(rooms: Vec<RoomInfo>) -> Self {
        Self { rooms }
    }
}

impl Message for RoomList {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 11;
}

/// Create a room (requires CHAT_CREATE_ROOM; you join it on creation).
/// → [`RoomInfoReply`] or `AlreadyExists`. Ad-hoc private rooms vanish
/// when the last member leaves; the lobby is permanent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomCreate {
    pub name: String,
    pub category: String,
    pub topic: String,
    pub private: bool,
}

impl RoomCreate {
    pub fn new(name: impl Into<String>, private: bool) -> Self {
        Self {
            name: name.into(),
            category: String::new(),
            topic: String::new(),
            private,
        }
    }
}

impl Message for RoomCreate {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 12;
}

/// Join a room. → [`RoomInfoReply`]; `Forbidden` for uninvited private
/// rooms or bans; `NotFound` otherwise.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomJoin {
    pub room: String,
}

impl RoomJoin {
    pub fn new(room: impl Into<String>) -> Self {
        Self { room: room.into() }
    }
}

impl Message for RoomJoin {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 13;
}

/// Leave a room. → empty ack. (You can't leave the lobby.)
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomLeave {
    pub room: String,
}

impl RoomLeave {
    pub fn new(room: impl Into<String>) -> Self {
        Self { room: room.into() }
    }
}

impl Message for RoomLeave {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 14;
}

/// Invite someone to a room you belong to. → empty ack; they receive a
/// [`RoomInvited`] push and may then join even if the room is private.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInvite {
    pub room: String,
    pub screen_name: String,
}

impl RoomInvite {
    pub fn new(room: impl Into<String>, screen_name: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            screen_name: screen_name.into(),
        }
    }
}

impl Message for RoomInvite {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 15;
}

/// Push: you were invited to a room.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInvited {
    pub room: String,
    pub from: String,
}

impl RoomInvited {
    pub fn new(room: impl Into<String>, from: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            from: from.into(),
        }
    }
}

impl Message for RoomInvited {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 16;
}

/// Set a room's topic (creator or CHAT_MODERATE). → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomTopicSet {
    pub room: String,
    pub topic: String,
}

impl RoomTopicSet {
    pub fn new(room: impl Into<String>, topic: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            topic: topic.into(),
        }
    }
}

impl Message for RoomTopicSet {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 17;
}

/// Kick someone from a room (creator or CHAT_MODERATE), optionally
/// banning them from rejoining. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomKick {
    pub room: String,
    pub screen_name: String,
    pub ban: bool,
}

impl RoomKick {
    pub fn new(room: impl Into<String>, screen_name: impl Into<String>, ban: bool) -> Self {
        Self {
            room: room.into(),
            screen_name: screen_name.into(),
            ban,
        }
    }
}

impl Message for RoomKick {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 18;
}

/// Reply carrying a single room.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfoReply {
    pub room: RoomInfo,
}

impl RoomInfoReply {
    pub fn new(room: RoomInfo) -> Self {
        Self { room }
    }
}

impl Message for RoomInfoReply {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 19;
}

/// Push: you were kicked from a room.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomKicked {
    pub room: String,
    pub banned: bool,
}

impl RoomKicked {
    pub fn new(room: impl Into<String>, banned: bool) -> Self {
        Self {
            room: room.into(),
            banned,
        }
    }
}

impl Message for RoomKicked {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 20;
}

/// List a room's members. → [`RoomMemberList`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMembersRequest {
    pub room: String,
}

impl RoomMembersRequest {
    pub fn new(room: impl Into<String>) -> Self {
        Self { room: room.into() }
    }
}

impl Message for RoomMembersRequest {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 21;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RoomMemberList {
    pub members: Vec<String>,
}

impl RoomMemberList {
    pub fn new(members: Vec<String>) -> Self {
        Self { members }
    }
}

impl Message for RoomMemberList {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 22;
}

// ---- Moderation: mute + slow-mode (Wave 13) --------------------------------

/// Mute a member in a room (creator or CHAT_MODERATE; room creators can't
/// be muted). A muted member stays in the room and keeps receiving events,
/// but their sends are refused with `Muted`. `duration_secs = None` is
/// permanent (until unmuted or the room is reaped); expiry is lazy.
/// → empty ack; room members get a [`RoomMuted`] push.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMute {
    pub room: String,
    pub screen_name: String,
    /// `None` = permanent; `Some(secs)` expires after that many seconds.
    pub duration_secs: Option<u32>,
}

impl RoomMute {
    pub fn new(
        room: impl Into<String>,
        screen_name: impl Into<String>,
        duration_secs: Option<u32>,
    ) -> Self {
        Self {
            room: room.into(),
            screen_name: screen_name.into(),
            duration_secs,
        }
    }
}

impl Message for RoomMute {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 23;
}

/// Lift a mute (creator or CHAT_MODERATE). → empty ack (`NotFound` when the
/// target wasn't muted); room members get a [`RoomMuted`] push with
/// `muted = false`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomUnmute {
    pub room: String,
    pub screen_name: String,
}

impl RoomUnmute {
    pub fn new(room: impl Into<String>, screen_name: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            screen_name: screen_name.into(),
        }
    }
}

impl Message for RoomUnmute {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 24;
}

/// Set a room's slow-mode interval: the between-message minimum per member
/// (creator and CHAT_MODERATE holders are exempt). `0` turns it off; values
/// above the server cap (3600) are clamped. Sends inside the window are
/// refused with `SlowMode { retry_after_secs }`. → empty ack; room members
/// get a [`RoomSlowModeChanged`] push carrying the applied value.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSlowMode {
    pub room: String,
    pub seconds: u32,
}

impl RoomSlowMode {
    pub fn new(room: impl Into<String>, seconds: u32) -> Self {
        Self {
            room: room.into(),
            seconds,
        }
    }
}

impl Message for RoomSlowMode {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 25;
}

/// Push to room members: someone was muted (`muted = true`, with the mute's
/// duration) or unmuted (`muted = false`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMuted {
    pub room: String,
    pub screen_name: String,
    pub muted: bool,
    /// Meaningful only when `muted`; `None` = permanent.
    pub duration_secs: Option<u32>,
}

impl RoomMuted {
    pub fn new(
        room: impl Into<String>,
        screen_name: impl Into<String>,
        muted: bool,
        duration_secs: Option<u32>,
    ) -> Self {
        Self {
            room: room.into(),
            screen_name: screen_name.into(),
            muted,
            duration_secs,
        }
    }
}

impl Message for RoomMuted {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 26;
}

/// Push to room members: the slow-mode interval changed (`0` = off).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSlowModeChanged {
    pub room: String,
    pub seconds: u32,
    pub by: String,
}

impl RoomSlowModeChanged {
    pub fn new(room: impl Into<String>, seconds: u32, by: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            seconds,
            by: by.into(),
        }
    }
}

impl Message for RoomSlowModeChanged {
    const FAMILY: Family = Family::CHAT;
    const MESSAGE_TYPE: u16 = 27;
}
