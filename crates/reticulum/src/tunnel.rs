//! Delay-tolerant tunnel message model — the store-and-forward unit.
//!
//! This module is the lowest layer of the crate's **delay-tolerant S2S tunnel**
//! core (PLAN §Wave 14, "Delay-tolerant Tunnels (S2S flood-fill) over RNS with
//! bandwidth-aware batching"). It defines the content-addressed
//! [`TunnelMessage`] that a Burrow hands to a peer for eventual delivery, and a
//! [`MessageStore`] that holds pending messages with a TTL and de-duplicates
//! re-deliveries — exactly mirroring [`AnnounceCache`](crate::announce::AnnounceCache)'s
//! TTL + injected-clock discipline, one layer up.
//!
//! Everything here is pure and sans-I/O: no clock reads (the caller injects
//! `now_ms`, a monotonic millisecond clock), no randomness, no networking.
//! [`TunnelMessage::decode`] is **total** — arbitrary or truncated input yields
//! a [`TunnelError`], never a panic.
//!
//! # Where this sits
//!
//! ```text
//!   tunnel      (this module)  — the message + the pending store
//!     ▲
//!   floodfill   — plan which peers to relay a message to (loop-safe)
//!     ▲
//!   batch       — pack messages per peer into MTU-bounded, rate-metered batches
//! ```
//!
//! A future **RNS tunnel adapter / sidecar** (PLAN §Wave 14) drives this core
//! from its socket loop: it discovers tunnel peers from announces, wraps batches
//! in Reticulum [`Packet`](crate::packet::Packet)s (or link/resource transfers),
//! and does all the actual I/O. Nothing in this module touches the wire.
//!
//! # Relationship to LXMF propagation nodes
//!
//! This is a **model** of the store-and-forward behaviour an
//! [LXMF](https://github.com/markqvist/LXMF) *propagation node* provides: a
//! propagation node accepts messages for offline peers, holds them, and syncs
//! them onward. A [`TunnelMessage`] is the delay-tolerant analogue of a held
//! LXMF message, and [`MessageStore`] is the analogue of the propagation node's
//! message store. The **framing here is this crate's own** — it is *not* the
//! LXMF transfer format nor an RNS wire format — so a later interop pass maps it
//! onto real LXMF propagation transfers. Points where the mapping is uncertain
//! are flagged with `// SPEC-CHECK:` and pinned by tests.
//!
//! # Wire layout (**model**, pinned)
//!
//! A [`TunnelMessage`] serializes as a fixed header followed by the payload:
//!
//! ```text
//! +----------+--------------+-------+-----------+----------+-------------+-------------+
//! | id       | created_ms   | hops  | ttl_hops  | priority | payload_len | payload     |
//! | 16 bytes | 8 (u64 BE)   | 1     | 1         | 1        | 2 (u16 BE)  | 0..N bytes  |
//! +----------+--------------+-------+-----------+----------+-------------+-------------+
//! ```
//!
//! The header is [`TUNNEL_MESSAGE_HEADER_LEN`] = 29 bytes; the payload is bounded
//! by [`MAX_TUNNEL_PAYLOAD`]. On decode the `id` is **recomputed from the
//! content and checked**, so the wire form authenticates its own content
//! address (see [`content_id`]).

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::destination::DestinationHash;

/// Length in bytes of a tunnel message id — a truncated SHA-256, the same
/// 128-bit truncation the crate uses for identity and destination hashes.
pub const TUNNEL_ID_LENGTH: usize = 16;

/// Fixed per-message header length: `id(16) || created_ms(8) || hops(1) ||
/// ttl_hops(1) || priority(1) || payload_len(2)`.
pub const TUNNEL_MESSAGE_HEADER_LEN: usize = TUNNEL_ID_LENGTH + 8 + 1 + 1 + 1 + 2;

