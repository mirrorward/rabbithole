//! LXMF — the Lightweight Extensible Message Format message layer.
//!
//! [LXMF](https://github.com/markqvist/LXMF) is the messaging format used across
//! Reticulum / NomadNet. A message is addressed *to* a destination hash, *from*
//! a source destination hash, and carries a structured, signed payload:
//! `[timestamp, title, content, fields]`. The 32-byte message *hash* binds the
//! addressing and the payload together:
//!
//! ```text
//! hash = SHA-256(destination_hash || source_hash || packed_payload)
//! ```
//!
//! and doubles as the message id. The source identity signs the message so any
//! recipient can authenticate it against the source's published identity.
//!
//! This module is pure and sans-I/O, mirroring the rest of the crate: it builds,
//! hashes, signs, verifies, packs and unpacks messages, but performs no clock
//! reads (the caller supplies `timestamp`), no randomness, and no networking.
//! [`SignedLxmf::unpack`] is **total** — arbitrary or truncated input yields an
//! [`LxmfError`], never a panic.
//!
//! # Divergence from upstream LXMF
//!
//! Upstream LXMF packs the payload as a MessagePack array
//! `[timestamp: float, title: bytes, content: bytes, fields: map]` (with
//! integer-keyed `fields`) and computes the Ed25519 signature over
//! `destination_hash || source_hash || packed_payload || hash`.
//!
//! This crate standardizes on `serde` + `postcard` and, to avoid introducing a
//! MessagePack dependency, makes two **intentional, documented** divergences:
//!
//! 1. **Payload packing.** The payload is packed with [`postcard`] in the field
//!    order `timestamp(f64) || title || content || fields`, where `fields` is a
//!    deterministically ordered [`BTreeMap<String, Vec<u8>>`]. This is *not*
//!    byte-compatible with upstream MessagePack, though the hash *construction*
//!    (`SHA-256(destination_hash || source_hash || packed_payload)`) is the
//!    same. A transport/bridge slice must re-pack against upstream MessagePack
//!    before exchanging messages with real LXMF peers.
//! 2. **Signed input.** We sign the 32-byte message [`hash`](LxmfMessage::hash)
//!    alone (which already binds all addressing and payload bytes), rather than
//!    upstream's `… || packed_payload || hash`. Signatures are therefore
//!    semantically equivalent but not produced over the same byte string.
//!
//! The LXMF *stamp* / proof-of-work cost field is **deferred**: it is a
//! spam-mitigation concern layered above the signed message and is not modeled
//! here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::destination::DESTINATION_HASH_LENGTH;
use crate::identity::{Identity, PublicIdentity, SIGNATURE_LENGTH};

/// Length in bytes of an LXMF message hash — a full (untruncated) SHA-256.
pub const LXMF_HASH_LENGTH: usize = 32;

/// Errors produced while unpacking an LXMF message from bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LxmfError {
    /// The bytes were not a valid packed [`SignedLxmf`] (truncated, trailing
    /// garbage, or otherwise malformed). Unpacking is total: this is returned
    /// instead of panicking on any arbitrary input.
    #[error("lxmf message malformed")]
    Malformed,
}

/// An addressed, structured LXMF message, prior to signing.
///
/// The `fields` map is string-keyed (see the module-level divergence note) and
/// stored in a [`BTreeMap`] so that packing — and therefore the message
/// [`hash`](Self::hash) — is deterministic regardless of insertion order.
///
/// Contains an `f64` `timestamp`, so this type is [`PartialEq`] but not [`Eq`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LxmfMessage {
    /// The 16-byte destination hash the message is addressed to.
    pub destination_hash: [u8; DESTINATION_HASH_LENGTH],
    /// The 16-byte destination hash the message originates from.
    pub source_hash: [u8; DESTINATION_HASH_LENGTH],
    /// Message timestamp, in seconds since the Unix epoch (as in upstream LXMF).
    pub timestamp: f64,
    /// The message title (opaque bytes; often UTF-8).
    pub title: Vec<u8>,
    /// The message content / body (opaque bytes; often UTF-8).
    pub content: Vec<u8>,
    /// Extensible, deterministically ordered field map (string key → bytes).
    pub fields: BTreeMap<String, Vec<u8>>,
}

