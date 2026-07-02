//! Presence family (1): who's online and roster pushes.
//!
//! Wave 1 scope: the connected-session list. Buddy lists, away states, and
//! Cheshire mode land in Wave 2 on the same family.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

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
        }
    }
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
