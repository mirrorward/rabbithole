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
