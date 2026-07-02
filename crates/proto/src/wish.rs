//! The Wishing Well — request system (family 10, Wave 3.2).
//!
//! Members post wishes (wanted files, boards, features), vote on them, and
//! privileged users claim and fulfill them.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// A wish as the wire sees it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WishView {
    pub id: i64,
    /// 0 file, 1 board, 2 feature, 3 other.
    pub kind: u8,
    pub title: String,
    pub details: String,
    pub requester: String,
    /// 0 open, 1 claimed, 2 fulfilled, 3 declined.
    pub status: u8,
    pub claimed_by: Option<String>,
    pub fulfillment: Option<String>,
    pub votes: u64,
    pub created_at_unix: i64,
}

impl WishView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: i64,
        kind: u8,
        title: impl Into<String>,
        details: impl Into<String>,
        requester: impl Into<String>,
        status: u8,
        claimed_by: Option<String>,
        fulfillment: Option<String>,
        votes: u64,
        created_at_unix: i64,
    ) -> Self {
        Self {
            id,
            kind,
            title: title.into(),
            details: details.into(),
            requester: requester.into(),
            status,
            claimed_by,
            fulfillment,
            votes,
            created_at_unix,
        }
    }
}

/// List wishes. `status` None = all. → [`WishList`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WishListRequest {
    pub status: Option<u8>,
    pub limit: u32,
}

impl WishListRequest {
    pub fn new(status: Option<u8>, limit: u32) -> Self {
        Self { status, limit }
    }
}

impl Message for WishListRequest {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 1;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WishList {
    pub wishes: Vec<WishView>,
}

impl WishList {
    pub fn new(wishes: Vec<WishView>) -> Self {
        Self { wishes }
    }
}

impl Message for WishList {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 2;
}

/// Make a wish. → [`WishReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WishCreate {
    pub kind: u8,
    pub title: String,
    pub details: String,
}

impl WishCreate {
    pub fn new(kind: u8, title: impl Into<String>, details: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            details: details.into(),
        }
    }
}

impl Message for WishCreate {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 3;
}

/// Toggle your vote on a wish. → [`WishReply`] (updated counts).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WishVote {
    pub id: i64,
}

impl WishVote {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Message for WishVote {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 4;
}

/// Change a wish's status (claim/fulfill/decline). Claiming requires
/// FILE_UPLOAD; declining another's wish requires BOARD_MODERATE; the
/// requester may decline (withdraw) their own. → [`WishReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WishSetStatus {
    pub id: i64,
    pub status: u8,
    /// Fulfillment link/note (for status = fulfilled).
    pub fulfillment: Option<String>,
}

impl WishSetStatus {
    pub fn new(id: i64, status: u8) -> Self {
        Self {
            id,
            status,
            fulfillment: None,
        }
    }

    pub fn with_fulfillment(mut self, note: impl Into<String>) -> Self {
        self.fulfillment = Some(note.into());
        self
    }
}

impl Message for WishSetStatus {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 5;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WishReply {
    pub wish: WishView,
}

impl WishReply {
    pub fn new(wish: WishView) -> Self {
        Self { wish }
    }
}

impl Message for WishReply {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 6;
}

/// Push: a wish you requested changed status (claimed/fulfilled/declined).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WishUpdated {
    pub wish: WishView,
}

impl WishUpdated {
    pub fn new(wish: WishView) -> Self {
        Self { wish }
    }
}

impl Message for WishUpdated {
    const FAMILY: Family = Family::WISHING_WELL;
    const MESSAGE_TYPE: u16 = 7;
}
