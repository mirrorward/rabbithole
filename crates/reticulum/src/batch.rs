//! Bandwidth-aware batching of tunnel messages for a single peer.
//!
//! The top layer of the delay-tolerant tunnel core. A [`Batcher`] queues
//! [`TunnelMessage`]s per destination peer and
//! packs them into [`Batch`]es that (a) fit an MTU-derived size budget, (b) send
//! the highest-priority, oldest messages first, and (c) are metered by a
//! per-peer **token-bucket** rate governor so a slow LoRa-class link is not
//! overrun. It performs **no I/O, no clock reads, and no randomness**: the caller
//! injects `now_ms` and transmits the batches [`encode`](Batch::encode) produces.
//!
//! # Batch envelope (**model**, pinned)
//!
//! A batch serializes as a tiny envelope followed by the self-delimiting
//! messages (each carries its own `payload_len`, so no per-message length prefix
//! is needed):
//!
//! ```text
//! +---------+-----------+===========================================+
//! | version | count u8  | TunnelMessage × count  (self-delimiting)  |
//! | 1 byte  | 1 byte    |                                           |
//! +---------+-----------+===========================================+
//! ```
//!
//! The envelope header is [`BATCH_ENVELOPE_HEADER_LEN`] = 2 bytes; at most
//! [`MAX_BATCH_MESSAGES`] (255) messages fit in one batch (the size budget caps
//! it far lower in practice). This framing is **this crate's own model**, not an
//! RNS or LXMF wire format.
//!
//! # MTU budget math
//!
//! Reticulum fixes the wire MTU at [`MTU`] = 500. A batch
//! rides in the *payload* of a Reticulum packet, so the budget starts from the
//! payload space of the largest header a transport-routed S2S packet uses —
//! HEADER_2, whose budget is [`max_data_len(Header2)`](crate::packet::max_data_len)
//! = 465 — and reserves [`BATCH_FRAMING_RESERVE`] bytes for the link-cipher
//! framing (an 8-byte counter + a 16-byte AEAD tag, per
//! [`link`](crate::link)) in case the adapter sends batches over an encrypted
//! link:
//!
//! ```text
//!   MTU                                    = 500
//!   − HEADER_2 header (flags..context)     =  35   → max_data_len(Header2) = 465
//!   − link counter(8) + AEAD tag(16)       =  24   → BATCH_FRAMING_RESERVE
//!   ────────────────────────────────────────────
//!   DEFAULT_BATCH_BUDGET                    = 441   (encoded batch ≤ this)
//! ```
//!
//! If the adapter instead sends a batch in a cleartext DATA packet, the reserve
//! is simply spare headroom. A single [`TunnelMessage`] of the maximum payload
//! ([`MAX_TUNNEL_PAYLOAD`](crate::tunnel::MAX_TUNNEL_PAYLOAD)) plus the envelope
//! header always fits this budget
//! (asserted by a test), so no message is ever unbatchable.
//!
//! # Token-bucket governor
//!
//! Each peer has an independent bucket: a `capacity` in bytes and a
//! `refill_per_sec` rate. The bucket starts full; [`Batcher::plan_batches`]
//! refills it for the elapsed time (integer math, `capacity` in *milli-bytes* so
//! sub-byte-per-ms refill is exact) and spends the encoded size of each batch it
//! emits. When the next batch would cost more bytes than remain, planning stops
//! and the messages stay queued — throttling the peer to its configured rate.
//! Time advancing between calls refills the bucket, so a slow link drains its
//! backlog gradually.
//!
//! # Partial-batch flushing on age
//!
//! To fill packets efficiently a partial (under-budget) batch is normally
//! **held** for more messages. So a message cannot wait forever, a partial batch
//! is flushed once its oldest queued message has waited `max_batch_age_ms`. A
//! full batch (no room for even the smallest further message) always flushes
//! immediately, subject to the token bucket.
//!
//! # Model vs. spec
//!
//! Batching and the token-bucket governor are a **model** for driving
//! bandwidth-constrained links; they have no direct RNS wire equivalent. Upstream
//! RNS meters per-interface bitrate and announce rates differently, and LXMF
//! propagation transfers negotiate their own framing.
//!
//! // SPEC-CHECK: [`DEFAULT_BATCH_BUDGET`] assumes a HEADER_2 packet with the
//! // link-cipher reserve; an adapter that uses resource transfers, IFAC fields,
//! // or cleartext packets must recompute the budget. The token-bucket
//! // parameters are policy, not protocol. Pinned by the budget and throttling
//! // tests so an interop pass adjusts them in one place.

