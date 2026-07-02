//! Tracker-to-tracker gossip: pure anti-entropy model + UDP wire codec.
//!
//! Trackers with a static peer list (`--gossip-peer`) exchange **signed**
//! descriptors so a server announcing to one tracker appears on all of them.
//! Only signed entries travel — a tracker cannot vouch for an unsigned HTRK
//! heartbeat beyond its own observation, so those stay local.
//!
//! ## The exchange (push–pull anti-entropy)
//!
//! On a timer, each tracker sends a [`GossipDigest`] — a compact list of
//! `(addr, key, timestamp)` for the signed entries it holds — to every peer.
//! A tracker receiving a digest answers with up to two messages:
//!
//! - a [`Want`] (pull): the addresses where the sender knows something newer
//!   than we hold, computed by [`diff`]; the sender answers with a
//!   [`GossipBatch`] of the signed descriptors;
//! - a [`GossipBatch`] (push): descriptors *we* hold that the digest shows
//!   the sender is missing — no extra round trip.
//!
//! Loop safety is structural: a digest never triggers a digest (so two
//! stubborn trackers can't storm each other), [`batch_for`] never includes an
//! entry learned *from* the peer being served (the `via` marker), and every
//! message is capped — digests to [`MAX_DIGEST_ENTRIES`], wants to
//! [`MAX_WANT_ENTRIES`], batches to [`MAX_GOSSIP_DATAGRAM`] encoded bytes.
//! Gossiped entries carry the registry's normal TTL: they expire unless
//! re-gossiped. Convergence for registries larger than a digest is
//! best-effort (the digest covers a name-ordered prefix); good enough for a
//! retro directory, revisit with sampling if it ever matters.
//!
//! We chose **UDP** (sharing nothing with the HTRK sockets; default port
//! 4656) in the classic tracker spirit: every message fits one datagram,
//! loss is harmless (the next tick repeats), and no connection state can be
//! exhausted. The same socket accepts a fourth message, [`Announce`]: a
//! server submitting its own signed descriptor directly — the signed
//! counterpart of the HTRK heartbeat.
//!
//! ## Wire format (one message per datagram)
//!
//! ```text
//! offset  size  field      value
//! ------  ----  ---------  --------------------------------------------
//!   0      4    magic      "RHGS"
//!   4      1    version    0x01
//!   5      1    type       postcard enum tag: 0=digest, 1=want,
//!                          2=batch, 3=announce
//!   6      n    payload    postcard body of the variant
//! ```
//!
//! Every decoder is total: malformed or truncated input yields
//! [`GossipError`], never a panic.
//!
//! [`Announce`]: GossipMessage::Announce

use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::descriptor::SignedDescriptor;
use crate::registry::ServerEntry;

/// The 4-byte magic opening every gossip datagram: `RHGS`.
pub const GOSSIP_MAGIC: [u8; 4] = *b"RHGS";

/// The gossip protocol version this tracker speaks: `1`.
pub const GOSSIP_VERSION: u8 = 1;

/// Wire length of the header (magic + version), in bytes.
pub const GOSSIP_HEADER_LEN: usize = 5;

/// Largest gossip datagram we build (conservative single-MTU budget).
pub const MAX_GOSSIP_DATAGRAM: usize = 1200;

/// Most entries a digest advertises per exchange.
pub const MAX_DIGEST_ENTRIES: usize = 16;

/// Most addresses a want requests per exchange.
pub const MAX_WANT_ENTRIES: usize = 16;

/// A total, panic-free gossip decode error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GossipError {
    /// Input ended before the header was complete.
    #[error("truncated gossip datagram")]
    Truncated,
    /// The datagram did not open with `RHGS`.
    #[error("bad magic: expected \"RHGS\", got {0:02x?}")]
    BadMagic([u8; 4]),
    /// An unsupported protocol version.
    #[error("unsupported gossip version {0}")]
    BadVersion(u8),
    /// The payload did not decode as any known message.
    #[error("malformed gossip payload")]
    BadPayload,
}

/// One line of a digest: what we hold for `addr` and how fresh it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestEntry {
    /// The listing slot (the descriptor's declared address).
    pub addr: SocketAddr,
    /// The verified server key holding that slot.
    pub server_key: [u8; 32],
    /// The descriptor's timestamp — its gossip generation.
    pub timestamp: i64,
}

