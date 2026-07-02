//! Swarm family (6, Wave 5) — the Warren's coordinator surface.
//!
//! "List-without-upload": a connected peer **advertises** files it holds
//! locally (by blake3 root) without sending any bytes; the server keeps a
//! TTL'd soft-state catalog of who has what, and anyone browsing can ask it
//! to **find sources** for a root. Advertisements are re-announced before
//! their TTL lapses and vanish with the session — the catalog never outlives
//! the peer's ability to serve. The peer wire (Have/RequestRange with Bao
//! proofs) rides on top in a later slice; until then a source is named by
//! its presence on this server.
//!
//! | type | name | direction |
//! |---|---|---|
//! | 1/2 | [`AdvertiseFiles`] → [`AdvertiseAck`] | Request/Reply |
//! | 3 | [`AdvertWithdraw`] | Request → ack |
//! | 4/5 | [`FindSources`] → [`SourceList`] | Request/Reply |

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// One advertised file: enough metadata for a catalog listing, no bytes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvertEntry {
    /// blake3 root of the file (== blob id / Bao verification anchor).
    pub root: [u8; 32],
    pub size: u64,
    pub name: String,
    pub mime: String,
}

impl AdvertEntry {
    pub fn new(
        root: [u8; 32],
        size: u64,
        name: impl Into<String>,
        mime: impl Into<String>,
    ) -> Self {
        Self {
            root,
            size,
            name: name.into(),
            mime: mime.into(),
        }
    }
}

/// Advertise (or re-announce) files this session can serve. Needs
/// `SWARM_ADVERTISE`. Re-sending an already-advertised root refreshes its
/// TTL and metadata. → [`AdvertiseAck`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvertiseFiles {
    pub entries: Vec<AdvertEntry>,
    /// Requested TTL in seconds; 0 = server default. The server clamps to
    /// its configured maximum and reports the granted value in the ack.
    pub ttl_secs: u32,
}

impl AdvertiseFiles {
    pub fn new(entries: Vec<AdvertEntry>, ttl_secs: u32) -> Self {
        Self { entries, ttl_secs }
    }
}

impl Message for AdvertiseFiles {
    const FAMILY: Family = Family::SWARM;
    const MESSAGE_TYPE: u16 = 1;
}

/// Reply to [`AdvertiseFiles`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AdvertiseAck {
    /// How many entries were accepted (the rest hit the per-account cap).
    pub accepted: u32,
    /// The TTL actually granted, in seconds — re-announce before it lapses.
    pub ttl_secs: u32,
    /// This account's total live advertisements after the call.
    pub total: u32,
}

impl AdvertiseAck {
    pub fn new(accepted: u32, ttl_secs: u32, total: u32) -> Self {
        Self {
            accepted,
            ttl_secs,
            total,
        }
    }
}

impl Message for AdvertiseAck {
    const FAMILY: Family = Family::SWARM;
    const MESSAGE_TYPE: u16 = 2;
}

/// Withdraw advertisements. Empty `roots` = withdraw everything this
/// session advertised. → ack.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AdvertWithdraw {
    pub roots: Vec<[u8; 32]>,
}

impl AdvertWithdraw {
    pub fn new(roots: Vec<[u8; 32]>) -> Self {
        Self { roots }
    }
}

impl Message for AdvertWithdraw {
    const FAMILY: Family = Family::SWARM;
    const MESSAGE_TYPE: u16 = 3;
}

/// Who has this root? Needs `FILE_LIST`. → [`SourceList`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindSources {
    pub root: [u8; 32],
}

impl FindSources {
    pub fn new(root: [u8; 32]) -> Self {
        Self { root }
    }
}

impl Message for FindSources {
    const FAMILY: Family = Family::SWARM;
    const MESSAGE_TYPE: u16 = 4;
}

/// One peer currently advertising a root.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// The advertising persona (peer endpoints arrive with the peer wire).
    pub screen_name: String,
    pub size: u64,
    pub name: String,
    pub mime: String,
}

impl SourceInfo {
    pub fn new(
        screen_name: impl Into<String>,
        size: u64,
        name: impl Into<String>,
        mime: impl Into<String>,
    ) -> Self {
        Self {
            screen_name: screen_name.into(),
            size,
            name: name.into(),
            mime: mime.into(),
        }
    }
}

/// Reply to [`FindSources`]. `sources.len()` doubles as the root's rarity
/// signal for scheduling (per-chunk rarity arrives with the peer wire).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceList {
    pub root: [u8; 32],
    /// Whether this server's own blob store holds the full file.
    pub server_has: bool,
    /// The blob's size when `server_has` (0 otherwise).
    pub server_size: u64,
    /// Peers currently advertising the root.
    pub sources: Vec<SourceInfo>,
}

impl SourceList {
    pub fn new(
        root: [u8; 32],
        server_has: bool,
        server_size: u64,
        sources: Vec<SourceInfo>,
    ) -> Self {
        Self {
            root,
            server_has,
            server_size,
            sources,
        }
    }
}

impl Message for SourceList {
    const FAMILY: Family = Family::SWARM;
    const MESSAGE_TYPE: u16 = 5;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Frame, RequestId};

    #[test]
    fn advertise_roundtrips_in_a_frame() {
        let msg = AdvertiseFiles::new(
            vec![AdvertEntry::new(
                [7u8; 32],
                42,
                "x.bin",
                "application/octet-stream",
            )],
            600,
        );
        let frame = Frame::request(RequestId(1), &msg).unwrap();
        assert_eq!(frame.family, Family::SWARM);
        let back: AdvertiseFiles = frame.decode().unwrap().unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn source_list_roundtrips() {
        let msg = SourceList::new(
            [1u8; 32],
            true,
            1024,
            vec![SourceInfo::new("alice", 1024, "x.bin", "text/plain")],
        );
        let frame = Frame::request(RequestId(2), &msg).unwrap();
        let back: SourceList = frame.decode().unwrap().unwrap();
        assert_eq!(back, msg);
    }
}
