//! Bulk file transfers (family 5, Wave 4.2).
//!
//! The W4.1 [`crate::filelib`] messages carry *small* files inline. Real
//! transfers negotiate a **ticket** on the control stream, then move bytes
//! either over a dedicated QUIC bulk stream (when the transport offers one)
//! or as windowed ranged chunks on the control connection — the WebSocket/
//! wasm fallback. Both paths carry the same byte ranges and both verify the
//! finished file against its blake3 root (which is also its blob id), so a
//! transfer is resumable and integrity-checked regardless of transport.
//!
//! Per-chunk merkle (Bao) verification for *untrusted* sources arrives with
//! the swarm in Wave 5; against the authenticated origin server, whole-file
//! root verification is the W4.2 guarantee.
//!
//! Types 20-42 (filelib owns 1-19; blob owns 100+).

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// Transfer direction.
pub const DIR_DOWNLOAD: u8 = 0;
pub const DIR_UPLOAD: u8 = 1;

/// Open a transfer and get a ticket. For downloads set `node_id`; for
/// uploads set the destination (`area`/`parent`/`name`) plus `size` and the
/// client-computed blake3 `root`. → [`TransferTicket`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferOpen {
    pub direction: u8,
    /// Download target.
    pub node_id: Option<i64>,
    /// Upload destination.
    pub area: String,
    pub parent: Option<String>,
    pub name: String,
    pub mime: String,
    pub comment: String,
    /// Upload: total size + client-declared blake3 root (== resulting blob id).
    pub size: u64,
    pub root: [u8; 32],
}

impl TransferOpen {
    /// A download of an existing node.
    pub fn download(node_id: i64) -> Self {
        Self {
            direction: DIR_DOWNLOAD,
            node_id: Some(node_id),
            area: String::new(),
            parent: None,
            name: String::new(),
            mime: String::new(),
            comment: String::new(),
            size: 0,
            root: [0; 32],
        }
    }

    /// An upload to `area` (optionally under `parent`) of `name`.
    pub fn upload(
        area: impl Into<String>,
        parent: Option<String>,
        name: impl Into<String>,
        size: u64,
        root: [u8; 32],
    ) -> Self {
        Self {
            direction: DIR_UPLOAD,
            node_id: None,
            area: area.into(),
            parent,
            name: name.into(),
            mime: "application/octet-stream".into(),
            comment: String::new(),
            size,
            root,
        }
    }

    pub fn with_meta(mut self, mime: impl Into<String>, comment: impl Into<String>) -> Self {
        self.mime = mime.into();
        self.comment = comment.into();
        self
    }
}

impl Message for TransferOpen {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 20;
}

/// The authorization + parameters for a transfer.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferTicket {
    pub transfer_id: u64,
    pub root: [u8; 32],
    pub size: u64,
    /// Bytes the server already holds (download: always `size`; upload: the
    /// verified staged prefix, for resume).
    pub server_have: u64,
    /// Opaque token binding a bulk stream to this authorization.
    pub token: [u8; 16],
    /// True if the server will serve this over a dedicated bulk stream when
    /// the client's transport offers one.
    pub supports_bulk: bool,
}

impl TransferTicket {
    pub fn new(transfer_id: u64, root: [u8; 32], size: u64, token: [u8; 16]) -> Self {
        Self {
            transfer_id,
            root,
            size,
            server_have: size,
            token,
            supports_bulk: true,
        }
    }

    pub fn with_server_have(mut self, have: u64) -> Self {
        self.server_have = have;
        self
    }

    pub fn with_bulk(mut self, supports: bool) -> Self {
        self.supports_bulk = supports;
        self
    }
}

impl Message for TransferTicket {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 21;
}

/// Re-authorize a transfer after a reconnect (re-reports `server_have`).
/// → [`TransferTicket`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferResume {
    pub transfer_id: u64,
    pub token: [u8; 16],
    pub local_have: u64,
}

impl TransferResume {
    pub fn new(transfer_id: u64, token: [u8; 16], local_have: u64) -> Self {
        Self {
            transfer_id,
            token,
            local_have,
        }
    }
}

impl Message for TransferResume {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 22;
}

/// Finish an upload: the server verifies the staged file's blake3 == `root`,
/// commits it to the blob store, and records the node. → filelib `NodeReply`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadFinish {
    pub transfer_id: u64,
}

impl UploadFinish {
    pub fn new(transfer_id: u64) -> Self {
        Self { transfer_id }
    }
}

impl Message for UploadFinish {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 23;
}

/// Abandon a transfer; drops the ticket and any upload staging. → ack.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferAbort {
    pub transfer_id: u64,
}