/// Maximum payload bytes in a single [`TunnelMessage`].
///
/// Chosen so that one message — plus the [`batch`](crate::batch) envelope header
/// and the link-cipher framing reserve — fits inside one HEADER_2 Reticulum
/// packet (see [`crate::batch::DEFAULT_BATCH_BUDGET`], cross-checked by a test
/// there). This slice is **single-packet only**: a payload larger than this is
/// rejected rather than fragmented across packets. Fragmentation
/// (≤ N fragments per message) is a documented deferred item.
pub const MAX_TUNNEL_PAYLOAD: usize = 384;

/// Domain-separation tag mixed into every content id, binding the id to this
/// construction and version (mirrors the crate's HKDF `info` convention).
const TUNNEL_ID_DOMAIN: &[u8] = b"rabbithole-reticulum:tunnel-msg-id:v1";

/// The identifier of a tunnel peer — a neighbouring store-and-forward node,
/// named by its 16-byte RNS destination hash. Reusing [`DestinationHash`] keeps
/// peers in the same currency the rest of the crate keys on (ordered, hashable,
/// hex-printable).
pub type PeerId = DestinationHash;

/// Errors produced while encoding or decoding a [`TunnelMessage`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TunnelError {
    /// The input was shorter than the fixed header or its declared payload.
    #[error("tunnel message truncated: need at least {needed} bytes, got {got}")]
    Truncated {
        /// Minimum number of bytes required.
        needed: usize,
        /// Number of bytes actually available.
        got: usize,
    },
    /// The payload exceeds [`MAX_TUNNEL_PAYLOAD`] (single-packet only; no
    /// fragmentation in this slice).
    #[error("tunnel payload of {0} bytes exceeds the {max}-byte cap", max = MAX_TUNNEL_PAYLOAD)]
    TooLarge(usize),
    /// The encoded id did not match the id recomputed from the content — the
    /// message is not a faithful content address of its own bytes.
    #[error("tunnel message id does not match its content")]
    IdMismatch,
    /// A standalone decode left unconsumed trailing bytes.
    #[error("tunnel message has {0} unexpected trailing bytes")]
    TrailingBytes(usize),
}

/// A delay-tolerant, content-addressed store-and-forward message.
///
/// The `id` is the **content address** of the message (see [`content_id`]): a
/// truncated SHA-256 over the creation time, hop-limit, priority, and payload —
/// **but not `hops`**, which mutates as the message floods the mesh. This
/// mirrors [`Packet::packet_hash`](crate::packet::Packet::packet_hash), whose
/// hashable part likewise excludes the in-transit hop counter, so a message and
/// every relayed copy of it share one id (the key that makes flood-fill
/// de-duplication loop-safe).
///
/// Fields are public to match the model spec; construct via [`new`](Self::new)
/// or [`decode`](Self::decode) so the `id` stays consistent with the content
/// (decode verifies it; [`verify_id`](Self::verify_id) checks it explicitly).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelMessage {
    /// Content address: `content_id(created_ms, ttl_hops, priority, payload)`.
    pub id: [u8; TUNNEL_ID_LENGTH],
    /// Creation time in the caller's monotonic millisecond clock.
    pub created_ms: u64,
    /// Hops travelled so far. Starts at 0 and increments at each relay (like
    /// [`Packet::hops`](crate::packet::Packet::hops)); the remaining hop budget
    /// is `ttl_hops - hops`.
    pub hops: u8,
    /// Hop horizon: the message is not relayed once `hops >= ttl_hops`.
    pub ttl_hops: u8,
    /// Relay priority (higher is more urgent); used to order batching.
    pub priority: u8,
    /// Opaque payload, bounded by [`MAX_TUNNEL_PAYLOAD`].
    pub payload: Vec<u8>,
}