use std::collections::BTreeMap;

use crate::destination::DESTINATION_HASH_LENGTH;
use crate::packet::MTU;
use crate::tunnel::{PeerId, TunnelError, TunnelMessage, TUNNEL_MESSAGE_HEADER_LEN};

/// Envelope header length: `version(1) || count(1)`.
pub const BATCH_ENVELOPE_HEADER_LEN: usize = 2;

/// Maximum messages in one batch (the `count` field is a `u8`). The size budget
/// caps it far lower in practice.
pub const MAX_BATCH_MESSAGES: usize = u8::MAX as usize;

/// Batch envelope wire-format version (see the module docs).
pub const BATCH_VERSION: u8 = 1;

/// Bytes reserved below the packet payload budget for the link-cipher framing
/// (an 8-byte counter + a 16-byte AEAD tag) in case a batch rides an encrypted
/// [`link`](crate::link). See the module-level budget math.
pub const BATCH_FRAMING_RESERVE: usize = 8 + crate::crypto::TAG_LENGTH;

/// The HEADER_2 payload budget: [`MTU`] minus the largest routed packet header
/// (`flags(1) + hops(1) + transport_id(16) + destination(16) + context(1)`).
/// Equal to [`max_data_len(HeaderType::Header2)`](crate::packet::max_data_len);
/// computed from const primitives here because that helper is not `const fn`
/// (the equality is pinned by a test).
const HEADER2_PAYLOAD_BUDGET: usize = MTU - (2 + 2 * DESTINATION_HASH_LENGTH + 1);

/// Default per-batch encoded-size budget: the HEADER_2 payload budget minus the
/// link-cipher framing reserve. An encoded [`Batch`] never exceeds this.
pub const DEFAULT_BATCH_BUDGET: usize = HEADER2_PAYLOAD_BUDGET - BATCH_FRAMING_RESERVE;

/// Errors produced while decoding a [`Batch`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// The input was shorter than the envelope header or a declared message.
    #[error("batch truncated: need at least {needed} bytes, got {got}")]
    Truncated {
        /// Minimum number of bytes required.
        needed: usize,
        /// Number of bytes actually available.
        got: usize,
    },
    /// The buffer exceeded the Reticulum [`MTU`].
    #[error("batch of {0} bytes exceeds the {mtu}-byte MTU", mtu = MTU)]
    TooLarge(usize),
    /// The envelope version byte was not [`BATCH_VERSION`].
    #[error("unsupported batch version {0}")]
    BadVersion(u8),
    /// A contained message was malformed (see [`TunnelError`]).
    #[error("batch contains a malformed message: {0}")]
    BadMessage(#[from] TunnelError),
    /// Bytes remained after the declared message count was decoded.
    #[error("batch has {0} unexpected trailing bytes")]
    TrailingBytes(usize),
}

/// An encode/decode-round-trippable envelope of tunnel messages bound for one
/// peer. Construction via [`Batcher::plan_batches`] guarantees the encoded size
/// stays within the configured budget.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Batch {
    /// The messages carried, in send order (priority-then-age).
    pub messages: Vec<TunnelMessage>,
}

impl Batch {
    /// An empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of messages carried.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the batch carries no messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// The encoded size in bytes ([`BATCH_ENVELOPE_HEADER_LEN`] + each message).
    pub fn encoded_len(&self) -> usize {
        BATCH_ENVELOPE_HEADER_LEN
            + self
                .messages
                .iter()
                .map(TunnelMessage::encoded_len)
                .sum::<usize>()
    }

    /// Serialize the batch (see the module-level envelope layout).
    ///
    /// Errors if the batch holds more than [`MAX_BATCH_MESSAGES`], if any
    /// message payload exceeds [`MAX_TUNNEL_PAYLOAD`](crate::tunnel::MAX_TUNNEL_PAYLOAD),
    /// or if the encoded batch would exceed the [`MTU`].
    pub fn encode(&self) -> Result<Vec<u8>, BatchError> {
        if self.messages.len() > MAX_BATCH_MESSAGES {
            // Encode `count` as a u8; a batch this large cannot exist via the
            // Batcher (the size budget caps it), but be total for hand-built ones.
            return Err(BatchError::TooLarge(self.encoded_len()));
        }
        let mut out = Vec::with_capacity(self.encoded_len());
        out.push(BATCH_VERSION);
        out.push(self.messages.len() as u8);
        for msg in &self.messages {
            out.extend_from_slice(&msg.encode()?);
        }
        if out.len() > MTU {
            return Err(BatchError::TooLarge(out.len()));
        }
        Ok(out)
    }