impl TransferAbort {
    pub fn new(transfer_id: u64) -> Self {
        Self { transfer_id }
    }
}

impl Message for TransferAbort {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 24;
}

/// A subtree listing for pipelined folder transfers. → [`FolderManifest`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderManifestRequest {
    pub area: String,
    pub path: Option<String>,
}

impl FolderManifestRequest {
    pub fn new(area: impl Into<String>, path: Option<String>) -> Self {
        Self {
            area: area.into(),
            path,
        }
    }
}

impl Message for FolderManifestRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 25;
}

/// One file in a [`FolderManifest`] (folders are implied by `rel_path`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub node_id: i64,
    pub rel_path: String,
    pub root: [u8; 32],
    pub size: u64,
    pub mime: String,
}

impl ManifestEntry {
    pub fn new(node_id: i64, rel_path: impl Into<String>, root: [u8; 32], size: u64) -> Self {
        Self {
            node_id,
            rel_path: rel_path.into(),
            root,
            size,
            mime: String::new(),
        }
    }

    pub fn with_mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = mime.into();
        self
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FolderManifest {
    pub entries: Vec<ManifestEntry>,
}

impl FolderManifest {
    pub fn new(entries: Vec<ManifestEntry>) -> Self {
        Self { entries }
    }
}

impl Message for FolderManifest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 26;
}

/// The first message on a freshly opened QUIC bulk stream (length-prefixed,
/// NOT a control [`crate::Frame`]): it binds the raw stream to a ticket. The
/// server validates `token` against the ticket, then streams `[offset,size)`
/// for a download or reads the remainder for an upload.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkPreamble {
    pub transfer_id: u64,
    pub token: [u8; 16],
    pub offset: u64,
    pub direction: u8,
}

impl BulkPreamble {
    pub fn new(transfer_id: u64, token: [u8; 16], offset: u64, direction: u8) -> Self {
        Self {
            transfer_id,
            token,
            offset,
            direction,
        }
    }
}

/// Request a byte range (control-stream / WS download path). Ranges are
/// bounded well under the 1 MiB frame cap. → [`FileChunk`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunkRequest {
    pub transfer_id: u64,
    pub offset: u64,
    pub len: u32,
}

impl FileChunkRequest {
    pub fn new(transfer_id: u64, offset: u64, len: u32) -> Self {
        Self {
            transfer_id,
            offset,
            len,
        }
    }
}

impl Message for FileChunkRequest {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 40;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunk {
    pub transfer_id: u64,
    pub offset: u64,
    pub last: bool,
    pub bytes: Vec<u8>,
}

impl FileChunk {
    pub fn new(transfer_id: u64, offset: u64, last: bool, bytes: Vec<u8>) -> Self {
        Self {
            transfer_id,
            offset,
            last,
            bytes,
        }
    }
}

impl Message for FileChunk {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 41;
}

/// Send a byte range (control-stream / WS upload path). → ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunkPut {
    pub transfer_id: u64,
    pub offset: u64,
    pub last: bool,
    pub bytes: Vec<u8>,
}

impl FileChunkPut {
    pub fn new(transfer_id: u64, offset: u64, last: bool, bytes: Vec<u8>) -> Self {
        Self {
            transfer_id,
            offset,
            last,
            bytes,
        }
    }
}

impl Message for FileChunkPut {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 42;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, RequestId};

    #[test]
    fn ticket_roundtrips_through_a_frame() {
        let t = TransferTicket::new(7, [3; 32], 4096, [9; 16]).with_server_have(1024);
        let frame = Frame::request(RequestId(1), &t).unwrap();
        let back = frame.decode::<TransferTicket>().unwrap().unwrap();
        assert_eq!(back, t);
        assert_eq!(back.server_have, 1024);
    }

    #[test]
    fn open_constructors_set_direction() {
        assert_eq!(TransferOpen::download(5).direction, DIR_DOWNLOAD);
        let up = TransferOpen::upload("warez", Some("utils".into()), "a.lha", 10, [1; 32]);
        assert_eq!(up.direction, DIR_UPLOAD);
        assert_eq!(up.size, 10);
    }

    #[test]
    fn chunk_and_preamble_roundtrip() {
        let c = FileChunk::new(1, 512, true, vec![1, 2, 3, 4]);
        let frame = Frame::request(RequestId(2), &c).unwrap();
        assert_eq!(frame.decode::<FileChunk>().unwrap().unwrap(), c);

        // The preamble is postcard-framed on a raw stream, not a Frame.
        let p = BulkPreamble::new(1, [7; 16], 2048, DIR_DOWNLOAD);
        let bytes = postcard::to_allocvec(&p).unwrap();
        let back: BulkPreamble = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, p);
    }
}
