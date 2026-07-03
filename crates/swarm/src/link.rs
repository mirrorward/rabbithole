//! `rabbit://` links — shareable, verifiable references into the swarm.
//!
//! Three target shapes, each reachable through one of two authority forms and
//! optionally carrying extra Reticulum (RNS) routes:
//!
//! ```text
//! rabbit://<authority>/files/<area>/<path>[?root=<root-hex>][&via=rns:<dest-hex>]…
//! rabbit://<authority>/manifest/<id-hex>[?via=rns:<dest-hex>]…
//! rabbit://<authority>/blob/<id-hex>[?via=rns:<dest-hex>]…
//!
//! <authority> = host[:port]       an IP home — an origin or introducer to dial
//!             | rns:<dest-hex>    a Reticulum home — the destination hash of a
//!                                 Burrow's RNS destination (no port)
//! ```
//!
//! `<root-hex>`/`<id-hex>` are 64 hex chars (a 32-byte blake3 root or id);
//! `<dest-hex>` is 32 hex chars (a 16-byte RNS [`DestinationHash`]), accepted
//! in either case and always displayed lowercase.
//!
//! The authority names where to *start* looking; the hex-encoded root/id is
//! the integrity anchor, so bytes may ultimately come from any peer and still
//! be checked. Paths are percent-encoded so spaces and non-ASCII names survive
//! a round trip.
//!
//! # Reticulum authorities and `via` routes
//!
//! A link may be *homed* on the mesh (`rabbit://rns:<dest-hex>/…`) or carry
//! RNS destinations as *alternates* next to an IP home
//! (`rabbit://host:port/…?via=rns:<dest-hex>`, repeatable) — a dual-authority
//! link stays resolvable when its IP home is unreachable.
//!
//! **This module defines the link format only; there is no resolver here.**
//! The intended resolution semantics for consumers: dial the IP authority
//! first when present (the fast, interactive path), then fall back to
//! [`RabbitLink::rns_routes`] in order for delay-tolerant retrieval over
//! Reticulum. The Looking Glass advertising RNS destinations for a root —
//! so fresh routes can be discovered rather than carried in the link — is the
//! tracker slice, not this one.
//!
//! # Canonical form and forward compatibility
//!
//! - [`Display`](std::fmt::Display) is canonical: for `files` targets the
//!   `root` parameter comes first, then `via` routes in ascending hash order,
//!   deduplicated, all hex lowercase. Parsing normalizes to the same order, so
//!   parse → display → parse is identity on the parsed value.
//! - Unknown query parameters are tolerated on parse (and dropped on
//!   re-display), so links minted by newer software still resolve here.
//! - `via` values with an unrecognized scheme (anything not `rns:…`) are
//!   likewise tolerated and ignored — a future transport can add routes
//!   without breaking old parsers. A `via=rns:…` value that *claims* to be an
//!   RNS hash but is malformed is an error, not ignored.
//! - The authority prefix `rns:` (ASCII case-insensitive) is **reserved**: a
//!   host literally named `rns` still parses when written without a port
//!   (`rabbit://rns/...`), but `rns:<anything>` is always interpreted as an
//!   RNS authority and `<anything>` must be a valid destination hash.

use crate::manifest::CHUNK_SIZE;

pub use rabbithole_reticulum::{DestinationHash, DestinationHashError};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LinkError {
    #[error("not a rabbit:// url")]
    Scheme,
    #[error("missing or invalid host")]
    Authority,
    #[error("missing or malformed target")]
    Target,
    #[error("unknown link kind: {0}")]
    UnknownKind(String),
    #[error("bad hex (expected 32-byte blake3 hash)")]
    Hex,
    #[error("rns authority: {0}")]
    RnsAuthority(DestinationHashError),
    #[error("via=rns route: {0}")]
    RnsVia(DestinationHashError),
}

/// Where a link is homed — the authority component of the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAuthority {
    /// An IP home: a DNS name or address to dial, with an optional port.
    Host { host: String, port: Option<u16> },
    /// A Reticulum home: the 16-byte destination hash of an RNS destination.
    Rns(DestinationHash),
}