/// Compute the content id: `SHA-256(DOMAIN || created_ms || ttl_hops ||
/// priority || payload)` truncated to [`TUNNEL_ID_LENGTH`].
///
/// `hops` is deliberately **excluded** so that a message keeps one stable id as
/// its hop counter changes in transit.
///
/// // SPEC-CHECK: which fields participate in the content address is a model
/// // choice. Excluding `hops` mirrors `RNS.Packet.get_hashable_part` (which
/// // masks out the forwarding hop count); an interop pass that maps this onto
/// // an LXMF message id (SHA-256 over the packed LXMF payload) adjusts here.
pub fn content_id(
    created_ms: u64,
    ttl_hops: u8,
    priority: u8,
    payload: &[u8],
) -> [u8; TUNNEL_ID_LENGTH] {
    let mut hasher = Sha256::new();
    hasher.update(TUNNEL_ID_DOMAIN);
    hasher.update(created_ms.to_be_bytes());
    hasher.update([ttl_hops, priority]);
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut out = [0u8; TUNNEL_ID_LENGTH];
    out.copy_from_slice(&digest[..TUNNEL_ID_LENGTH]);
    out
}

impl TunnelMessage {
    /// Build a fresh message (`hops = 0`) with a content-addressed id.
    ///
    /// The payload is copied. Note this does **not** enforce
    /// [`MAX_TUNNEL_PAYLOAD`]: construction is infallible so callers can model
    /// oversized inputs, and the cap is enforced at the boundaries that matter —
    /// [`encode`](Self::encode), [`MessageStore::offer`], and
    /// [`decode`](Self::decode).
    pub fn new(created_ms: u64, ttl_hops: u8, priority: u8, payload: &[u8]) -> Self {
        let id = content_id(created_ms, ttl_hops, priority, payload);
        Self {
            id,
            created_ms,
            hops: 0,
            ttl_hops,
            priority,
            payload: payload.to_vec(),
        }
    }

    /// Recompute the content id from the current fields.
    pub fn recomputed_id(&self) -> [u8; TUNNEL_ID_LENGTH] {
        content_id(self.created_ms, self.ttl_hops, self.priority, &self.payload)
    }

    /// Whether the stored `id` matches the content (see [`content_id`]).
    pub fn verify_id(&self) -> bool {
        self.id == self.recomputed_id()
    }

    /// A copy of this message advanced one hop (`hops` saturating-incremented).
    /// The id is unchanged — the relayed copy is the same message.
    pub fn forwarded(&self) -> Self {
        let mut next = self.clone();
        next.hops = next.hops.saturating_add(1);
        next
    }

    /// Whether the message has reached its hop horizon and must not be relayed
    /// further (`hops >= ttl_hops`).
    pub fn at_horizon(&self) -> bool {
        self.hops >= self.ttl_hops
    }

    /// The remaining hop budget (`ttl_hops - hops`, saturating).
    pub fn hops_remaining(&self) -> u8 {
        self.ttl_hops.saturating_sub(self.hops)
    }

    /// Total encoded size in bytes ([`TUNNEL_MESSAGE_HEADER_LEN`] + payload).
    pub fn encoded_len(&self) -> usize {
        TUNNEL_MESSAGE_HEADER_LEN + self.payload.len()
    }

    /// Serialize the message (see the module-level wire layout). Returns
    /// [`TunnelError::TooLarge`] if the payload exceeds [`MAX_TUNNEL_PAYLOAD`].
    pub fn encode(&self) -> Result<Vec<u8>, TunnelError> {
        if self.payload.len() > MAX_TUNNEL_PAYLOAD {
            return Err(TunnelError::TooLarge(self.payload.len()));
        }
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.created_ms.to_be_bytes());
        out.push(self.hops);
        out.push(self.ttl_hops);
        out.push(self.priority);
        // `payload.len() <= MAX_TUNNEL_PAYLOAD` (checked above) < u16::MAX.
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Decode a single message, requiring it to consume the whole buffer.
    ///
    /// Total: truncated, oversized, id-mismatched, or trailing input yields a
    /// [`TunnelError`], never a panic.
    pub fn decode(bytes: &[u8]) -> Result<Self, TunnelError> {
        let (msg, consumed) = Self::decode_from(bytes, 0)?;
        if consumed != bytes.len() {
            return Err(TunnelError::TrailingBytes(bytes.len() - consumed));
        }
        Ok(msg)
    }

