//! Pure codecs for the classic Hotline **tracker** protocol (HTRK).
//!
//! HTRK is *not* the Hotline server protocol: servers speak `TRTP`/`HOTL`
//! transactions on 5500 (see `rabbithole-legacy-hotline`), while the tracker
//! has its own, much simpler wire format on 5498/5499. All multi-byte
//! integers are big-endian; all strings are pascal strings (one length byte,
//! then that many bytes).
//!
//! ## Registration heartbeat (server → tracker, one UDP datagram, port 5499)
//!
//! ```text
//! offset  size  field         value
//! ------  ----  ------------  ------------------------------------------
//!   0      2    version       0x0001
//!   2      2    port          TCP port the server accepts clients on
//!   4      2    users online  current user count
//!   6      2    unused        0
//!   8      4    password/id   tracker password / registration id (opaque)
//!  12      1+n  name          pascal string
//!   …      1+n  description   pascal string
//! ```
//!
//! The server's IP is **never** carried in the packet — the tracker uses the
//! observed UDP source address, which is why HTRK listings are IPv4-only.
//!
//! ## Listing session (client ↔ tracker, TCP port 5498)
//!
//! ```text
//! client → tracker:   'H' 'T' 'R' 'K'  version(2) = 0x0001
//! tracker → client:   'H' 'T' 'R' 'K'  version(2) = 0x0001
//! tracker → client:   message type(2) = 0x0001   (server list)
//!                     message size(2)            (bytes after this field)
//!                     server count(2)
//!                     server count(2)            (repeated, historical quirk)
//!                     then per server:
//!                       ip(4)  port(2)  users(2)  unused(2)
//!                       name (pascal)  description (pascal)
//! ```
//!
//! The tracker closes the connection after the full list. Every decoder here
//! is total: bad input yields [`HtrkError`], never a panic.

use std::net::Ipv4Addr;

/// The 4-byte magic opening both sides of a listing session: `HTRK`.
pub const MAGIC: [u8; 4] = *b"HTRK";

/// The HTRK protocol version this tracker speaks: `1`.
pub const VERSION: u16 = 1;

/// Message type `1`: the server list.
pub const MSG_SERVER_LIST: u16 = 1;

/// Wire length of a client hello (`HTRK` + version), in bytes.
pub const HELLO_LEN: usize = 6;

/// Fixed-size prefix of a registration datagram, before the pascal strings.
pub const REGISTRATION_PREFIX_LEN: usize = 12;

/// A total, panic-free HTRK decode error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HtrkError {
    /// Input ended before the structure was complete.
    #[error("truncated HTRK message")]
    Truncated,
    /// A listing session did not open with `HTRK`.
    #[error("bad magic: expected \"HTRK\", got {0:02x?}")]
    BadMagic([u8; 4]),
    /// An unsupported protocol version.
    #[error("unsupported HTRK version {0}")]
    BadVersion(u16),
    /// An unexpected message type in a listing reply.
    #[error("unexpected HTRK message type {0}")]
    BadMessageType(u16),
}

/// A decoded registration heartbeat (the UDP datagram body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// TCP port the registering server accepts clients on.
    pub port: u16,
    /// Users currently online, as reported by the server.
    pub users_online: u16,
    /// Opaque password / registration id bytes (unused by this tracker).
    pub pass_id: [u8; 4],
    /// Server display name.
    pub name: String,
    /// One-line server description.
    pub description: String,
}

impl Registration {
    /// Decodes a registration datagram.
    ///
    /// Strings are decoded lossily (classic senders used MacRoman; invalid
    /// UTF-8 becomes U+FFFD). A datagram that ends cleanly after the name is
    /// accepted with an empty description, since some registrants omit it.
    pub fn decode(buf: &[u8]) -> Result<Self, HtrkError> {
        if buf.len() < REGISTRATION_PREFIX_LEN {
            return Err(HtrkError::Truncated);
        }
        let version = u16::from_be_bytes([buf[0], buf[1]]);
        if version != VERSION {
            return Err(HtrkError::BadVersion(version));
        }
        let port = u16::from_be_bytes([buf[2], buf[3]]);
        let users_online = u16::from_be_bytes([buf[4], buf[5]]);
        // buf[6..8] unused.
        let pass_id = [buf[8], buf[9], buf[10], buf[11]];
        let rest = &buf[REGISTRATION_PREFIX_LEN..];
        let (name, rest) = read_pascal(rest)?;
        let description = if rest.is_empty() {
            String::new()
        } else {
            read_pascal(rest)?.0
        };
        Ok(Self {
            port,
            users_online,
            pass_id,
            name,
            description,
        })
    }