impl std::fmt::Display for LinkAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host { host, port } => {
                f.write_str(host)?;
                if let Some(p) = port {
                    write!(f, ":{p}")?;
                }
                Ok(())
            }
            Self::Rns(dest) => write!(f, "rns:{dest}"),
        }
    }
}

/// What a link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A single file by area + path, optionally pinned to its blake3 root.
    File {
        area: String,
        path: String,
        root: Option<[u8; 32]>,
    },
    /// A whole fileset by manifest id.
    Manifest([u8; 32]),
    /// A raw blob by id.
    Blob([u8; 32]),
}

/// A parsed `rabbit://` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RabbitLink {
    pub authority: LinkAuthority,
    pub target: LinkTarget,
    /// RNS alternates from `via=rns:<hex>` query params — kept sorted and
    /// deduplicated so equality and [`Display`](std::fmt::Display) are
    /// canonical. Private so the invariant holds; read via
    /// [`Self::rns_via`] / [`Self::rns_routes`], extend via
    /// [`Self::with_rns_alternate`].
    rns_via: Vec<DestinationHash>,
}

impl RabbitLink {
    /// The swarm chunk size these links' roots are computed with.
    pub const CHUNK_SIZE: u32 = CHUNK_SIZE;

    fn new(authority: LinkAuthority, target: LinkTarget) -> Self {
        Self {
            authority,
            target,
            rns_via: Vec::new(),
        }
    }

    fn host_authority(host: impl Into<String>, port: Option<u16>) -> LinkAuthority {
        LinkAuthority::Host {
            host: host.into(),
            port,
        }
    }

    pub fn manifest(host: impl Into<String>, port: Option<u16>, id: [u8; 32]) -> Self {
        Self::new(Self::host_authority(host, port), LinkTarget::Manifest(id))
    }

    pub fn blob(host: impl Into<String>, port: Option<u16>, id: [u8; 32]) -> Self {
        Self::new(Self::host_authority(host, port), LinkTarget::Blob(id))
    }

    pub fn file(
        host: impl Into<String>,
        port: Option<u16>,
        area: impl Into<String>,
        path: impl Into<String>,
        root: Option<[u8; 32]>,
    ) -> Self {
        Self::new(
            Self::host_authority(host, port),
            LinkTarget::File {
                area: area.into(),
                path: path.into(),
                root,
            },
        )
    }

    /// A manifest link homed on the Reticulum mesh.
    pub fn manifest_rns(dest: DestinationHash, id: [u8; 32]) -> Self {
        Self::new(LinkAuthority::Rns(dest), LinkTarget::Manifest(id))
    }

    /// A blob link homed on the Reticulum mesh.
    pub fn blob_rns(dest: DestinationHash, id: [u8; 32]) -> Self {
        Self::new(LinkAuthority::Rns(dest), LinkTarget::Blob(id))
    }

    /// A file link homed on the Reticulum mesh.
    pub fn file_rns(
        dest: DestinationHash,
        area: impl Into<String>,
        path: impl Into<String>,
        root: Option<[u8; 32]>,
    ) -> Self {
        Self::new(
            LinkAuthority::Rns(dest),
            LinkTarget::File {
                area: area.into(),
                path: path.into(),
                root,
            },
        )
    }

    /// The IP host to dial, if this link has an IP home.
    pub fn host(&self) -> Option<&str> {
        match &self.authority {
            LinkAuthority::Host { host, .. } => Some(host),
            LinkAuthority::Rns(_) => None,
        }
    }

    /// The explicit port of the IP home, if any.
    pub fn port(&self) -> Option<u16> {
        match &self.authority {
            LinkAuthority::Host { port, .. } => *port,
            LinkAuthority::Rns(_) => None,
        }
    }

    /// The RNS destination this link is homed on, if its authority is `rns:`.
    pub fn rns_destination(&self) -> Option<DestinationHash> {
        match &self.authority {
            LinkAuthority::Host { .. } => None,
            LinkAuthority::Rns(dest) => Some(*dest),
        }
    }

