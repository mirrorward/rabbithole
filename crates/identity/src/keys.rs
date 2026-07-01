//! Ed25519 identity keys.
//!
//! Every account may enroll one or more identity keys (passwordless login,
//! event signing); every server has exactly one active signing key (event
//! counter-signatures, theme bundles, tracker descriptors). Key material is
//! zeroized on drop.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};

/// A private Ed25519 identity key.
pub struct IdentityKey {
    signing: ed25519_dalek::SigningKey,
}

/// A public Ed25519 key (32 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

/// A detached Ed25519 signature (64 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(#[serde(with = "serde_bytes_64")] pub [u8; 64]);

impl IdentityKey {
    /// Generate a fresh key from the OS RNG.
    pub fn generate() -> Self {
        let mut rng = rand::rngs::OsRng;
        Self {
            signing: ed25519_dalek::SigningKey::generate(&mut rng),
        }
    }

    /// Reconstruct from the 32-byte secret seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            signing: ed25519_dalek::SigningKey::from_bytes(seed),
        }
    }

    /// The 32-byte secret seed (handle with care; zeroize copies).
    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key().to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.signing.sign(message).to_bytes())
    }
}

impl Drop for IdentityKey {
    fn drop(&mut self) {
        // SigningKey zeroizes internally via the `zeroize` feature.
    }
}

impl PublicKey {
    pub fn verify(&self, message: &[u8], signature: &Signature) -> bool {
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        key.verify(message, &ed25519_dalek::Signature::from_bytes(&signature.0))
            .is_ok()
    }

    /// Short human-readable fingerprint: first 16 hex chars of blake3(key).
    pub fn fingerprint(&self) -> String {
        hex::encode(&blake3::hash(&self.0).as_bytes()[..8])
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let key = IdentityKey::generate();
        let sig = key.sign(b"down the rabbit hole");
        assert!(key.public().verify(b"down the rabbit hole", &sig));
        assert!(!key.public().verify(b"tampered", &sig));
    }

    #[test]
    fn seed_roundtrip() {
        let key = IdentityKey::generate();
        let restored = IdentityKey::from_seed(&key.seed());
        assert_eq!(key.public(), restored.public());
    }

    #[test]
    fn wrong_key_rejects() {
        let a = IdentityKey::generate();
        let b = IdentityKey::generate();
        let sig = a.sign(b"msg");
        assert!(!b.public().verify(b"msg", &sig));
    }

    #[test]
    fn fingerprint_is_stable() {
        let key = IdentityKey::generate();
        assert_eq!(key.public().fingerprint(), key.public().fingerprint());
        assert_eq!(key.public().fingerprint().len(), 16);
    }
}
