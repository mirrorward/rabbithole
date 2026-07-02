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
//!   destination hash.
//! - [`packet`]: the Reticulum wire packet header and body, with a
//!   bounds-checked codec that **never panics on truncated or arbitrary input**.
//! - [`announce`]: announce payload construction and Ed25519 verification.
//! - [`crypto`]: signing/verification and an X25519 + AEAD encrypt/decrypt
//!   token (see the divergence note below).
//! - [`lxmf`]: a Lightweight Extensible Message Format (LXMF) message — the
//!   addressed, Ed25519-signed message that rides inside a Reticulum packet
//!   body (see the divergence note below).
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
//!
//! Nothing in this crate performs I/O.

#![forbid(unsafe_code)]

pub mod announce;
pub mod crypto;
pub mod destination;
pub mod identity;
pub mod lxmf;
pub mod packet;

pub use announce::Announce;
pub use destination::{Destination, DESTINATION_HASH_LENGTH, NAME_HASH_LENGTH};
pub use identity::{Identity, PublicIdentity, IDENTITY_HASH_LENGTH, PUBLIC_IDENTITY_LENGTH};
pub use lxmf::{LxmfError, LxmfMessage, SignedLxmf, LXMF_HASH_LENGTH};
pub use packet::{DestinationType, HeaderType, Packet, PacketError, PacketType, PropagationType};
