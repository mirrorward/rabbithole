//! Client-side E2EE helpers for opt-in 1:1 DM encryption (Wave 13).
//!
//! Wraps the [`rabbithole_e2ee`] crypto core (X3DH-lite + Double Ratchet) and
//! the [`rabbithole_proto`] wire types into the two things a frontend needs:
//!
//! 1. [`E2eeIdentity`] — the local key material (X25519 identity, an Ed25519
//!    prekey-signing key, and an X25519 signed prekey) plus
//!    [`E2eeIdentity::publish`], which builds a [`KeyBundlePublish`] advertising
//!    the public halves and a batch of one-time prekeys.
//! 2. [`E2eeSession`] — a live ratchet session. An initiator builds one from a
//!    fetched [`KeyBundle`] ([`E2eeSession::initiate`]); a responder builds one
//!    from the first message's prologue ([`E2eeSession::respond`]). Either side
//!    then [`E2eeSession::encrypt`]s plaintext into an [`EncryptedPayload`] and
//!    [`E2eeSession::decrypt`]s payloads back to plaintext.
//!
//! The plaintext DM API is untouched; these are additive, opt-in helpers. This
//! module performs no I/O and is generic over the caller's RNG, so it stays
//! wasm-compatible.
//!
//! ## X3DH-lite / one-time prekeys
//!
//! The crypto core implements the **3-DH** X3DH-lite handshake (identity +
//! signed prekey; see [`rabbithole_e2ee::x3dh`]); it does not fold a one-time
//! prekey into the shared secret. One-time prekeys are still published and the
//! server still atomically consumes one per fetch (exercising that path and
//! leaving room to upgrade to 4-DH later), but a bundle with `one_time_prekey =
//! None` establishes a session exactly the same way — so pool exhaustion never
//! blocks an encrypted conversation.

use rabbithole_e2ee::{
    initiator_shared_secret, responder_shared_secret, sig_verify, Header, IdentityKeyPair, Message,
    PreKeyPair, PublicKey, Session, SigningKeyPair,
};
use rabbithole_proto::dm::{EncryptedPayload, PrekeyPrologue};
use rabbithole_proto::keybundle::{KeyBundle, KeyBundlePublish};
use rand_core::{CryptoRng, RngCore};

pub use rabbithole_e2ee::Error as E2eeError;

/// AEAD associated data binding every RabbitHole E2EE DM to this protocol slice.
const DM_E2EE_AD: &[u8] = b"rabbithole-dm-e2ee v1";

/// A client's long-lived E2EE identity: the key material behind its published
/// prekey bundle.
///
/// Hold one per account. The X25519 `identity` anchors X3DH; the Ed25519
/// `signing` key authenticates the signed prekey; `signed_prekey` is the
/// (rotatable) X25519 prekey peers use to boot a session and that also serves as
/// the responder's initial Double Ratchet key.
#[derive(Clone)]
pub struct E2eeIdentity {
    identity: IdentityKeyPair,
    signing: SigningKeyPair,
    signed_prekey: PreKeyPair,
}