    /// The `via=rns:` alternates carried by this link (sorted, deduplicated).
    pub fn rns_via(&self) -> &[DestinationHash] {
        &self.rns_via
    }

    /// Every RNS destination this link can be resolved through, in preference
    /// order: the `rns:` authority first (when present), then the `via`
    /// alternates, with no duplicates.
    pub fn rns_routes(&self) -> Vec<DestinationHash> {
        let mut routes = Vec::with_capacity(self.rns_via.len() + 1);
        if let LinkAuthority::Rns(dest) = &self.authority {
            routes.push(*dest);
        }
        for via in &self.rns_via {
            if !routes.contains(via) {
                routes.push(*via);
            }
        }
        routes
    }

    /// Add an RNS alternate route (`?via=rns:<hex>`); keeps the list sorted
    /// and deduplicated, so adding the same hash twice is a no-op.
    #[must_use]
    pub fn with_rns_alternate(mut self, dest: DestinationHash) -> Self {
        if let Err(at) = self.rns_via.binary_search(&dest) {
            self.rns_via.insert(at, dest);
        }
        self
    }

    /// Parse a `rabbit://` link.
    pub fn parse(s: &str) -> Result<Self, LinkError> {
        let rest = s.strip_prefix("rabbit://").ok_or(LinkError::Scheme)?;
        let (authority, pathq) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => return Err(LinkError::Target),
        };
        let authority = parse_authority(authority)?;
        if pathq.is_empty() {
            return Err(LinkError::Target);
        }
        let (path, query) = match pathq.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (pathq, None),
        };
        let mut segs = path.split('/');
        let kind = segs.next().unwrap_or("");
        let target = match kind {
            "manifest" => LinkTarget::Manifest(hex32(segs.next().unwrap_or(""))?),
            "blob" => LinkTarget::Blob(hex32(segs.next().unwrap_or(""))?),
            "files" => {
                let area = segs
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or(LinkError::Target)?;
                let rel: Vec<&str> = segs.filter(|s| !s.is_empty()).collect();
                if rel.is_empty() {
                    return Err(LinkError::Target);
                }
                let root = match query {
                    Some(q) => query_root(q)?,
                    None => None,
                };
                LinkTarget::File {
                    area: pct_decode(area),
                    path: pct_decode(&rel.join("/")),
                    root,
                }
            }
            other => return Err(LinkError::UnknownKind(other.to_string())),
        };
        let rns_via = match query {
            Some(q) => query_via(q)?,
            None => Vec::new(),
        };
        Ok(Self {
            authority,
            target,
            rns_via,
        })
    }
}

impl std::fmt::Display for RabbitLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rabbit://{}", self.authority)?;
        match &self.target {
            LinkTarget::Manifest(id) => write!(f, "/manifest/{}", hex::encode(id))?,
            LinkTarget::Blob(id) => write!(f, "/blob/{}", hex::encode(id))?,
            LinkTarget::File { area, path, .. } => {
                write!(f, "/files/{}/{}", pct_encode(area), pct_encode(path))?;
            }
        }
        // Canonical query: `root` first (files only), then `via` routes in
        // ascending hash order (the field invariant).
        let mut pairs = Vec::with_capacity(self.rns_via.len() + 1);
        if let LinkTarget::File { root: Some(r), .. } = &self.target {
            pairs.push(format!("root={}", hex::encode(r)));
        }
        for via in &self.rns_via {
            pairs.push(format!("via=rns:{via}"));
        }
        if !pairs.is_empty() {
            write!(f, "?{}", pairs.join("&"))?;
        }
        Ok(())
    }
}

/// Strip an ASCII-case-insensitive `rns:` scheme prefix.
fn strip_rns_scheme(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.len() >= 4 && bytes[..4].eq_ignore_ascii_case(b"rns:") {
        // `bytes[3]` is ASCII `:`, so byte 4 is a char boundary.
        Some(&s[4..])
    } else {
        None
    }
}

