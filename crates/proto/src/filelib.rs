//! File libraries (family 5, Wave 4.1).
//!
//! Areas hold a tree of folders, files, and aliases. Bytes are content-
//! addressed in the blob store; the wire carries a projected [`FileNodeView`]
//! plus, for downloads, the bytes themselves (small files ride the control
//! stream; Wave 4.2 adds dedicated streaming + resume for large transfers).
//! Small-blob transfer (avatars/banners) lives in [`crate::blob`] at type
//! 100+; this module keeps the low type numbers.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// A file library.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAreaView {
    pub slug: String,
    pub title: String,
    pub description: String,
}

impl FileAreaView {
    pub fn new(
        slug: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            slug: slug.into(),
            title: title.into(),
            description: description.into(),
        }
    }
}

/// A node in a file area's tree, projected for display.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileNodeView {
    pub id: i64,
    pub area: String,
    /// 0 folder, 1 file, 2 alias.
    pub kind: u8,
    pub name: String,
    pub path: String,
    pub is_dropbox: bool,
    pub blob_id: Option<[u8; 32]>,
    pub size: i64,
    pub mime: String,
    pub icon: String,
    pub comment: String,
    pub uploader: String,
    pub downloads: i64,
    pub rating_avg: f64,
    pub rating_count: i64,
    pub created_at_unix: i64,
}

impl FileNodeView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: i64,
        area: impl Into<String>,
        kind: u8,
        name: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            id,
            area: area.into(),
            kind,
            name: name.into(),
            path: path.into(),
            is_dropbox: false,
            blob_id: None,
            size: 0,
            mime: String::new(),
            icon: String::new(),
            comment: String::new(),
            uploader: String::new(),
            downloads: 0,
            rating_avg: 0.0,
            rating_count: 0,
            created_at_unix: 0,
        }
    }
}

/// List file areas. → [`AreaList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AreaListRequest;

impl Message for AreaListRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 1;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AreaList {
    pub areas: Vec<FileAreaView>,
}

impl AreaList {
    pub fn new(areas: Vec<FileAreaView>) -> Self {
        Self { areas }
    }
}

impl Message for AreaList {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 2;
}

/// List a folder's children (`path` None/empty = area root). → [`NodeList`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderListRequest {
    pub area: String,
    pub path: Option<String>,
}

impl FolderListRequest {
    pub fn new(area: impl Into<String>, path: Option<String>) -> Self {
        Self {
            area: area.into(),
            path,
        }
    }
}

impl Message for FolderListRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 3;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeList {
    pub nodes: Vec<FileNodeView>,
}

impl NodeList {
    pub fn new(nodes: Vec<FileNodeView>) -> Self {
        Self { nodes }
    }
}

impl Message for NodeList {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 4;
}

/// Fetch one node's metadata. → [`NodeReply`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeGet {
    pub id: i64,
}

impl NodeGet {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Message for NodeGet {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 5;
}

/// The reply carrying a single node (create/edit/rate/alias all return this).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeReply {
    pub node: FileNodeView,
}

impl NodeReply {
    pub fn new(node: FileNodeView) -> Self {
        Self { node }
    }
}

impl Message for NodeReply {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 6;
}

/// Create a library. Requires FILE_MANAGE. → [`AreaReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaCreate {
    pub slug: String,
    pub title: String,
    pub description: String,
}

impl AreaCreate {
    pub fn new(slug: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            title: title.into(),
            description: String::new(),
        }
    }

    pub fn with_description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }
}

impl Message for AreaCreate {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 7;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaReply {
    pub area: FileAreaView,
}

impl AreaReply {
    pub fn new(area: FileAreaView) -> Self {
        Self { area }
    }
}

impl Message for AreaReply {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 8;
}

/// Create a folder (`is_dropbox` = write-only). Requires FILE_MANAGE.
/// → [`NodeReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCreate {
    pub area: String,
    pub parent: Option<String>,
    pub name: String,
    pub is_dropbox: bool,
}

impl FolderCreate {
    pub fn new(area: impl Into<String>, parent: Option<String>, name: impl Into<String>) -> Self {
        Self {
            area: area.into(),
            parent,
            name: name.into(),
            is_dropbox: false,
        }
    }

    pub fn dropbox(mut self) -> Self {
        self.is_dropbox = true;
        self
    }
}

impl Message for FolderCreate {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 9;
}

/// Upload a file (bytes inline; small files only until W4.2 streaming).
/// Requires FILE_UPLOAD. → [`NodeReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileUpload {
    pub area: String,
    pub parent: Option<String>,
    pub name: String,
    pub mime: String,
    pub icon: String,
    pub comment: String,
    pub bytes: Vec<u8>,
}