impl E2eeIdentity {
    /// Generate a fresh identity (identity key, signing key, signed prekey).
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        Self {
            identity: IdentityKeyPair::generate(rng),
            signing: SigningKeyPair::generate(rng),
            signed_prekey: PreKeyPair::generate(rng),
        }
    }

    /// Reconstruct from persisted secret scalars (identity, signing seed,
    /// signed prekey).
    pub fn from_secret_bytes(
        identity: [u8; 32],
        signing: [u8; 32],
        signed_prekey: [u8; 32],
    ) -> Self {
        Self {
            identity: IdentityKeyPair::from_secret_bytes(identity),
            signing: SigningKeyPair::from_secret_bytes(signing),
            signed_prekey: PreKeyPair::from_secret_bytes(signed_prekey),
        }
    }

    /// This identity's X25519 identity public key.
    pub fn identity_public(&self) -> [u8; 32] {
        *self.identity.public().as_bytes()
    }

    /// The Ed25519 signature over the signed prekey (what a fetcher verifies).
    pub fn signed_prekey_signature(&self) -> [u8; 64] {
        self.signing.sign(self.signed_prekey.public().as_bytes())
    }

    /// Generate `n` fresh one-time prekey pairs. The caller publishes the public
    /// halves (via [`E2eeIdentity::publish`]) and may keep the pairs; the 3-DH
    /// handshake never needs the secrets, so dropping them is safe.
    pub fn generate_one_time_prekeys<R: RngCore + CryptoRng>(
        rng: &mut R,
        n: usize,
    ) -> Vec<PreKeyPair> {
        (0..n).map(|_| PreKeyPair::generate(rng)).collect()
    }

    /// Build a [`KeyBundlePublish`] advertising this identity plus the public
    /// halves of `one_time_prekeys`.
    pub fn publish(&self, one_time_prekeys: &[PreKeyPair]) -> KeyBundlePublish {
        KeyBundlePublish::new(
            self.identity_public(),
            self.signing.verifying_key(),
            *self.signed_prekey.public().as_bytes(),
            self.signed_prekey_signature().to_vec(),
            one_time_prekeys
                .iter()
                .map(|p| *p.public().as_bytes())
                .collect(),
        )
    }
}

/// A live Double Ratchet session with one peer, ready to encrypt/decrypt DMs.
pub struct E2eeSession<R: RngCore + CryptoRng> {
    inner: Session<R>,
    /// The X3DH prologue to attach to the FIRST outgoing message. `Some` on a
    /// freshly-initiated session until the first [`E2eeSession::encrypt`];
    /// always `None` for a responder.
    pending_prologue: Option<PrekeyPrologue>,
}

impl<R: RngCore + CryptoRng> E2eeSession<R> {
    /// Initiator side: establish a session toward a peer from their fetched
    /// [`KeyBundle`]. Verifies the signed-prekey signature before proceeding.
    ///
    /// `rng` is moved into the session (used for DH-ratchet key generation).
    pub fn initiate(our: &E2eeIdentity, bundle: &KeyBundle, mut rng: R) -> Result<Self, E2eeError> {
        let sig: [u8; 64] = bundle
            .signed_prekey_sig
            .as_slice()
            .try_into()
            .map_err(|_| E2eeError::Decrypt)?;
        if !sig_verify(&bundle.signing_key, &bundle.signed_prekey, &sig) {
            return Err(E2eeError::Decrypt);
        }
        let their_identity = PublicKey(bundle.identity_key);
        let their_spk = PublicKey(bundle.signed_prekey);
        let ephemeral = PreKeyPair::generate(&mut rng);
        let shared =
            initiator_shared_secret(&our.identity, &ephemeral, &their_identity, &their_spk);
        let inner = Session::initiator(&shared, their_spk, rng);
        let prologue = PrekeyPrologue::new(
            our.identity_public(),
            *ephemeral.public().as_bytes(),
            bundle.one_time_prekey,
        );
        Ok(Self {
            inner,
            pending_prologue: Some(prologue),
        })
    }

    /// Responder side: establish a session from the [`PrekeyPrologue`] carried on
    /// an incoming first message.
    pub fn respond(our: &E2eeIdentity, prologue: &PrekeyPrologue, rng: R) -> Self {
        let their_identity = PublicKey(prologue.identity_key);
        let their_ephemeral = PublicKey(prologue.ephemeral_key);
        let shared = responder_shared_secret(
            &our.identity,
            &our.signed_prekey,
            &their_identity,
            &their_ephemeral,
        );
        let inner = Session::responder(&shared, our.signed_prekey.clone(), rng);
        Self {
            inner,
            pending_prologue: None,
        }
    }

