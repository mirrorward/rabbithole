//! Small-blob transfer on the control stream (file family, Wave 2).
//!
//! For avatars, banners, and theme assets — things comfortably under the
//! 1 MiB frame cap. Real file transfers (Wave 4) get dedicated streams;
//! these message types deliberately live at 100+ to leave low numbers for
//! them.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// What a blob is for — servers enforce per-purpose size caps.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobPurpose {
    Avatar,
    Banner,
    ThemeAsset,
}

/// Upload a small blob. → [`BlobRef`] (its blake3 id) or `TooLarge`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPut {
    pub purpose: BlobPurpose,
    pub bytes: Vec<u8>,
}

impl BlobPut {
    pub fn new(purpose: BlobPurpose, bytes: Vec<u8>) -> Self {
        Self { purpose, bytes }
    }
}

impl Message for BlobPut {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 100;
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub id: [u8; 32],
}

impl BlobRef {
    pub fn new(id: [u8; 32]) -> Self {
        Self { id }
    }
}

impl Message for BlobRef {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 101;
}

/// Fetch a small blob by id. → [`BlobData`] or `NotFound`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobGet {
    pub id: [u8; 32],
}

impl BlobGet {
    pub fn new(id: [u8; 32]) -> Self {
        Self { id }
    }
}

impl Message for BlobGet {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 102;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobData {
    pub bytes: Vec<u8>,
}

impl BlobData {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl Message for BlobData {
    const FAMILY: Family = Family::FILE;
    const MESSAGE_TYPE: u16 = 103;
}