impl FileUpload {
    pub fn new(
        area: impl Into<String>,
        parent: Option<String>,
        name: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            area: area.into(),
            parent,
            name: name.into(),
            mime: "application/octet-stream".into(),
            icon: String::new(),
            comment: String::new(),
            bytes,
        }
    }

    pub fn with_meta(
        mut self,
        mime: impl Into<String>,
        icon: impl Into<String>,
        comment: impl Into<String>,
    ) -> Self {
        self.mime = mime.into();
        self.icon = icon.into();
        self.comment = comment.into();
        self
    }
}

impl Message for FileUpload {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 10;
}

/// Download a file (bumps the counter). Requires FILE_DOWNLOAD.
/// → [`FileContent`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDownloadRequest {
    pub id: i64,
}

impl FileDownloadRequest {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Message for FileDownloadRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 11;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileContent {
    pub node: FileNodeView,
    pub bytes: Vec<u8>,
}

impl FileContent {
    pub fn new(node: FileNodeView, bytes: Vec<u8>) -> Self {
        Self { node, bytes }
    }
}

impl Message for FileContent {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 12;
}

/// Delete a node (uploader or FILE_MANAGE). → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDelete {
    pub id: i64,
}

impl NodeDelete {
    pub fn new(id: i64) -> Self {
        Self { id }
    }
}

impl Message for NodeDelete {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 13;
}

/// Edit a file's icon/comment (uploader or FILE_MANAGE). → [`NodeReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMetadata {
    pub id: i64,
    pub icon: String,
    pub comment: String,
}

impl SetMetadata {
    pub fn new(id: i64, icon: impl Into<String>, comment: impl Into<String>) -> Self {
        Self {
            id,
            icon: icon.into(),
            comment: comment.into(),
        }
    }
}

impl Message for SetMetadata {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 14;
}

/// Search files by name/comment/uploader. → [`SearchResults`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub area: Option<String>,
    pub query: String,
    pub limit: u32,
}

impl SearchRequest {
    pub fn new(area: Option<String>, query: impl Into<String>, limit: u32) -> Self {
        Self {
            area,
            query: query.into(),
            limit,
        }
    }
}

impl Message for SearchRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 15;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchResults {
    pub nodes: Vec<FileNodeView>,
}

impl SearchResults {
    pub fn new(nodes: Vec<FileNodeView>) -> Self {
        Self { nodes }
    }
}

impl Message for SearchResults {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 16;
}

/// Rate a file 1..5 (one vote per account). Requires FILE_DOWNLOAD.
/// → [`NodeReply`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateFile {
    pub id: i64,
    pub stars: u8,
}

impl RateFile {
    pub fn new(id: i64, stars: u8) -> Self {
        Self { id, stars }
    }
}

impl Message for RateFile {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 17;
}

/// Create an alias pointing at an existing node. Requires FILE_MANAGE.
/// → [`NodeReply`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasCreate {
    pub area: String,
    pub parent: Option<String>,
    pub name: String,
    pub target_path: String,
}

impl AliasCreate {
    pub fn new(
        area: impl Into<String>,
        parent: Option<String>,
        name: impl Into<String>,
        target_path: impl Into<String>,
    ) -> Self {
        Self {
            area: area.into(),
            parent,
            name: name.into(),
            target_path: target_path.into(),
        }
    }
}

impl Message for AliasCreate {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 18;
}

/// Push: a file landed in an area (clients refresh listings/search).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAdded {
    pub area: String,
    pub id: i64,
}

impl FileAdded {
    pub fn new(area: impl Into<String>, id: i64) -> Self {
        Self {
            area: area.into(),
            id,
        }
    }
}

impl Message for FileAdded {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 19;
}

/// Change what a library is called and says about itself. → empty ack.
/// Requires FILE_MANAGE. The slug is the area's identity (it is in every path
/// and every download link) and cannot be changed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaUpdate {
    pub slug: String,
    pub title: String,
    pub description: String,
}

impl AreaUpdate {
    pub fn new(
        slug: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            slug: slug.into(),
            title: title.into(),
            description: description.into(),
        }
    }
}

impl Message for AreaUpdate {
    const FAMILY: Family = Family::FILE;
    // 20..26 are the ticketed transfer messages (`transfer.rs`).
    const MESSAGE_TYPE: u16 = 27;
}

/// Remove an empty library. → empty ack. Requires FILE_MANAGE. `BadRequest`
/// while anything is in it: an area takes its whole tree with it, and that is
/// not something to do with one click.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaDelete {
    pub slug: String,
}

impl AreaDelete {
    pub fn new(slug: impl Into<String>) -> Self {
        Self { slug: slug.into() }
    }
}

impl Message for AreaDelete {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 28;
}