    /// Decode a batch from bytes. **Total**: truncated, over-MTU, bad-version,
    /// malformed-message, or trailing input yields a [`BatchError`], never a
    /// panic.
    pub fn decode(bytes: &[u8]) -> Result<Self, BatchError> {
        if bytes.len() > MTU {
            return Err(BatchError::TooLarge(bytes.len()));
        }
        if bytes.len() < BATCH_ENVELOPE_HEADER_LEN {
            return Err(BatchError::Truncated {
                needed: BATCH_ENVELOPE_HEADER_LEN,
                got: bytes.len(),
            });
        }
        let version = bytes[0];
        if version != BATCH_VERSION {
            return Err(BatchError::BadVersion(version));
        }
        let count = bytes[1] as usize;
        let mut messages = Vec::with_capacity(count);
        let mut cursor = BATCH_ENVELOPE_HEADER_LEN;
        for _ in 0..count {
            let (msg, next) = TunnelMessage::decode_from(bytes, cursor)?;
            messages.push(msg);
            cursor = next;
        }
        if cursor != bytes.len() {
            return Err(BatchError::TrailingBytes(bytes.len() - cursor));
        }
        Ok(Self { messages })
    }
}

/// A per-peer token-bucket byte-rate governor (injected clock).
///
/// `capacity` is the burst size in bytes; `refill_per_sec` is the sustained
/// byte rate. Tokens are tracked internally in *milli-bytes* so that the
/// per-millisecond refill (`refill_per_sec` milli-bytes per ms) is exact integer
/// arithmetic. All operations saturate, so a backwards or huge clock jump never
/// panics or overflows. A `capacity` of 0 blocks all sending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenBucket {
    capacity_milli: u64,
    refill_per_sec: u64,
    tokens_milli: u64,
    last_ms: u64,
}

/// Milli-bytes per byte (the internal token scale).
const MILLI: u64 = 1_000;

impl TokenBucket {
    /// Create a bucket that starts **full** at `now_ms`.
    pub fn new(capacity_bytes: u64, refill_per_sec: u64, now_ms: u64) -> Self {
        let capacity_milli = capacity_bytes.saturating_mul(MILLI);
        Self {
            capacity_milli,
            refill_per_sec,
            tokens_milli: capacity_milli,
            last_ms: now_ms,
        }
    }

    /// Would-be token count (milli-bytes) after refilling to `now_ms`, without
    /// mutating the bucket.
    fn peek_milli(&self, now_ms: u64) -> u64 {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        let refill = elapsed.saturating_mul(self.refill_per_sec);
        self.tokens_milli
            .saturating_add(refill)
            .min(self.capacity_milli)
    }

    /// Refill the bucket up to `now_ms`.
    pub fn refill(&mut self, now_ms: u64) {
        // Only advance `last_ms` forward, so a backwards clock does not "steal"
        // refill on a later forward step.
        if now_ms > self.last_ms {
            self.tokens_milli = self.peek_milli(now_ms);
            self.last_ms = now_ms;
        }
    }

    /// Bytes currently available (after refilling to `now_ms`), without
    /// mutating the bucket.
    pub fn available_bytes(&self, now_ms: u64) -> u64 {
        self.peek_milli(now_ms) / MILLI
    }

    /// Try to spend `bytes` at `now_ms`, refilling first. Returns `true` and
    /// deducts the cost on success; returns `false` and leaves the bucket
    /// untouched (beyond the refill) when there are not enough tokens.
    pub fn try_spend(&mut self, bytes: usize, now_ms: u64) -> bool {
        self.refill(now_ms);
        let cost = (bytes as u64).saturating_mul(MILLI);
        if self.tokens_milli >= cost {
            self.tokens_milli -= cost;
            true
        } else {
            false
        }
    }
}

/// A message queued for a peer, tagged with when it entered the queue (for
/// age-based partial flushing).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Queued {
    msg: TunnelMessage,
    enqueued_ms: u64,
}

/// One peer's pending queue plus its rate governor.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PeerQueue {
    /// id → queued message (dedup by content id).
    queue: BTreeMap<[u8; crate::tunnel::TUNNEL_ID_LENGTH], Queued>,
    bucket: TokenBucket,
}

/// Bandwidth-aware, per-peer message batcher.
///
/// Configured once with a batch size `budget`, a partial-flush `max_batch_age_ms`,
/// and the per-peer token-bucket parameters (`capacity`, `refill_per_sec`) that
/// every peer's bucket is created with. See the module docs for the packing,
/// throttling, and flushing rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batcher {
    budget: usize,
    max_batch_age_ms: u64,
    capacity_bytes: u64,
    refill_per_sec: u64,
    peers: BTreeMap<PeerId, PeerQueue>,
}

