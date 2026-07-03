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
//!
//! # Ingestion: [`AnnounceCache`]
//!
//! Announces are flooded, so any ingesting node needs de-duplication and
//! per-destination rate limiting. [`AnnounceCache`] is that helper as a pure
//! state machine: keyed by [`DestinationHash`], with a TTL and a minimum
//! re-announce interval, and a **caller-injected clock** (monotonic
//! milliseconds) — it never reads time itself. The future RNS gateway
//! sidecar/adapter feeds every verified announce through it and only acts on
//! [`AnnounceVerdict::Accept`].
//!
//! // SPEC-CHECK: upstream Transport de-duplicates by packet hash and applies
//! // per-interface `announce_rate_target`-style policies; this cache is a
//! // deliberately simplified per-destination policy (duplicate = same random
//! // hash within TTL, rate limit = new random hash before `min_interval`)
//! // for local ingestion, not a wire-behavior clone.

use std::collections::BTreeMap;

use crate::destination::{
    destination_hash, DestinationHash, DESTINATION_HASH_LENGTH, NAME_HASH_LENGTH,
};
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
    /// with a freshly generated random hash from the OS CSPRNG.
    pub fn create(
        identity: &Identity,
        destination_hash: [u8; DESTINATION_HASH_LENGTH],
        name_hash: [u8; NAME_HASH_LENGTH],
        app_data: &[u8],
    ) -> Self {
        let mut random_hash = [0u8; RANDOM_HASH_LENGTH];
        fill_random(&mut random_hash);
        Self::create_with_random_hash(identity, destination_hash, name_hash, random_hash, app_data)
    }

    /// Build and sign an announce with a **caller-supplied** random hash.
    ///
    /// This is the injected-randomness core of [`create`](Self::create):
    /// Ed25519 signing is deterministic, so with a fixed identity and random
    /// hash the whole announce is reproducible — which is what lets tests pin
    /// the serialized layout byte-for-byte.
    pub fn create_with_random_hash(
        identity: &Identity,
        destination_hash: [u8; DESTINATION_HASH_LENGTH],
        name_hash: [u8; NAME_HASH_LENGTH],
        random_hash: [u8; RANDOM_HASH_LENGTH],
        app_data: &[u8],
    ) -> Self {
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

/// The disposition of an observed announce (see [`AnnounceCache::observe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnounceVerdict {
    /// First sighting for this destination (or its previous entry expired):
    /// act on it and remember it.
    Accept,
    /// The exact same announce (same destination, same random hash) was
    /// already accepted within the TTL — a flood re-delivery; drop it.
    Duplicate,
    /// A *new* announce for a destination that re-announced faster than the
    /// configured minimum interval; drop it without refreshing the entry.
    RateLimited,
}

/// A pure de-duplication / rate-limiting table for announce ingestion.
///
/// Keyed by [`DestinationHash`]. All time is a caller-injected monotonic
/// clock in milliseconds — the cache performs no I/O and reads no clock. An
/// entry lives for `ttl_ms` after the moment it was accepted; a re-announce
/// with fresh randomness is only accepted `min_interval_ms` or more after the
/// previous acceptance. Expired entries are dropped lazily on
/// [`observe`](Self::observe)/[`contains`](Self::contains) and eagerly by
/// [`purge_expired`](Self::purge_expired).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnounceCache {
    ttl_ms: u64,
    min_interval_ms: u64,
    entries: BTreeMap<DestinationHash, CacheEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheEntry {
    random_hash: [u8; RANDOM_HASH_LENGTH],
    accepted_at_ms: u64,
}

impl CacheEntry {
    fn expired(&self, ttl_ms: u64, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.accepted_at_ms) >= ttl_ms
    }
}

impl AnnounceCache {
    /// Create a cache. `ttl_ms` bounds how long an accepted announce
    /// suppresses duplicates; `min_interval_ms` bounds how often a
    /// destination may be re-announced with fresh randomness (`0` disables
    /// rate limiting).
    pub fn new(ttl_ms: u64, min_interval_ms: u64) -> Self {
        Self {
            ttl_ms,
            min_interval_ms,
            entries: BTreeMap::new(),
        }
    }

