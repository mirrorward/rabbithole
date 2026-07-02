//! Group messaging with **Sender Keys** ([Signal Sender Keys design]).
//!
//! Pairwise Double Ratchet ([`crate::ratchet`]) is the right tool for a 1:1
//! conversation, but it does not scale to a room: with `N` members a sender would
//! have to encrypt every message `N-1` times, once per pairwise session. The
//! Sender Keys construction (Signal's design for private groups) trades that
//! `O(N)` fan-out for `O(1)` per message:
//!
//! - Each member owns a **sender chain** — a symmetric chain key that ratchets
//!   forward once per message ([`crate::kdf::kdf_sender_ck`], the group analogue
//!   of the 1:1 symmetric-key ratchet) — plus an **Ed25519 signing key**.
//! - A member bootstraps the room by sending each peer a
//!   [`SenderKeyDistributionMessage`] over an existing confidential 1:1 channel
//!   (e.g. [`crate::sealed`] or [`crate::ratchet`]). This module only produces and
//!   consumes that struct; the transport is the caller's concern.
//! - To send, a member advances its own chain, derives a one-time message key,
//!   AEAD-encrypts with the shared [`crate::aead`] primitive, and **signs the
//!   ciphertext with Ed25519** so recipients get per-sender authenticity (a shared
//!   symmetric key alone could not prove *which* member sent a message).
//! - To receive, a member looks up the sender's registered chain, ratchets it
//!   forward to the message's iteration (bounded, to resist DoS), **verifies the
//!   signature before AEAD**, and decrypts.
//!
//! # Forward secrecy and post-compromise security
//!
//! Ratcheting the chain key gives **forward secrecy**: the KDF is one-way, so
//! compromising a member's current chain key does not reveal keys for earlier
//! iterations, whose plaintext stays protected. A member handed the chain key at
//! iteration `n` can likewise only read messages from iteration `n` onward, never
//! prior ones — this is exactly what lets a late joiner read new traffic but not
//! history.
//!
//! Sender Keys do **not** provide post-compromise security on their own: there is
//! no DH ratchet mixing fresh entropy into the chain, so an attacker who learns a
//! chain key can follow that sender forward indefinitely. Recovery — and, more
//! importantly, **removing a member** — requires a *rekey*: every remaining member
//! generates a fresh sender key and redistributes a new
//! [`SenderKeyDistributionMessage`]. A removed member keeps the old chain keys but
//! learns nothing about the new ones. Use [`GroupSession::rekey`] to reset the
//! local sender chain (and signing key), then send fresh distribution messages to
//! the surviving members.
//!
//! # Determinism / wasm
//!
//! Like the rest of the crate this module performs no I/O and is generic over the
//! caller-supplied RNG (used only to generate sender keys), so it runs unchanged
//! in the browser and lets tests inject a seeded RNG.
//!
//! [Signal Sender Keys design]: https://signal.org/blog/private-groups/

use std::collections::HashMap;

use ed25519_dalek::{Signer, Verifier};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::aead;
use crate::kdf::kdf_sender_ck;
use crate::{Error, Result};

/// Opaque 32-byte group (room) identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GroupId(pub [u8; 32]);

/// Opaque 32-byte member identifier (e.g. a hash of the member's identity key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemberId(pub [u8; 32]);

/// An Ed25519 verifying (public) key for sender-key authenticity, 32 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SigningPublicKey(pub [u8; 32]);

/// A detached Ed25519 signature over a group ciphertext (64 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(#[serde(with = "serde_bytes_64")] pub [u8; 64]);

/// serde helper: fixed 64-byte arrays (serde's array impls stop at 32).
mod serde_bytes_64 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], ser: S) -> Result<S::Ok, S::Error> {
        bytes.as_slice().serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<[u8; 64], D::Error> {
        let v = <Vec<u8>>::deserialize(de)?;
        v.try_into()
            .map_err(|_| serde::de::Error::custom("expected 64 bytes"))
    }
}

