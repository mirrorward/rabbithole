//! `rabbit://` links — shareable, verifiable references into the swarm.
//!
//! Three shapes, all pinning enough to verify what comes back:
//!
//! ```text
//! rabbit://host[:port]/files/<area>/<path>[?root=<hex>]   one file (root optional but recommended)
//! rabbit://host[:port]/manifest/<id-hex>                  a whole fileset
//! rabbit://host[:port]/blob/<id-hex>                      a raw content-addressed blob
//! ```
//!
//! `host` names where to *start* looking (an origin or introducer); the
//! hex-encoded root/id is the integrity anchor, so bytes may ultimately come
//! from any peer and still be checked. Paths are percent-encoded so spaces
//! and non-ASCII names survive a round trip.

use crate::manifest::CHUNK_SIZE;

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
    pub host: String,
    pub port: Option<u16>,
    pub target: LinkTarget,
}

impl RabbitLink {
    /// The swarm chunk size these links' roots are computed with.
    pub const CHUNK_SIZE: u32 = CHUNK_SIZE;

    pub fn manifest(host: impl Into<String>, port: Option<u16>, id: [u8; 32]) -> Self {
        Self {
            host: host.into(),
            port,
            target: LinkTarget::Manifest(id),
        }
    }

    pub fn blob(host: impl Into<String>, port: Option<u16>, id: [u8; 32]) -> Self {
        Self {
            host: host.into(),
            port,
            target: LinkTarget::Blob(id),
        }
    }

    pub fn file(
        host: impl Into<String>,
        port: Option<u16>,
        area: impl Into<String>,
        path: impl Into<String>,
        root: Option<[u8; 32]>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            target: LinkTarget::File {
                area: area.into(),
                path: path.into(),
                root,
            },
        }
    }

    /// Parse a `rabbit://` link.
    pub fn parse(s: &str) -> Result<Self, LinkError> {
        let rest = s.strip_prefix("rabbit://").ok_or(LinkError::Scheme)?;
        let (authority, pathq) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => return Err(LinkError::Target),
        };
        let (host, port) = parse_authority(authority)?;
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
        Ok(Self { host, port, target })
    }
}

impl std::fmt::Display for RabbitLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rabbit://{}", self.host)?;
        if let Some(p) = self.port {
            write!(f, ":{p}")?;
        }
        match &self.target {
            LinkTarget::Manifest(id) => write!(f, "/manifest/{}", hex::encode(id)),
            LinkTarget::Blob(id) => write!(f, "/blob/{}", hex::encode(id)),
            LinkTarget::File { area, path, root } => {
                write!(f, "/files/{}/{}", pct_encode(area), pct_encode(path))?;
                if let Some(r) = root {
                    write!(f, "?root={}", hex::encode(r))?;
                }
                Ok(())
            }
        }
    }
}

fn parse_authority(a: &str) -> Result<(String, Option<u16>), LinkError> {
    if a.is_empty() {
        return Err(LinkError::Authority);
    }
    match a.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port.parse().map_err(|_| LinkError::Authority)?;
            if host.is_empty() {
                return Err(LinkError::Authority);
            }
            Ok((host.to_string(), Some(port)))
        }
        None => Ok((a.to_string(), None)),
    }
}

/// Find `root=<hex>` among `&`-separated query params.
fn query_root(q: &str) -> Result<Option<[u8; 32]>, LinkError> {
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("root=") {
            return Ok(Some(hex32(v)?));
        }
    }
    Ok(None)
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
}