/// Rename a file or folder in place. Its uploader, or `FILE_MANAGE` on the
/// area. → [`NodeReply`]. A folder takes everything below it along: their
/// paths follow. `BadRequest` for an empty name or one with a slash;
/// `AlreadyExists` when something by that name is already beside it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRename {
    pub id: i64,
    pub name: String,
}

impl NodeRename {
    pub fn new(id: i64, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
        }
    }
}

impl Message for NodeRename {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 29;
}

/// Move a file or folder into another folder of the same area (`folder`
/// `None` or empty: the area's root). `FILE_MANAGE` on the area, where it is
/// and where it goes. → [`NodeReply`]. `BadRequest` for a folder moved into
/// itself or a destination that is not a folder; `NotFound` for a
/// destination that is not there; `AlreadyExists` when the destination has
/// something by that name.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMove {
    pub id: i64,
    pub folder: Option<String>,
}

impl NodeMove {
    pub fn new(id: i64, folder: Option<String>) -> Self {
        Self { id, folder }
    }
}

impl Message for NodeMove {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 30;
}

/// What this burrow lets the caller upload, asked before a file is sent so a
/// refusal can say which limit and by how much. → [`UploadLimits`]. Any
/// session. A burrow that predates it answers `Unsupported`: its uploads are
/// still checked, only not announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadLimitsRequest;

impl Message for UploadLimitsRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 31;
}

/// Reply to [`UploadLimitsRequest`]. Zero means no limit.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadLimits {
    /// The largest single file, in bytes.
    pub max_file_bytes: u64,
    /// What one account may keep in the library altogether, in bytes.
    pub quota_bytes: u64,
    /// What the caller's account keeps there now, in bytes.
    pub used_bytes: u64,
}

impl UploadLimits {
    pub fn new(max_file_bytes: u64, quota_bytes: u64, used_bytes: u64) -> Self {
        Self {
            max_file_bytes,
            quota_bytes,
            used_bytes,
        }
    }
}

impl Message for UploadLimits {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 32;
}

// ---------------------------------------------------------------------------
// Pulls between burrows (FILE 33..38): a person sends files from one burrow
// they are on to another. The source grants, the destination fetches over its
// federation session with the source. See docs/design/server-to-server-transfers.md.
// ---------------------------------------------------------------------------

/// Where a pull is, in a [`RemotePullStatus`].
pub mod pull_state {
    /// Files are on their way.
    pub const RUNNING: u8 = 0;
    /// Everything that could be fetched is filed.
    pub const DONE: u8 = 1;
    /// It stopped; `reason` says why.
    pub const FAILED: u8 = 2;
}

/// Why a pull stopped, in a [`RemotePullStatus`]. The client turns these into
/// sentences.
pub mod pull_reason {
    pub const NONE: u8 = 0;
    /// The source refused to serve (grant no longer holds there, or pulls off).
    pub const SOURCE_REFUSED: u8 = 1;
    /// The session with the source is gone.
    pub const SOURCE_UNREACHABLE: u8 = 2;
    /// A file is bigger than this burrow takes.
    pub const TOO_LARGE: u8 = 3;
    /// It would put the person over their space here.
    pub const OVER_QUOTA: u8 = 4;
    /// This burrow refuses that content.
    pub const DENIED_CONTENT: u8 = 5;
    /// A file did not match the content id the source granted.
    pub const VERIFY_FAILED: u8 = 6;
    /// The person cancelled it.
    pub const CANCELLED: u8 = 7;
    /// Something went wrong here.
    pub const INTERNAL: u8 = 8;
    /// This burrow stopped taking it: pulls were switched off, or the
    /// person's account was closed.
    pub const STOPPED: u8 = 9;
}

/// At the **source**: sign a grant letting the burrow whose server key is
/// `fetcher_key` fetch `nodes` (files, or folders with everything the person
/// may download in them). → [`PullGrantIssued`]. `Unsupported` when this
/// burrow does not issue grants; `Unavailable` when `fetcher_key` is not an
/// approved federation peer here; `Forbidden` when the person may download
/// none of it; `NotFound` for a node that is gone or an empty folder;
/// `BadRequest` for no nodes, or more files than one grant may name.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullGrantRequest {
    pub fetcher_key: [u8; 32],
    pub nodes: Vec<i64>,
}

impl PullGrantRequest {
    pub fn new(fetcher_key: [u8; 32], nodes: Vec<i64>) -> Self {
        Self { fetcher_key, nodes }
    }
}

impl Message for PullGrantRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 33;
}

/// Reply to [`PullGrantRequest`]: the signed grant, opaque to the client,
/// and what it covers. `skipped` counts files left out because the person may
/// not download them or the source will not send them.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullGrantIssued {
    pub grant: Vec<u8>,
    pub files: u32,
    pub bytes: u64,
    pub skipped: u32,
    pub expires_unix: i64,
}