    /// Observe an announce for `destination` carrying `random_hash` at
    /// `now_ms`, updating the table when the verdict is
    /// [`AnnounceVerdict::Accept`].
    ///
    /// Duplicates do **not** refresh the entry: the TTL measures the age of
    /// the accepted announce, not of its latest flood re-delivery.
    pub fn observe(
        &mut self,
        destination: DestinationHash,
        random_hash: [u8; RANDOM_HASH_LENGTH],
        now_ms: u64,
    ) -> AnnounceVerdict {
        match self.entries.get(&destination) {
            Some(entry) if !entry.expired(self.ttl_ms, now_ms) => {
                if entry.random_hash == random_hash {
                    return AnnounceVerdict::Duplicate;
                }
                if now_ms.saturating_sub(entry.accepted_at_ms) < self.min_interval_ms {
                    return AnnounceVerdict::RateLimited;
                }
            }
            _ => {}
        }
        self.entries.insert(
            destination,
            CacheEntry {
                random_hash,
                accepted_at_ms: now_ms,
            },
        );
        AnnounceVerdict::Accept
    }

    /// Convenience: observe a parsed [`Announce`] (keyed by its derived
    /// destination hash). Callers should [`verify`](Announce::verify) first.
    pub fn observe_announce(&mut self, announce: &Announce, now_ms: u64) -> AnnounceVerdict {
        self.observe(
            DestinationHash(announce.destination_hash()),
            announce.random_hash,
            now_ms,
        )
    }

    /// Whether a live (non-expired) entry exists for `destination` at `now_ms`.
    pub fn contains(&self, destination: &DestinationHash, now_ms: u64) -> bool {
        self.entries
            .get(destination)
            .is_some_and(|e| !e.expired(self.ttl_ms, now_ms))
    }

    /// Drop every entry whose TTL has elapsed at `now_ms`.
    pub fn purge_expired(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        self.entries.retain(|_, e| !e.expired(ttl, now_ms));
    }

    /// Number of entries currently stored (including not-yet-purged expired
    /// ones — see [`purge_expired`](Self::purge_expired)).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
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

    #[test]
    fn injected_random_hash_is_deterministic_and_pinned() {
        use sha2::Digest;
        // Fixed identity + fixed random hash ⇒ Ed25519 is deterministic, so
        // the entire serialized announce is reproducible. Pinning SHA-256 of
        // the bytes pins every field's position for a later interop pass.
        let id = Identity::from_private_bytes(&[0x11; 32], &[0x22; 32]);
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow", "control"]);
        let a = Announce::create_with_random_hash(
            &id,
            dest.hash(),
            dest.name_hash(),
            [0x77; RANDOM_HASH_LENGTH],
            b"burrow=warren",
        );
        let b = Announce::create_with_random_hash(
            &id,
            dest.hash(),
            dest.name_hash(),
            [0x77; RANDOM_HASH_LENGTH],
            b"burrow=warren",
        );
        assert_eq!(a, b);
        assert!(a.verify(dest.hash()));
        assert_eq!(
            hex::encode(sha2::Sha256::digest(a.to_bytes())),
            "bd6f648a39d9bbe345a97ef255c1bba75f741ffa79854d88a6f632de16fc267c"
        );
    }

    // --- AnnounceCache ---------------------------------------------------

    fn dh(seed: u8) -> DestinationHash {
        DestinationHash([seed; DESTINATION_HASH_LENGTH])
    }

    fn rh(seed: u8) -> [u8; RANDOM_HASH_LENGTH] {
        [seed; RANDOM_HASH_LENGTH]
    }

    #[test]
    fn cache_accepts_then_dedupes_within_ttl() {
        let mut cache = AnnounceCache::new(10_000, 0);
        assert_eq!(cache.observe(dh(1), rh(9), 1_000), AnnounceVerdict::Accept);
        assert_eq!(
            cache.observe(dh(1), rh(9), 1_001),
            AnnounceVerdict::Duplicate
        );
        assert_eq!(
            cache.observe(dh(1), rh(9), 10_999),
            AnnounceVerdict::Duplicate
        );
        // TTL elapsed (>= accepted_at + ttl): same bytes accepted afresh.
        assert_eq!(cache.observe(dh(1), rh(9), 11_000), AnnounceVerdict::Accept);
    }