/// A view of the packed-payload fields, in the exact order they are hashed.
///
/// Serializing this with postcard yields `timestamp || title || content ||
/// fields`, mirroring the upstream `[timestamp, title, content, fields]` array.
#[derive(Serialize)]
struct PackedPayload<'a> {
    timestamp: f64,
    title: &'a [u8],
    content: &'a [u8],
    fields: &'a BTreeMap<String, Vec<u8>>,
}

impl LxmfMessage {
    /// Build a message. `timestamp` is supplied by the caller (this module reads
    /// no clock); `title`/`content` are copied.
    pub fn new(
        destination_hash: [u8; DESTINATION_HASH_LENGTH],
        source_hash: [u8; DESTINATION_HASH_LENGTH],
        timestamp: f64,
        title: &[u8],
        content: &[u8],
        fields: BTreeMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            destination_hash,
            source_hash,
            timestamp,
            title: title.to_vec(),
            content: content.to_vec(),
            fields,
        }
    }

    /// The deterministically packed payload (`timestamp || title || content ||
    /// fields`) — the bytes fed, together with the addressing, into the hash.
    pub fn packed_payload(&self) -> Vec<u8> {
        let view = PackedPayload {
            timestamp: self.timestamp,
            title: &self.title,
            content: &self.content,
            fields: &self.fields,
        };
        // postcard serialization of these plain data fields cannot fail: there
        // are no custom `Serialize` impls and `to_allocvec` grows its buffer.
        postcard::to_allocvec(&view).expect("postcard packing of an LXMF payload is infallible")
    }

    /// The 32-byte LXMF message hash / id:
    /// `SHA-256(destination_hash || source_hash || packed_payload)`.
    pub fn hash(&self) -> [u8; LXMF_HASH_LENGTH] {
        let mut hasher = Sha256::new();
        hasher.update(self.destination_hash);
        hasher.update(self.source_hash);
        hasher.update(self.packed_payload());
        let digest = hasher.finalize();
        let mut out = [0u8; LXMF_HASH_LENGTH];
        out.copy_from_slice(&digest);
        out
    }

    /// Sign the message with `identity`'s Ed25519 key, over the message
    /// [`hash`](Self::hash) (see the module-level divergence note), yielding a
    /// [`SignedLxmf`].
    pub fn sign(&self, identity: &Identity) -> SignedLxmf {
        let signature = identity.sign(&self.hash());
        SignedLxmf {
            message: self.clone(),
            signature,
        }
    }
}

/// An [`LxmfMessage`] together with its detached Ed25519 signature.
///
/// This is the unit that rides inside a Reticulum packet body: use
/// [`pack`](Self::pack) to serialize it and [`unpack`](Self::unpack) to parse it
/// back (totally, without panicking on malformed input).
///
/// The packed form is `signature(64) || postcard(message)`. The signature is
/// framed manually (rather than via `serde`) because `serde`'s derived
/// `Deserialize` only covers fixed arrays up to length 32, not the 64-byte
/// Ed25519 signature.
#[derive(Clone, Debug, PartialEq)]
pub struct SignedLxmf {
    /// The signed message.
    pub message: LxmfMessage,
    /// Ed25519 signature over the message hash by the source identity.
    pub signature: [u8; SIGNATURE_LENGTH],
}

impl SignedLxmf {
    /// The message hash / id (see [`LxmfMessage::hash`]).
    pub fn hash(&self) -> [u8; LXMF_HASH_LENGTH] {
        self.message.hash()
    }

    /// Verify the signature against the source's public identity.
    ///
    /// Returns `true` only if the Ed25519 signature verifies against
    /// `source_identity` over the recomputed message [`hash`](Self::hash).
    ///
    /// Note: this authenticates the *content* against the signing identity. It
    /// does **not** by itself prove that `source_identity` owns the message's
    /// `source_hash`; a caller that also holds the source's name hash can bind
    /// the two via [`crate::destination::destination_hash`].
    pub fn verify(&self, source_identity: &PublicIdentity) -> bool {
        source_identity.verify(&self.message.hash(), &self.signature)
    }

    /// Serialize the whole signed message deterministically for carriage in a
    /// Reticulum packet body, as `signature(64) || postcard(message)`.
    pub fn pack(&self) -> Vec<u8> {
        // As with payload packing, postcard serialization of these plain data
        // fields is infallible.
        let body = postcard::to_allocvec(&self.message)
            .expect("postcard packing of an LXMF message is infallible");
        let mut out = Vec::with_capacity(SIGNATURE_LENGTH + body.len());
        out.extend_from_slice(&self.signature);
        out.extend_from_slice(&body);
        out
    }

