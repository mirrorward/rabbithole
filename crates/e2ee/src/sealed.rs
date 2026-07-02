//! Sealed-sender envelopes for metadata-light delivery.
//!
//! Inspired by Signal's [sealed sender]: the sender encrypts to the recipient's
//! public key using a **fresh ephemeral X25519 key**, so the envelope on the wire
//! reveals no sender identity — only an ephemeral public key that is unlinkable to
//! the sender's long-term keys. This is the classic ECIES / hybrid public-key
//! encryption construction (ephemeral DH -> KDF -> AEAD).
//!
//! This helper deliberately does **not** authenticate the sender; hiding the sender
//! is the goal. When sender authentication is needed, carry a signature (or run a
//! full [`crate::ratchet`] session) *inside* the sealed payload.
//!
//! [sealed sender]: https://signal.org/blog/sealed-sender/

use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::aead;
use crate::kdf::kdf_sealed;
use crate::keys::{IdentityKeyPair, KeyPair, PublicKey};
use crate::Result;

/// A sealed-sender envelope: an ephemeral public key plus AEAD ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedEnvelope {
    /// The sender's one-time ephemeral public key for this envelope.
    pub ephemeral_pub: PublicKey,
    /// ChaCha20-Poly1305 ciphertext (tag appended).
    pub ciphertext: Vec<u8>,
}

/// Bind the ephemeral public key into the AEAD associated data.
fn sealed_ad(ad: &[u8], ephemeral_pub: &PublicKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(ad.len() + 32);
    out.extend_from_slice(ad);
    out.extend_from_slice(ephemeral_pub.as_bytes());
    out
}

/// Encrypt `plaintext` to `recipient_pub`, hiding the sender.
pub fn sealed_seal<R: RngCore + CryptoRng>(
    recipient_pub: &PublicKey,
    plaintext: &[u8],
    ad: &[u8],
    rng: &mut R,
) -> SealedEnvelope {
    let ephemeral = KeyPair::generate(rng);
    let shared = ephemeral.dh(recipient_pub);
    let mk = kdf_sealed(&shared);
    let ephemeral_pub = ephemeral.public();
    let ciphertext = aead::seal(&mk, plaintext, &sealed_ad(ad, &ephemeral_pub));
    SealedEnvelope {
        ephemeral_pub,
        ciphertext,
    }
}

/// Decrypt a [`SealedEnvelope`] addressed to `recipient`.
pub fn sealed_open(
    recipient: &IdentityKeyPair,
    envelope: &SealedEnvelope,
    ad: &[u8],
) -> Result<Vec<u8>> {
    let shared = recipient.dh(&envelope.ephemeral_pub);
    let mk = kdf_sealed(&shared);
    aead::open(
        &mk,
        &envelope.ciphertext,
        &sealed_ad(ad, &envelope.ephemeral_pub),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn roundtrip() {
        let mut rng = StdRng::seed_from_u64(11);
        let bob = KeyPair::generate(&mut rng);
        let env = sealed_seal(&bob.public(), b"secret", b"ctx", &mut rng);
        assert_eq!(sealed_open(&bob, &env, b"ctx").unwrap(), b"secret");
    }

    #[test]
    fn wrong_recipient_fails() {
        let mut rng = StdRng::seed_from_u64(12);
        let bob = KeyPair::generate(&mut rng);
        let eve = KeyPair::generate(&mut rng);
        let env = sealed_seal(&bob.public(), b"secret", b"ctx", &mut rng);
        assert!(matches!(
            sealed_open(&eve, &env, b"ctx"),
            Err(Error::Decrypt)
        ));
    }

    #[test]
    fn tampered_ephemeral_fails() {
        let mut rng = StdRng::seed_from_u64(13);
        let bob = KeyPair::generate(&mut rng);
        let mut env = sealed_seal(&bob.public(), b"secret", b"ctx", &mut rng);
        env.ephemeral_pub.0[0] ^= 0xff;
        assert!(matches!(
            sealed_open(&bob, &env, b"ctx"),
            Err(Error::Decrypt)
        ));
    }
}