    /// Encrypt `plaintext` into an [`EncryptedPayload`] ready for
    /// [`rabbithole_proto::dm::DmSend::new_encrypted`]. The first call on an
    /// initiator session attaches the X3DH prologue.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<EncryptedPayload, E2eeError> {
        let msg = self.inner.encrypt(plaintext, DM_E2EE_AD)?;
        let header = postcard::to_allocvec(&msg.header).expect("ratchet Header serializes");
        Ok(EncryptedPayload::new(
            self.pending_prologue.take(),
            header,
            msg.ciphertext,
        ))
    }

    /// Decrypt an [`EncryptedPayload`] back to plaintext, ratcheting the session
    /// forward. Rejects tampered/mis-keyed payloads with [`E2eeError::Decrypt`].
    pub fn decrypt(&mut self, payload: &EncryptedPayload) -> Result<Vec<u8>, E2eeError> {
        let header: Header =
            postcard::from_bytes(&payload.header).map_err(|_| E2eeError::Decrypt)?;
        let msg = Message {
            header,
            ciphertext: payload.ciphertext.clone(),
        };
        self.inner.decrypt(&msg, DM_E2EE_AD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    /// Simulate a fetch: build the KeyBundle a server would return from Bob's
    /// published bundle, consuming the first one-time prekey (or none).
    fn fetch(publish: &KeyBundlePublish, consume_otp: bool) -> KeyBundle {
        KeyBundle::new(
            publish.identity_key,
            publish.signing_key,
            publish.signed_prekey,
            publish.signed_prekey_sig.clone(),
            if consume_otp {
                publish.one_time_prekeys.first().copied()
            } else {
                None
            },
        )
    }

    #[test]
    fn end_to_end_roundtrip_and_ratchet() {
        let mut rng = StdRng::seed_from_u64(1);
        let alice = E2eeIdentity::generate(&mut rng);
        let bob = E2eeIdentity::generate(&mut rng);
        let bob_otps = E2eeIdentity::generate_one_time_prekeys(&mut rng, 3);
        let bob_publish = bob.publish(&bob_otps);

        let bundle = fetch(&bob_publish, true);
        let mut a_sess = E2eeSession::initiate(&alice, &bundle, StdRng::seed_from_u64(2)).unwrap();

        let p1 = a_sess.encrypt(b"hello bob").unwrap();
        assert!(p1.prekey.is_some(), "first message carries the prologue");

        let prologue = p1.prekey.clone().unwrap();
        let mut b_sess = E2eeSession::respond(&bob, &prologue, StdRng::seed_from_u64(3));
        assert_eq!(b_sess.decrypt(&p1).unwrap(), b"hello bob");

        // Second message from Alice: no prologue, decrypts fine.
        let p2 = a_sess.encrypt(b"second").unwrap();
        assert!(p2.prekey.is_none());
        assert_eq!(b_sess.decrypt(&p2).unwrap(), b"second");

        // Bob replies (ratchet turns over).
        let r1 = b_sess.encrypt(b"hi alice").unwrap();
        assert_eq!(a_sess.decrypt(&r1).unwrap(), b"hi alice");
    }

    #[test]
    fn works_without_one_time_prekey() {
        let mut rng = StdRng::seed_from_u64(10);
        let alice = E2eeIdentity::generate(&mut rng);
        let bob = E2eeIdentity::generate(&mut rng);
        // Bob published no OTPs (or the pool is exhausted): fetch returns None.
        let bob_publish = bob.publish(&[]);
        let bundle = fetch(&bob_publish, false);
        assert!(bundle.one_time_prekey.is_none());

        let mut a_sess = E2eeSession::initiate(&alice, &bundle, StdRng::seed_from_u64(11)).unwrap();
        let p1 = a_sess.encrypt(b"no-otp").unwrap();
        let mut b_sess =
            E2eeSession::respond(&bob, &p1.prekey.clone().unwrap(), StdRng::seed_from_u64(12));
        assert_eq!(b_sess.decrypt(&p1).unwrap(), b"no-otp");
    }

    #[test]
    fn tampered_signed_prekey_signature_is_rejected() {
        let mut rng = StdRng::seed_from_u64(20);
        let alice = E2eeIdentity::generate(&mut rng);
        let bob = E2eeIdentity::generate(&mut rng);
        let mut bundle = fetch(&bob.publish(&[]), false);
        bundle.signed_prekey[0] ^= 0xFF; // signature no longer matches
        assert!(matches!(
            E2eeSession::initiate(&alice, &bundle, StdRng::seed_from_u64(21)),
            Err(E2eeError::Decrypt)
        ));
    }
}