/// A member's sender key, delivered 1:1 so peers can decrypt that member's future
/// messages.
///
/// It carries the chain key **at a specific iteration**; a recipient can derive
/// keys for that iteration onward but nothing earlier. Send it over a confidential
/// channel — anyone who reads a distribution message can read the sender's
/// subsequent group traffic. Reprocessing a newer distribution message for the
/// same member (e.g. after a [`GroupSession::rekey`]) replaces the old sender key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderKeyDistributionMessage {
    /// The room this sender key belongs to.
    pub group_id: GroupId,
    /// The member who owns this sender chain.
    pub member_id: MemberId,
    /// The chain iteration `chain_key` corresponds to.
    pub iteration: u32,
    /// The sender chain key at `iteration`.
    pub chain_key: [u8; 32],
    /// The member's Ed25519 verifying key, used to authenticate their messages.
    pub signing_public: SigningPublicKey,
}

/// An encrypted, signed group message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMessage {
    /// The member who produced this message.
    pub member_id: MemberId,
    /// This message's index within the sender's chain.
    pub iteration: u32,
    /// ChaCha20-Poly1305 ciphertext (tag appended).
    pub ciphertext: Vec<u8>,
    /// Ed25519 signature over the metadata + ciphertext (see module docs).
    pub signature: Signature,
}

/// Our own outbound sender state: chain key, iteration, and signing key.
struct OwnSenderState {
    chain_key: [u8; 32],
    iteration: u32,
    signing: ed25519_dalek::SigningKey,
}

impl OwnSenderState {
    fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        let mut chain_key = [0u8; 32];
        rng.fill_bytes(&mut chain_key);
        Self {
            chain_key,
            iteration: 0,
            signing: ed25519_dalek::SigningKey::generate(rng),
        }
    }
}

/// A peer's inbound sender state, reconstructed from their distribution message.
struct PeerSenderState {
    chain_key: [u8; 32],
    iteration: u32,
    signing_public: ed25519_dalek::VerifyingKey,
    /// Skipped-but-derived message keys, keyed by iteration (bounded, DoS-safe).
    skipped: HashMap<u32, [u8; 32]>,
}

/// A member's live view of one encrypted group.
///
/// Holds this member's own sender chain plus the registered sender keys of every
/// peer whose [`SenderKeyDistributionMessage`] has been processed via
/// [`GroupSession::add_member`].
pub struct GroupSession {
    group_id: GroupId,
    self_id: MemberId,
    own: OwnSenderState,
    peers: HashMap<MemberId, PeerSenderState>,
    max_skip: u32,
}

impl GroupSession {
    /// Default bound on message keys skipped/stored per sender (DoS protection),
    /// mirroring [`crate::ratchet::Session::DEFAULT_MAX_SKIP`].
    pub const DEFAULT_MAX_SKIP: u32 = 1000;

    /// Create a fresh session for `self_id` in `group_id`, generating a new local
    /// sender key (chain key + Ed25519 signing key) from `rng`.
    ///
    /// After construction, obtain [`GroupSession::distribution_message`] and send
    /// it to every other member so they can decrypt this member's messages.
    pub fn new<R: RngCore + CryptoRng>(group_id: GroupId, self_id: MemberId, rng: &mut R) -> Self {
        Self {
            group_id,
            self_id,
            own: OwnSenderState::generate(rng),
            peers: HashMap::new(),
            max_skip: Self::DEFAULT_MAX_SKIP,
        }
    }

    /// This room's id.
    pub fn group_id(&self) -> GroupId {
        self.group_id
    }

    /// This member's id.
    pub fn member_id(&self) -> MemberId {
        self.self_id
    }

    /// Override the per-sender skip bound (mainly for testing).
    pub fn set_max_skip(&mut self, max_skip: u32) {
        self.max_skip = max_skip;
    }

    /// Produce this member's current sender key for 1:1 distribution to a peer.
    ///
    /// Reflects the *current* chain key and iteration, so a recipient can only
    /// read messages from this point forward. Call it again after
    /// [`GroupSession::rekey`] to distribute the fresh sender key.
    pub fn distribution_message(&self) -> SenderKeyDistributionMessage {
        SenderKeyDistributionMessage {
            group_id: self.group_id,
            member_id: self.self_id,
            iteration: self.own.iteration,
            chain_key: self.own.chain_key,
            signing_public: SigningPublicKey(self.own.signing.verifying_key().to_bytes()),
        }
    }

