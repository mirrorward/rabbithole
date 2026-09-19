//! Pulls between burrows: a source burrow's signed permission for one other
//! burrow to fetch named files on a person's behalf, and the request that
//! fetcher sends on a stream of the live federation session to get them.
//!
//! The flow, from `docs/design/server-to-server-transfers.md` (option B):
//!
//! 1. A person asks the **source** for a [`PullGrant`] naming the files they
//!    may download there and the **destination**'s server key as the only
//!    party that may use it. The source signs it under [`PULL_GRANT_CONTEXT`].
//! 2. The person hands the grant to the destination with a folder. The
//!    destination checks the signature against the source's key from its own
//!    peer registry, that the grant names itself, that it has not expired or
//!    been used, and the person's standing there, before moving a byte.
//! 3. The destination opens a bulk stream on the federation session it
//!    already holds with the source, and sends a [`PullStreamRequest`] per
//!    file. The source serves a file only to the peer the grant names,
//!    authenticated by that session's handshake.
//!
//! A grant is worthless to anyone but the named destination (the source
//! checks the session's peer key), expires, and is single-use at the
//! destination (its nonce). Pure and I/O-free: signing, verification and the
//! path rules are host-tested here.

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain separation for grant signatures: a signature minted for a catalog,
/// a handshake or a swarm capability can never verify as a grant.
pub const PULL_GRANT_CONTEXT: &[u8] = b"rhp-fed-pull-grant-v1";

/// The grant format this crate writes and accepts.
pub const PULL_GRANT_VERSION: u32 = 1;

/// Most files one grant may name.
pub const MAX_PULL_ITEMS: usize = 1000;

/// Longest relative path an item may carry, in bytes.
pub const MAX_PULL_PATH: usize = 1024;

/// Deepest folder nesting an item may carry.
pub const MAX_PULL_DEPTH: usize = 16;

/// Longest one path segment may be, in bytes (the file library's own rule).
pub const MAX_SEGMENT: usize = 128;

/// Largest encoded [`SignedPullGrant`], in bytes: it rides every stream
/// request back to the source, and the reply that carries it to the person
/// must fit one protocol frame.
pub const MAX_GRANT_BYTES: usize = 384 * 1024;

/// Largest encoded [`PullStreamRequest`], in bytes: a grant and a little.
pub const MAX_STREAM_REQUEST: usize = MAX_GRANT_BYTES + 4096;

/// One file a grant names, as the source knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullItem {
    /// The source's file node id.
    pub node_id: i64,
    /// Where it lands relative to the destination folder: its name, or
    /// `folder/…/name` for a file inside a pulled folder. `/`-separated.
    pub rel_path: String,
    /// Its blake3 content id; the destination verifies the bytes against it.
    pub root: [u8; 32],
    pub size: u64,
    pub mime: String,
}

/// The permission itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullGrant {
    pub version: u32,
    /// The source burrow's server key: the signer.
    pub source_key: [u8; 32],
    /// The destination burrow's server key: the only fetcher.
    pub fetcher_key: [u8; 32],
    pub issued_unix: i64,
    pub expires_unix: i64,
    /// Single use: the destination refuses a nonce it has seen.
    pub nonce: [u8; 16],
    pub items: Vec<PullItem>,
}

/// A grant and the source's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPullGrant {
    pub grant: PullGrant,
    /// Ed25519 over [`PULL_GRANT_CONTEXT`] ‖ postcard(grant), by
    /// `grant.source_key`.
    pub sig: Signature,
}

/// Why a grant is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PullGrantError {
    #[error("the grant could not be read")]
    Decode,
    #[error("the grant is a version this burrow does not read")]
    Version,
    #[error("the grant was not signed by the burrow it names")]
    BadSignature,
    #[error("the grant is from a different burrow")]
    WrongSource,
    #[error("the grant names a different burrow as its fetcher")]
    WrongFetcher,
    #[error("the grant has expired")]
    Expired,
    #[error("the grant names no files, or too many")]
    Count,
    #[error("the grant carries a path this burrow will not file")]
    BadPath,
}

