//! `.REP` reply-packet ingest, validation, and dedupe.
//!
//! When an offline reader finishes composing replies it ships them back as a
//! `.REP` file: a ZIP whose single interesting member is `<BBSID>.MSG`. That
//! member is byte-for-byte the same shape as [`MESSAGES.DAT`](crate::messages) —
//! a 128-byte header/ID block followed by 128-byte message records — with **one
//! twist**: the reply's target conference number is carried in the two-byte
//! *reader-index* slot (header offsets 125..127), not the usual conference slot
//! at 123..125. (Historically the reader wrote the conference where the packer
//! had written the packet-local reader index.)
//!
//! ```text
//!  <BBSID>.MSG
//!  ┌──────────────────────────────┐ block 1: BBS-id / header text (128 bytes)
//!  ├──────────────────────────────┤ block 2: reply #1 header (conf @125..127)
//!  │   reply #1 body blocks …      │
//!  ├──────────────────────────────┤ reply #2 header …
//!  │   reply #2 body blocks …      │
//!  └──────────────────────────────┘
//! ```
//!
//! This module is pure and sans-I/O: it operates on the already-unzipped
//! `<BBSID>.MSG` bytes and in-memory structures. It never touches the network or
//! filesystem, and — like the rest of the crate — is **total**: malformed,
//! truncated, or hostile input yields a [`QwkError`], never a panic.
//!
//! Three concerns live here:
//!
//! - [`ReplyPacket::parse`] / [`ReplyPacket::encode`] — ingest and (for tests /
//!   fixtures) re-emit the `<BBSID>.MSG` byte layout.
//! - [`validate`] — split parsed replies into accepted and rejected, reporting a
//!   [`ReplyProblem`] per bad record (out-of-range conference, empty body,
//!   malformed header) so the caller can accept the good and surface the bad.
//! - [`content_hash`] / [`dedupe`] — a stable blake3 digest per reply so a
//!   re-uploaded `.REP` does not double-post.

use std::collections::HashSet;

use crate::error::QwkError;
use crate::messages::{decode_message, encode_message, BLOCK, DEFAULT_PRODUCER};
use crate::model::QwkMessage;
use crate::text::{read_field, write_field};

/// A single reply extracted from a `.REP` packet.
///
/// The [`conference`](Self::conference) is read from the reader-index slot (see
/// the [module docs](self)); every other field is the ordinary QWK header/body
/// content, with the body normalized to `\n` line endings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyMessage {
    /// Target conference for this reply (from header offsets 125..127, LE).
    pub conference: u16,
    /// Status flag byte (offset 0).
    pub status: u8,
    /// Message number as written by the reader (offsets 1..8); often `0`.
    pub number: u32,
    /// Date stamp `MM-DD-YY` (offsets 8..16).
    pub date: String,
    /// Time stamp `HH:MM` (offsets 16..21).
    pub time: String,
    /// Recipient (offsets 21..46).
    pub to: String,
    /// Sender — the replying user (offsets 46..71).
    pub from: String,
    /// Subject (offsets 71..96).
    pub subject: String,
    /// Password field (offsets 96..108); usually empty.
    pub password: String,
    /// Reference (reply-to) message number (offsets 108..116). `0` means none.
    pub reference: u32,
    /// Message body with `\n` line endings.
    pub body: String,
}

impl ReplyMessage {
    /// Build a plain reply with the given routing/subject/body.
    pub fn new(
        conference: u16,
        to: impl Into<String>,
        from: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            conference,
            status: b' ',
            number: 0,
            date: String::new(),
            time: String::new(),
            to: to.into(),
            from: from.into(),
            subject: subject.into(),
            password: String::new(),
            reference: 0,
            body: body.into(),
        }
    }

    /// Re-assemble the [`QwkMessage`] view used by the shared record codec.
    ///
    /// The conference travels in the reader-index slot for `.REP`, so the
    /// [`QwkMessage::conference`] field (offsets 123..125) is left `0`; the value
    /// is passed as the `logical` argument to [`encode_message`] instead.
    fn to_qwk(&self) -> QwkMessage {
        QwkMessage {
            status: self.status,
            number: self.number,
            conference: 0,
            date: self.date.clone(),
            time: self.time.clone(),
            to: self.to.clone(),
            from: self.from.clone(),
            subject: self.subject.clone(),
            password: self.password.clone(),
            reference: self.reference,
            active: true,
            body: self.body.clone(),
        }
    }

    /// Build a [`ReplyMessage`] from a decoded record and its reader-index slot.
    fn from_qwk(msg: QwkMessage, conference: u16) -> Self {
        Self {
            conference,
            status: msg.status,
            number: msg.number,
            date: msg.date,
            time: msg.time,
            to: msg.to,
            from: msg.from,
            subject: msg.subject,
            password: msg.password,
            reference: msg.reference,
            body: msg.body,
        }
    }
}

