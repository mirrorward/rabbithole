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

/// The grant format this crate writes for destinations that are not the
/// source's federation peers: 2 adds the source's certificate fingerprint and
/// addresses.
pub const PULL_GRANT_VERSION: u32 = 2;

/// The first grant format, without the certificate or addresses. Still read,
/// and still written for approved federation peers, which fetch over their
/// federation session: a peer on a release before version 2 reads it.
pub const PULL_GRANT_V1: u32 = 1;

/// Longest a grant may stand from the moment it is checked, in seconds. A
/// source signs for an hour; this leaves room for two burrows' clocks to
/// disagree, and refuses a grant made to last.
pub const MAX_GRANT_LIFETIME_SECS: i64 = 2 * 3600;

/// Most addresses a grant carries.
pub const MAX_ENDPOINTS: usize = 4;

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
    /// The source's TLS certificate fingerprint (blake3 of the DER), which a
    /// destination that is not a federation peer pins when it connects.
    pub tls_fingerprint: [u8; 32],
    /// Where the source's QUIC client port can be reached, as `host:port`,
    /// for a destination that is not a federation peer. The source vouches
    /// for them by signing; the destination still refuses private ones.
    pub endpoints: Vec<String>,
}

/// A grant and the source's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPullGrant {
    pub grant: PullGrant,
    /// Ed25519 over [`PULL_GRANT_CONTEXT`] ‖ postcard(grant), by
    /// `grant.source_key`.
    pub sig: Signature,
}

/// The first grant format on the wire: [`PullGrant`] without its last two
/// fields.
#[derive(Serialize, Deserialize)]
struct PullGrantV1 {
    version: u32,
    source_key: [u8; 32],
    fetcher_key: [u8; 32],
    issued_unix: i64,
    expires_unix: i64,
    nonce: [u8; 16],
    items: Vec<PullItem>,
}

#[derive(Serialize, Deserialize)]
struct SignedPullGrantV1 {
    grant: PullGrantV1,
    sig: Signature,
}

impl PullGrantV1 {
    fn of(g: &PullGrant) -> Self {
        Self {
            version: g.version,
            source_key: g.source_key,
            fetcher_key: g.fetcher_key,
            issued_unix: g.issued_unix,
            expires_unix: g.expires_unix,
            nonce: g.nonce,
            items: g.items.clone(),
        }
    }

    fn into_grant(self) -> PullGrant {
        PullGrant {
            version: self.version,
            source_key: self.source_key,
            fetcher_key: self.fetcher_key,
            issued_unix: self.issued_unix,
            expires_unix: self.expires_unix,
            nonce: self.nonce,
            items: self.items,
            tls_fingerprint: [0; 32],
            endpoints: Vec::new(),
        }
    }
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
    #[error("the grant stands longer than any burrow signs one for")]
    TooLong,
}

impl PullGrant {
    /// Sign as the source. `source_key` is set from `key`, so the grant
    /// always names its signer. A version 1 grant carries no certificate or
    /// addresses: that format has nowhere to put them.
    pub fn sign(mut self, key: &IdentityKey) -> Result<SignedPullGrant, PullGrantError> {
        if self.version == PULL_GRANT_V1
            && (self.tls_fingerprint != [0; 32] || !self.endpoints.is_empty())
        {
            return Err(PullGrantError::Version);
        }
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
    /// The grant in its own version's layout.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.grant.version == PULL_GRANT_V1 {
            let v1 = SignedPullGrantV1 {
                grant: PullGrantV1::of(&self.grant),
                sig: self.sig,
            };
            postcard::to_allocvec(&v1).expect("a grant always encodes")
        } else {
            postcard::to_allocvec(self).expect("a grant always encodes")
        }
    }

    /// Either version: the leading version number says which layout
    /// follows.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PullGrantError> {
        let (version, _) =
            postcard::take_from_bytes::<u32>(bytes).map_err(|_| PullGrantError::Decode)?;
        if version == PULL_GRANT_V1 {
            let v1: SignedPullGrantV1 =
                postcard::from_bytes(bytes).map_err(|_| PullGrantError::Decode)?;
            Ok(Self {
                grant: v1.grant.into_grant(),
                sig: v1.sig,
            })
        } else {
            postcard::from_bytes(bytes).map_err(|_| PullGrantError::Decode)
        }
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
        if g.version != PULL_GRANT_VERSION && g.version != PULL_GRANT_V1 {
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
        if g.expires_unix.saturating_sub(now_unix) > MAX_GRANT_LIFETIME_SECS {
            return Err(PullGrantError::TooLong);
        }
        if g.items.is_empty() || g.items.len() > MAX_PULL_ITEMS {
            return Err(PullGrantError::Count);
        }
        if !g.items.iter().all(|i| rel_path_is_acceptable(&i.rel_path)) {
            return Err(PullGrantError::BadPath);
        }
        let repeated = g
            .endpoints
            .iter()
            .enumerate()
            .any(|(i, e)| g.endpoints[..i].contains(e));
        if g.endpoints.len() > MAX_ENDPOINTS
            || repeated
            || !g.endpoints.iter().all(|e| endpoint_is_acceptable(e))
        {
            return Err(PullGrantError::BadPath);
        }
        Ok(())
    }
}