    /// Encodes this registration as a UDP datagram body. Strings longer than
    /// 255 bytes are truncated (on a UTF-8 boundary).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(REGISTRATION_PREFIX_LEN + 2 + 255 + 255);
        out.extend_from_slice(&VERSION.to_be_bytes());
        out.extend_from_slice(&self.port.to_be_bytes());
        out.extend_from_slice(&self.users_online.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&self.pass_id);
        write_pascal(&mut out, &self.name);
        write_pascal(&mut out, &self.description);
        out
    }
}

/// One server record in a listing reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedServer {
    /// IPv4 address of the server (HTRK listings are IPv4-only).
    pub ip: Ipv4Addr,
    /// TCP port the server accepts clients on.
    pub port: u16,
    /// Users currently online.
    pub users_online: u16,
    /// Server display name.
    pub name: String,
    /// One-line server description.
    pub description: String,
}

impl ListedServer {
    fn encoded_len(&self) -> usize {
        4 + 2 + 2 + 2 + 1 + self.name.len().min(255) + 1 + self.description.len().min(255)
    }
}

/// Encodes the 6-byte hello (`HTRK` + version) each side sends.
pub fn encode_hello() -> [u8; HELLO_LEN] {
    let mut out = [0u8; HELLO_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4..].copy_from_slice(&VERSION.to_be_bytes());
    out
}

/// Validates a 6-byte hello (magic + version 1).
pub fn decode_hello(buf: &[u8]) -> Result<(), HtrkError> {
    if buf.len() < HELLO_LEN {
        return Err(HtrkError::Truncated);
    }
    if buf[..4] != MAGIC {
        return Err(HtrkError::BadMagic([buf[0], buf[1], buf[2], buf[3]]));
    }
    let version = u16::from_be_bytes([buf[4], buf[5]]);
    if version != VERSION {
        return Err(HtrkError::BadVersion(version));
    }
    Ok(())
}

/// Encodes a server-list message (type, size, twin counts, records).
///
/// The message-size and count fields are 16-bit, so a pathologically large
/// registry is truncated to whatever prefix of `servers` fits — clients never
/// see a size that lies about the bytes that follow.
pub fn encode_listing(servers: &[ListedServer]) -> Vec<u8> {
    // Message size counts everything after the size field: the two count
    // fields plus the records.
    let mut body_len: usize = 4;
    let mut count: usize = 0;
    for server in servers {
        let len = server.encoded_len();
        if body_len + len > usize::from(u16::MAX) || count == usize::from(u16::MAX) {
            break;
        }
        body_len += len;
        count += 1;
    }

    let mut out = Vec::with_capacity(4 + body_len);
    out.extend_from_slice(&MSG_SERVER_LIST.to_be_bytes());
    out.extend_from_slice(&(body_len as u16).to_be_bytes());
    out.extend_from_slice(&(count as u16).to_be_bytes());
    out.extend_from_slice(&(count as u16).to_be_bytes());
    for server in &servers[..count] {
        out.extend_from_slice(&server.ip.octets());
        out.extend_from_slice(&server.port.to_be_bytes());
        out.extend_from_slice(&server.users_online.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        write_pascal(&mut out, &server.name);
        write_pascal(&mut out, &server.description);
    }
    out
}

/// Decodes a server-list message (the bytes after the hello exchange).
pub fn decode_listing(buf: &[u8]) -> Result<Vec<ListedServer>, HtrkError> {
    if buf.len() < 8 {
        return Err(HtrkError::Truncated);
    }
    let msg_type = u16::from_be_bytes([buf[0], buf[1]]);
    if msg_type != MSG_SERVER_LIST {
        return Err(HtrkError::BadMessageType(msg_type));
    }
    // buf[2..4]: message size (validated implicitly by parsing to count).
    let count = u16::from_be_bytes([buf[4], buf[5]]);
    let mut rest = &buf[8..];
    let mut servers = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        if rest.len() < 10 {
            return Err(HtrkError::Truncated);
        }
        let ip = Ipv4Addr::new(rest[0], rest[1], rest[2], rest[3]);
        let port = u16::from_be_bytes([rest[4], rest[5]]);
        let users_online = u16::from_be_bytes([rest[6], rest[7]]);
        // rest[8..10] unused.
        rest = &rest[10..];
        let (name, after) = read_pascal(rest)?;
        let (description, after) = read_pascal(after)?;
        rest = after;
        servers.push(ListedServer {
            ip,
            port,
            users_online,
            name,
            description,
        });
    }
    Ok(servers)
}

/// Reads a pascal string; returns the (lossily decoded) string and the rest.
fn read_pascal(buf: &[u8]) -> Result<(String, &[u8]), HtrkError> {
    let (&len, rest) = buf.split_first().ok_or(HtrkError::Truncated)?;
    let len = usize::from(len);
    if rest.len() < len {
        return Err(HtrkError::Truncated);
    }
    let (bytes, rest) = rest.split_at(len);
    Ok((String::from_utf8_lossy(bytes).into_owned(), rest))
}