    /// Register (or replace) a peer's sender key from their distribution message.
    ///
    /// Replacing is intentional: a peer that rekeys sends a new distribution
    /// message, and processing it discards the stale chain and skipped-key cache.
    ///
    /// Returns [`Error::WrongGroup`] if the message is for a different room, or
    /// [`Error::BadSignature`] if the advertised verifying key is malformed.
    pub fn add_member(&mut self, dm: SenderKeyDistributionMessage) -> Result<()> {
        if dm.group_id != self.group_id {
            return Err(Error::WrongGroup);
        }
        let signing_public = ed25519_dalek::VerifyingKey::from_bytes(&dm.signing_public.0)
            .map_err(|_| Error::BadSignature)?;
        self.peers.insert(
            dm.member_id,
            PeerSenderState {
                chain_key: dm.chain_key,
                iteration: dm.iteration,
                signing_public,
                skipped: HashMap::new(),
            },
        );
        Ok(())
    }

    /// Whether a sender key is registered for `member`.
    pub fn has_member(&self, member: &MemberId) -> bool {
        self.peers.contains_key(member)
    }

    /// Encrypt `plaintext` for the group, binding `ad` as associated data.
    ///
    /// Advances the local sender chain, AEAD-encrypts under the freshly derived
    /// one-time message key, and signs the metadata + ciphertext with this
    /// member's Ed25519 key. Infallible: there is no receiving state to skip.
    pub fn encrypt(&mut self, plaintext: &[u8], ad: &[u8]) -> GroupMessage {
        let iteration = self.own.iteration;
        let (next_ck, mk) = kdf_sender_ck(&self.own.chain_key);

        let aead_ad = associated_data(&self.group_id, &self.self_id, iteration, ad);
        let ciphertext = aead::seal(&mk, plaintext, &aead_ad);

        let sig_input = signing_input(&self.group_id, &self.self_id, iteration, ad, &ciphertext);
        let signature = Signature(self.own.signing.sign(&sig_input).to_bytes());

        self.own.chain_key = next_ck;
        self.own.iteration = self.own.iteration.saturating_add(1);

        GroupMessage {
            member_id: self.self_id,
            iteration,
            ciphertext,
            signature,
        }
    }

    /// Decrypt and authenticate `msg`, checking `ad`.
    ///
    /// The Ed25519 signature is verified **before** any AEAD work, so a forged or
    /// wrong-signer message is rejected without ratcheting the receiving chain.
    /// Out-of-order and skipped messages are handled with a bounded per-sender
    /// key cache; the chain is only ratcheted forward (never backward), so a
    /// replayed or too-old iteration whose key is no longer cached fails.
    /// Receiving state is mutated only after the AEAD tag verifies.
    ///
    /// Errors: [`Error::UnknownSender`] (no registered sender key),
    /// [`Error::BadSignature`], [`Error::TooManySkipped`], or [`Error::Decrypt`].
    pub fn decrypt(&mut self, msg: &GroupMessage, ad: &[u8]) -> Result<Vec<u8>> {
        let group_id = self.group_id;
        let max_skip = self.max_skip;
        let peer = self
            .peers
            .get_mut(&msg.member_id)
            .ok_or(Error::UnknownSender)?;

        // 1) Authenticity first: verify the Ed25519 signature before touching AEAD
        //    or the chain state.
        let sig_input = signing_input(
            &group_id,
            &msg.member_id,
            msg.iteration,
            ad,
            &msg.ciphertext,
        );
        peer.signing_public
            .verify(
                &sig_input,
                &ed25519_dalek::Signature::from_bytes(&msg.signature.0),
            )
            .map_err(|_| Error::BadSignature)?;

        let aead_ad = associated_data(&group_id, &msg.member_id, msg.iteration, ad);

        // 2) A previously skipped (out-of-order) message?
        if let Some(mk) = peer.skipped.get(&msg.iteration).copied() {
            let pt = aead::open(&mk, &msg.ciphertext, &aead_ad)?;
            peer.skipped.remove(&msg.iteration);
            return Ok(pt);
        }

        // 3) Anything strictly before the chain head that we did not cache is
        //    already consumed (or replayed); the KDF cannot run backwards.
        if msg.iteration < peer.iteration {
            return Err(Error::Decrypt);
        }

        // 4) Ratchet forward to the message's iteration, bounding the skip.
        let to_skip = msg.iteration - peer.iteration;
        if to_skip > max_skip || peer.skipped.len() + to_skip as usize > max_skip as usize {
            return Err(Error::TooManySkipped { max: max_skip });
        }

        // Stage the skipped keys and the target key; commit only after AEAD verifies.
        let mut ck = peer.chain_key;
        let mut staged: Vec<(u32, [u8; 32])> = Vec::with_capacity(to_skip as usize);
        let mut it = peer.iteration;
        while it < msg.iteration {
            let (next, mk) = kdf_sender_ck(&ck);
            staged.push((it, mk));
            ck = next;
            it += 1;
        }
        let (next_ck, mk) = kdf_sender_ck(&ck);
        let pt = aead::open(&mk, &msg.ciphertext, &aead_ad)?;

        // 5) Commit.
        for (i, k) in staged {
            peer.skipped.insert(i, k);
        }
        peer.chain_key = next_ck;
        peer.iteration = msg.iteration.saturating_add(1);
        Ok(pt)
    }

