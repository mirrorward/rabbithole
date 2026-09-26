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
//! re-gossiped. [`GossipCursor`] rotates advertisements and push replies in
//! address order, so a registry larger than one digest or batch is covered
//! over repeated rounds. Pull requests compare against the full local snapshot.
//!
//! We chose **UDP** (sharing nothing with the HTRK sockets; default port
//! 4656) in the classic tracker spirit: every message fits one datagram,
//! lost pages recur on later rotations, and the listener bounds per-peer
//! rotation bookkeeping. The same socket accepts a fourth message, [`Announce`]: a
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

/// An address bookmark for bounded, repeating coverage of a live registry.
/// Names and generations may change without restarting a traversal; removed
/// addresses need not remain in the snapshot. No wire-format cursor is needed.
#[derive(Debug, Default)]
pub struct GossipCursor {
    after: Option<SocketAddr>,
}

impl GossipCursor {
    fn ordered<'a>(&self, entries: &'a [ServerEntry]) -> Vec<&'a ServerEntry> {
        let mut signed: Vec<_> = entries.iter().filter(|e| e.signed.is_some()).collect();
        signed.sort_unstable_by_key(|e| e.addr);
        let start = self.after.map_or(0, |after| {
            signed.iter().position(|e| e.addr > after).unwrap_or(0)
        });
        signed.rotate_left(start);
        signed
    }

    /// The next signed page, bounded by both count and encoded datagram size.
    pub fn digest(&mut self, entries: &[ServerEntry]) -> GossipDigest {
        let mut digest = GossipDigest::default();
        for entry in self.ordered(entries).into_iter().take(MAX_DIGEST_ENTRIES) {
            digest.entries.push(DigestEntry {
                addr: entry.addr,
                server_key: entry.server_key().expect("signed entry"),
                timestamp: entry.timestamp().expect("signed entry"),
            });
            if GossipMessage::Digest(digest.clone()).encode().len() > MAX_GOSSIP_DATAGRAM {
                digest.entries.pop();
                break;
            }
            self.after = Some(entry.addr);
        }
        digest
    }

    /// A rotating push reply to a peer's digest. Its partial advertisement
    /// does not prove which other entries it already holds; duplicates are safe.
    /// A descriptor that does not fit the remaining space goes first next time.
    /// An entry too large even by itself is skipped, so it cannot block others.
    pub fn push_batch(
        &mut self,
        entries: &[ServerEntry],
        theirs: &GossipDigest,
        peer: SocketAddr,
        max_bytes: usize,
    ) -> GossipBatch {
        let held: HashMap<_, _> = theirs
            .entries
            .iter()
            .map(|e| (e.addr, e.timestamp))
            .collect();
        let mut batch = GossipBatch::default();
        for entry in self.ordered(entries).into_iter().take(MAX_WANT_ENTRIES) {
            if entry.via != Some(peer)
                && !held
                    .get(&entry.addr)
                    .is_some_and(|&ts| ts >= entry.timestamp().unwrap())
            {
                batch
                    .descriptors
                    .push(entry.signed.clone().expect("signed entry"));
                if GossipMessage::Batch(batch.clone()).encode().len()
                    > max_bytes.min(MAX_GOSSIP_DATAGRAM)
                {
                    batch.descriptors.pop();
                    if !batch.is_empty() {
                        // Do not advance past this entry: it gets the first
                        // chance at a full datagram in the next exchange.
                        break;
                    }
                }
            }
            self.after = Some(entry.addr);
        }
        batch
    }
}

/// Builds the first signed page of a registry snapshot. Long-running callers
/// should retain a [`GossipCursor`] to cover the remaining entries in later rounds.
pub fn digest_of(entries: &[ServerEntry]) -> GossipDigest {
    GossipCursor::default().digest(entries)
}

/// What `ours` should request from `theirs`: every slot they advertise that
/// we either don't hold or hold at an older timestamp. Capped at
/// [`MAX_WANT_ENTRIES`]. (A key change at the same slot rides the timestamp:
/// if theirs is newer we ask, and the registry's conflict policy decides.)
pub fn diff(ours: &GossipDigest, theirs: &GossipDigest) -> Want {
    wanted(ours.entries.iter().map(|e| (e.addr, e.timestamp)), theirs)
}

