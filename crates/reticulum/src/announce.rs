//! Reticulum announce payloads.
//!
//! An *announce* advertises a destination and the identity that owns it so the
//! mesh can learn the path to it (see `RNS.Destination.announce`). The payload
//! carries the announcing identity's public key, the destination's name hash, a
//! random hash (freshness / replay resistance), optional application data, and
//! an Ed25519 signature.
//!
//! # Signed content (upstream-faithful)
//!
//! The signature covers, in order:
//! `destination_hash || public_identity || name_hash || random_hash || app_data`
//! — identical to upstream Reticulum.
//!
//! # Wire layout (**divergence**)
//!
//! This crate serializes the announce as
//! `public_identity(64) || name_hash(10) || random_hash(10) || app_data || signature(64)`
//! (signature **last**). Upstream Reticulum places the signature *before* the
//! trailing app-data (`… || random_hash || signature || app_data`). Because the
//! signed content is the same, signatures are semantically interchangeable; only
//! the byte order of the serialized announce differs. Ratchet keys (present in
//! newer upstream announces) are not modeled here.

use crate::destination::{destination_hash, DESTINATION_HASH_LENGTH, NAME_HASH_LENGTH};
use crate::identity::{
    fill_random, Identity, PublicIdentity, PUBLIC_IDENTITY_LENGTH, SIGNATURE_LENGTH,
};

/// Length of the random freshness hash in an announce.
pub const RANDOM_HASH_LENGTH: usize = 10;

/// Minimum serialized announce length (no app data).
const MIN_ANNOUNCE_LEN: usize =
    PUBLIC_IDENTITY_LENGTH + NAME_HASH_LENGTH + RANDOM_HASH_LENGTH + SIGNATURE_LENGTH;

/// A parsed / constructable announce payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    /// The announcing identity's 64-byte public identity.
    pub public_identity: PublicIdentity,
    /// The destination's 10-byte name hash.
    pub name_hash: [u8; NAME_HASH_LENGTH],
    /// A 10-byte random freshness hash.
    pub random_hash: [u8; RANDOM_HASH_LENGTH],
    /// Optional application data.
    pub app_data: Vec<u8>,
    /// Ed25519 signature over the signed content (see module docs).
    pub signature: [u8; SIGNATURE_LENGTH],
}

/// Errors produced while decoding an announce.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnnounceError {
    /// The input was too short to contain the fixed announce fields.
    #[error("announce truncated: need at least {MIN_ANNOUNCE_LEN} bytes, got {0}")]
    Truncated(usize),
}

impl Announce {
    /// Build and sign an announce for `destination_hash` owned by `identity`,
    /// with a freshly generated random hash.
    pub fn create(
        identity: &Identity,
        destination_hash: [u8; DESTINATION_HASH_LENGTH],
        name_hash: [u8; NAME_HASH_LENGTH],
        app_data: &[u8],
    ) -> Self {
        let mut random_hash = [0u8; RANDOM_HASH_LENGTH];
        fill_random(&mut random_hash);
        let public_identity = identity.public_identity();
        let signed = signed_content(
            &destination_hash,
            &public_identity,
            &name_hash,
            &random_hash,
            app_data,
        );
        let signature = identity.sign(&signed);
        Self {
            public_identity,
            name_hash,
            random_hash,
            app_data: app_data.to_vec(),
            signature,
        }
    }

    /// The destination hash implied by this announce
    /// (`SHA-256(name_hash || identity_hash)[..16]`).
    pub fn destination_hash(&self) -> [u8; DESTINATION_HASH_LENGTH] {
        destination_hash(&self.name_hash, &self.public_identity.identity_hash())
    }

    /// Verify the announce against `destination_hash`.
    ///
    /// Returns `true` only if (a) the supplied destination hash matches the one
    /// derived from the announced name hash and identity, and (b) the Ed25519
    /// signature verifies against the announced identity over the signed
    /// content.
    pub fn verify(&self, destination_hash: [u8; DESTINATION_HASH_LENGTH]) -> bool {
        if self.destination_hash() != destination_hash {
            return false;
        }
        let signed = signed_content(
            &destination_hash,
            &self.public_identity,
            &self.name_hash,
            &self.random_hash,
            &self.app_data,
        );
        self.public_identity.verify(&signed, &self.signature)
    }

    /// Serialize the announce (see the module-level wire-layout note).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MIN_ANNOUNCE_LEN + self.app_data.len());
        out.extend_from_slice(&self.public_identity.0);
        out.extend_from_slice(&self.name_hash);
        out.extend_from_slice(&self.random_hash);
        out.extend_from_slice(&self.app_data);
        out.extend_from_slice(&self.signature);
        out
    }

    /// Parse an announce from bytes, with bounds checks (never panics).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AnnounceError> {
        if bytes.len() < MIN_ANNOUNCE_LEN {
            return Err(AnnounceError::Truncated(bytes.len()));
        }
        let mut off = 0usize;

        let mut pi = [0u8; PUBLIC_IDENTITY_LENGTH];
        pi.copy_from_slice(&bytes[off..off + PUBLIC_IDENTITY_LENGTH]);
        off += PUBLIC_IDENTITY_LENGTH;

        let mut name_hash = [0u8; NAME_HASH_LENGTH];
        name_hash.copy_from_slice(&bytes[off..off + NAME_HASH_LENGTH]);
        off += NAME_HASH_LENGTH;

        let mut random_hash = [0u8; RANDOM_HASH_LENGTH];
        random_hash.copy_from_slice(&bytes[off..off + RANDOM_HASH_LENGTH]);
        off += RANDOM_HASH_LENGTH;

        // Signature occupies the final SIGNATURE_LENGTH bytes; app_data is the
        // slice in between (possibly empty). `len >= MIN_ANNOUNCE_LEN` above
        // guarantees `sig_start >= off`.
        let sig_start = bytes.len() - SIGNATURE_LENGTH;
        let app_data = bytes[off..sig_start].to_vec();

        let mut signature = [0u8; SIGNATURE_LENGTH];
        signature.copy_from_slice(&bytes[sig_start..]);

        Ok(Self {
            public_identity: PublicIdentity(pi),
            name_hash,
            random_hash,
            app_data,
            signature,
        })
    }
}