    #[test]
    fn cache_duplicates_do_not_extend_ttl() {
        let mut cache = AnnounceCache::new(10_000, 0);
        assert_eq!(cache.observe(dh(1), rh(9), 0), AnnounceVerdict::Accept);
        // A re-flood just before expiry…
        assert_eq!(
            cache.observe(dh(1), rh(9), 9_999),
            AnnounceVerdict::Duplicate
        );
        // …does not push the expiry out.
        assert!(!cache.contains(&dh(1), 10_000));
        assert_eq!(cache.observe(dh(1), rh(9), 10_000), AnnounceVerdict::Accept);
    }

    #[test]
    fn cache_rate_limits_fresh_reannounces() {
        let mut cache = AnnounceCache::new(60_000, 5_000);
        assert_eq!(cache.observe(dh(1), rh(1), 0), AnnounceVerdict::Accept);
        // New randomness too soon → rate limited, entry unchanged.
        assert_eq!(
            cache.observe(dh(1), rh(2), 4_999),
            AnnounceVerdict::RateLimited
        );
        // The original announce still dedupes.
        assert_eq!(
            cache.observe(dh(1), rh(1), 4_999),
            AnnounceVerdict::Duplicate
        );
        // At the interval boundary the fresh announce is accepted…
        assert_eq!(cache.observe(dh(1), rh(2), 5_000), AnnounceVerdict::Accept);
        // …and replaces the stored randomness.
        assert_eq!(
            cache.observe(dh(1), rh(1), 5_001),
            AnnounceVerdict::RateLimited
        );
        assert_eq!(
            cache.observe(dh(1), rh(2), 5_001),
            AnnounceVerdict::Duplicate
        );
    }

    #[test]
    fn cache_zero_interval_disables_rate_limiting() {
        let mut cache = AnnounceCache::new(60_000, 0);
        assert_eq!(cache.observe(dh(1), rh(1), 0), AnnounceVerdict::Accept);
        assert_eq!(cache.observe(dh(1), rh(2), 0), AnnounceVerdict::Accept);
        assert_eq!(cache.observe(dh(1), rh(3), 1), AnnounceVerdict::Accept);
    }

    #[test]
    fn cache_tracks_destinations_independently() {
        let mut cache = AnnounceCache::new(60_000, 5_000);
        assert_eq!(cache.observe(dh(1), rh(1), 0), AnnounceVerdict::Accept);
        assert_eq!(cache.observe(dh(2), rh(1), 1), AnnounceVerdict::Accept);
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&dh(1), 100));
        assert!(cache.contains(&dh(2), 100));
        assert!(!cache.contains(&dh(3), 100));
    }

    #[test]
    fn cache_purge_expired_drops_only_stale_entries() {
        let mut cache = AnnounceCache::new(1_000, 0);
        cache.observe(dh(1), rh(1), 0);
        cache.observe(dh(2), rh(2), 600);
        cache.purge_expired(1_000); // dh(1) expired exactly now; dh(2) alive
        assert_eq!(cache.len(), 1);
        assert!(!cache.contains(&dh(1), 1_000));
        assert!(cache.contains(&dh(2), 1_000));
        cache.purge_expired(1_600);
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_observe_announce_uses_derived_destination() {
        let (_id, dest, announce) = make();
        let mut cache = AnnounceCache::new(60_000, 0);
        assert_eq!(
            cache.observe_announce(&announce, 5),
            AnnounceVerdict::Accept
        );
        assert_eq!(
            cache.observe_announce(&announce, 6),
            AnnounceVerdict::Duplicate
        );
        assert!(cache.contains(&DestinationHash(dest.hash()), 6));
    }

    #[test]
    fn cache_clock_regression_is_total() {
        // A clock that jumps backwards must not panic or underflow; the entry
        // simply remains live (saturating arithmetic).
        let mut cache = AnnounceCache::new(1_000, 500);
        assert_eq!(cache.observe(dh(1), rh(1), 10_000), AnnounceVerdict::Accept);
        assert_eq!(cache.observe(dh(1), rh(1), 0), AnnounceVerdict::Duplicate);
        assert_eq!(cache.observe(dh(1), rh(2), 0), AnnounceVerdict::RateLimited);
        assert!(cache.contains(&dh(1), 0));
    }
}