    /// Decode one message starting at `offset`, returning it and the new offset
    /// (one past the last byte consumed). Used by [`batch`](crate::batch) to
    /// stream several self-delimiting messages out of one envelope.
    pub fn decode_from(bytes: &[u8], offset: usize) -> Result<(Self, usize), TunnelError> {
        let mut cursor = offset;
        let need = |cursor: usize, n: usize| -> Result<(), TunnelError> {
            let end = cursor.checked_add(n).ok_or(TunnelError::Truncated {
                needed: usize::MAX,
                got: bytes.len(),
            })?;
            if end > bytes.len() {
                return Err(TunnelError::Truncated {
                    needed: end,
                    got: bytes.len(),
                });
            }
            Ok(())
        };

        need(cursor, TUNNEL_MESSAGE_HEADER_LEN)?;

        let mut id = [0u8; TUNNEL_ID_LENGTH];
        id.copy_from_slice(&bytes[cursor..cursor + TUNNEL_ID_LENGTH]);
        cursor += TUNNEL_ID_LENGTH;

        let mut created = [0u8; 8];
        created.copy_from_slice(&bytes[cursor..cursor + 8]);
        let created_ms = u64::from_be_bytes(created);
        cursor += 8;

        let hops = bytes[cursor];
        cursor += 1;
        let ttl_hops = bytes[cursor];
        cursor += 1;
        let priority = bytes[cursor];
        cursor += 1;

        let payload_len = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
        cursor += 2;

        if payload_len > MAX_TUNNEL_PAYLOAD {
            return Err(TunnelError::TooLarge(payload_len));
        }
        need(cursor, payload_len)?;
        let payload = bytes[cursor..cursor + payload_len].to_vec();
        cursor += payload_len;

        let msg = Self {
            id,
            created_ms,
            hops,
            ttl_hops,
            priority,
            payload,
        };
        if !msg.verify_id() {
            return Err(TunnelError::IdMismatch);
        }
        Ok((msg, cursor))
    }
}

/// The disposition of a message offered to a [`MessageStore`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferOutcome {
    /// First sighting of this id within the TTL (or its prior entry expired):
    /// stored as pending and remembered.
    Accept,
    /// The same id was already accepted within the TTL — a flood re-delivery;
    /// dropped without disturbing the stored copy.
    Duplicate,
    /// The message is already older than the TTL (`now - created_ms >= ttl`);
    /// too stale to store or forward.
    Expired,
    /// The payload exceeds [`MAX_TUNNEL_PAYLOAD`]; rejected (single-packet
    /// only — no fragmentation in this slice).
    TooLarge,
}

/// A pure, delay-tolerant message store with an injected clock.
///
/// Holds pending [`TunnelMessage`]s keyed by content id, with a TTL and a
/// de-duplication *seen-set* — the same discipline as
/// [`AnnounceCache`](crate::announce::AnnounceCache), one layer up. All time is
/// a caller-injected monotonic millisecond clock; the store performs no I/O and
/// reads no clock. A message is *live* while its own age
/// (`now - created_ms`) is below the TTL; the seen-set suppresses re-offers of
/// an id for the same window, so a flood re-delivery does not re-queue a body.
///
/// The seen-set outlives an individual body: [`remove`](Self::remove) drops a
/// message from the pending set (e.g. after it has been handed to every peer)
/// while keeping its id in the seen-set, so a later re-flood is still a
/// [`OfferOutcome::Duplicate`] rather than a fresh [`OfferOutcome::Accept`].
/// Expired entries are dropped lazily on [`offer`](Self::offer) and eagerly by
/// [`purge_expired`](Self::purge_expired).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageStore {
    ttl_ms: u64,
    max_payload: usize,
    /// id → created_ms of every accepted message (the de-dup authority).
    seen: BTreeMap<[u8; TUNNEL_ID_LENGTH], u64>,
    /// id → body of messages still held for forwarding/delivery.
    pending: BTreeMap<[u8; TUNNEL_ID_LENGTH], TunnelMessage>,
}

