//! Reticulum (RNS) interop foundation for RabbitHole — Wave 14, slice 1.
//!
//! This crate implements the **protocol data model and cryptographic identity**
//! of [Reticulum](https://reticulum.network) so that a Burrow can eventually be
//! reached over the Reticulum mesh (PLAN §Wave 14, TODO "Reticulum & off-grid
//! mesh"). It deliberately contains **no transport, no interfaces, and no
//! networking** — only the pure, testable primitives that a later transport
//! slice will drive:
//!
//! - [`identity`]: a Reticulum [`Identity`] — an X25519 (encryption) plus
//!   Ed25519 (signing) keypair, whose public form is the 64-byte concatenation
//!   `x25519_public(32) || ed25519_public(32)` and whose identity hash is
//!   `SHA-256(public_identity)` truncated to 16 bytes.
//! - [`destination`]: destination naming (`app_name` + aspects) and the two
//!   Reticulum hashes — the 10-byte `NAME_HASH` and the 16-byte truncated
//!   destination hash — plus [`DestinationHash`], the hex-printable/parsable
//!   value type higher layers key on.
//! - [`packet`]: the Reticulum wire packet header and body, with a
//!   bounds-checked codec that **never panics on truncated or arbitrary
//!   input**, enforces the 500-byte RNS MTU, and exposes the forwarding-
//!   stable packet hash that de-duplication and link ids derive from.
//! - [`announce`]: announce payload construction and Ed25519 verification,
//!   plus [`AnnounceCache`] — a TTL + rate-limit ingestion helper with an
//!   injected clock.
//! - [`link`]: sans-I/O link establishment ([`LinkInitiator`] /
//!   [`LinkResponder`]): link request → proof → RTT, states
//!   `Pending → Handshake → Active → Closed` with injected-clock timeouts,
//!   and the per-direction encrypt/decrypt seam of an established link.
//! - [`crypto`]: signing/verification and an X25519 + AEAD encrypt/decrypt
//!   token (see the divergence note below).
//! - [`lxmf`]: a Lightweight Extensible Message Format (LXMF) message — the
//!   addressed, Ed25519-signed message that rides inside a Reticulum packet
//!   body (see the divergence note below).
//! - [`tunnel`]: the delay-tolerant, content-addressed [`TunnelMessage`] and a
//!   [`MessageStore`] (TTL + de-dup seen-set, injected clock) — the
//!   store-and-forward unit of the S2S tunnel core.
//! - [`floodfill`]: the sans-I/O flood-fill (epidemic dissemination) engine —
//!   [`FloodFill::plan_forward`] computes the loop-safe set of peers to relay a
//!   message to, backed by a [`ForwardLedger`] (seen-by TTL, injected clock).
//! - [`batch`]: bandwidth-aware batching — a [`Batcher`] packs messages per
//!   peer into MTU-bounded, priority/age-ordered [`Batch`]es metered by a
//!   per-peer token-bucket rate governor (see the divergence note below).
//!
//! # Fidelity to upstream Reticulum
//!
//! The hashing, identity layout, packet header bit-packing, and announce
//! payload follow the reference Python stack (`RNS`) as published at
//! <https://reticulum.network> and <https://github.com/markqvist/Reticulum>.
//! The following points are **intentional divergences** for this
//! interop-scaffolding slice; each is flagged again at its call site so the
//! transport slice can reconcile them:
//!
//! 1. **Symmetric cipher.** Upstream Reticulum encrypts SINGLE-destination
//!    payloads with a Fernet-like token: an ephemeral X25519 key exchange,
//!    HKDF-SHA256 key derivation, then **AES-128-CBC + HMAC-SHA256**. To keep
//!    the dependency surface small for this slice we substitute a single
//!    **ChaCha20-Poly1305** AEAD pass over the same ephemeral-ECDH + HKDF-SHA256
//!    key. The token framing (`ephemeral_public || nonce/iv || ciphertext`) is
//!    analogous but **not byte-compatible** with upstream; see [`crypto`].
//! 2. **Announce field order.** We serialize the announce as
//!    `public_identity || name_hash || random_hash || app_data || signature`
//!    (signature last, per this crate's spec). Upstream places the signature
//!    *before* the trailing app-data. The **signed content** is identical
//!    (`destination_hash || public_identity || name_hash || random_hash ||
//!    app_data`), so signatures are semantically equivalent; the wire order
//!    differs. See [`announce`].
//! 3. **Ratchets and IFAC bodies.** Ratchet keys (newer announces) and the
//!    per-interface IFAC authentication field are out of scope here. The packet
//!    codec preserves the IFAC *flag bit* but carries no IFAC field body.
//! 4. **LXMF payload packing.** Upstream [LXMF](https://github.com/markqvist/LXMF)
//!    packs the signed payload as a MessagePack array
//!    `[timestamp, title, content, fields]` with integer-keyed `fields`, and
//!    signs `destination_hash || source_hash || packed_payload || hash`. To
//!    avoid a new MessagePack dependency (this workspace standardizes on
//!    `serde` + `postcard`), [`lxmf`] packs the payload deterministically with
//!    postcard and uses a string-keyed `fields` map, and signs the 32-byte
//!    message `hash` alone. The hash construction
//!    (`SHA-256(destination_hash || source_hash || packed_payload)`) mirrors
//!    upstream, but the packed bytes and the signed input are **not
//!    byte-compatible**; a transport/bridge slice must reconcile the packing
//!    (and the stamp/proof-of-work cost field, deferred here) with upstream
//!    MessagePack before exchanging messages with real LXMF peers. See
//!    [`lxmf`].
//! 5. **Link cipher and RTT framing.** Established links reuse divergence 1's
//!    AEAD substitution: the HKDF salt is the link id (as upstream), but the
//!    64-byte output is split into **per-direction ChaCha20-Poly1305 keys**
//!    with counter nonces and replay rejection, instead of upstream's single
//!    32-byte key + random-IV token; the RTT message is a u64 of
//!    milliseconds, not a msgpack float of seconds. See [`link`].
//! 6. **Delay-tolerant tunnel model.** The [`tunnel`] / [`floodfill`] / [`batch`]
//!    layers are a **pure model** of delay-tolerant store-and-forward over the
//!    mesh (the analogue of an LXMF *propagation node*), not an RNS/LXMF wire
//!    format. The [`TunnelMessage`] and [`Batch`] framings are this crate's own;
//!    the content-addressed message id excludes the in-transit `hops` counter
//!    (mirroring [`Packet::packet_hash`](packet::Packet::packet_hash));
//!    flood-fill's "all peers but the source and known holders" rule, the
//!    message-level hop horizon, the [`ForwardLedger`] seen-by TTL, and the
//!    token-bucket bandwidth governor are all local policy. A later RNS tunnel
//!    adapter/sidecar drives this core and maps it onto real RNS
//!    packets/resources and LXMF propagation transfers; fragmenting a payload
//!    larger than [`packet::MTU`] across multiple packets is **deferred** (this
//!    slice is single-packet only). See [`tunnel`], [`floodfill`], and
//!    [`batch`].
//!
//! Uncertain spec interpretations are additionally flagged inline with
//! `// SPEC-CHECK:` comments and pinned by tests, so a later interop pass can
//! adjust each byte layout in exactly one place.
//!
//! Nothing in this crate performs I/O: randomness is injected (or drawn from
//! the OS CSPRNG only in explicit `generate`/`*_generated` constructors) and
//! every time-sensitive state machine takes a caller-supplied monotonic
//! millisecond clock. This is the pure core that the future RNS gateway
//! sidecar/adapter (PLAN §Wave 14) will drive from its socket loop;
//! `rabbit://` links gaining RNS destination hashes is a later swarm-crate
//! slice.