    /// Number of skipped message keys cached for `member` (tests/metrics).
    pub fn skipped_len(&self, member: &MemberId) -> usize {
        self.peers.get(member).map_or(0, |p| p.skipped.len())
    }

    /// Reset the local sender chain and signing key (a *rekey*).
    ///
    /// Required after a membership change (see the module-level forward-secrecy
    /// note): removing a member is only effective once every remaining member
    /// rekeys and redistributes a fresh [`SenderKeyDistributionMessage`]. The old
    /// chain keys the removed member holds are useless against the new chain.
    /// After calling this, send [`GroupSession::distribution_message`] to the
    /// surviving members.
    pub fn rekey<R: RngCore + CryptoRng>(&mut self, rng: &mut R) {
        self.own = OwnSenderState::generate(rng);
    }
}

/// AEAD associated data: bind the group, sender, and iteration to `ad`.
fn associated_data(group_id: &GroupId, member_id: &MemberId, iteration: u32, ad: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + 32 + 4 + ad.len());
    out.extend_from_slice(&group_id.0);
    out.extend_from_slice(&member_id.0);
    out.extend_from_slice(&iteration.to_le_bytes());
    out.extend_from_slice(ad);
    out
}

/// Ed25519 signing transcript: the associated data plus the ciphertext, so a
/// signature authenticates the exact metadata and bytes a recipient will verify.
fn signing_input(
    group_id: &GroupId,
    member_id: &MemberId,
    iteration: u32,
    ad: &[u8],
    ciphertext: &[u8],
) -> Vec<u8> {
    let mut out = associated_data(group_id, member_id, iteration, ad);
    out.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    out.extend_from_slice(ciphertext);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const GID: GroupId = GroupId([0xABu8; 32]);

    fn mid(n: u8) -> MemberId {
        MemberId([n; 32])
    }

    /// Build `count` sessions that all share each other's sender keys.
    fn make_group(count: u8, seed: u64) -> Vec<GroupSession> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut sessions: Vec<GroupSession> = (0..count)
            .map(|i| GroupSession::new(GID, mid(i + 1), &mut rng))
            .collect();
        // Everyone distributes to everyone.
        let dms: Vec<SenderKeyDistributionMessage> =
            sessions.iter().map(|s| s.distribution_message()).collect();
        for (i, s) in sessions.iter_mut().enumerate() {
            for (j, dm) in dms.iter().enumerate() {
                if i != j {
                    s.add_member(*dm).unwrap();
                }
            }
        }
        sessions
    }

    #[test]
    fn three_members_all_read_each_other() {
        let mut g = make_group(3, 1);
        // member 0 (id 1) sends; members 1 and 2 read.
        let m = g[0].encrypt(b"hello room", b"ad");
        assert_eq!(g[1].decrypt(&m, b"ad").unwrap(), b"hello room");
        assert_eq!(g[2].decrypt(&m, b"ad").unwrap(), b"hello room");

        // member 2 (id 3) replies; members 0 and 1 read.
        let r = g[2].encrypt(b"hi back", b"ad");
        assert_eq!(g[0].decrypt(&r, b"ad").unwrap(), b"hi back");
        assert_eq!(g[1].decrypt(&r, b"ad").unwrap(), b"hi back");
    }

    #[test]
    fn multiple_messages_ratchet_in_order() {
        let mut g = make_group(2, 2);
        for i in 0..5u8 {
            let pt = [i; 4];
            let m = g[0].encrypt(&pt, b"ad");
            assert_eq!(m.iteration, u32::from(i));
            assert_eq!(g[1].decrypt(&m, b"ad").unwrap(), pt);
        }
    }

    #[test]
    fn late_joiner_reads_subsequent_not_prior() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut alice = GroupSession::new(GID, mid(1), &mut rng);
        let mut carol = GroupSession::new(GID, mid(3), &mut rng);

        // Alice sends two messages before Carol joins.
        let early0 = alice.encrypt(b"secret-0", b"ad");
        let early1 = alice.encrypt(b"secret-1", b"ad");

        // Carol joins now: she receives Alice's *current* sender key.
        carol.add_member(alice.distribution_message()).unwrap();

        // Alice sends a message after Carol joined; Carol can read it.
        let later = alice.encrypt(b"secret-2", b"ad");
        assert_eq!(carol.decrypt(&later, b"ad").unwrap(), b"secret-2");

        // Carol cannot read the pre-join messages (iterations before her head).
        assert!(matches!(carol.decrypt(&early0, b"ad"), Err(Error::Decrypt)));
        assert!(matches!(carol.decrypt(&early1, b"ad"), Err(Error::Decrypt)));
    }

    #[test]
    fn out_of_order_and_skipped_message() {
        let mut g = make_group(2, 4);
        let m0 = g[0].encrypt(b"m0", b"ad");
        let m1 = g[0].encrypt(b"m1", b"ad");
        let m2 = g[0].encrypt(b"m2", b"ad");

        // Receiver sees m2 first (m0, m1 skipped and cached), then m0 out of order.
        assert_eq!(g[1].decrypt(&m2, b"ad").unwrap(), b"m2");
        assert_eq!(g[1].skipped_len(&mid(1)), 2);
        assert_eq!(g[1].decrypt(&m0, b"ad").unwrap(), b"m0");
        assert_eq!(g[1].skipped_len(&mid(1)), 1);
        assert_eq!(g[1].decrypt(&m1, b"ad").unwrap(), b"m1");
        assert_eq!(g[1].skipped_len(&mid(1)), 0);
    }

    #[test]
    fn replayed_message_fails() {
        let mut g = make_group(2, 5);
        let m0 = g[0].encrypt(b"once", b"ad");
        assert_eq!(g[1].decrypt(&m0, b"ad").unwrap(), b"once");
        // Same iteration again: consumed, not cached -> rejected.
        assert!(matches!(g[1].decrypt(&m0, b"ad"), Err(Error::Decrypt)));
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let mut g = make_group(2, 6);
        let mut m = g[0].encrypt(b"payload", b"ad");
        // Flip a ciphertext byte: it breaks the signature (which covers the
        // ciphertext), so it is rejected before AEAD even runs.
        m.ciphertext[0] ^= 0xff;
        assert!(matches!(g[1].decrypt(&m, b"ad"), Err(Error::BadSignature)));
    }

    #[test]
    fn wrong_ad_fails() {
        let mut g = make_group(2, 7);
        let m = g[0].encrypt(b"payload", b"ad");
        assert!(matches!(
            g[1].decrypt(&m, b"other"),
            Err(Error::BadSignature)
        ));
    }

    #[test]
    fn forged_signature_wrong_signer_fails() {
        // Two independent senders; splice sender B's signature onto A's message.
        let mut g = make_group(2, 8);
        let mut rng = StdRng::seed_from_u64(99);
        let mut imposter = GroupSession::new(GID, mid(1), &mut rng);

        let genuine = g[0].encrypt(b"legit", b"ad");
        // Imposter (different signing key, same member id) signs its own message.
        let forged_sig = imposter.encrypt(b"legit", b"ad").signature;
        let tampered = GroupMessage {
            signature: forged_sig,
            ..genuine
        };
        assert!(matches!(
            g[1].decrypt(&tampered, b"ad"),
            Err(Error::BadSignature)
        ));
    }

    #[test]
    fn unknown_sender_fails() {
        let mut rng = StdRng::seed_from_u64(10);
        let mut alice = GroupSession::new(GID, mid(1), &mut rng);
        let mut bob = GroupSession::new(GID, mid(2), &mut rng);
        // Bob never registered Alice's sender key.
        let m = alice.encrypt(b"hi", b"ad");
        assert!(matches!(bob.decrypt(&m, b"ad"), Err(Error::UnknownSender)));
    }

    #[test]
    fn wrong_group_distribution_rejected() {
        let mut rng = StdRng::seed_from_u64(11);
        let alice = GroupSession::new(GroupId([1u8; 32]), mid(1), &mut rng);
        let mut bob = GroupSession::new(GroupId([2u8; 32]), mid(2), &mut rng);
        assert!(matches!(
            bob.add_member(alice.distribution_message()),
            Err(Error::WrongGroup)
        ));
    }

    #[test]
    fn skip_bound_enforced() {
        let mut g = make_group(2, 12);
        g[1].set_max_skip(4);
        // Advance sender well past the bound, then deliver a far-future message.
        let mut last = g[0].encrypt(b"x", b"ad");
        for _ in 0..9 {
            last = g[0].encrypt(b"x", b"ad");
        }
        assert!(last.iteration > 4);
        assert!(matches!(
            g[1].decrypt(&last, b"ad"),
            Err(Error::TooManySkipped { max: 4 })
        ));
    }

    #[test]
    fn skip_at_exact_bound_succeeds() {
        let mut g = make_group(2, 13);
        g[1].set_max_skip(3);
        // Deliver iteration 3 directly: skips iterations 0,1,2 (== bound of 3).
        let mut m = g[0].encrypt(b"x", b"ad");
        for _ in 0..3 {
            m = g[0].encrypt(b"y", b"ad");
        }
        assert_eq!(m.iteration, 3);
        assert_eq!(g[1].decrypt(&m, b"ad").unwrap(), b"y");
        assert_eq!(g[1].skipped_len(&mid(1)), 3);
    }

    #[test]
    fn rekey_produces_fresh_chain() {
        let mut rng = StdRng::seed_from_u64(14);
        let mut alice = GroupSession::new(GID, mid(1), &mut rng);
        let mut bob = GroupSession::new(GID, mid(2), &mut rng);
        bob.add_member(alice.distribution_message()).unwrap();

        let before = alice.encrypt(b"pre-rekey", b"ad");
        assert_eq!(bob.decrypt(&before, b"ad").unwrap(), b"pre-rekey");

        let old_dm = alice.distribution_message();
        alice.rekey(&mut rng);
        let new_dm = alice.distribution_message();
        // Fresh chain: different key material and reset iteration.
        assert_ne!(old_dm.chain_key, new_dm.chain_key);
        assert_ne!(old_dm.signing_public, new_dm.signing_public);
        assert_eq!(new_dm.iteration, 0);

        // A message on the new chain fails until Bob re-adds the fresh sender key.
        let after = alice.encrypt(b"post-rekey", b"ad");
        assert!(matches!(
            bob.decrypt(&after, b"ad"),
            Err(Error::BadSignature)
        ));
        bob.add_member(new_dm).unwrap();
        assert_eq!(bob.decrypt(&after, b"ad").unwrap(), b"post-rekey");
    }

    #[test]
    fn distribution_message_serde_roundtrip() {
        let mut rng = StdRng::seed_from_u64(15);
        let s = GroupSession::new(GID, mid(1), &mut rng);
        let dm = s.distribution_message();
        let bytes = serde_json::to_vec(&dm).unwrap();
        let back: SenderKeyDistributionMessage = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dm, back);
    }

    #[test]
    fn group_message_serde_roundtrip() {
        let mut g = make_group(2, 16);
        let m = g[0].encrypt(b"wire", b"ad");
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: GroupMessage = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m, back);
        assert_eq!(g[1].decrypt(&back, b"ad").unwrap(), b"wire");
    }
}