impl PullGrant {
    /// Sign as the source. `source_key` is set from `key`, so the grant
    /// always names its signer.
    pub fn sign(mut self, key: &IdentityKey) -> Result<SignedPullGrant, PullGrantError> {
        self.source_key = key.public().0;
        let sig = key.sign(&signed_bytes(&self)?);
        Ok(SignedPullGrant { grant: self, sig })
    }

    /// Everything the grant names, in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.items
            .iter()
            .map(|i| i.size)
            .fold(0, u64::saturating_add)
    }
}

impl SignedPullGrant {
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a grant always encodes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PullGrantError> {
        postcard::from_bytes(bytes).map_err(|_| PullGrantError::Decode)
    }

    /// Everything a burrow must check before acting on the grant: its
    /// version, that `source` signed it, that `fetcher` is who it names,
    /// that it is live at `now_unix`, and that its items are sane.
    pub fn check(
        &self,
        source: &[u8; 32],
        fetcher: &[u8; 32],
        now_unix: i64,
    ) -> Result<(), PullGrantError> {
        let g = &self.grant;
        if g.version != PULL_GRANT_VERSION {
            return Err(PullGrantError::Version);
        }
        if &g.source_key != source {
            return Err(PullGrantError::WrongSource);
        }
        if !PublicKey(*source).verify(&signed_bytes(g)?, &self.sig) {
            return Err(PullGrantError::BadSignature);
        }
        if &g.fetcher_key != fetcher {
            return Err(PullGrantError::WrongFetcher);
        }
        if now_unix >= g.expires_unix {
            return Err(PullGrantError::Expired);
        }
        if g.items.is_empty() || g.items.len() > MAX_PULL_ITEMS {
            return Err(PullGrantError::Count);
        }
        if !g.items.iter().all(|i| rel_path_is_acceptable(&i.rel_path)) {
            return Err(PullGrantError::BadPath);
        }
        Ok(())
    }
}

fn signed_bytes(grant: &PullGrant) -> Result<Vec<u8>, PullGrantError> {
    let body = postcard::to_allocvec(grant).map_err(|_| PullGrantError::Decode)?;
    let mut msg = Vec::with_capacity(PULL_GRANT_CONTEXT.len() + body.len());
    msg.extend_from_slice(PULL_GRANT_CONTEXT);
    msg.extend_from_slice(&body);
    Ok(msg)
}

/// Whether one path segment is a name the file library would take: not
/// empty, not `.` or `..`, no slash, no control characters, at most
/// [`MAX_SEGMENT`] bytes.
pub fn segment_is_acceptable(segment: &str) -> bool {
    let trimmed = segment.trim();
    !trimmed.is_empty()
        && trimmed == segment
        && segment != "."
        && segment != ".."
        && segment.len() <= MAX_SEGMENT
        && !segment.contains('/')
        && !segment.chars().any(char::is_control)
}

/// Whether an item's relative path is one a destination may file: relative,
/// `/`-separated acceptable segments, at most [`MAX_PULL_DEPTH`] deep and
/// [`MAX_PULL_PATH`] bytes long. Nothing here can climb out of the
/// destination folder.
pub fn rel_path_is_acceptable(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    !path.is_empty()
        && path.len() <= MAX_PULL_PATH
        && segments.len() <= MAX_PULL_DEPTH
        && segments.iter().all(|s| segment_is_acceptable(s))
}

/// What the destination sends first on a bulk stream of the federation
/// session: which file of which grant, from where. Length-prefixed postcard,
/// like the client transfer preamble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullStreamRequest {
    /// The [`SignedPullGrant`] as issued.
    pub grant: Vec<u8>,
    /// Which of its items.
    pub item: u32,
    /// Resume from here.
    pub offset: u64,
}

/// The source's first byte in answer to a [`PullStreamRequest`].
pub mod stream_status {
    /// The bytes follow, from the requested offset to the end.
    pub const OK: u8 = 0;
    /// The grant does not hold for this session (signer, fetcher, expiry).
    pub const DENIED: u8 = 1;
    /// The file is gone, changed, or no longer served.
    pub const GONE: u8 = 2;
    /// This burrow no longer issues or serves pulls.
    pub const OFF: u8 = 3;
    /// The request itself was malformed.
    pub const BAD: u8 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> IdentityKey {
        IdentityKey::from_seed(&[n; 32])
    }