/// A decoded `<BBSID>.MSG`: the leading header/ID block plus the replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyPacket {
    /// Text of the first 128-byte block. In a real `.REP` this is the BBS id the
    /// reader echoes back; trailing padding is trimmed.
    pub header: String,
    /// The replies in packet order.
    pub replies: Vec<ReplyMessage>,
}

impl ReplyPacket {
    /// Construct a packet from replies, using [`DEFAULT_PRODUCER`] as the header
    /// block text.
    pub fn new(replies: Vec<ReplyMessage>) -> Self {
        Self {
            header: DEFAULT_PRODUCER.to_string(),
            replies,
        }
    }

    /// Parse a `<BBSID>.MSG` byte stream into structured replies.
    ///
    /// Returns [`QwkError::Truncated`] if the header block or any record is
    /// incomplete and [`QwkError::BadBlockCount`] if a record's block-count field
    /// is missing, non-numeric, or less than one. Never panics — every possible
    /// truncation or garbage input is handled.
    pub fn parse(bytes: &[u8]) -> Result<Self, QwkError> {
        if bytes.len() < BLOCK {
            return Err(QwkError::Truncated {
                need: BLOCK,
                have: bytes.len(),
            });
        }
        let header = read_field(&bytes[..BLOCK]);
        let mut replies = Vec::new();
        let mut pos = BLOCK;
        while pos < bytes.len() {
            let (msg, conference, consumed) = decode_message(&bytes[pos..])?;
            replies.push(ReplyMessage::from_qwk(msg, conference));
            pos += consumed;
        }
        Ok(Self { header, replies })
    }

    /// Encode this packet back to `<BBSID>.MSG` bytes.
    ///
    /// The output always starts with one header block and is a whole number of
    /// [`BLOCK`]-sized records. `parse(encode(p)) == p` for any packet whose
    /// bodies do not depend on trailing whitespace (padding is trimmed on
    /// decode, matching [`MESSAGES.DAT`](crate::messages)).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![b' '; BLOCK];
        write_field(&mut out[..BLOCK], &self.header);
        for reply in &self.replies {
            // Conference travels in the reader-index slot for `.REP`.
            encode_message(&reply.to_qwk(), reply.conference, &mut out);
        }
        out
    }
}

/// Why a single reply was rejected during [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplyProblem {
    /// The reply targets a conference the caller does not recognize.
    ConferenceOutOfRange {
        /// The offending conference number.
        conference: u16,
    },
    /// The body was empty or contained only whitespace once decoded.
    EmptyBody,
    /// A required header field was missing/blank (a malformed header).
    MalformedHeader {
        /// Which header expectation was violated.
        reason: &'static str,
    },
}

/// The outcome of validating a batch of replies: the good and the bad.
///
/// `accepted` preserves input order; `rejected` pairs each bad reply with the
/// full list of problems found, so a caller can log or surface every reason.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Validated {
    /// Replies that passed every check, in input order.
    pub accepted: Vec<ReplyMessage>,
    /// Replies that failed, each with its non-empty list of problems.
    pub rejected: Vec<(ReplyMessage, Vec<ReplyProblem>)>,
}

/// Collect the problems (if any) with a single reply against the known
/// conferences. An empty result means the reply is valid.
pub fn check(reply: &ReplyMessage, valid_conferences: &HashSet<u16>) -> Vec<ReplyProblem> {
    let mut problems = Vec::new();
    if !valid_conferences.contains(&reply.conference) {
        problems.push(ReplyProblem::ConferenceOutOfRange {
            conference: reply.conference,
        });
    }
    if reply.body.trim().is_empty() {
        problems.push(ReplyProblem::EmptyBody);
    }
    if reply.to.trim().is_empty() {
        problems.push(ReplyProblem::MalformedHeader {
            reason: "empty recipient",
        });
    }
    problems
}

/// Split a batch of replies into accepted and rejected against the set of known
/// conference numbers.
///
/// A reply is rejected if its conference is not in `valid_conferences`, its body
/// is empty/whitespace, or its recipient is blank. This never fails and never
/// panics — every reply lands in exactly one of the two buckets.
pub fn validate(replies: Vec<ReplyMessage>, valid_conferences: &HashSet<u16>) -> Validated {
    let mut out = Validated::default();
    for reply in replies {
        let problems = check(&reply, valid_conferences);
        if problems.is_empty() {
            out.accepted.push(reply);
        } else {
            out.rejected.push((reply, problems));
        }
    }
    out
}