/// What the source signs: the context, then the grant in its own version's
/// layout.
fn signed_bytes(grant: &PullGrant) -> Result<Vec<u8>, PullGrantError> {
    let body = if grant.version == PULL_GRANT_V1 {
        postcard::to_allocvec(&PullGrantV1::of(grant))
    } else {
        postcard::to_allocvec(grant)
    }
    .map_err(|_| PullGrantError::Decode)?;
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

/// Whether `host` is a bare host name or IP address a grant may name: DNS
/// labels (letters, digits, hyphens, dots), an IPv4 literal, or an IPv6
/// literal with or without brackets. No scheme, port, path or user info.
pub fn host_is_acceptable(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if bare.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// `host:port` for a QUIC endpoint, IPv6 literals bracketed.
pub fn endpoint(host: &str, port: u16) -> String {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if bare.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{bare}]:{port}")
    } else {
        format!("{bare}:{port}")
    }
}

/// Whether an endpoint in a grant is a `host:port` this crate would write.
pub fn endpoint_is_acceptable(endpoint: &str) -> bool {
    match endpoint.rsplit_once(':') {
        Some((host, port)) => {
            port.parse::<u16>().is_ok_and(|p| p != 0)
                && host_is_acceptable(host)
                && (!host.contains(':') || (host.starts_with('[') && host.ends_with(']')))
        }
        None => false,
    }
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

/// What the destination asks on a bulk stream that opens with an empty
/// frame, in the frame after it: something other than a file's bytes. A
/// source from before these reads the empty frame as a bad request and
/// answers [`stream_status::BAD`], so asking never harms an older source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullStreamAsk {
    /// The swarm peers holding one granted file, and a capability for them.
    /// Answered with [`stream_status::OK`] and a [`PullSources`] frame, or
    /// the status a [`PullStreamRequest`] for that item would get
    /// ([`stream_status::OFF`] too when the source does not share its swarm).
    Sources { grant: Vec<u8>, item: u32 },
}

/// A swarm peer holding a granted file: where it listens, as `host:port`,
/// and its certificate's fingerprint, which the fetcher pins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmSource {
    pub endpoint: String,
    pub cert_fp: [u8; 32],
}

/// The source's answer to [`PullStreamAsk::Sources`]: its swarm peers
/// holding the file, and a capability (a `rabbithole_swarm::S2sCapToken`
/// naming the fetching burrow) they accept for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullSources {
    pub token: Vec<u8>,
    pub expires_unix: i64,
    pub sources: Vec<SwarmSource>,
}

/// Most swarm peers one answer names.
pub const MAX_SWARM_SOURCES: usize = 16;

