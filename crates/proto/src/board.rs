//! Message bases / boards (family 4, Wave 3.1).
//!
//! Boards form a tree (category → bundle → board) with dotted slugs. Posts
//! are append-only; the wire carries a projected view (`PostView`) while
//! the server holds the signed event. Edits and tombstones are follow-up
//! actions, surfaced here as flags on the view.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// A node in the board tree.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardInfo {
    pub slug: String,
    pub title: String,
    pub description: String,
    /// 0 category, 1 bundle, 2 board (only boards hold posts).
    pub kind: u8,
    pub parent_slug: Option<String>,
    /// Posts newer than the caller's read mark.
    pub unread: u64,
}

impl BoardInfo {
    pub fn new(slug: impl Into<String>, title: impl Into<String>, kind: u8) -> Self {
        Self {
            slug: slug.into(),
            title: title.into(),
            description: String::new(),
            kind,
            parent_slug: None,
            unread: 0,
        }
    }
}

/// A projected post for display.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostView {
    pub id: [u8; 32],
    pub board: String,
    pub root: Option<[u8; 32]>,
    pub parent: Option<[u8; 32]>,
    pub author: String,
    pub subject: String,
    pub body: String,
    pub mime: String,
    pub created_at_unix_ms: i64,
    pub edited: bool,
    pub tombstoned: bool,
}

impl PostView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: [u8; 32],
        board: impl Into<String>,
        root: Option<[u8; 32]>,
        parent: Option<[u8; 32]>,
        author: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
        mime: impl Into<String>,
        created_at_unix_ms: i64,
        edited: bool,
        tombstoned: bool,
    ) -> Self {
        Self {
            id,
            board: board.into(),
            root,
            parent,
            author: author.into(),
            subject: subject.into(),
            body: body.into(),
            mime: mime.into(),
            created_at_unix_ms,
            edited,
            tombstoned,
        }
    }
}

/// List the board tree with per-board unread counts. → [`BoardList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BoardListRequest;

impl Message for BoardListRequest {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 1;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BoardList {
    pub boards: Vec<BoardInfo>,
}

impl BoardList {
    pub fn new(boards: Vec<BoardInfo>) -> Self {
        Self { boards }
    }
}

impl Message for BoardList {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 2;
}

/// List top-level threads in a board (newest activity first). → [`ThreadList`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadListRequest {
    pub board: String,
    pub limit: u32,
}

impl ThreadListRequest {
    pub fn new(board: impl Into<String>, limit: u32) -> Self {
        Self {
            board: board.into(),
            limit,
        }
    }
}

impl Message for ThreadListRequest {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 3;
}

/// A thread summary: its root post + reply count + latest activity.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub root: PostView,
    pub replies: u64,
    pub last_activity_unix_ms: i64,
}

impl ThreadSummary {
    pub fn new(root: PostView, replies: u64, last_activity_unix_ms: i64) -> Self {
        Self {
            root,
            replies,
            last_activity_unix_ms,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThreadList {
    pub threads: Vec<ThreadSummary>,
}

impl ThreadList {
    pub fn new(threads: Vec<ThreadSummary>) -> Self {
        Self { threads }
    }
}

impl Message for ThreadList {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 4;
}

/// Fetch a full thread (root + descendants, oldest first). → [`ThreadPosts`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRequest {
    pub root: [u8; 32],
    pub limit: u32,
}

impl ThreadRequest {
    pub fn new(root: [u8; 32], limit: u32) -> Self {
        Self { root, limit }
    }
}

impl Message for ThreadRequest {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 5;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThreadPosts {
    pub posts: Vec<PostView>,
}

impl ThreadPosts {
    pub fn new(posts: Vec<PostView>) -> Self {
        Self { posts }
    }
}

impl Message for ThreadPosts {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 6;
}

/// Post to a board (or reply when `parent` is set). Requires BOARD_POST.
/// → [`PostReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostCreate {
    pub board: String,
    pub parent: Option<[u8; 32]>,
    pub subject: String,
    pub body: String,
    pub mime: String,
}

impl PostCreate {
    pub fn new(
        board: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            board: board.into(),
            parent: None,
            subject: subject.into(),
            body: body.into(),
            mime: "text/plain".into(),
        }
    }

    pub fn reply_to(mut self, parent: [u8; 32]) -> Self {
        self.parent = Some(parent);
        self
    }
}

impl Message for PostCreate {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 7;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostReply {
    pub post: PostView,
}

impl PostReply {
    pub fn new(post: PostView) -> Self {
        Self { post }
    }
}

impl Message for PostReply {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 8;
}

/// Edit a post (author, or BOARD_MODERATE). → [`PostReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostEdit {
    pub target: [u8; 32],
    pub subject: String,
    pub body: String,
    pub mime: String,
}

impl PostEdit {
    pub fn new(
        target: [u8; 32],
        subject: impl Into<String>,
        body: impl Into<String>,
        mime: impl Into<String>,
    ) -> Self {
        Self {
            target,
            subject: subject.into(),
            body: body.into(),
            mime: mime.into(),
        }
    }
}

impl Message for PostEdit {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 9;
}

/// Retract a post (author, or BOARD_MODERATE). → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostDelete {
    pub target: [u8; 32],
}

impl PostDelete {
    pub fn new(target: [u8; 32]) -> Self {
        Self { target }
    }
}

impl Message for PostDelete {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 10;
}

/// Advance the read mark for a board. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkRead {
    pub board: String,
    /// Highest post timestamp read (unix ms); 0 = "now".
    pub up_to_unix_ms: i64,
}

impl MarkRead {
    pub fn new(board: impl Into<String>, up_to_unix_ms: i64) -> Self {
        Self {
            board: board.into(),
            up_to_unix_ms,
        }
    }
}

impl Message for MarkRead {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 11;
}

/// Admin: create a board-tree node. Requires BOARD_MODERATE. → [`BoardInfo`]
/// via [`BoardCreated`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardCreate {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub kind: u8,
    pub parent_slug: Option<String>,
    pub max_threads: u32,
}

impl BoardCreate {
    pub fn new(slug: impl Into<String>, title: impl Into<String>, kind: u8) -> Self {
        Self {
            slug: slug.into(),
            title: title.into(),
            description: String::new(),
            kind,
            parent_slug: None,
            max_threads: 0,
        }
    }
}

impl Message for BoardCreate {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 12;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardCreated {
    pub board: BoardInfo,
}

impl BoardCreated {
    pub fn new(board: BoardInfo) -> Self {
        Self { board }
    }
}

impl Message for BoardCreated {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 13;
}

/// Push: a new post landed in a board (id + board; clients refetch as
/// needed). Delivered to every session so unread counts stay live.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostPosted {
    pub board: String,
    pub id: [u8; 32],
    pub root: Option<[u8; 32]>,
}

impl PostPosted {
    pub fn new(board: impl Into<String>, id: [u8; 32], root: Option<[u8; 32]>) -> Self {
        Self {
            board: board.into(),
            id,
            root,
        }
    }
}

impl Message for PostPosted {
    const FAMILY: Family = Family::BOARD;
    const MESSAGE_TYPE: u16 = 14;
}