/// Assemble the signed content:
/// `destination_hash || public_identity || name_hash || random_hash || app_data`.
fn signed_content(
    destination_hash: &[u8; DESTINATION_HASH_LENGTH],
    public_identity: &PublicIdentity,
    name_hash: &[u8; NAME_HASH_LENGTH],
    random_hash: &[u8; RANDOM_HASH_LENGTH],
    app_data: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(
        DESTINATION_HASH_LENGTH
            + PUBLIC_IDENTITY_LENGTH
            + NAME_HASH_LENGTH
            + RANDOM_HASH_LENGTH
            + app_data.len(),
    );
    v.extend_from_slice(destination_hash);
    v.extend_from_slice(&public_identity.0);
    v.extend_from_slice(name_hash);
    v.extend_from_slice(random_hash);
    v.extend_from_slice(app_data);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::Destination;
    use crate::packet::{context, DestinationType, Packet, PacketType};

    fn make() -> (Identity, Destination, Announce) {
        let id = Identity::generate();
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow", "control"]);
        let announce = Announce::create(&id, dest.hash(), dest.name_hash(), b"burrow=warren");
        (id, dest, announce)
    }

    #[test]
    fn roundtrip_verifies() {
        let (_id, dest, announce) = make();
        assert!(announce.verify(dest.hash()));
        assert_eq!(announce.destination_hash(), dest.hash());
    }

    #[test]
    fn serialize_roundtrip_preserves_and_verifies() {
        let (_id, dest, announce) = make();
        let bytes = announce.to_bytes();
        let parsed = Announce::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, announce);
        assert!(parsed.verify(dest.hash()));
    }

    #[test]
    fn empty_app_data_roundtrip() {
        let id = Identity::generate();
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow"]);
        let announce = Announce::create(&id, dest.hash(), dest.name_hash(), b"");
        assert!(announce.app_data.is_empty());
        let parsed = Announce::from_bytes(&announce.to_bytes()).unwrap();
        assert_eq!(parsed, announce);
        assert!(parsed.verify(dest.hash()));
    }

    #[test]
    fn tampering_app_data_breaks_verification() {
        let (_id, dest, mut announce) = make();
        announce.app_data[0] ^= 0xFF;
        assert!(!announce.verify(dest.hash()));
    }

    #[test]
    fn tampering_signature_breaks_verification() {
        let (_id, dest, mut announce) = make();
        announce.signature[0] ^= 0xFF;
        assert!(!announce.verify(dest.hash()));
    }

    #[test]
    fn wrong_destination_hash_rejected() {
        let (_id, _dest, announce) = make();
        assert!(!announce.verify([0u8; DESTINATION_HASH_LENGTH]));
    }

    #[test]
    fn substituted_identity_breaks_verification() {
        let (_id, dest, mut announce) = make();
        // Swap in a different identity's public key; destination hash no longer
        // derives correctly and the signature no longer matches.
        let other = Identity::generate();
        announce.public_identity = other.public_identity();
        assert!(!announce.verify(dest.hash()));
    }

    #[test]
    fn from_bytes_rejects_truncated() {
        let (_id, _dest, announce) = make();
        let bytes = announce.to_bytes();
        for len in 0..MIN_ANNOUNCE_LEN {
            assert!(matches!(
                Announce::from_bytes(&bytes[..len]),
                Err(AnnounceError::Truncated(_))
            ));
        }
        // Exactly the minimum (empty app data) parses.
        assert!(Announce::from_bytes(&bytes[..bytes.len()]).is_ok());
    }

    #[test]
    fn from_bytes_arbitrary_never_panics() {
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        for _ in 0..3000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 55) as usize % 200;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 40) as u8);
            }
            let _ = Announce::from_bytes(&buf);
        }
    }

    #[test]
    fn packs_into_announce_packet() {
        // Sanity: an announce naturally rides in an ANNOUNCE / SINGLE packet.
        let (_id, dest, announce) = make();
        let packet = Packet::new_header1(
            DestinationType::Single,
            PacketType::Announce,
            dest.hash(),
            context::NONE,
            announce.to_bytes(),
        );
        let decoded = Packet::decode(&packet.encode().unwrap()).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Announce);
        let parsed = Announce::from_bytes(&decoded.data).unwrap();
        assert!(parsed.verify(dest.hash()));
    }
}