fn parse_authority(a: &str) -> Result<LinkAuthority, LinkError> {
    if a.is_empty() {
        return Err(LinkError::Authority);
    }
    if let Some(hash) = strip_rns_scheme(a) {
        // Reserved prefix: everything after `rns:` must be a destination
        // hash — 32 hex chars, no port.
        let dest = hash.parse().map_err(LinkError::RnsAuthority)?;
        return Ok(LinkAuthority::Rns(dest));
    }
    match a.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port.parse().map_err(|_| LinkError::Authority)?;
            if host.is_empty() {
                return Err(LinkError::Authority);
            }
            Ok(LinkAuthority::Host {
                host: host.to_string(),
                port: Some(port),
            })
        }
        None => Ok(LinkAuthority::Host {
            host: a.to_string(),
            port: None,
        }),
    }
}

/// Find `root=<hex>` among `&`-separated query params (first one wins;
/// other params are ignored here for forward compatibility).
fn query_root(q: &str) -> Result<Option<[u8; 32]>, LinkError> {
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("root=") {
            return Ok(Some(hex32(v)?));
        }
    }
    Ok(None)
}

/// Collect every `via=rns:<hex>` route among `&`-separated query params,
/// sorted and deduplicated. `via` values with any other scheme are ignored
/// (forward compatibility); a malformed `rns:` value is an error.
fn query_via(q: &str) -> Result<Vec<DestinationHash>, LinkError> {
    let mut via = Vec::new();
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("via=") {
            if let Some(hash) = strip_rns_scheme(v) {
                via.push(hash.parse().map_err(LinkError::RnsVia)?);
            }
        }
    }
    via.sort_unstable();
    via.dedup();
    Ok(via)
}

fn hex32(s: &str) -> Result<[u8; 32], LinkError> {
    let bytes = hex::decode(s).map_err(|_| LinkError::Hex)?;
    bytes.try_into().map_err(|_| LinkError::Hex)
}