impl PullGrantIssued {
    pub fn new(grant: Vec<u8>, files: u32, bytes: u64, skipped: u32, expires_unix: i64) -> Self {
        Self {
            grant,
            files,
            bytes,
            skipped,
            expires_unix,
        }
    }
}

impl Message for PullGrantIssued {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 34;
}

/// At the **destination**: fetch what `grant` names into `area`/`folder`
/// (`None` or empty: the area root), filed under the caller.
/// → [`RemotePullAccepted`], then [`RemotePullStatus`] pushes. Refused before
/// a byte moves: `Unsupported` when this burrow does not pull; `Unavailable`
/// when the source is not an approved peer with a live session here;
/// `SessionExpired` for an expired grant; `BadRequest` for a grant that is
/// not for this burrow or does not verify; `AlreadyExists` for a grant
/// already used; `Forbidden` when the caller may not upload there or a file
/// is refused content; `NotFound` for a folder that is not there;
/// `TooLarge` for a file over the largest this burrow takes, or a total over
/// the caller's space or a pull's ceiling; `RateLimited` when the caller
/// already has as many pulls running as allowed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePull {
    pub grant: Vec<u8>,
    pub area: String,
    pub folder: Option<String>,
}

impl RemotePull {
    pub fn new(grant: Vec<u8>, area: impl Into<String>, folder: Option<String>) -> Self {
        Self {
            grant,
            area: area.into(),
            folder,
        }
    }
}

impl Message for RemotePull {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 35;
}

/// Reply to [`RemotePull`]: the pull is under way.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePullAccepted {
    pub pull_id: u64,
    pub files: u32,
    pub bytes: u64,
    /// The source burrow's federation name.
    pub source: String,
}

impl RemotePullAccepted {
    pub fn new(pull_id: u64, files: u32, bytes: u64, source: impl Into<String>) -> Self {
        Self {
            pull_id,
            files,
            bytes,
            source: source.into(),
        }
    }
}

impl Message for RemotePullAccepted {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 36;
}

/// Stop a pull the caller started. → empty ack; `NotFound` for one that is
/// not running or not theirs.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePullCancel {
    pub pull_id: u64,
}

impl RemotePullCancel {
    pub fn new(pull_id: u64) -> Self {
        Self { pull_id }
    }
}

impl Message for RemotePullCancel {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 37;
}

/// Push to the person who started a pull: how far it has got, or how it
/// ended. Progress is not replayed after a reconnect; the ending is.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePullStatus {
    pub pull_id: u64,
    /// One of [`pull_state`].
    pub state: u8,
    pub files_done: u32,
    pub files_total: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// One of [`pull_reason`]; `NONE` unless it failed.
    pub reason: u8,
    /// Files the source no longer had, left out of a finished pull.
    pub missing: u32,
    /// The source burrow's federation name.
    pub source: String,
    /// Where it landed: the area and the path of the first thing filed.
    pub area: String,
    pub landed: String,
}

impl RemotePullStatus {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pull_id: u64,
        state: u8,
        files_done: u32,
        files_total: u32,
        bytes_done: u64,
        bytes_total: u64,
        reason: u8,
        missing: u32,
        source: impl Into<String>,
        area: impl Into<String>,
        landed: impl Into<String>,
    ) -> Self {
        Self {
            pull_id,
            state,
            files_done,
            files_total,
            bytes_done,
            bytes_total,
            reason,
            missing,
            source: source.into(),
            area: area.into(),
            landed: landed.into(),
        }
    }
}

impl Message for RemotePullStatus {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 38;
}

/// At the **source**, as [`PullGrantRequest`], with the host the person's
/// app reaches this burrow at. A burrow that is not a federation peer has no
/// session to fetch over; it connects to this burrow's QUIC port, at this
/// host or at the one the operator advertises, and the grant carries both,
/// signed, with this burrow's certificate fingerprint to pin.
/// → [`PullGrantIssued`], refusals as [`PullGrantRequest`]. A burrow that
/// predates it answers `Unsupported`; ask again with [`PullGrantRequest`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullGrantAsk {
    pub fetcher_key: [u8; 32],
    pub nodes: Vec<i64>,
    /// A host name or IP address, without a port or scheme; empty for none.
    pub reach_host: String,
}

impl PullGrantAsk {
    pub fn new(fetcher_key: [u8; 32], nodes: Vec<i64>, reach_host: impl Into<String>) -> Self {
        Self {
            fetcher_key,
            nodes,
            reach_host: reach_host.into(),
        }
    }
}

impl Message for PullGrantAsk {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 39;
}