impl Batcher {
    /// Create a batcher with an explicit size `budget`.
    ///
    /// `budget` is clamped to `[BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN,
    /// DEFAULT_BATCH_BUDGET]` so a batch always fits one packet and can always
    /// hold at least one (minimal) message.
    pub fn new(
        budget: usize,
        max_batch_age_ms: u64,
        capacity_bytes: u64,
        refill_per_sec: u64,
    ) -> Self {
        let min_budget = BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN;
        Self {
            budget: budget.clamp(min_budget, DEFAULT_BATCH_BUDGET),
            max_batch_age_ms,
            capacity_bytes,
            refill_per_sec,
            peers: BTreeMap::new(),
        }
    }

    /// Create a batcher with the [`DEFAULT_BATCH_BUDGET`].
    pub fn with_default_budget(
        max_batch_age_ms: u64,
        capacity_bytes: u64,
        refill_per_sec: u64,
    ) -> Self {
        Self::new(
            DEFAULT_BATCH_BUDGET,
            max_batch_age_ms,
            capacity_bytes,
            refill_per_sec,
        )
    }

    /// The effective per-batch size budget (after clamping).
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Queue `msg` for `peer` at `now_ms`, creating the peer's bucket (full) on
    /// first use. Deduplicated by content id: re-queuing an id already present
    /// keeps the original enqueue time (so its age keeps advancing) but refreshes
    /// the body. Returns `true` if the id was newly queued.
    pub fn enqueue(&mut self, peer: PeerId, msg: TunnelMessage, now_ms: u64) -> bool {
        let capacity = self.capacity_bytes;
        let refill = self.refill_per_sec;
        let pq = self.peers.entry(peer).or_insert_with(|| PeerQueue {
            queue: BTreeMap::new(),
            bucket: TokenBucket::new(capacity, refill, now_ms),
        });
        match pq.queue.get_mut(&msg.id) {
            Some(existing) => {
                existing.msg = msg; // refresh body, keep enqueued_ms
                false
            }
            None => {
                pq.queue.insert(
                    msg.id,
                    Queued {
                        msg,
                        enqueued_ms: now_ms,
                    },
                );
                true
            }
        }
    }

    /// Number of messages queued for `peer`.
    pub fn queued_len(&self, peer: &PeerId) -> usize {
        self.peers.get(peer).map_or(0, |pq| pq.queue.len())
    }

    /// Bytes currently available in `peer`'s token bucket at `now_ms`.
    pub fn available_bytes(&self, peer: &PeerId, now_ms: u64) -> u64 {
        self.peers
            .get(peer)
            .map_or(0, |pq| pq.bucket.available_bytes(now_ms))
    }