impl MessageStore {
    /// Create a store whose messages live for `ttl_ms` and whose payload cap is
    /// [`MAX_TUNNEL_PAYLOAD`].
    pub fn new(ttl_ms: u64) -> Self {
        Self::with_max_payload(ttl_ms, MAX_TUNNEL_PAYLOAD)
    }

    /// Create a store with an explicit payload cap (`max_payload` is clamped to
    /// [`MAX_TUNNEL_PAYLOAD`] so a stored message always fits one packet).
    pub fn with_max_payload(ttl_ms: u64, max_payload: usize) -> Self {
        Self {
            ttl_ms,
            max_payload: max_payload.min(MAX_TUNNEL_PAYLOAD),
            seen: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }

    /// Whether an id is a live duplicate at `now` (seen within the TTL).
    fn is_live_duplicate(&self, id: &[u8; TUNNEL_ID_LENGTH], now_ms: u64) -> bool {
        self.seen
            .get(id)
            .is_some_and(|&created| now_ms.saturating_sub(created) < self.ttl_ms)
    }

    /// Offer a message to the store at `now_ms`.
    ///
    /// De-duplicates by content id (mirroring the announce cache): a live
    /// re-offer is a [`OfferOutcome::Duplicate`]. A payload over the cap is
    /// [`OfferOutcome::TooLarge`]; a message already older than the TTL is
    /// [`OfferOutcome::Expired`]. Only [`OfferOutcome::Accept`] stores the body.
    pub fn offer(&mut self, msg: TunnelMessage, now_ms: u64) -> OfferOutcome {
        if msg.payload.len() > self.max_payload {
            return OfferOutcome::TooLarge;
        }
        if now_ms.saturating_sub(msg.created_ms) >= self.ttl_ms {
            return OfferOutcome::Expired;
        }
        if self.is_live_duplicate(&msg.id, now_ms) {
            return OfferOutcome::Duplicate;
        }
        self.seen.insert(msg.id, msg.created_ms);
        self.pending.insert(msg.id, msg);
        OfferOutcome::Accept
    }

    /// The live pending messages at `now_ms` — those still within their TTL —
    /// in deterministic (id-sorted) order. These are due for (re-)forwarding.
    pub fn due(&self, now_ms: u64) -> Vec<&TunnelMessage> {
        self.pending
            .values()
            .filter(|m| now_ms.saturating_sub(m.created_ms) < self.ttl_ms)
            .collect()
    }

    /// Fetch a pending body by id (regardless of TTL).
    pub fn get(&self, id: &[u8; TUNNEL_ID_LENGTH]) -> Option<&TunnelMessage> {
        self.pending.get(id)
    }

    /// Whether a live pending body exists for `id` at `now_ms`.
    pub fn contains(&self, id: &[u8; TUNNEL_ID_LENGTH], now_ms: u64) -> bool {
        self.pending
            .get(id)
            .is_some_and(|m| now_ms.saturating_sub(m.created_ms) < self.ttl_ms)
    }

    /// Drop a body from the pending set while **keeping** its id in the
    /// seen-set, so a subsequent re-flood is still a duplicate. Returns the
    /// removed body if it was present.
    pub fn remove(&mut self, id: &[u8; TUNNEL_ID_LENGTH]) -> Option<TunnelMessage> {
        self.pending.remove(id)
    }

    /// Drop every pending body and seen id whose message has aged past the TTL.
    pub fn purge_expired(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        self.pending
            .retain(|_, m| now_ms.saturating_sub(m.created_ms) < ttl);
        self.seen
            .retain(|_, &mut created| now_ms.saturating_sub(created) < ttl);
    }

    /// Number of pending bodies held (including not-yet-purged expired ones).
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether the store holds no pending bodies.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of ids in the de-duplication seen-set.
    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{context, DestinationType, Packet, PacketType, MTU};

    fn msg(created_ms: u64, ttl_hops: u8, priority: u8, payload: &[u8]) -> TunnelMessage {
        TunnelMessage::new(created_ms, ttl_hops, priority, payload)
    }

    // --- TunnelMessage --------------------------------------------------

    #[test]
    fn new_is_content_addressed_and_starts_at_zero_hops() {
        let m = msg(1_000, 8, 3, b"down the hole");
        assert_eq!(m.hops, 0);
        assert!(m.verify_id());
        assert_eq!(m.id, content_id(1_000, 8, 3, b"down the hole"));
    }

    #[test]
    fn id_excludes_hops_but_covers_every_other_field() {
        let base = msg(1_000, 8, 3, b"payload");
        // Advancing hops does not change the id — same message, different hop.
        assert_eq!(base.forwarded().id, base.id);
        assert_eq!(base.forwarded().forwarded().id, base.id);
        // Every other field participates.
        assert_ne!(base.id, msg(1_001, 8, 3, b"payload").id);
        assert_ne!(base.id, msg(1_000, 9, 3, b"payload").id);
        assert_ne!(base.id, msg(1_000, 8, 4, b"payload").id);
        assert_ne!(base.id, msg(1_000, 8, 3, b"payloae").id);
    }

    #[test]
    fn forwarded_increments_and_saturates() {
        let m = msg(0, 200, 0, b"x");
        let f = m.forwarded();
        assert_eq!(f.hops, 1);
        assert_eq!(f.hops_remaining(), 199);
        let mut maxed = m.clone();
        maxed.hops = u8::MAX;
        assert_eq!(maxed.forwarded().hops, u8::MAX);
    }

    #[test]
    fn at_horizon_and_remaining() {
        let mut m = msg(0, 3, 0, b"x");
        assert!(!m.at_horizon());
        assert_eq!(m.hops_remaining(), 3);
        m.hops = 3;
        assert!(m.at_horizon());
        assert_eq!(m.hops_remaining(), 0);
        m.hops = 5; // beyond horizon: still at_horizon, remaining saturates to 0
        assert!(m.at_horizon());
        assert_eq!(m.hops_remaining(), 0);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mut m = msg(0x0102_0304_0506_0708, 12, 7, b"reach the warren");
        m.hops = 4;
        let bytes = m.encode().unwrap();
        assert_eq!(bytes.len(), m.encoded_len());
        assert_eq!(bytes.len(), TUNNEL_MESSAGE_HEADER_LEN + m.payload.len());
        let decoded = TunnelMessage::decode(&bytes).unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn empty_payload_roundtrip() {
        let m = msg(42, 1, 0, b"");
        let bytes = m.encode().unwrap();
        assert_eq!(bytes.len(), TUNNEL_MESSAGE_HEADER_LEN);
        assert_eq!(TunnelMessage::decode(&bytes).unwrap(), m);
    }

    #[test]
    fn max_payload_roundtrip_and_over_cap_rejected() {
        let ok = msg(1, 4, 0, &vec![0xAB; MAX_TUNNEL_PAYLOAD]);
        let bytes = ok.encode().unwrap();
        assert_eq!(TunnelMessage::decode(&bytes).unwrap(), ok);

        let over = msg(1, 4, 0, &vec![0xAB; MAX_TUNNEL_PAYLOAD + 1]);
        assert_eq!(
            over.encode(),
            Err(TunnelError::TooLarge(MAX_TUNNEL_PAYLOAD + 1))
        );
    }

    #[test]
    fn decode_rejects_truncated() {
        let m = msg(7, 4, 1, b"hello mesh");
        let bytes = m.encode().unwrap();
        for len in 0..bytes.len() {
            assert!(
                matches!(
                    TunnelMessage::decode(&bytes[..len]),
                    Err(TunnelError::Truncated { .. })
                ),
                "expected truncated error at len {len}"
            );
        }
        assert!(TunnelMessage::decode(&bytes).is_ok());
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let m = msg(7, 4, 1, b"hi");
        let mut bytes = m.encode().unwrap();
        bytes.push(0xFF);
        assert_eq!(
            TunnelMessage::decode(&bytes),
            Err(TunnelError::TrailingBytes(1))
        );
    }

    #[test]
    fn decode_rejects_tampered_id() {
        let m = msg(7, 4, 1, b"hi");
        let mut bytes = m.encode().unwrap();
        bytes[0] ^= 0xFF; // corrupt the id
        assert_eq!(TunnelMessage::decode(&bytes), Err(TunnelError::IdMismatch));
    }

    #[test]
    fn decode_rejects_tampered_payload() {
        // Flipping a payload byte breaks the content address.
        let m = msg(7, 4, 1, b"hi there");
        let mut bytes = m.encode().unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(TunnelMessage::decode(&bytes), Err(TunnelError::IdMismatch));
    }

    #[test]
    fn decode_rejects_declared_payload_over_cap() {
        // Hand-craft a header claiming an oversized payload length.
        let mut bytes = vec![0u8; TUNNEL_MESSAGE_HEADER_LEN];
        let len_pos = TUNNEL_ID_LENGTH + 8 + 3;
        bytes[len_pos..len_pos + 2]
            .copy_from_slice(&((MAX_TUNNEL_PAYLOAD + 1) as u16).to_be_bytes());
        assert_eq!(
            TunnelMessage::decode(&bytes),
            Err(TunnelError::TooLarge(MAX_TUNNEL_PAYLOAD + 1))
        );
    }

    #[test]
    fn decode_arbitrary_bytes_never_panics() {
        let mut state: u64 = 0x0BAD_F00D_1234_5678;
        for _ in 0..5000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 55) as usize % 200;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 40) as u8);
            }
            let _ = TunnelMessage::decode(&buf);
            let _ = TunnelMessage::decode_from(&buf, 0);
        }
    }

    #[test]
    fn pinned_wire_vector() {
        // Fixed message → pinned encoding, so the layout cannot drift silently.
        let m = msg(0x0000_0000_0000_002A, 8, 3, b"AB");
        let bytes = m.encode().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&content_id(0x2A, 8, 3, b"AB"));
        expected.extend_from_slice(&0x2Au64.to_be_bytes()); // created_ms
        expected.push(0x00); // hops
        expected.push(0x08); // ttl_hops
        expected.push(0x03); // priority
        expected.extend_from_slice(&2u16.to_be_bytes()); // payload_len
        expected.extend_from_slice(b"AB");
        assert_eq!(bytes, expected);
        // Pin the content id itself.
        assert_eq!(
            hex::encode(m.id),
            hex::encode(content_id(0x2A, 8, 3, b"AB"))
        );
    }

    #[test]
    fn rides_inside_a_reticulum_packet() {
        let m = msg(1_000, 4, 2, b"delay tolerant");
        let packet = Packet::new_header1(
            DestinationType::Single,
            PacketType::Data,
            [0x11; 16],
            context::NONE,
            m.encode().unwrap(),
        );
        let encoded = packet.encode().unwrap();
        assert!(encoded.len() <= MTU);
        let decoded = Packet::decode(&encoded).unwrap();
        assert_eq!(TunnelMessage::decode(&decoded.data).unwrap(), m);
    }

    // --- MessageStore ---------------------------------------------------

    #[test]
    fn store_accepts_then_dedupes_within_ttl() {
        let mut store = MessageStore::new(10_000);
        let m = msg(1_000, 4, 0, b"a");
        assert_eq!(store.offer(m.clone(), 1_000), OfferOutcome::Accept);
        assert_eq!(store.offer(m.clone(), 1_001), OfferOutcome::Duplicate);
        // Forwarded copy (same id, more hops) is still a duplicate.
        assert_eq!(store.offer(m.forwarded(), 1_002), OfferOutcome::Duplicate);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn store_rejects_expired_and_too_large() {
        let mut store = MessageStore::new(5_000);
        // created at 0, offered at 5_000 → age == ttl → expired.
        assert_eq!(
            store.offer(msg(0, 4, 0, b"x"), 5_000),
            OfferOutcome::Expired
        );
        // just inside the window is accepted.
        assert_eq!(store.offer(msg(0, 4, 0, b"x"), 4_999), OfferOutcome::Accept);
        // over the payload cap.
        let big = msg(1, 4, 0, &vec![0u8; MAX_TUNNEL_PAYLOAD + 1]);
        assert_eq!(store.offer(big, 1), OfferOutcome::TooLarge);
    }

    #[test]
    fn store_message_is_permanently_expired_past_its_ttl() {
        // `created_ms` is part of the content id, so a message with the same id
        // always has the same age — once past its TTL it is stale forever and is
        // rejected as Expired (the expiry check precedes de-duplication).
        let mut store = MessageStore::new(1_000);
        let m = msg(0, 4, 0, b"x");
        assert_eq!(store.offer(m.clone(), 0), OfferOutcome::Accept);
        assert_eq!(store.offer(m.clone(), 999), OfferOutcome::Duplicate);
        // At/after its own TTL the message is stale: Expired, not re-Accepted.
        assert_eq!(store.offer(m.clone(), 1_000), OfferOutcome::Expired);
        assert_eq!(store.offer(m, 5_000), OfferOutcome::Expired);
    }

    #[test]
    fn store_due_returns_only_live_messages_sorted() {
        let mut store = MessageStore::new(10_000);
        // Distinct payloads → distinct ids.
        let a = msg(0, 4, 0, b"aaa");
        let b = msg(0, 4, 0, b"bbb");
        store.offer(a.clone(), 0);
        store.offer(b.clone(), 0);
        let due: Vec<_> = store.due(100).into_iter().map(|m| m.id).collect();
        assert_eq!(due.len(), 2);
        // Sorted by id (BTreeMap order).
        let mut sorted = due.clone();
        sorted.sort();
        assert_eq!(due, sorted);
        // After the TTL, nothing is due (bodies remain until purged).
        assert!(store.due(10_000).is_empty());
    }

    #[test]
    fn store_remove_keeps_dedup_memory() {
        let mut store = MessageStore::new(10_000);
        let m = msg(0, 4, 0, b"once");
        assert_eq!(store.offer(m.clone(), 0), OfferOutcome::Accept);
        assert_eq!(store.remove(&m.id), Some(m.clone()));
        assert!(store.is_empty());
        // Body gone, but a re-flood is still a duplicate (seen-set persists).
        assert_eq!(store.offer(m, 1), OfferOutcome::Duplicate);
        assert_eq!(store.seen_len(), 1);
    }

    #[test]
    fn store_purge_drops_stale_bodies_and_seen() {
        let mut store = MessageStore::new(1_000);
        store.offer(msg(0, 4, 0, b"old"), 0);
        store.offer(msg(600, 4, 0, b"new"), 600);
        store.purge_expired(1_000); // "old" (age 1000) expired; "new" (age 400) live
        assert_eq!(store.len(), 1);
        assert_eq!(store.seen_len(), 1);
        store.purge_expired(1_600);
        assert!(store.is_empty());
        assert_eq!(store.seen_len(), 0);
    }

    #[test]
    fn store_clock_regression_is_total() {
        let mut store = MessageStore::new(1_000);
        let m = msg(10_000, 4, 0, b"x");
        assert_eq!(store.offer(m.clone(), 10_000), OfferOutcome::Accept);
        // Clock jumps backwards: saturating arithmetic, entry stays live.
        assert_eq!(store.offer(m, 0), OfferOutcome::Duplicate);
    }

    #[test]
    fn store_max_payload_clamped_to_cap() {
        // Asking for a larger cap is clamped so a stored message still fits.
        let mut store = MessageStore::with_max_payload(10_000, MAX_TUNNEL_PAYLOAD + 1000);
        let over = msg(0, 4, 0, &vec![0u8; MAX_TUNNEL_PAYLOAD + 1]);
        assert_eq!(store.offer(over, 0), OfferOutcome::TooLarge);
    }
}