/// Pull missing/newer descriptors using the entire live local registry, not
/// just the page currently being advertised. Otherwise each partial digest
/// would repeatedly request already-held entries and crowd out missing ones.
pub fn want_from(entries: &[ServerEntry], theirs: &GossipDigest) -> Want {
    wanted(
        entries
            .iter()
            .filter_map(|e| Some((e.addr, e.timestamp()?))),
        theirs,
    )
}

fn wanted(ours: impl Iterator<Item = (SocketAddr, i64)>, theirs: &GossipDigest) -> Want {
    let held: HashMap<SocketAddr, i64> = ours.collect();
    let mut addrs = Vec::new();
    for entry in &theirs.entries {
        if addrs.len() >= MAX_WANT_ENTRIES {
            break;
        }
        match held.get(&entry.addr) {
            Some(&ts) if ts >= entry.timestamp => {}
            _ if !addrs.contains(&entry.addr) => addrs.push(entry.addr),
            _ => {}
        }
    }
    Want { addrs }
}

/// Builds the batch answering `want` for `peer`, from a registry snapshot.
///
/// Loop safety: entries learned *from* `peer` (their `via` marker names it)
/// are never sent back. Requests are handled in their advertised order, capped
/// at [`MAX_WANT_ENTRIES`]. Entries that exceed the remaining encoded-byte budget
/// are skipped without blocking later requests. The datagram is also capped at
/// [`MAX_GOSSIP_DATAGRAM`].
pub fn batch_for(
    entries: &[ServerEntry],
    want: &Want,
    peer: SocketAddr,
    max_bytes: usize,
) -> GossipBatch {
    let mut batch = GossipBatch::default();
    let held: HashMap<_, _> = entries.iter().map(|e| (e.addr, e)).collect();
    let mut seen = Vec::new();
    for addr in want.addrs.iter().take(MAX_WANT_ENTRIES) {
        if seen.contains(addr) {
            continue;
        }
        seen.push(*addr);
        let Some(entry) = held.get(addr) else {
            continue;
        };
        let Some(signed) = &entry.signed else {
            continue;
        };
        if entry.via == Some(peer) {
            continue;
        }
        batch.descriptors.push(signed.clone());
        if GossipMessage::Batch(batch.clone()).encode().len() > max_bytes.min(MAX_GOSSIP_DATAGRAM) {
            batch.descriptors.pop();
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

        // A single page remains capped even for an over-full registry.
        for seed in 2..(MAX_DIGEST_ENTRIES as u8 + 4) {
            entries.push(entry_of(&signed(seed, "S", 5500, 100), None));
        }
        assert_eq!(digest_of(&entries).entries.len(), MAX_DIGEST_ENTRIES);
    }

    #[test]
    fn digest_rotation_covers_all_signed_addresses_despite_name_order_and_churn() {
        let mut entries: Vec<_> = (1..=41)
            .rev()
            .map(|seed| {
                entry_of(
                    &signed(seed, &format!("name-{}", 42 - seed), 5500, 100),
                    None,
                )
            })
            .collect();
        entries.push(ServerEntry::unsigned(
            "Unsigned",
            "",
            ([10, 0, 0, 99], 5500).into(),
            0,
        ));
        let mut cursor = GossipCursor::default();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            let digest = cursor.digest(&entries);
            assert_eq!(digest.entries.len(), MAX_DIGEST_ENTRIES);
            assert!(GossipMessage::Digest(digest.clone()).encode().len() <= MAX_GOSSIP_DATAGRAM);
            seen.extend(digest.entries.iter().map(|e| e.addr));
        }
        assert_eq!(seen.len(), 41);
        // Remove the exact bookmark and rename the remaining entries. The
        // next address, not a shifted index or a display name, resumes progress.
        let after = cursor.after.unwrap();
        entries.retain(|e| e.addr != after);
        for entry in &mut entries {
            entry.name = "renamed".into();
        }
        let next = cursor.digest(&entries);
        assert!(next.entries[0].addr > after);
        assert!(cursor.digest(&[]).entries.is_empty());
    }

    #[test]
    fn pull_compares_with_the_entire_registry_not_its_current_page() {
        let entries: Vec<_> = (1..=40)
            .map(|seed| entry_of(&signed(seed, "S", 5500, 100), None))
            .collect();
        let mut cursor = GossipCursor::default();
        let first = cursor.digest(&entries);
        let second = cursor.digest(&entries);
        assert!(!diff(&first, &second).is_empty());
        assert!(want_from(&entries, &second).is_empty());
        let mut newer = second;
        newer.entries[0].timestamp += 1;
        assert_eq!(
            want_from(&entries, &newer).addrs,
            vec![newer.entries[0].addr]
        );
    }

    #[test]
    fn rotating_pushes_do_not_starve_entries_at_batch_boundaries() {
        let peer: SocketAddr = ([192, 0, 2, 9], 4656).into();
        let entries: Vec<_> = (1..=40)
            .map(|seed| {
                let sd = Descriptor::new("N".repeat(200), ([10, 0, 0, seed], 5500).into())
                    .with_description("D".repeat(200))
                    .sign(&IdentityKey::from_seed(&[seed; 32]))
                    .unwrap();
                entry_of(&sd, None)
            })
            .collect();
        let mut cursor = GossipCursor::default();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..entries.len() {
            let batch = cursor.push_batch(
                &entries,
                &GossipDigest::default(),
                peer,
                MAX_GOSSIP_DATAGRAM,
            );
            assert!(!batch.is_empty());
            assert!(GossipMessage::Batch(batch.clone()).encode().len() <= MAX_GOSSIP_DATAGRAM);
            seen.extend(batch.descriptors.iter().map(|s| s.descriptor.addr));
        }
        assert_eq!(seen.len(), entries.len());
    }

    #[test]
    fn oversized_and_loop_excluded_entries_cannot_block_smaller_descriptors() {
        let peer: SocketAddr = ([192, 0, 2, 9], 4656).into();
        let large = Descriptor::new("N".repeat(255), ([10, 0, 0, 1], 5500).into())
            .with_description("D".repeat(255))
            .sign(&IdentityKey::from_seed(&[1; 32]))
            .unwrap();
        large.verify().unwrap();
        let small = signed(2, "Small", 5500, 100);
        let echo = signed(3, "Loop", 5500, 100);
        let entries = vec![
            entry_of(&large, None),
            entry_of(&small, None),
            entry_of(&echo, Some(peer)),
        ];
        let budget = GossipMessage::Batch(GossipBatch {
            descriptors: vec![small.clone()],
        })
        .encode()
        .len();
        let want = Want {
            addrs: entries.iter().map(|e| e.addr).collect(),
        };
        assert_eq!(
            batch_for(&entries, &want, peer, budget).descriptors,
            vec![small.clone()]
        );
        let mut cursor = GossipCursor::default();
        for _ in 0..3 {
            assert_eq!(
                cursor
                    .push_batch(&entries, &GossipDigest::default(), peer, budget)
                    .descriptors,
                vec![small.clone()]
            );
        }
        let reversed = Want {
            addrs: vec![
                small.descriptor.addr,
                large.descriptor.addr,
                small.descriptor.addr,
            ],
        };
        assert_eq!(
            batch_for(&entries, &reversed, peer, MAX_GOSSIP_DATAGRAM).descriptors,
            vec![small, large]
        );
    }

    #[test]
    fn maximum_ipv6_digest_and_want_fit_the_datagram_cap() {
        let mut entries: Vec<_> = (1..=16)
            .map(|seed| entry_of(&signed(seed, "S", u16::MAX, i64::MAX), None))
            .collect();
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.addr = SocketAddr::new(
                std::net::Ipv6Addr::new(
                    u16::MAX,
                    u16::MAX,
                    u16::MAX,
                    u16::MAX,
                    u16::MAX,
                    u16::MAX,
                    u16::MAX,
                    index as u16,
                )
                .into(),
                u16::MAX,
            );
        }
        let digest = digest_of(&entries);
        assert_eq!(digest.entries.len(), MAX_DIGEST_ENTRIES);
        let want = diff(&GossipDigest::default(), &digest);
        assert_eq!(want.addrs.len(), MAX_WANT_ENTRIES);
        assert!(GossipMessage::Digest(digest).encode().len() <= MAX_GOSSIP_DATAGRAM);
        assert!(GossipMessage::Want(want).encode().len() <= MAX_GOSSIP_DATAGRAM);
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