/// A 32-byte blake3 content digest of a reply.
///
/// Plain `[u8; 32]` (rather than [`blake3::Hash`]) so it drops straight into a
/// [`HashSet`] and is trivial to persist as "already seen".
pub type ReplyDigest = [u8; 32];

/// Compute a stable content digest for a reply.
///
/// The digest covers the *semantic* content — conference, routing, subject, and
/// body — and deliberately excludes volatile header bookkeeping (status flag,
/// message number, timestamps) so the same reply re-uploaded in a fresh `.REP`
/// hashes identically and can be recognized as a duplicate. Fields are
/// length-prefixed before hashing so no concatenation collision is possible.
pub fn content_hash(reply: &ReplyMessage) -> ReplyDigest {
    let mut hasher = blake3::Hasher::new();
    let mut feed = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    feed(&reply.conference.to_le_bytes());
    feed(reply.to.as_bytes());
    feed(reply.from.as_bytes());
    feed(reply.subject.as_bytes());
    feed(reply.body.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Partition replies into `(fresh, duplicates)` against a set of already-seen
/// digests.
///
/// A reply is a duplicate if its [`content_hash`] is already in `seen` **or** if
/// an earlier reply in this same batch shared its digest (so a `.REP` that
/// repeats a message internally does not post it twice). Order is preserved
/// within each bucket. `seen` is not mutated; the caller persists the digests of
/// whatever it ultimately posts.
pub fn dedupe(
    replies: Vec<ReplyMessage>,
    seen: &HashSet<ReplyDigest>,
) -> (Vec<ReplyMessage>, Vec<ReplyMessage>) {
    let mut fresh = Vec::new();
    let mut duplicates = Vec::new();
    let mut batch_seen: HashSet<ReplyDigest> = HashSet::new();
    for reply in replies {
        let digest = content_hash(&reply);
        if seen.contains(&digest) || !batch_seen.insert(digest) {
            duplicates.push(reply);
        } else {
            fresh.push(reply);
        }
    }
    (fresh, duplicates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ReplyMessage> {
        vec![
            ReplyMessage {
                conference: 5,
                status: b'*',
                number: 0,
                date: "07-02-26".into(),
                time: "13:45".into(),
                to: "SYSOP".into(),
                from: "KEVIN".into(),
                subject: "Re: Hello there".into(),
                password: String::new(),
                reference: 42,
                body: "Thanks for the note\nsee you around".into(),
            },
            ReplyMessage::new(1, "ALL", "KEVIN", "New topic", "Single line body"),
            ReplyMessage::new(0, "SYSOP", "KEVIN", "Empty-ish", "x"),
        ]
    }

    #[test]
    fn round_trip_reply_packet() {
        let packet = ReplyPacket::new(sample());
        let bytes = packet.encode();
        assert_eq!(bytes.len() % BLOCK, 0);
        let back = ReplyPacket::parse(&bytes).unwrap();
        assert_eq!(back, packet);
    }

    #[test]
    fn conference_travels_in_reader_slot() {
        let reply = ReplyMessage::new(0x0102, "A", "B", "S", "hi");
        let bytes = ReplyPacket::new(vec![reply]).encode();
        // Reader-index slot at header offsets 125..127 carries the conference.
        assert_eq!(bytes[BLOCK + 125], 0x02);
        assert_eq!(bytes[BLOCK + 126], 0x01);
        // The ordinary conference slot at 123..125 stays zero for `.REP`.
        assert_eq!(bytes[BLOCK + 123], 0x00);
        assert_eq!(bytes[BLOCK + 124], 0x00);
        let back = ReplyPacket::parse(&bytes).unwrap();
        assert_eq!(back.replies[0].conference, 0x0102);
    }

    #[test]
    fn parse_truncated_never_panics() {
        let full = ReplyPacket::new(sample()).encode();
        for n in 0..full.len() {
            let _ = ReplyPacket::parse(&full[..n]);
        }
    }

    #[test]
    fn parse_random_bytes_never_panics() {
        let mut seed = 0x9E37_79B9u32;
        for len in [0usize, 1, 5, 128, 200, 256, 384, 500] {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            let _ = ReplyPacket::parse(&bytes);
        }
    }

    #[test]
    fn parse_rejects_empty_input() {
        assert!(matches!(
            ReplyPacket::parse(&[]),
            Err(QwkError::Truncated {
                need: BLOCK,
                have: 0
            })
        ));
    }

    #[test]
    fn parse_reports_bad_block_count() {
        let mut bytes = ReplyPacket::new(vec![ReplyMessage::new(0, "A", "B", "S", "x")]).encode();
        for b in &mut bytes[BLOCK + 116..BLOCK + 122] {
            *b = b'Q';
        }
        assert!(matches!(
            ReplyPacket::parse(&bytes),
            Err(QwkError::BadBlockCount { .. })
        ));
    }

    #[test]
    fn validate_splits_good_and_bad() {
        let valid: HashSet<u16> = [0u16, 1, 5].into_iter().collect();
        let replies = vec![
            ReplyMessage::new(5, "SYSOP", "K", "ok", "a good body"),
            ReplyMessage::new(99, "SYSOP", "K", "bad conf", "body"),
            ReplyMessage::new(1, "SYSOP", "K", "empty body", "   \n  "),
            ReplyMessage::new(0, "", "K", "no recipient", "body"),
        ];
        let out = validate(replies, &valid);
        assert_eq!(out.accepted.len(), 1);
        assert_eq!(out.accepted[0].subject, "ok");
        assert_eq!(out.rejected.len(), 3);
        assert_eq!(
            out.rejected[0].1,
            vec![ReplyProblem::ConferenceOutOfRange { conference: 99 }]
        );
        assert_eq!(out.rejected[1].1, vec![ReplyProblem::EmptyBody]);
        assert_eq!(
            out.rejected[2].1,
            vec![ReplyProblem::MalformedHeader {
                reason: "empty recipient"
            }]
        );
    }

    #[test]
    fn validate_reports_multiple_problems_per_record() {
        let valid: HashSet<u16> = HashSet::new();
        let out = validate(vec![ReplyMessage::new(7, "", "K", "s", "")], &valid);
        assert_eq!(out.accepted.len(), 0);
        let problems = &out.rejected[0].1;
        assert!(problems.contains(&ReplyProblem::ConferenceOutOfRange { conference: 7 }));
        assert!(problems.contains(&ReplyProblem::EmptyBody));
        assert!(problems.contains(&ReplyProblem::MalformedHeader {
            reason: "empty recipient"
        }));
    }

    #[test]
    fn content_hash_is_stable_and_content_addressed() {
        let a = ReplyMessage::new(3, "SYSOP", "KEVIN", "Subject", "the body");
        // Same content but different volatile bookkeeping => same digest.
        let mut b = a.clone();
        b.status = b'-';
        b.number = 999;
        b.date = "01-01-70".into();
        b.time = "00:00".into();
        assert_eq!(content_hash(&a), content_hash(&b));
        // Changing real content changes the digest.
        let mut c = a.clone();
        c.body.push('!');
        assert_ne!(content_hash(&a), content_hash(&c));
    }

    #[test]
    fn content_hash_has_no_field_boundary_collision() {
        // Length-prefixing must keep "ab"+"c" distinct from "a"+"bc".
        let x = ReplyMessage::new(0, "ab", "c", "", "");
        let y = ReplyMessage::new(0, "a", "bc", "", "");
        assert_ne!(content_hash(&x), content_hash(&y));
    }

    #[test]
    fn dedupe_against_seen_set() {
        let replies = sample();
        let seen: HashSet<ReplyDigest> = [content_hash(&replies[1])].into_iter().collect();
        let (fresh, dups) = dedupe(replies.clone(), &seen);
        assert_eq!(fresh.len(), 2);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0], replies[1]);
    }

    #[test]
    fn dedupe_catches_within_batch_repeats() {
        let one = ReplyMessage::new(0, "A", "B", "S", "same content");
        let replies = vec![one.clone(), one.clone(), one.clone()];
        let (fresh, dups) = dedupe(replies, &HashSet::new());
        assert_eq!(fresh.len(), 1);
        assert_eq!(dups.len(), 2);
    }

    #[test]
    fn dedupe_empty_batch() {
        let (fresh, dups) = dedupe(Vec::new(), &HashSet::new());
        assert!(fresh.is_empty());
        assert!(dups.is_empty());
    }

    #[test]
    fn reingest_of_reencoded_packet_all_duplicates() {
        // Build a REP, ingest it, then simulate a re-upload of the same bytes.
        let packet = ReplyPacket::new(sample());
        let first = ReplyPacket::parse(&packet.encode()).unwrap();
        let seen: HashSet<ReplyDigest> = first.replies.iter().map(content_hash).collect();
        let second = ReplyPacket::parse(&packet.encode()).unwrap();
        let (fresh, dups) = dedupe(second.replies, &seen);
        assert!(fresh.is_empty());
        assert_eq!(dups.len(), 3);
    }
}
