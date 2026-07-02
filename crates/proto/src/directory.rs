//! Member directory & profile lookup (presence family additions, Wave 2).

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};
use crate::persona::{PersonaInfo, Profile};

/// Fetch a persona's public profile card. → [`ProfileCard`] or `NotFound`
/// (which is also the answer for directory-hidden personas).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileGet {
    pub screen_name: String,
}

impl ProfileGet {
    pub fn new(screen_name: impl Into<String>) -> Self {
        Self {
            screen_name: screen_name.into(),
        }
    }
}

impl Message for ProfileGet {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 10;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileCard {
    pub screen_name: String,
    pub profile: Profile,
    pub avatar: Option<[u8; 32]>,
    pub banner: Option<[u8; 32]>,
    /// Present iff the persona is online right now ("locate a member").
    pub online_transport: Option<String>,
}

impl ProfileCard {
    pub fn new(screen_name: impl Into<String>, profile: Profile) -> Self {
        Self {
            screen_name: screen_name.into(),
            profile,
            avatar: None,
            banner: None,
            online_transport: None,
        }
    }
}

impl Message for ProfileCard {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 11;
}

/// Search visible personas by name/profile substring. → [`DirectoryResults`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorySearch {
    pub query: String,
    pub limit: u32,
}

impl DirectorySearch {
    pub fn new(query: impl Into<String>, limit: u32) -> Self {
        Self {
            query: query.into(),
            limit,
        }
    }
}

impl Message for DirectorySearch {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 12;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DirectoryResults {
    pub personas: Vec<PersonaInfo>,
}

impl DirectoryResults {
    pub fn new(personas: Vec<PersonaInfo>) -> Self {
        Self { personas }
    }
}

impl Message for DirectoryResults {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 13;
}

/// Push: a user's visible identity or presence changed (persona switch,
/// away/idle state, status message).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserChanged {
    pub session_id: u64,
    pub screen_name: String,
    pub state: crate::presence::PresenceState,
    pub status: Option<String>,
}

impl UserChanged {
    pub fn new(session_id: u64, screen_name: impl Into<String>) -> Self {
        Self {
            session_id,
            screen_name: screen_name.into(),
            state: crate::presence::PresenceState::Online,
            status: None,
        }
    }

    pub fn with_state(
        mut self,
        state: crate::presence::PresenceState,
        status: Option<String>,
    ) -> Self {
        self.state = state;
        self.status = status;
        self
    }
}

impl Message for UserChanged {
    const FAMILY: Family = Family::PRESENCE;
    const MESSAGE_TYPE: u16 = 5;
}