#![forbid(unsafe_code)]

pub mod announce;
pub mod batch;
pub mod crypto;
pub mod destination;
pub mod floodfill;
pub mod identity;
pub mod link;
pub mod lxmf;
pub mod packet;
pub mod tunnel;

pub use announce::{Announce, AnnounceCache, AnnounceVerdict};
pub use batch::{
    Batch, BatchError, Batcher, TokenBucket, BATCH_ENVELOPE_HEADER_LEN, BATCH_FRAMING_RESERVE,
    BATCH_VERSION, DEFAULT_BATCH_BUDGET, MAX_BATCH_MESSAGES,
};
pub use destination::{
    Destination, DestinationHash, DestinationHashError, DESTINATION_HASH_LENGTH, NAME_HASH_LENGTH,
};
pub use floodfill::{FloodFill, ForwardLedger};
pub use identity::{Identity, PublicIdentity, IDENTITY_HASH_LENGTH, PUBLIC_IDENTITY_LENGTH};
pub use link::{
    CloseReason, LinkError, LinkId, LinkInitiator, LinkProof, LinkRequest, LinkResponder, LinkRole,
    LinkRtt, LinkState,
};
pub use lxmf::{LxmfError, LxmfMessage, SignedLxmf, LXMF_HASH_LENGTH};
pub use packet::{
    max_data_len, DestinationType, HeaderType, Packet, PacketError, PacketType, PropagationType,
    MTU,
};
pub use tunnel::{
    content_id, MessageStore, OfferOutcome, PeerId, TunnelError, TunnelMessage, MAX_TUNNEL_PAYLOAD,
    TUNNEL_ID_LENGTH, TUNNEL_MESSAGE_HEADER_LEN,
};