    /// Parse a signed message from bytes.
    ///
    /// This is **total**: any arbitrary or truncated buffer yields
    /// [`LxmfError::Malformed`] rather than panicking.
    pub fn unpack(bytes: &[u8]) -> Result<Self, LxmfError> {
        if bytes.len() < SIGNATURE_LENGTH {
            return Err(LxmfError::Malformed);
        }
        let mut signature = [0u8; SIGNATURE_LENGTH];
        signature.copy_from_slice(&bytes[..SIGNATURE_LENGTH]);
        let message =
            postcard::from_bytes(&bytes[SIGNATURE_LENGTH..]).map_err(|_| LxmfError::Malformed)?;
        Ok(Self { message, signature })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::Destination;
    use crate::packet::{context, DestinationType, Packet, PacketType};

    fn fields() -> BTreeMap<String, Vec<u8>> {
        let mut m = BTreeMap::new();
        m.insert("mime".to_string(), b"text/plain".to_vec());
        m.insert("ttl".to_string(), vec![0x00, 0x10]);
        m
    }

    fn sample() -> (Identity, [u8; DESTINATION_HASH_LENGTH], LxmfMessage) {
        let src_id = Identity::generate();
        let dest_id = Identity::generate();
        let dest = Destination::new(dest_id.public_identity(), "rabbithole", &["burrow", "lxmf"]);
        let source = Destination::new(src_id.public_identity(), "rabbithole", &["burrow", "lxmf"]);
        let msg = LxmfMessage::new(
            dest.hash(),
            source.hash(),
            1_735_000_000.5,
            b"Down the hole",
            b"We're all mad here.",
            fields(),
        );
        (src_id, dest.hash(), msg)
    }

    #[test]
    fn hash_is_32_bytes_and_deterministic() {
        let (_id, _dh, msg) = sample();
        let h = msg.hash();
        assert_eq!(h.len(), LXMF_HASH_LENGTH);
        assert_eq!(h, msg.hash());
    }

    #[test]
    fn hash_matches_manual_formula() {
        let (_id, _dh, msg) = sample();
        let mut hasher = Sha256::new();
        hasher.update(msg.destination_hash);
        hasher.update(msg.source_hash);
        hasher.update(msg.packed_payload());
        let expected = hasher.finalize();
        assert_eq!(&msg.hash()[..], &expected[..]);
    }

    #[test]
    fn hash_independent_of_field_insertion_order() {
        let (_id, dh, msg) = sample();
        // Same fields inserted in the opposite order must hash identically.
        let mut reordered = BTreeMap::new();
        reordered.insert("ttl".to_string(), vec![0x00, 0x10]);
        reordered.insert("mime".to_string(), b"text/plain".to_vec());
        let msg2 = LxmfMessage::new(
            dh,
            msg.source_hash,
            msg.timestamp,
            &msg.title,
            &msg.content,
            reordered,
        );
        assert_eq!(msg.hash(), msg2.hash());
    }

    #[test]
    fn hash_changes_with_each_field() {
        let (_id, _dh, base) = sample();
        let mut c = base.clone();
        c.content = b"different".to_vec();
        assert_ne!(base.hash(), c.hash());

        let mut t = base.clone();
        t.title = b"different".to_vec();
        assert_ne!(base.hash(), t.hash());

        let mut ts = base.clone();
        ts.timestamp += 1.0;
        assert_ne!(base.hash(), ts.hash());

        let mut d = base.clone();
        d.destination_hash[0] ^= 0xFF;
        assert_ne!(base.hash(), d.hash());

        let mut s = base.clone();
        s.source_hash[0] ^= 0xFF;
        assert_ne!(base.hash(), s.hash());

        let mut f = base.clone();
        f.fields.insert("extra".to_string(), vec![1]);
        assert_ne!(base.hash(), f.hash());
    }

    #[test]
    fn sign_verify_roundtrip() {
        let (id, _dh, msg) = sample();
        let signed = msg.sign(&id);
        assert!(signed.verify(&id.public_identity()));
        assert_eq!(signed.hash(), msg.hash());
    }

    #[test]
    fn wrong_identity_rejected() {
        let (id, _dh, msg) = sample();
        let signed = msg.sign(&id);
        let other = Identity::generate();
        assert!(!signed.verify(&other.public_identity()));
    }

    #[test]
    fn tampered_content_breaks_verification() {
        let (id, _dh, msg) = sample();
        let mut signed = msg.sign(&id);
        signed.message.content[0] ^= 0xFF;
        assert!(!signed.verify(&id.public_identity()));
    }

    #[test]
    fn tampered_title_breaks_verification() {
        let (id, _dh, msg) = sample();
        let mut signed = msg.sign(&id);
        signed.message.title[0] ^= 0xFF;
        assert!(!signed.verify(&id.public_identity()));
    }

    #[test]
    fn tampered_fields_break_verification() {
        let (id, _dh, msg) = sample();
        let mut signed = msg.sign(&id);
        signed.message.fields.insert("evil".to_string(), vec![0xAA]);
        assert!(!signed.verify(&id.public_identity()));
    }

    #[test]
    fn tampered_addressing_breaks_verification() {
        let (id, _dh, msg) = sample();
        let mut signed = msg.sign(&id);
        signed.message.destination_hash[0] ^= 0xFF;
        assert!(!signed.verify(&id.public_identity()));

        let mut signed2 = msg.sign(&id);
        signed2.message.source_hash[0] ^= 0xFF;
        assert!(!signed2.verify(&id.public_identity()));
    }

    #[test]
    fn tampered_signature_breaks_verification() {
        let (id, _dh, msg) = sample();
        let mut signed = msg.sign(&id);
        signed.signature[0] ^= 0xFF;
        assert!(!signed.verify(&id.public_identity()));
    }

    #[test]
    fn pack_is_deterministic() {
        let (id, _dh, msg) = sample();
        let signed = msg.sign(&id);
        assert_eq!(signed.pack(), signed.pack());
    }

    #[test]
    fn pack_unpack_roundtrip_preserves_and_verifies() {
        let (id, _dh, msg) = sample();
        let signed = msg.sign(&id);
        let bytes = signed.pack();
        let parsed = SignedLxmf::unpack(&bytes).unwrap();
        assert_eq!(parsed, signed);
        assert!(parsed.verify(&id.public_identity()));
    }

    #[test]
    fn empty_title_content_and_fields_roundtrip() {
        let src = Identity::generate();
        let dst = Identity::generate();
        let msg = LxmfMessage::new(
            dst.identity_hash(),
            src.identity_hash(),
            0.0,
            b"",
            b"",
            BTreeMap::new(),
        );
        let signed = msg.sign(&src);
        let parsed = SignedLxmf::unpack(&signed.pack()).unwrap();
        assert_eq!(parsed, signed);
        assert!(parsed.verify(&src.public_identity()));
    }

    #[test]
    fn rides_inside_a_reticulum_packet() {
        let (id, dh, msg) = sample();
        let signed = msg.sign(&id);
        let packet = Packet::new_header1(
            DestinationType::Single,
            PacketType::Data,
            dh,
            context::NONE,
            signed.pack(),
        );
        let decoded = Packet::decode(&packet.encode().unwrap()).unwrap();
        let parsed = SignedLxmf::unpack(&decoded.data).unwrap();
        assert_eq!(parsed, signed);
        assert!(parsed.verify(&id.public_identity()));
    }

    #[test]
    fn unpack_truncated_never_panics() {
        let (id, _dh, msg) = sample();
        let bytes = msg.sign(&id).pack();
        for len in 0..bytes.len() {
            // Must not panic; a prefix of a valid message is almost always
            // malformed (and never a *different* valid message we assert on).
            let _ = SignedLxmf::unpack(&bytes[..len]);
        }
    }

    #[test]
    fn unpack_arbitrary_bytes_never_panics() {
        let mut state: u64 = 0xA5A5_1234_DEAD_BEEF;
        for _ in 0..5000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 55) as usize % 256;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 40) as u8);
            }
            // Totality: never panics; Ok or Err are both acceptable.
            let _ = SignedLxmf::unpack(&buf);
        }
    }

    #[test]
    fn unpack_empty_is_malformed() {
        assert_eq!(SignedLxmf::unpack(&[]), Err(LxmfError::Malformed));
    }
}