/// A compact statement of the signed entries a tracker knows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GossipDigest {
    /// One entry per live signed listing, capped at [`MAX_DIGEST_ENTRIES`].
    pub entries: Vec<DigestEntry>,
}

/// The addresses one side wants full descriptors for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Want {
    /// Slots to send, capped at [`MAX_WANT_ENTRIES`].
    pub addrs: Vec<SocketAddr>,
}

impl Want {
    /// True when nothing is wanted (no reply needed).
    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }
}

/// Signed descriptors in flight between trackers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GossipBatch {
    /// Verified-at-origin descriptors; the receiver re-verifies each one.
    pub descriptors: Vec<SignedDescriptor>,
}

impl GossipBatch {
    /// True when the batch carries nothing (no send needed).
    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
}

/// Every message the gossip socket speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GossipMessage {
    /// "Here is what I hold" (tracker → tracker, on a timer).
    Digest(GossipDigest),
    /// "Send me these" (reply to a digest).
    Want(Want),
    /// "Here they are" (reply to a want, or an unsolicited push).
    Batch(GossipBatch),
    /// A server submitting its own signed descriptor directly.
    Announce(Box<SignedDescriptor>),
}

impl GossipMessage {
    /// Encodes header + postcard payload as one datagram body.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAX_GOSSIP_DATAGRAM);
        out.extend_from_slice(&GOSSIP_MAGIC);
        out.push(GOSSIP_VERSION);
        out.extend(postcard::to_allocvec(self).expect("gossip message serializes"));
        out
    }

    /// Decodes a datagram body. Total: bad input errors, never panics.
    pub fn decode(buf: &[u8]) -> Result<Self, GossipError> {
        if buf.len() < 4 {
            return Err(GossipError::Truncated);
        }
        let magic = [buf[0], buf[1], buf[2], buf[3]];
        if magic != GOSSIP_MAGIC {
            return Err(GossipError::BadMagic(magic));
        }
        if buf.len() < GOSSIP_HEADER_LEN {
            return Err(GossipError::Truncated);
        }
        if buf[4] != GOSSIP_VERSION {
            return Err(GossipError::BadVersion(buf[4]));
        }
        postcard::from_bytes(&buf[GOSSIP_HEADER_LEN..]).map_err(|_| GossipError::BadPayload)
    }
}

/// Builds the digest for a registry snapshot: signed entries only, capped at
/// [`MAX_DIGEST_ENTRIES`] (the snapshot's stable name order picks the prefix).
pub fn digest_of(entries: &[ServerEntry]) -> GossipDigest {
    let entries = entries
        .iter()
        .filter_map(|e| {
            Some(DigestEntry {
                addr: e.addr,
                server_key: e.server_key()?,
                timestamp: e.timestamp()?,
            })
        })
        .take(MAX_DIGEST_ENTRIES)
        .collect();
    GossipDigest { entries }
}

/// What `ours` should request from `theirs`: every slot they advertise that
/// we either don't hold or hold at an older timestamp. Capped at
/// [`MAX_WANT_ENTRIES`]. (A key change at the same slot rides the timestamp:
/// if theirs is newer we ask, and the registry's conflict policy decides.)
pub fn diff(ours: &GossipDigest, theirs: &GossipDigest) -> Want {
    let held: HashMap<SocketAddr, i64> =
        ours.entries.iter().map(|e| (e.addr, e.timestamp)).collect();
    let mut addrs = Vec::new();
    for entry in &theirs.entries {
        if addrs.len() >= MAX_WANT_ENTRIES {
            break;
        }
        match held.get(&entry.addr) {
            Some(&ts) if ts >= entry.timestamp => {}
            _ => addrs.push(entry.addr),
        }
    }
    Want { addrs }
}

