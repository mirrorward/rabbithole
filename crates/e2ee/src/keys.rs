//! X25519 key agreement keys ([RFC 7748]).
//!
//! Two roles of key exist:
//!
//! - [`IdentityKeyPair`] — a **long-term** key that anchors a peer's identity in
//!   the [`crate::x3dh`] handshake.
//! - [`PreKeyPair`] — a (semi-)ephemeral **prekey**. A peer publishes the public
//!   half so others can begin a session while the peer is offline; it also serves
//!   as the responder's initial Double Ratchet key.
//!
//! Both wrap an X25519 [`StaticSecret`] (rather than `EphemeralSecret`) because the
//! Double Ratchet must perform two Diffie–Hellman operations with the same private
//! key. Secret key material is zeroized on drop by `x25519-dalek`'s `zeroize`
//! feature.
//!
//! [RFC 7748]: https://www.rfc-editor.org/rfc/rfc7748

use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use x25519_dalek::StaticSecret;

/// A public X25519 key (32 bytes) — safe to publish and serialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<x25519_dalek::PublicKey> for PublicKey {
    fn from(pk: x25519_dalek::PublicKey) -> Self {
        PublicKey(pk.to_bytes())
    }
}

impl From<PublicKey> for x25519_dalek::PublicKey {
    fn from(pk: PublicKey) -> Self {
        x25519_dalek::PublicKey::from(pk.0)
    }
}

/// A generic X25519 key pair: a private [`StaticSecret`] plus its public key.
///
/// Not exposed directly; use the [`IdentityKeyPair`] / [`PreKeyPair`] aliases,
/// which document intent. All key generation flows through
/// [`KeyPair::generate`], which takes a caller-supplied RNG so tests can inject
/// a deterministic seed.
#[derive(Clone)]
pub struct KeyPair {
    secret: StaticSecret,
    public: PublicKey,
}

impl KeyPair {
    /// Generate a fresh key pair from a caller-supplied CSPRNG.
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        let secret = StaticSecret::random_from_rng(rng);
        let public = PublicKey::from(x25519_dalek::PublicKey::from(&secret));
        Self { secret, public }
    }

    /// Reconstruct from a 32-byte secret scalar (e.g. loaded from storage).
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(x25519_dalek::PublicKey::from(&secret));
        Self { secret, public }
    }

    /// The public half.
    pub fn public(&self) -> PublicKey {
        self.public
    }

    /// The raw 32-byte secret scalar (handle with care).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    /// Diffie–Hellman: combine our secret with a peer's public key.
    pub(crate) fn dh(&self, their_public: &PublicKey) -> [u8; 32] {
        let their: x25519_dalek::PublicKey = (*their_public).into();
        self.secret.diffie_hellman(&their).to_bytes()
    }
}

/// A long-term X25519 identity key pair.
pub type IdentityKeyPair = KeyPair;

/// A (semi-)ephemeral X25519 prekey pair.
pub type PreKeyPair = KeyPair;

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn dh_agrees_both_directions() {
        let mut rng = StdRng::seed_from_u64(1);
        let a = KeyPair::generate(&mut rng);
        let b = KeyPair::generate(&mut rng);
        assert_eq!(a.dh(&b.public()), b.dh(&a.public()));
    }

    #[test]
    fn secret_roundtrip_reproduces_public() {
        let mut rng = StdRng::seed_from_u64(2);
        let a = KeyPair::generate(&mut rng);
        let restored = KeyPair::from_secret_bytes(a.secret_bytes());
        assert_eq!(a.public(), restored.public());
    }

    #[test]
    fn public_key_wire_roundtrip() {
        let mut rng = StdRng::seed_from_u64(3);
        let a = KeyPair::generate(&mut rng);
        let bytes = *a.public().as_bytes();
        assert_eq!(PublicKey(bytes), a.public());
    }
}