/// Percent-encode everything outside the unreserved set, leaving `/` literal
/// so path structure survives.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn roundtrip(link: RabbitLink) {
        let s = link.to_string();
        assert_eq!(RabbitLink::parse(&s).unwrap(), link, "roundtrip via {s}");
    }

    #[test]
    fn manifest_and_blob_roundtrip() {
        roundtrip(RabbitLink::manifest("warren.example", Some(4653), id(7)));
        roundtrip(RabbitLink::blob("warren.example", None, id(9)));
    }

    #[test]
    fn file_roundtrip_with_and_without_root() {
        roundtrip(RabbitLink::file(
            "h",
            Some(4653),
            "warez",
            "iso/big.bin",
            Some(id(3)),
        ));
        roundtrip(RabbitLink::file("h", None, "warez", "readme.txt", None));
    }

    #[test]
    fn file_path_with_spaces_and_unicode() {
        let link = RabbitLink::file("h", None, "warez", "cool games/pÖng.zip", Some(id(1)));
        let s = link.to_string();
        assert!(s.contains("%20"), "space is percent-encoded: {s}");
        assert_eq!(RabbitLink::parse(&s).unwrap(), link);
    }

    #[test]
    fn concrete_format() {
        assert_eq!(
            RabbitLink::manifest("h", Some(4653), id(0)).to_string(),
            format!("rabbit://h:4653/manifest/{}", hex::encode(id(0)))
        );
        assert_eq!(
            RabbitLink::file("h", None, "a", "b.txt", None).to_string(),
            "rabbit://h/files/a/b.txt"
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(RabbitLink::parse("http://h/blob/x"), Err(LinkError::Scheme));
        assert_eq!(RabbitLink::parse("rabbit://h"), Err(LinkError::Target));
        assert_eq!(
            RabbitLink::parse("rabbit://h/manifest/zz"),
            Err(LinkError::Hex)
        );
        assert!(matches!(
            RabbitLink::parse("rabbit://h/wat/x"),
            Err(LinkError::UnknownKind(_))
        ));
        assert_eq!(
            RabbitLink::parse("rabbit:///blob/x"),
            Err(LinkError::Authority)
        );
        assert_eq!(
            RabbitLink::parse("rabbit://h/files/onlyarea"),
            Err(LinkError::Target)
        );
    }

    #[test]
    fn port_parsing() {
        let l = RabbitLink::parse("rabbit://host:4653/blob/{}").err();
        assert_eq!(l, Some(LinkError::Hex)); // host/port fine, hex bad
        assert_eq!(
            RabbitLink::parse("rabbit://host:notaport/blob/aa"),
            Err(LinkError::Authority)
        );
    }

    // ---- Wave 14: RNS authorities and via routes -------------------------

    fn dh(n: u8) -> DestinationHash {
        DestinationHash::from([n; 16])
    }

    #[test]
    fn old_form_links_parse_to_host_authority() {
        // Regression sweep: pre-RNS link strings keep their exact meaning.
        let l = RabbitLink::parse(&format!(
            "rabbit://warren.example:4653/files/warez/pong.zip?root={}",
            hex::encode(id(3))
        ))
        .unwrap();
        assert_eq!(l.host(), Some("warren.example"));
        assert_eq!(l.port(), Some(4653));
        assert_eq!(l.rns_destination(), None);
        assert!(l.rns_via().is_empty());
        assert!(l.rns_routes().is_empty());
        assert_eq!(
            l.authority,
            LinkAuthority::Host {
                host: "warren.example".into(),
                port: Some(4653),
            }
        );

        let m = RabbitLink::parse(&format!("rabbit://h/manifest/{}", hex::encode(id(1)))).unwrap();
        assert_eq!(m, RabbitLink::manifest("h", None, id(1)));
    }

    #[test]
    fn rns_authority_roundtrip() {
        roundtrip(RabbitLink::manifest_rns(dh(7), id(1)));
        roundtrip(RabbitLink::blob_rns(dh(9), id(2)));
        roundtrip(RabbitLink::file_rns(
            dh(1),
            "warez",
            "iso/big.bin",
            Some(id(3)),
        ));
        roundtrip(RabbitLink::file_rns(dh(2), "warez", "readme.txt", None));
    }

    #[test]
    fn rns_concrete_format() {
        let link = RabbitLink::manifest_rns(dh(0xA5), id(0));
        assert_eq!(
            link.to_string(),
            format!(
                "rabbit://rns:{}/manifest/{}",
                "a5".repeat(16),
                hex::encode(id(0))
            )
        );
        assert_eq!(link.host(), None);
        assert_eq!(link.port(), None);
        assert_eq!(link.rns_destination(), Some(dh(0xA5)));
        assert_eq!(link.rns_routes(), vec![dh(0xA5)]);
    }

    #[test]
    fn rns_authority_case_normalizes() {
        let upper = format!(
            "rabbit://RNS:{}/blob/{}",
            "AB".repeat(16),
            hex::encode(id(1))
        );
        let link = RabbitLink::parse(&upper).unwrap();
        assert_eq!(link.rns_destination(), Some(dh(0xAB)));
        // Display is canonical lowercase.
        let canon = link.to_string();
        assert_eq!(
            canon,
            format!(
                "rabbit://rns:{}/blob/{}",
                "ab".repeat(16),
                hex::encode(id(1))
            )
        );
        assert_eq!(RabbitLink::parse(&canon).unwrap(), link);
    }

    #[test]
    fn rns_authority_rejects_bad_hashes() {
        let blob = format!("/blob/{}", hex::encode(id(1)));
        // Wrong lengths, with the specific byte count reported.
        assert_eq!(
            RabbitLink::parse(&format!("rabbit://rns:abcd{blob}")),
            Err(LinkError::RnsAuthority(DestinationHashError::BadLength(4)))
        );
        assert_eq!(
            RabbitLink::parse(&format!("rabbit://rns:{blob}")),
            Err(LinkError::RnsAuthority(DestinationHashError::BadLength(0)))
        );
        // A port after the hash is not allowed — the tail is counted as hash.
        assert_eq!(
            RabbitLink::parse(&format!("rabbit://rns:{}:4653{blob}", "ab".repeat(16))),
            Err(LinkError::RnsAuthority(DestinationHashError::BadLength(37)))
        );
        // Non-hex, with the offending offset.
        let mut nonhex = "0".repeat(31);
        nonhex.push('g');
        assert_eq!(
            RabbitLink::parse(&format!("rabbit://rns:{nonhex}{blob}")),
            Err(LinkError::RnsAuthority(DestinationHashError::BadChar(31)))
        );
    }

    #[test]
    fn rns_prefix_is_reserved_but_bare_rns_host_still_parses() {
        // A host literally named `rns` (no port) is still an IP authority…
        let l = RabbitLink::parse(&format!("rabbit://rns/blob/{}", hex::encode(id(1)))).unwrap();
        assert_eq!(l.host(), Some("rns"));
        // …but `rns:<anything>` is always claimed by the RNS form, so a host
        // named `rns` can no longer carry an explicit port.
        assert_eq!(
            RabbitLink::parse(&format!("rabbit://rns:4653/blob/{}", hex::encode(id(1)))),
            Err(LinkError::RnsAuthority(DestinationHashError::BadLength(4)))
        );
    }

    #[test]
    fn dual_authority_roundtrip_all_kinds() {
        roundtrip(RabbitLink::manifest("h", Some(4653), id(7)).with_rns_alternate(dh(1)));
        roundtrip(RabbitLink::blob("h", None, id(9)).with_rns_alternate(dh(2)));
        roundtrip(
            RabbitLink::file("h", Some(1), "warez", "iso/big.bin", Some(id(3)))
                .with_rns_alternate(dh(4))
                .with_rns_alternate(dh(5)),
        );
        // RNS-homed links can carry alternates too.
        roundtrip(RabbitLink::manifest_rns(dh(7), id(1)).with_rns_alternate(dh(8)));
    }

    #[test]
    fn via_display_is_canonical_root_first_then_sorted() {
        let link = RabbitLink::file("h", None, "a", "b.txt", Some(id(1)))
            .with_rns_alternate(dh(9))
            .with_rns_alternate(dh(2));
        assert_eq!(
            link.to_string(),
            format!(
                "rabbit://h/files/a/b.txt?root={}&via=rns:{}&via=rns:{}",
                hex::encode(id(1)),
                "02".repeat(16),
                "09".repeat(16)
            )
        );
    }

    #[test]
    fn via_parse_sorts_dedupes_and_case_normalizes() {
        let s = format!(
            "rabbit://h/blob/{}?via=rns:{}&via=RNS:{}&via=rns:{}",
            hex::encode(id(1)),
            "0b".repeat(16), // b, out of order
            "0A".repeat(16), // a, uppercase scheme + hex
            "0b".repeat(16), // duplicate of b
        );
        let link = RabbitLink::parse(&s).unwrap();
        assert_eq!(link.rns_via(), &[dh(0x0A), dh(0x0B)]);
        assert_eq!(
            link,
            RabbitLink::blob("h", None, id(1))
                .with_rns_alternate(dh(0x0B))
                .with_rns_alternate(dh(0x0A))
        );
        // Canonical display round-trips to itself.
        let canon = link.to_string();
        assert_eq!(RabbitLink::parse(&canon).unwrap().to_string(), canon);
    }

    #[test]
    fn via_rejects_malformed_rns_values() {
        let blob = format!("rabbit://h/blob/{}", hex::encode(id(1)));
        assert_eq!(
            RabbitLink::parse(&format!("{blob}?via=rns:12")),
            Err(LinkError::RnsVia(DestinationHashError::BadLength(2)))
        );
        assert_eq!(
            RabbitLink::parse(&format!("{blob}?via=rns:")),
            Err(LinkError::RnsVia(DestinationHashError::BadLength(0)))
        );
        let mut nonhex = "g".to_string();
        nonhex.push_str(&"0".repeat(31));
        assert_eq!(
            RabbitLink::parse(&format!("{blob}?via=rns:{nonhex}")),
            Err(LinkError::RnsVia(DestinationHashError::BadChar(0)))
        );
    }

    #[test]
    fn unknown_query_params_and_via_schemes_are_tolerated() {
        // Unknown params before/after known ones, unknown via schemes, and
        // valueless params: all ignored, known ones still honored.
        let s = format!(
            "rabbit://h/files/a/b.txt?x=1&root={}&via=lora:deadbeef&via=&via=rns:{}&flag",
            hex::encode(id(2)),
            "07".repeat(16),
        );
        let link = RabbitLink::parse(&s).unwrap();
        assert_eq!(
            link,
            RabbitLink::file("h", None, "a", "b.txt", Some(id(2))).with_rns_alternate(dh(7))
        );
        // Manifest/blob links keep ignoring queries they don't understand.
        let m = RabbitLink::parse(&format!(
            "rabbit://h/manifest/{}?future=stuff",
            hex::encode(id(1))
        ))
        .unwrap();
        assert_eq!(m, RabbitLink::manifest("h", None, id(1)));
    }

    #[test]
    fn with_rns_alternate_dedupes_and_rns_routes_orders() {
        let link = RabbitLink::manifest("h", None, id(1))
            .with_rns_alternate(dh(3))
            .with_rns_alternate(dh(3))
            .with_rns_alternate(dh(1));
        assert_eq!(link.rns_via(), &[dh(1), dh(3)]);
        assert_eq!(link.rns_routes(), vec![dh(1), dh(3)]);

        // An RNS authority leads the route list and is not repeated even if
        // it also appears as a via alternate.
        let s = format!(
            "rabbit://rns:{}/manifest/{}?via=rns:{}&via=rns:{}",
            "0c".repeat(16),
            hex::encode(id(1)),
            "0a".repeat(16),
            "0c".repeat(16),
        );
        let rns = RabbitLink::parse(&s).unwrap();
        assert_eq!(rns.rns_via(), &[dh(0x0A), dh(0x0C)]);
        assert_eq!(rns.rns_routes(), vec![dh(0x0C), dh(0x0A)]);
    }

    #[test]
    fn via_and_percent_encoded_paths_interplay() {
        let link = RabbitLink::file("h", Some(4653), "warez", "cool games/pÖng.zip", Some(id(1)))
            .with_rns_alternate(dh(0xEE));
        let s = link.to_string();
        assert!(s.contains("%20"), "space still percent-encoded: {s}");
        assert!(s.contains("via=rns:"), "via emitted: {s}");
        assert_eq!(RabbitLink::parse(&s).unwrap(), link);
    }

    #[test]
    fn parse_is_total_and_canonical_display_reparses() {
        // Deterministic fuzz-ish sweep: single-byte mutations of valid links
        // plus LCG noise. Parsing must never panic, and any accepted string
        // must display to a canonical form that reparses to the same value
        // (and is a display fixed point).
        let corpus = [
            format!("rabbit://h:4653/manifest/{}", hex::encode(id(7))),
            format!(
                "rabbit://rns:{}/blob/{}",
                "a5".repeat(16),
                hex::encode(id(9))
            ),
            format!(
                "rabbit://warren.example/files/warez/pong.zip?root={}&via=rns:{}",
                hex::encode(id(3)),
                "07".repeat(16)
            ),
        ];
        let mutations = [b'%', b'?', b'&', b'/', b':', b'=', b'g', b'A', 0u8, 0xFF];
        let check = |s: &str| {
            if let Ok(link) = RabbitLink::parse(s) {
                let canon = link.to_string();
                let reparsed = RabbitLink::parse(&canon)
                    .unwrap_or_else(|e| panic!("canonical form must reparse: {canon} ({e})"));
                assert_eq!(reparsed, link, "value stable via {canon}");
                assert_eq!(reparsed.to_string(), canon, "display fixed point");
            }
        };
        for base in &corpus {
            check(base);
            let bytes = base.as_bytes();
            for i in 0..bytes.len() {
                for &m in &mutations {
                    let mut v = bytes.to_vec();
                    v[i] = m;
                    check(&String::from_utf8_lossy(&v));
                }
            }
        }
        // Plain noise: never panic on arbitrary short ASCII.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state
        };
        for _ in 0..2000 {
            let len = (next() % 48) as usize;
            let s: String = (0..len)
                .map(|_| ((next() >> 33) % 128) as u8 as char)
                .collect();
            let _ = RabbitLink::parse(&s);
            check(&format!("rabbit://{s}"));
        }
    }
}