    fn grant(fetcher: [u8; 32], items: Vec<PullItem>) -> PullGrant {
        PullGrant {
            version: PULL_GRANT_VERSION,
            source_key: [0; 32],
            fetcher_key: fetcher,
            issued_unix: 100,
            expires_unix: 700,
            nonce: [9; 16],
            items,
        }
    }

    fn item(path: &str, size: u64) -> PullItem {
        PullItem {
            node_id: 1,
            rel_path: path.into(),
            root: [3; 32],
            size,
            mime: "text/plain".into(),
        }
    }

    #[test]
    fn a_grant_holds_only_for_its_signer_its_fetcher_and_its_time() {
        let source = key(1);
        let dest = key(2).public().0;
        let signed = grant(
            dest,
            vec![item("tapes/side-a.mp3", 10), item("notes.txt", 5)],
        )
        .sign(&source)
        .unwrap();
        assert_eq!(
            signed.grant.source_key,
            source.public().0,
            "names its signer"
        );
        assert_eq!(signed.grant.total_bytes(), 15);
        let src = source.public().0;
        assert_eq!(signed.check(&src, &dest, 500), Ok(()));

        let bytes = signed.to_bytes();
        assert_eq!(SignedPullGrant::from_bytes(&bytes).unwrap(), signed);
        assert_eq!(
            SignedPullGrant::from_bytes(&bytes[..bytes.len() - 3]),
            Err(PullGrantError::Decode)
        );

        assert_eq!(signed.check(&src, &dest, 700), Err(PullGrantError::Expired));
        assert_eq!(
            signed.check(&src, &key(3).public().0, 500),
            Err(PullGrantError::WrongFetcher)
        );
        assert_eq!(
            signed.check(&key(3).public().0, &dest, 500),
            Err(PullGrantError::WrongSource)
        );

        // Any change after signing breaks it, including the fetcher.
        let mut tampered = signed.clone();
        tampered.grant.fetcher_key = key(3).public().0;
        assert_eq!(
            tampered.check(&src, &key(3).public().0, 500),
            Err(PullGrantError::BadSignature)
        );
        let mut grown = signed.clone();
        grown.grant.items[0].size = 1 << 40;
        assert_eq!(
            grown.check(&src, &dest, 500),
            Err(PullGrantError::BadSignature)
        );

        // Someone else's key signing a grant that names the source is caught.
        let mut forged = grant(dest, vec![item("x", 1)]).sign(&key(4)).unwrap();
        forged.grant.source_key = src;
        assert_eq!(
            forged.check(&src, &dest, 500),
            Err(PullGrantError::BadSignature)
        );
    }

    #[test]
    fn a_grant_signature_is_its_own_and_no_other_surfaces() {
        let source = key(1);
        let g = grant(key(2).public().0, vec![item("a", 1)])
            .sign(&source)
            .unwrap();
        let body = postcard::to_allocvec(&g.grant).unwrap();
        assert!(
            !source.public().verify(&body, &g.sig),
            "the signature covers the context, not the bare body"
        );
    }

    #[test]
    fn nothing_in_a_grant_can_climb_out_of_its_folder() {
        for good in [
            "a",
            "tapes/side a.mp3",
            "Tapes (2)/x/y/z.bin",
            "caf\u{e9}.txt",
        ] {
            assert!(rel_path_is_acceptable(good), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../up",
            "a/../../up",
            "a//b",
            "a/./b",
            "a/",
            " padded",
            "bell\u{7}",
            "new\nline",
        ] {
            assert!(!rel_path_is_acceptable(bad), "{bad:?}");
        }
        assert!(!rel_path_is_acceptable(&"x".repeat(129)));
        assert!(!rel_path_is_acceptable(
            &vec!["d"; MAX_PULL_DEPTH + 1].join("/")
        ));

        let source = key(1);
        let dest = key(2).public().0;
        let bad = grant(dest, vec![item("../escape", 1)])
            .sign(&source)
            .unwrap();
        assert_eq!(
            bad.check(&source.public().0, &dest, 500),
            Err(PullGrantError::BadPath)
        );
        let empty = grant(dest, vec![]).sign(&source).unwrap();
        assert_eq!(
            empty.check(&source.public().0, &dest, 500),
            Err(PullGrantError::Count)
        );
    }
}
