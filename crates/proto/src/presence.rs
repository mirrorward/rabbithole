//! Presence family (1): who's online and roster pushes.
//!
//! Wave 1 scope: the connected-session list. Buddy lists, away states, and
//! Cheshire mode land in Wave 2 on the same family.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// Presence states. `Invisible` is **Cheshire mode**: connected but shown
/// as offline to everyone below moderator.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PresenceState {
    Online,
    /// Away, optionally with a status message (the beloved AOL ritual).
    Away,
    /// Auto-set by idle clients; cleared by activity.
    Idle,
    /// Cheshire mode.
    Invisible,
}

impl PresenceState {
    pub fn from_ordinal(n: u8) -> PresenceState {
        match n {
            1 => PresenceState::Away,
            2 => PresenceState::Idle,
            3 => PresenceState::Invisible,
            _ => PresenceState::Online,
        }
    }
}

/// One visible session in the who-list.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSummary {
    pub session_id: u64,
    pub screen_name: String,
    /// Role ordinal (see [`crate::session::AuthOk::role`]).
    pub role: u8,
    /// Which door they came in through: "quic", "websocket", later
    /// "telnet", "hotline", …
    pub transport: String,
    pub connected_secs: u64,
    pub state: PresenceState,
    /// Away/status message, when set.
    pub status: Option<String>,
}

impl UserSummary {
    pub fn new(
        session_id: u64,
        screen_name: impl Into<String>,
        role: u8,
        transport: impl Into<String>,
        connected_secs: u64,
    ) -> Self {
        Self {
            session_id,
            screen_name: screen_name.into(),
            role,
            transport: transport.into(),
            connected_secs,
            state: PresenceState::Online,
            status: None,
        }
    }

    pub fn with_state(mut self, state: PresenceState, status: Option<String>) -> Self {
        self.state = state;
        self.status = status;
        self
    }
}

/// Set my presence state (away message ≤ 200 chars). → empty ack.
/// Broadcast to others as a `UserChanged` push; going `Invisible` emits
/// `UserLeft` to non-moderators, coming back emits `UserJoined`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceSet {
    pub state: PresenceState,
    pub status: Option<String>,
}

impl PresenceSet {
    pub fn new(state: PresenceState, status: Option<String>) -> Self {
        Self { state, status }
    }
}

impl Message for PresenceSet {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 20;
}

/// Fetch my buddy list + block list. → [`BuddyList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BuddyListRequest;

impl Message for BuddyListRequest {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 21;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuddyEntry {
    pub screen_name: String,
    pub group: String,
    pub online: bool,
    pub state: PresenceState,
    pub status: Option<String>,
}

impl BuddyEntry {
    pub fn new(screen_name: impl Into<String>, group: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
            group: group.into(),
            online: false,
            state: PresenceState::Online,
            status: None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BuddyList {
    pub buddies: Vec<BuddyEntry>,
    pub blocked: Vec<String>,
}

impl BuddyList {
    pub fn new(buddies: Vec<BuddyEntry>, blocked: Vec<String>) -> Self {
        Self { buddies, blocked }
    }
}

impl Message for BuddyList {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 22;
}

/// Add (or move) a buddy. → empty ack; `NotFound` if no such persona.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuddyAdd {
    pub screen_name: String,
    pub group: String,
}

impl BuddyAdd {
    pub fn new(screen_name: impl Into<String>, group: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
            group: group.into(),
        }
    }
}

impl Message for BuddyAdd {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 23;
}

/// Remove a buddy. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuddyRemove {
    pub screen_name: String,
}

impl BuddyRemove {
    pub fn new(screen_name: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
        }
    }
}

impl Message for BuddyRemove {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 24;
}

/// Block a persona's account (no DMs; they see you offline). → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockAdd {
    pub screen_name: String,
}

impl BlockAdd {
    pub fn new(screen_name: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
        }
    }
}

impl Message for BlockAdd {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 25;
}

/// Unblock. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRemove {
    pub screen_name: String,
}

impl BlockRemove {
    pub fn new(screen_name: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
        }
    }
}

impl Message for BlockRemove {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 26;
}

/// Request the who-list. → [`WhoList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Who;

impl Message for Who {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 1;
}

/// Reply to [`Who`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WhoList {
    pub users: Vec<UserSummary>,
}

impl WhoList {
    pub fn new(users: Vec<UserSummary>) -> Self {
        Self { users }
    }
}

impl Message for WhoList {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 2;
}

/// Push: a user joined.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserJoined {
    pub user: UserSummary,
}

impl UserJoined {
    pub fn new(user: UserSummary) -> Self {
        Self { user }
    }
}

impl Message for UserJoined {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 3;
}

/// Push: a user left.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserLeft {
    pub session_id: u64,
    pub screen_name: String,
}

impl UserLeft {
    pub fn new(session_id: u64, screen_name: impl Into<String>) -> Self {
        Self {
            session_id,
            screen_name: screen_name.into(),
        }
    }
}

impl Message for UserLeft {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 4;
}