/// Largest encoded [`PullSources`], in bytes.
pub const MAX_SOURCES_ANSWER: usize = 16 * 1024;

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
            tls_fingerprint: [4; 32],
            endpoints: vec!["burrow.example:4653".into()],
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

    /// The first format exactly as releases before version 2 wrote and
    /// signed it, defined here apart from the crate's own copy.
    #[derive(Serialize)]
    struct OldGrant {
        version: u32,
        source_key: [u8; 32],
        fetcher_key: [u8; 32],
        issued_unix: i64,
        expires_unix: i64,
        nonce: [u8; 16],
        items: Vec<PullItem>,
    }

    #[derive(Serialize)]
    struct OldSigned {
        grant: OldGrant,
        sig: Signature,
    }

    #[test]
    fn a_first_format_grant_still_reads_verifies_and_is_written_for_peers() {
        let source = key(1);
        let dest = key(2).public().0;
        let old = OldGrant {
            version: 1,
            source_key: source.public().0,
            fetcher_key: dest,
            issued_unix: 100,
            expires_unix: 700,
            nonce: [9; 16],
            items: vec![item("a", 1)],
        };
        let mut msg = PULL_GRANT_CONTEXT.to_vec();
        msg.extend(postcard::to_allocvec(&old).unwrap());
        let sig = source.sign(&msg);
        let bytes = postcard::to_allocvec(&OldSigned { grant: old, sig }).unwrap();

        // An older source's grant reads here and verifies.
        let read = SignedPullGrant::from_bytes(&bytes).unwrap();
        assert_eq!(read.grant.version, PULL_GRANT_V1);
        assert!(read.grant.endpoints.is_empty());
        assert_eq!(read.check(&source.public().0, &dest, 500), Ok(()));
        // What this crate writes for a peer is byte for byte what an older
        // destination reads.
        assert_eq!(read.to_bytes(), bytes);
        let mut fresh = grant(dest, vec![item("a", 1)]);
        fresh.version = PULL_GRANT_V1;
        fresh.tls_fingerprint = [0; 32];
        fresh.endpoints.clear();
        let signed = fresh.sign(&source).unwrap();
        assert_eq!(signed.to_bytes(), bytes);

        // The first format has nowhere to put addresses.
        let mut mixed = grant(dest, vec![item("a", 1)]);
        mixed.version = PULL_GRANT_V1;
        assert_eq!(mixed.sign(&source).err(), Some(PullGrantError::Version));
        // Nor is a version this crate never wrote read.
        let mut later = grant(dest, vec![item("a", 1)]);
        later.version = 3;
        let later = later.sign(&source).unwrap();
        let back = SignedPullGrant::from_bytes(&later.to_bytes()).unwrap();
        assert_eq!(
            back.check(&source.public().0, &dest, 500),
            Err(PullGrantError::Version)
        );
    }

    #[test]
    fn a_question_on_a_stream_is_never_read_as_a_request_for_bytes() {
        // An older source decodes the empty first frame as a request for a
        // file's bytes, fails, and answers BAD.
        assert!(postcard::from_bytes::<PullStreamRequest>(&[]).is_err());
        let ask = PullStreamAsk::Sources {
            grant: vec![1, 2, 3],
            item: 7,
        };
        let bytes = postcard::to_allocvec(&ask).unwrap();
        assert_eq!(postcard::from_bytes::<PullStreamAsk>(&bytes).unwrap(), ask);
        let most = PullSources {
            token: vec![0; 256],
            expires_unix: i64::MAX,
            sources: vec![
                SwarmSource {
                    endpoint: format!("[{}]:65535", "ffff:".repeat(7) + "ffff"),
                    cert_fp: [0xff; 32],
                };
                MAX_SWARM_SOURCES
            ],
        };
        assert!(postcard::to_allocvec(&most).unwrap().len() <= MAX_SOURCES_ANSWER);
    }

    #[test]
    fn a_grant_made_to_last_or_naming_an_address_twice_is_refused() {
        let source = key(1);
        let dest = key(2).public().0;
        let mut g = grant(dest, vec![item("a", 1)]);
        g.expires_unix = 500 + MAX_GRANT_LIFETIME_SECS + 1;
        let long = g.sign(&source).unwrap();
        assert_eq!(
            long.check(&source.public().0, &dest, 500),
            Err(PullGrantError::TooLong)
        );
        assert_eq!(long.check(&source.public().0, &dest, 501), Ok(()));
        let mut g = grant(dest, vec![item("a", 1)]);
        g.endpoints = vec!["a.example:1".into(), "a.example:1".into()];
        let twice = g.sign(&source).unwrap();
        assert_eq!(
            twice.check(&source.public().0, &dest, 500),
            Err(PullGrantError::BadPath)
        );
    }

    #[test]
    fn a_grant_names_only_hosts_and_ports() {
        for good in [
            "burrow.example",
            "a-b.c",
            "127.0.0.1",
            "::1",
            "[2001:db8::1]",
        ] {
            assert!(host_is_acceptable(good), "{good}");
        }
        for bad in [
            "",
            "http://x",
            "x/y",
            "x:1",
            "-x.example",
            "x..y",
            "user@x",
            "x example",
        ] {
            assert!(!host_is_acceptable(bad), "{bad:?}");
        }
        assert_eq!(endpoint("burrow.example", 4653), "burrow.example:4653");
        assert_eq!(endpoint("::1", 4653), "[::1]:4653");
        assert_eq!(endpoint("[::1]", 4653), "[::1]:4653");
        for good in ["burrow.example:4653", "127.0.0.1:1", "[::1]:4653"] {
            assert!(endpoint_is_acceptable(good), "{good}");
        }
        for bad in [
            "burrow.example",
            "burrow.example:0",
            "::1:4653",
            "x:99999",
            "http://x:1",
        ] {
            assert!(!endpoint_is_acceptable(bad), "{bad}");
        }
        let source = key(1);
        let dest = key(2).public().0;
        let mut g = grant(dest, vec![item("a", 1)]);
        g.endpoints = vec!["http://evil:1".into()];
        let bad = g.sign(&source).unwrap();
        assert_eq!(
            bad.check(&source.public().0, &dest, 500),
            Err(PullGrantError::BadPath)
        );
        let mut g = grant(dest, vec![item("a", 1)]);
        g.endpoints = vec!["a.example:1".into(); MAX_ENDPOINTS + 1];
        let many = g.sign(&source).unwrap();
        assert_eq!(
            many.check(&source.public().0, &dest, 500),
            Err(PullGrantError::BadPath)
        );
        // The fingerprint and addresses are signed: changing them breaks it.
        let mut moved = grant(dest, vec![item("a", 1)]).sign(&source).unwrap();
        moved.grant.endpoints = vec!["elsewhere.example:4653".into()];
        assert_eq!(
            moved.check(&source.public().0, &dest, 500),
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