/// Builds the batch answering `want` for `peer`, from a registry snapshot.
///
/// Loop safety: entries learned *from* `peer` (their `via` marker names it)
/// are never sent back. The batch stops growing once its encoded
/// [`GossipMessage::Batch`] would exceed `max_bytes` — descriptors are
/// size-limited at verification, so at least one always fits under
/// [`MAX_GOSSIP_DATAGRAM`].
pub fn batch_for(
    entries: &[ServerEntry],
    want: &Want,
    peer: SocketAddr,
    max_bytes: usize,
) -> GossipBatch {
    let mut batch = GossipBatch::default();
    for entry in entries {
        let Some(signed) = &entry.signed else {
            continue;
        };
        if entry.via == Some(peer) || !want.addrs.contains(&entry.addr) {
            continue;
        }
        batch.descriptors.push(signed.clone());
        if GossipMessage::Batch(batch.clone()).encode().len() > max_bytes {
            batch.descriptors.pop();
            break;
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Descriptor;
    use rabbithole_identity::IdentityKey;

    fn signed(seed: u8, name: &str, port: u16, ts: i64) -> SignedDescriptor {
        Descriptor::new(name, ([10, 0, 0, seed], port).into())
            .with_description("a test server")
            .with_category("chat")
            .with_timestamp(ts)
            .sign(&IdentityKey::from_seed(&[seed; 32]))
            .unwrap()
    }

    fn entry_of(sd: &SignedDescriptor, via: Option<SocketAddr>) -> ServerEntry {
        ServerEntry::from_signed(sd.clone(), via)
    }

    fn digest_entry(seed: u8, port: u16, ts: i64) -> DigestEntry {
        DigestEntry {
            addr: ([10, 0, 0, seed], port).into(),
            server_key: IdentityKey::from_seed(&[seed; 32]).public().0,
            timestamp: ts,
        }
    }

    #[test]
    fn every_message_round_trips() {
        let messages = [
            GossipMessage::Digest(GossipDigest {
                entries: vec![digest_entry(1, 5500, 100), digest_entry(2, 5510, 200)],
            }),
            GossipMessage::Want(Want {
                addrs: vec![([10, 0, 0, 1], 5500).into()],
            }),
            GossipMessage::Batch(GossipBatch {
                descriptors: vec![signed(1, "Wonderland", 5500, 100)],
            }),
            GossipMessage::Announce(Box::new(signed(2, "Tea Party", 5510, 200))),
        ];
        for msg in messages {
            let wire = msg.encode();
            assert_eq!(&wire[..4], b"RHGS");
            assert_eq!(wire[4], GOSSIP_VERSION);
            assert_eq!(GossipMessage::decode(&wire).unwrap(), msg);
        }
    }

    #[test]
    fn decoder_rejects_garbage_without_panicking() {
        assert_eq!(GossipMessage::decode(&[]), Err(GossipError::Truncated));
        assert_eq!(GossipMessage::decode(b"RHG"), Err(GossipError::Truncated));
        assert_eq!(
            GossipMessage::decode(b"HTRK\x01\x00"),
            Err(GossipError::BadMagic(*b"HTRK"))
        );
        assert_eq!(GossipMessage::decode(b"RHGS"), Err(GossipError::Truncated));
        assert_eq!(
            GossipMessage::decode(b"RHGS\x09\x00"),
            Err(GossipError::BadVersion(9))
        );
        assert_eq!(
            GossipMessage::decode(b"RHGS\x01\xff\xff\xff"),
            Err(GossipError::BadPayload)
        );
        // Every truncation of a real message errors cleanly.
        let wire = GossipMessage::Announce(Box::new(signed(1, "W", 5500, 1))).encode();
        for end in 0..wire.len() {
            assert!(GossipMessage::decode(&wire[..end]).is_err());
        }
    }

    #[test]
    fn digest_covers_signed_entries_only_and_is_capped() {
        let sd = signed(1, "Signed", 5500, 100);
        let mut entries = vec![
            entry_of(&sd, None),
            ServerEntry::unsigned("Plain", "no key", ([10, 0, 0, 9], 5500).into(), 0),
        ];
        let digest = digest_of(&entries);
        assert_eq!(digest.entries.len(), 1);
        assert_eq!(digest.entries[0].addr, sd.descriptor.addr);
        assert_eq!(digest.entries[0].server_key, sd.descriptor.server_key);
        assert_eq!(digest.entries[0].timestamp, 100);

        // Over-full registries advertise a capped prefix.
        for seed in 2..(MAX_DIGEST_ENTRIES as u8 + 4) {
            entries.push(entry_of(&signed(seed, "S", 5500, 100), None));
        }
        assert_eq!(digest_of(&entries).entries.len(), MAX_DIGEST_ENTRIES);
    }

    #[test]
    fn diff_wants_missing_and_newer_but_not_held_or_stale() {
        let ours = GossipDigest {
            entries: vec![
                digest_entry(1, 5500, 100), // theirs is newer → want
                digest_entry(2, 5510, 200), // equal → skip
                digest_entry(3, 5520, 300), // theirs is older → skip
            ],
        };
        let theirs = GossipDigest {
            entries: vec![
                digest_entry(1, 5500, 150),
                digest_entry(2, 5510, 200),
                digest_entry(3, 5520, 250),
                digest_entry(4, 5530, 400), // unknown to us → want
            ],
        };
        let want = diff(&ours, &theirs);
        assert_eq!(
            want.addrs,
            vec![
                SocketAddr::from(([10, 0, 0, 1], 5500)),
                SocketAddr::from(([10, 0, 0, 4], 5530)),
            ]
        );
        // Converged digests want nothing.
        assert!(diff(&theirs, &theirs).is_empty());
        // Wants are capped.
        let many = GossipDigest {
            entries: (0..MAX_DIGEST_ENTRIES as u8)
                .map(|i| digest_entry(i + 1, 6000, 1))
                .collect(),
        };
        assert!(many.entries.len() > MAX_WANT_ENTRIES || MAX_DIGEST_ENTRIES <= MAX_WANT_ENTRIES);
        assert!(diff(&GossipDigest::default(), &many).addrs.len() <= MAX_WANT_ENTRIES);
    }

    #[test]
    fn batch_answers_wants_but_never_echoes_the_peers_own_entries() {
        let peer: SocketAddr = ([192, 0, 2, 9], 4656).into();
        let other: SocketAddr = ([192, 0, 2, 10], 4656).into();
        let mine = signed(1, "Mine", 5500, 100);
        let from_peer = signed(2, "FromPeer", 5510, 200);
        let from_other = signed(3, "FromOther", 5520, 300);
        let entries = vec![
            entry_of(&mine, None),
            entry_of(&from_peer, Some(peer)),
            entry_of(&from_other, Some(other)),
        ];
        let want = Want {
            addrs: vec![
                mine.descriptor.addr,
                from_peer.descriptor.addr,
                from_other.descriptor.addr,
            ],
        };
        // Loop safety: the entry learned from `peer` is not sent back to it,
        // but entries learned elsewhere are fair game.
        let batch = batch_for(&entries, &want, peer, MAX_GOSSIP_DATAGRAM);
        assert_eq!(batch.descriptors, vec![mine.clone(), from_other.clone()]);
        // A different peer gets everything it asked for.
        let batch = batch_for(&entries, &want, other, MAX_GOSSIP_DATAGRAM);
        assert_eq!(batch.descriptors, vec![mine.clone(), from_peer]);
        // Unrequested entries are never volunteered.
        let narrow = Want {
            addrs: vec![mine.descriptor.addr],
        };
        let batch = batch_for(&entries, &narrow, other, MAX_GOSSIP_DATAGRAM);
        assert_eq!(batch.descriptors, vec![mine]);
    }

    #[test]
    fn batch_respects_the_byte_cap() {
        let descriptors: Vec<SignedDescriptor> = (1..=8)
            .map(|seed| {
                Descriptor::new("N".repeat(200), ([10, 0, 0, seed], 5500).into())
                    .with_description("D".repeat(200))
                    .with_timestamp(1)
                    .sign(&IdentityKey::from_seed(&[seed; 32]))
                    .unwrap()
            })
            .collect();
        let entries: Vec<ServerEntry> = descriptors.iter().map(|d| entry_of(d, None)).collect();
        let want = Want {
            addrs: descriptors.iter().map(|d| d.descriptor.addr).collect(),
        };
        let peer: SocketAddr = ([192, 0, 2, 9], 4656).into();
        let batch = batch_for(&entries, &want, peer, MAX_GOSSIP_DATAGRAM);
        // The cap kicked in, at least one descriptor fits, and the encoded
        // message honors the budget.
        assert!(!batch.is_empty());
        assert!(batch.descriptors.len() < descriptors.len());
        assert!(GossipMessage::Batch(batch).encode().len() <= MAX_GOSSIP_DATAGRAM);
    }
}