/// Appends a pascal string, truncating to 255 bytes on a UTF-8 boundary.
fn write_pascal(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(255);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = &s.as_bytes()[..end];
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_round_trip() {
        let reg = Registration {
            port: 5500,
            users_online: 42,
            pass_id: [0, 0, 0, 0],
            name: "Wonderland".into(),
            description: "Down the rabbit hole".into(),
        };
        let wire = reg.encode();
        assert_eq!(Registration::decode(&wire).unwrap(), reg);
    }

    #[test]
    fn registration_wire_layout() {
        let reg = Registration {
            port: 0x157C, // 5500
            users_online: 3,
            pass_id: *b"pass",
            name: "A".into(),
            description: "B".into(),
        };
        assert_eq!(
            reg.encode(),
            [
                0x00, 0x01, // version
                0x15, 0x7C, // port
                0x00, 0x03, // users
                0x00, 0x00, // unused
                b'p', b'a', b's', b's', // pass/id
                1, b'A', // name
                1, b'B', // description
            ]
        );
    }

    #[test]
    fn registration_missing_description_is_empty() {
        let reg = Registration {
            port: 5500,
            users_online: 0,
            pass_id: [0; 4],
            name: "NoDesc".into(),
            description: String::new(),
        };
        let mut wire = reg.encode();
        wire.pop(); // drop the trailing zero-length description byte
        assert_eq!(Registration::decode(&wire).unwrap(), reg);
    }

    #[test]
    fn registration_rejects_garbage_without_panicking() {
        assert_eq!(Registration::decode(&[]), Err(HtrkError::Truncated));
        assert_eq!(Registration::decode(&[0xFF; 4]), Err(HtrkError::Truncated));
        assert_eq!(
            Registration::decode(&[0xFF; 64]),
            Err(HtrkError::BadVersion(0xFFFF))
        );
        // Valid prefix but the name length byte overruns the datagram.
        let mut wire = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        wire.push(200); // claims a 200-byte name with no bytes following
        assert_eq!(Registration::decode(&wire), Err(HtrkError::Truncated));
    }

    #[test]
    fn hello_round_trip_and_rejection() {
        let hello = encode_hello();
        assert_eq!(&hello[..4], b"HTRK");
        decode_hello(&hello).unwrap();
        assert_eq!(decode_hello(b"HTRK"), Err(HtrkError::Truncated));
        assert_eq!(
            decode_hello(b"TRTP\x00\x01"),
            Err(HtrkError::BadMagic(*b"TRTP"))
        );
        assert_eq!(decode_hello(b"HTRK\x00\x09"), Err(HtrkError::BadVersion(9)));
    }

    #[test]
    fn listing_round_trip() {
        let servers = vec![
            ListedServer {
                ip: Ipv4Addr::new(203, 0, 113, 7),
                port: 5500,
                users_online: 12,
                name: "Tea Party".into(),
                description: "No room! No room!".into(),
            },
            ListedServer {
                ip: Ipv4Addr::new(198, 51, 100, 2),
                port: 5510,
                users_online: 0,
                name: "Queen's Court".into(),
                description: String::new(),
            },
        ];
        let wire = encode_listing(&servers);
        // type=1, size covers everything after the size field.
        assert_eq!(&wire[..2], &[0x00, 0x01]);
        let size = u16::from_be_bytes([wire[2], wire[3]]);
        assert_eq!(usize::from(size), wire.len() - 4);
        // Twin counts.
        assert_eq!(&wire[4..6], &[0x00, 0x02]);
        assert_eq!(&wire[6..8], &[0x00, 0x02]);
        assert_eq!(decode_listing(&wire).unwrap(), servers);
    }

    #[test]
    fn empty_listing() {
        let wire = encode_listing(&[]);
        assert_eq!(wire, [0x00, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(decode_listing(&wire).unwrap(), Vec::new());
    }

    #[test]
    fn pascal_strings_truncate_to_255_bytes() {
        let reg = Registration {
            port: 1,
            users_online: 0,
            pass_id: [0; 4],
            name: "x".repeat(300),
            description: "é".repeat(200), // 400 bytes; boundary lands mid-char
        };
        let decoded = Registration::decode(&reg.encode()).unwrap();
        assert_eq!(decoded.name.len(), 255);
        assert_eq!(decoded.description, "é".repeat(127)); // 254 bytes
    }

    #[test]
    fn truncated_listing_errors() {
        let servers = vec![ListedServer {
            ip: Ipv4Addr::LOCALHOST,
            port: 5500,
            users_online: 1,
            name: "Short".into(),
            description: "d".into(),
        }];
        let wire = encode_listing(&servers);
        for end in 0..wire.len() {
            assert_eq!(decode_listing(&wire[..end]), Err(HtrkError::Truncated));
        }
    }
}