    /// Plan (and commit) the batches to send to `peer` now.
    ///
    /// Messages are ordered highest-priority first, then oldest (smallest
    /// `created_ms`) first, then by id for determinism, and packed next-fit into
    /// budget-bounded batches. A batch is emitted when it is *full* (no room for
    /// even a minimal further message) or *aged* (its oldest queued message has
    /// waited `max_batch_age_ms`); a partial, un-aged tail batch is held for
    /// later. Each emitted batch's encoded size is spent from the peer's token
    /// bucket; once the bucket cannot afford the next ready batch, planning stops
    /// and the remainder stays queued.
    ///
    /// This **commits**: emitted messages are removed from the queue and the
    /// token bucket is debited. Returns the batches to transmit, in send order.
    pub fn plan_batches(&mut self, peer: &PeerId, now_ms: u64) -> Vec<Batch> {
        let Some(pq) = self.peers.get_mut(peer) else {
            return Vec::new();
        };
        pq.bucket.refill(now_ms);
        if pq.queue.is_empty() {
            return Vec::new();
        }

        // Order: priority desc, then created_ms asc, then id asc (deterministic).
        let mut ordered: Vec<Queued> = pq.queue.values().cloned().collect();
        ordered.sort_by(|a, b| {
            b.msg
                .priority
                .cmp(&a.msg.priority)
                .then(a.msg.created_ms.cmp(&b.msg.created_ms))
                .then(a.msg.id.cmp(&b.msg.id))
        });

        // Next-fit packing into size-budgeted groups.
        let mut groups: Vec<Vec<Queued>> = Vec::new();
        let mut current: Vec<Queued> = Vec::new();
        let mut current_len = BATCH_ENVELOPE_HEADER_LEN;
        for q in ordered {
            let add = q.msg.encoded_len();
            // A message that cannot fit even alone within the budget can never be
            // batched for this peer (fragmentation across packets is deferred);
            // skip it so it never forms an over-budget batch. With the
            // DEFAULT_BATCH_BUDGET no valid message is ever skipped. Skipped
            // messages stay queued (held pending fragmentation support).
            if BATCH_ENVELOPE_HEADER_LEN + add > self.budget {
                continue;
            }
            let fits_size = current_len + add <= self.budget;
            let fits_count = current.len() < MAX_BATCH_MESSAGES;
            if !current.is_empty() && (!fits_size || !fits_count) {
                groups.push(std::mem::take(&mut current));
                current_len = BATCH_ENVELOPE_HEADER_LEN;
            }
            current_len += add;
            current.push(q);
        }
        if !current.is_empty() {
            groups.push(current);
        }

        let min_message = TUNNEL_MESSAGE_HEADER_LEN;
        let mut out = Vec::new();
        let mut emitted_ids: Vec<[u8; crate::tunnel::TUNNEL_ID_LENGTH]> = Vec::new();
        for group in groups {
            let encoded_len = BATCH_ENVELOPE_HEADER_LEN
                + group.iter().map(|q| q.msg.encoded_len()).sum::<usize>();
            // Full = no room for even a minimal further message.
            let full = encoded_len + min_message > self.budget;
            // Aged = the oldest queued message has waited long enough.
            let aged = group
                .iter()
                .map(|q| now_ms.saturating_sub(q.enqueued_ms))
                .max()
                .unwrap_or(0)
                >= self.max_batch_age_ms;
            if !full && !aged {
                break; // hold this partial, un-aged tail (and everything after)
            }
            if !pq.bucket.try_spend(encoded_len, now_ms) {
                break; // throttled: not enough tokens for this batch
            }
            for q in &group {
                emitted_ids.push(q.msg.id);
            }
            out.push(Batch {
                messages: group.into_iter().map(|q| q.msg).collect(),
            });
        }

        for id in emitted_ids {
            pq.queue.remove(&id);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::DestinationHash;
    use crate::packet::{max_data_len, HeaderType};
    use crate::tunnel::{MAX_TUNNEL_PAYLOAD, TUNNEL_ID_LENGTH};

    fn peer(n: u8) -> PeerId {
        DestinationHash([n; TUNNEL_ID_LENGTH])
    }

    fn msg(created_ms: u64, priority: u8, payload: &[u8]) -> TunnelMessage {
        TunnelMessage::new(created_ms, 8, priority, payload)
    }

    // --- budget math ----------------------------------------------------

    #[test]
    fn budget_math_is_pinned() {
        // The const-computed HEADER_2 budget must equal the runtime helper
        // (max_data_len is not const, so we recompute it from primitives).
        assert_eq!(HEADER2_PAYLOAD_BUDGET, max_data_len(HeaderType::Header2));
        assert_eq!(max_data_len(HeaderType::Header2), 465);
        assert_eq!(BATCH_FRAMING_RESERVE, 24);
        assert_eq!(DEFAULT_BATCH_BUDGET, 441);
        assert_eq!(BATCH_ENVELOPE_HEADER_LEN, 2);
    }

    #[test]
    fn one_max_message_always_fits_the_budget() {
        // The core invariant tying tunnel.rs's cap to batch.rs's budget, checked
        // at compile time.
        const {
            assert!(
                BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN + MAX_TUNNEL_PAYLOAD
                    <= DEFAULT_BATCH_BUDGET
            );
        }
        let m = msg(0, 0, &vec![0xAB; MAX_TUNNEL_PAYLOAD]);
        let batch = Batch { messages: vec![m] };
        assert!(batch.encoded_len() <= DEFAULT_BATCH_BUDGET);
        assert!(batch.encode().unwrap().len() <= MTU);
    }

    // --- Batch encode/decode -------------------------------------------

    #[test]
    fn batch_roundtrip() {
        let batch = Batch {
            messages: vec![msg(1, 0, b"first"), msg(2, 0, b"second"), msg(3, 0, b"")],
        };
        let bytes = batch.encode().unwrap();
        assert_eq!(bytes.len(), batch.encoded_len());
        assert_eq!(bytes[0], BATCH_VERSION);
        assert_eq!(bytes[1], 3);
        assert_eq!(Batch::decode(&bytes).unwrap(), batch);
    }

    #[test]
    fn empty_batch_roundtrip() {
        let batch = Batch::new();
        let bytes = batch.encode().unwrap();
        assert_eq!(bytes, vec![BATCH_VERSION, 0]);
        assert_eq!(Batch::decode(&bytes).unwrap(), batch);
    }

    #[test]
    fn decode_rejects_bad_version() {
        let mut bytes = Batch {
            messages: vec![msg(1, 0, b"x")],
        }
        .encode()
        .unwrap();
        bytes[0] = 0x02;
        assert_eq!(Batch::decode(&bytes), Err(BatchError::BadVersion(0x02)));
    }

    #[test]
    fn decode_rejects_trailing_and_truncated() {
        let batch = Batch {
            messages: vec![msg(1, 0, b"hello")],
        };
        let bytes = batch.encode().unwrap();
        // Trailing garbage.
        let mut extra = bytes.clone();
        extra.push(0x00);
        assert_eq!(Batch::decode(&extra), Err(BatchError::TrailingBytes(1)));
        // Every truncation errors, never panics.
        for len in 0..bytes.len() {
            assert!(Batch::decode(&bytes[..len]).is_err());
        }
    }

    #[test]
    fn decode_rejects_over_mtu() {
        assert_eq!(
            Batch::decode(&vec![0u8; MTU + 1]),
            Err(BatchError::TooLarge(MTU + 1))
        );
    }

    #[test]
    fn decode_arbitrary_bytes_never_panics() {
        let mut state: u64 = 0xF00D_BABE_1357_9BDF;
        for _ in 0..5000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 55) as usize % 300;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 40) as u8);
            }
            let _ = Batch::decode(&buf);
        }
    }

    // --- TokenBucket ----------------------------------------------------

    #[test]
    fn bucket_starts_full_and_refills_to_capacity() {
        let mut b = TokenBucket::new(100, 10, 0);
        assert_eq!(b.available_bytes(0), 100);
        // Spend all of it.
        assert!(b.try_spend(100, 0));
        assert_eq!(b.available_bytes(0), 0);
        // Refills 10 bytes/sec → 5 bytes after 500 ms.
        assert_eq!(b.available_bytes(500), 5);
        // Never exceeds capacity, however long we wait.
        assert_eq!(b.available_bytes(1_000_000), 100);
    }

    #[test]
    fn bucket_sub_byte_refill_is_exact() {
        // 1 byte/sec → 1 milli-byte/ms; after 1 ms, 0 whole bytes, but exact.
        let mut b = TokenBucket::new(10, 1, 0);
        assert!(b.try_spend(10, 0));
        assert_eq!(b.available_bytes(1), 0); // 1 milli-byte, floors to 0
        assert_eq!(b.available_bytes(1_000), 1); // exactly 1 byte after 1 s
        assert_eq!(b.available_bytes(2_500), 2); // 2.5 → 2 bytes
    }

    #[test]
    fn bucket_try_spend_rejects_when_insufficient() {
        let mut b = TokenBucket::new(50, 0, 0); // no refill
        assert!(!b.try_spend(51, 0));
        assert!(b.try_spend(50, 0));
        assert!(!b.try_spend(1, 0));
    }

    #[test]
    fn bucket_backwards_clock_is_total() {
        let mut b = TokenBucket::new(100, 10, 1_000);
        assert!(b.try_spend(100, 1_000));
        // Clock jumps backwards: no refill stolen, no panic.
        assert_eq!(b.available_bytes(0), 0);
        b.refill(0);
        assert_eq!(b.available_bytes(1_000), 0);
        // Forward again refills normally.
        assert_eq!(b.available_bytes(2_000), 10);
    }

    #[test]
    fn bucket_zero_capacity_blocks() {
        let mut b = TokenBucket::new(0, 100, 0);
        assert!(!b.try_spend(1, 0));
        assert!(b.try_spend(0, 0)); // spending nothing always succeeds
    }

    // --- Batcher packing ------------------------------------------------

    #[test]
    fn enqueue_dedupes_by_id_keeping_age() {
        let mut b = Batcher::with_default_budget(0, 10_000, 10_000);
        let m = msg(1, 0, b"same");
        assert!(b.enqueue(peer(1), m.clone(), 0));
        assert!(!b.enqueue(peer(1), m.clone(), 100)); // dup id
        assert_eq!(b.queued_len(&peer(1)), 1);
    }

    #[test]
    fn batches_never_exceed_budget() {
        // Budget with room for exactly one empty (29-byte) message; ten distinct
        // empty messages → ten single-message batches, each within budget.
        let budget = BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN; // 31
        let mut b = Batcher::new(budget, 0, 1_000_000, 1_000_000);
        for i in 0..10u64 {
            // Distinct created_ms → distinct content ids (empty payload).
            b.enqueue(peer(1), msg(i, 0, b""), 0);
        }
        let batches = b.plan_batches(&peer(1), 0);
        assert!(!batches.is_empty());
        let mut total_msgs = 0;
        for batch in &batches {
            assert!(batch.encoded_len() <= b.budget());
            assert!(batch.encode().unwrap().len() <= MTU);
            total_msgs += batch.len();
        }
        assert_eq!(total_msgs, 10);
        assert_eq!(b.queued_len(&peer(1)), 0);
    }

    #[test]
    fn many_messages_pack_into_multiple_full_batches() {
        // A larger budget packs several messages per batch; verify every batch
        // stays within budget and all messages are accounted for.
        let mut b = Batcher::new(200, 0, 10_000_000, 10_000_000);
        for i in 0..20u64 {
            b.enqueue(peer(1), msg(i, 0, &[0xEE; 20]), 0);
        }
        let batches = b.plan_batches(&peer(1), 0);
        let mut total = 0;
        for batch in &batches {
            assert!(batch.encoded_len() <= 200);
            assert!(batch.len() > 1, "expected multi-message batches");
            total += batch.len();
        }
        assert_eq!(total, 20);
        assert_eq!(b.queued_len(&peer(1)), 0);
    }

    #[test]
    fn message_too_large_for_custom_budget_is_skipped() {
        // A tiny budget that fits an empty message but not a 100-byte-payload
        // one: the big message is held (fragmentation deferred); the small one
        // flows, and no emitted batch exceeds the budget.
        let budget = BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN; // 31
        let mut b = Batcher::new(budget, 0, 1_000_000, 1_000_000);
        b.enqueue(peer(1), msg(1, 0, &[0u8; 100]), 0); // 131 encoded > budget
        b.enqueue(peer(1), msg(2, 0, b""), 0); // 29 encoded, fits
        let batches = b.plan_batches(&peer(1), 0);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
        assert!(batches[0].encoded_len() <= budget);
        assert_eq!(batches[0].messages[0].payload, b"");
        // The oversized message is still queued.
        assert_eq!(b.queued_len(&peer(1)), 1);
    }

    #[test]
    fn packing_orders_by_priority_then_age() {
        // max_batch_age_ms = 0 → flush immediately regardless of fill.
        let mut b = Batcher::with_default_budget(0, 1_000_000, 1_000_000);
        b.enqueue(peer(1), msg(10, 1, b"low-old"), 0);
        b.enqueue(peer(1), msg(5, 5, b"high-old"), 0);
        b.enqueue(peer(1), msg(20, 5, b"high-new"), 0);
        let batches = b.plan_batches(&peer(1), 0);
        assert_eq!(batches.len(), 1);
        let order: Vec<&[u8]> = batches[0]
            .messages
            .iter()
            .map(|m| m.payload.as_slice())
            .collect();
        // priority 5 before priority 1; within priority 5, older (created 5) first.
        assert_eq!(
            order,
            vec![&b"high-old"[..], &b"high-new"[..], &b"low-old"[..]]
        );
    }

    #[test]
    fn partial_batch_is_held_until_aged() {
        // Big budget, so one message is a partial batch. age threshold 1000 ms.
        let mut b = Batcher::with_default_budget(1_000, 1_000_000, 1_000_000);
        b.enqueue(peer(1), msg(0, 0, b"lonely"), 0);
        // Too soon: partial batch held, nothing emitted.
        assert!(b.plan_batches(&peer(1), 500).is_empty());
        assert_eq!(b.queued_len(&peer(1)), 1);
        // Aged out: flushed.
        let batches = b.plan_batches(&peer(1), 1_000);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
        assert_eq!(b.queued_len(&peer(1)), 0);
    }

    #[test]
    fn full_batch_flushes_immediately_even_if_not_aged() {
        // Budget for exactly one empty message → the batch is "full" at once.
        let budget = BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN;
        let mut b = Batcher::new(budget, 1_000_000, 1_000_000, 1_000_000);
        b.enqueue(peer(1), msg(0, 0, b""), 0);
        // age threshold is huge, but a full batch flushes now.
        let batches = b.plan_batches(&peer(1), 0);
        assert_eq!(batches.len(), 1);
    }

    #[test]
    fn token_bucket_throttles_across_injected_time() {
        // Each message encodes to 29 bytes (empty payload). Budget for exactly
        // one message per batch. Bucket: capacity 31 (one batch of 2+29), refill
        // 31 bytes/sec.
        let one_batch = BATCH_ENVELOPE_HEADER_LEN + TUNNEL_MESSAGE_HEADER_LEN; // 31
        let mut b = Batcher::new(one_batch, 0, one_batch as u64, one_batch as u64);
        for i in 0..3u64 {
            // Empty payload (29 bytes); distinct created_ms → distinct ids.
            b.enqueue(peer(1), msg(i, 0, b""), 0);
        }
        // At t=0 the bucket holds exactly one batch worth → one batch emitted.
        let first = b.plan_batches(&peer(1), 0);
        assert_eq!(first.len(), 1);
        assert_eq!(b.queued_len(&peer(1)), 2);
        // Immediately again: bucket empty → throttled, nothing emitted.
        assert!(b.plan_batches(&peer(1), 0).is_empty());
        assert_eq!(b.queued_len(&peer(1)), 2);
        // After 1 s the bucket refills one batch worth → one more batch.
        let second = b.plan_batches(&peer(1), 1_000);
        assert_eq!(second.len(), 1);
        assert_eq!(b.queued_len(&peer(1)), 1);
        // After another second, the last one drains.
        let third = b.plan_batches(&peer(1), 2_000);
        assert_eq!(third.len(), 1);
        assert_eq!(b.queued_len(&peer(1)), 0);
    }

    #[test]
    fn plan_for_unknown_peer_is_empty() {
        let mut b = Batcher::with_default_budget(0, 1_000, 1_000);
        assert!(b.plan_batches(&peer(9), 0).is_empty());
    }

    #[test]
    fn per_peer_queues_and_buckets_are_independent() {
        let mut b = Batcher::with_default_budget(0, 1_000_000, 1_000_000);
        b.enqueue(peer(1), msg(0, 0, b"for-one"), 0);
        b.enqueue(peer(2), msg(0, 0, b"for-two"), 0);
        let b1 = b.plan_batches(&peer(1), 0);
        assert_eq!(b1.len(), 1);
        assert_eq!(b1[0].messages[0].payload, b"for-one");
        // peer(2) still has its message.
        assert_eq!(b.queued_len(&peer(2)), 1);
    }

    // --- end-to-end: offer → flood plan → batch → decode round-trip -----

    #[test]
    fn end_to_end_offer_flood_batch_decode() {
        use crate::floodfill::{FloodFill, ForwardLedger};
        use crate::tunnel::{MessageStore, OfferOutcome};

        let now = 1_000u64;
        // A node with three tunnel peers and a live store.
        let peers = [peer(1), peer(2), peer(3)];
        let ff = FloodFill::with_peers(peers);
        let mut store = MessageStore::new(60_000);
        let mut ledger = ForwardLedger::new(60_000);
        // Immediate flush, generous bandwidth for the round-trip check.
        let mut batcher = Batcher::with_default_budget(0, 1_000_000, 1_000_000);

        // 1. A message arrives from peer(1).
        let incoming = msg(now, 3, b"delay-tolerant hello");
        let from = peer(1);
        assert_eq!(store.offer(incoming.clone(), now), OfferOutcome::Accept);
        assert_eq!(store.offer(incoming.clone(), now), OfferOutcome::Duplicate);

        // 2. Flood-plan: relay to every peer but the source.
        ledger.record(incoming.id, from, now);
        let plan = ff.plan_forward(&incoming, Some(from), &ledger, now);
        assert_eq!(plan, vec![peer(2), peer(3)]);

        // 3. Enqueue the forwarded copy (hops + 1) to each planned peer.
        let relay = store.get(&incoming.id).unwrap().forwarded();
        assert_eq!(relay.hops, 1);
        for p in &plan {
            batcher.enqueue(*p, relay.clone(), now);
        }
        ledger.record_all(incoming.id, &plan, now);

        // 4. Batch per peer, encode, and decode round-trip.
        for p in &plan {
            let batches = batcher.plan_batches(p, now);
            assert_eq!(batches.len(), 1);
            let bytes = batches[0].encode().unwrap();
            assert!(bytes.len() <= MTU);
            let decoded = Batch::decode(&bytes).unwrap();
            assert_eq!(decoded, batches[0]);
            assert_eq!(decoded.messages.len(), 1);
            let round = &decoded.messages[0];
            assert_eq!(round.id, incoming.id); // same content address
            assert_eq!(round.hops, 1); // one hop travelled
            assert_eq!(round.payload, b"delay-tolerant hello");
            assert!(round.verify_id());
        }

        // 5. Re-offering the same id does not re-flood (all peers known holders).
        assert!(ff
            .plan_forward(&incoming, Some(peer(3)), &ledger, now)
            .is_empty());
    }
}
