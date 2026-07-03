//! Ed25519 detached signatures for prekey-bundle authenticity.
//!
//! X3DH ([`crate::x3dh`]) authenticates the *session* via the identity keys
//! mixed into the handshake, but a peer fetching a published prekey bundle must
//! also be able to check that the **signed prekey** in that bundle was vouched
//! for by the advertised identity — otherwise a malicious relay could swap in
//! its own prekey. The X25519 identity key ([`crate::keys::IdentityKeyPair`]) is
//! a Diffie–Hellman key and cannot itself produce a signature, so this module
//! provides a dedicated Ed25519 signing key that lives alongside the X25519
//! identity: its verifying key is published in the bundle and its signature over
//! the signed prekey is what a fetcher checks before starting a session.
//!
//! Like the rest of the crate this performs no I/O and is generic over the
//! caller-supplied RNG, so it runs unchanged in the browser (wasm) and lets
//! tests inject a seeded RNG.

use ed25519_dalek::{Signer, Verifier};
use rand_core::{CryptoRng, RngCore};

/// An Ed25519 signing key pair used to authenticate a peer's published prekeys.
///
/// The private half signs the signed prekey at publish time; the public
/// (verifying) half travels in the bundle so any fetcher can verify it.
#[derive(Clone)]
pub struct SigningKeyPair(ed25519_dalek::SigningKey);

impl SigningKeyPair {
    /// Generate a fresh signing key pair from a caller-supplied CSPRNG.
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        Self(ed25519_dalek::SigningKey::generate(rng))
    }

    /// Reconstruct from a 32-byte secret seed (e.g. loaded from storage).
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(&bytes))
    }

    /// The raw 32-byte secret seed (handle with care).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// The 32-byte Ed25519 verifying (public) key to publish in a bundle.
    pub fn verifying_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// Produce a detached 64-byte signature over `msg`.
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.0.sign(msg).to_bytes()
    }
}

/// Verify a detached Ed25519 `signature` over `msg` under `verifying_key`.
///
/// Returns `false` for a malformed verifying key or a signature that does not
/// check out — never panics on attacker-controlled bytes.
pub fn verify(verifying_key: &[u8; 32], msg: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(verifying_key) else {
        return false;
    };
    vk.verify(msg, &ed25519_dalek::Signature::from_bytes(signature))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn sign_and_verify_roundtrip() {
        let mut rng = StdRng::seed_from_u64(1);
        let kp = SigningKeyPair::generate(&mut rng);
        let sig = kp.sign(b"prekey-bytes");
        assert!(verify(&kp.verifying_key(), b"prekey-bytes", &sig));
    }

    #[test]
    fn wrong_message_fails() {
        let mut rng = StdRng::seed_from_u64(2);
        let kp = SigningKeyPair::generate(&mut rng);
        let sig = kp.sign(b"prekey-bytes");
        assert!(!verify(&kp.verifying_key(), b"other-bytes", &sig));
    }

    #[test]
    fn wrong_key_fails() {
        let mut rng = StdRng::seed_from_u64(3);
        let kp = SigningKeyPair::generate(&mut rng);
        let other = SigningKeyPair::generate(&mut rng);
        let sig = kp.sign(b"prekey-bytes");
        assert!(!verify(&other.verifying_key(), b"prekey-bytes", &sig));
    }

    #[test]
    fn secret_roundtrip_reproduces_verifying_key() {
        let mut rng = StdRng::seed_from_u64(4);
        let kp = SigningKeyPair::generate(&mut rng);
        let restored = SigningKeyPair::from_secret_bytes(kp.secret_bytes());
        assert_eq!(kp.verifying_key(), restored.verifying_key());
    }

    #[test]
    fn malformed_signature_bytes_do_not_panic() {
        let mut rng = StdRng::seed_from_u64(5);
        let kp = SigningKeyPair::generate(&mut rng);
        assert!(!verify(&kp.verifying_key(), b"m", &[0xFF; 64]));
    }
}
