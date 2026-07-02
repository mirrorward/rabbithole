//! Signing / verification and an X25519 + AEAD encryption token.
//!
//! Signing is plain Ed25519 (see [`Identity::sign`](crate::Identity::sign) and
//! [`PublicIdentity::verify`](crate::identity::PublicIdentity::verify)); this
//! module re-exports thin wrappers for symmetry.
//!
//! # Encryption — **documented divergence from upstream Reticulum**
//!
//! Upstream Reticulum encrypts SINGLE-destination payloads with a Fernet-like
//! token derived from an ephemeral X25519 exchange and HKDF-SHA256, using
//! **AES-128-CBC for confidentiality and HMAC-SHA256 for integrity**
//! (see `RNS.Cryptography`). To keep this interop-scaffolding slice's dependency
//! surface minimal, we instead perform a single **ChaCha20-Poly1305** AEAD pass
//! over a key derived by the *same* ephemeral-ECDH + HKDF-SHA256 construction.
//!
//! Token layout produced here:
//!
//! ```text
//! ephemeral_x25519_public(32) || nonce(12) || ciphertext_with_tag(N+16)
//! ```
//!
//! This is analogous to upstream (which likewise prepends the ephemeral public
//! key) but is **not byte-compatible**: the cipher, the AEAD tag vs. the
//! separate HMAC, and the nonce vs. CBC-IV all differ. The HKDF salt here is the
//! recipient's X25519 public key; upstream salts with the destination hash. The
//! transport slice must reconcile this before talking to real RNS peers.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as XPublicKey};

use crate::identity::{fill_random, Identity, PublicIdentity, SIGNATURE_LENGTH};

/// Length of the ephemeral X25519 public key prefixed to a token.
pub const EPHEMERAL_PUBLIC_LENGTH: usize = 32;
/// Length of the AEAD nonce.
pub const NONCE_LENGTH: usize = 12;
/// Length of the Poly1305 authentication tag.
pub const TAG_LENGTH: usize = 16;
/// Smallest possible token (empty plaintext): eph pub + nonce + tag.
pub const MIN_TOKEN_LENGTH: usize = EPHEMERAL_PUBLIC_LENGTH + NONCE_LENGTH + TAG_LENGTH;

/// HKDF `info` string binding derived keys to this construction and version.
const HKDF_INFO: &[u8] = b"rabbithole-reticulum-interop:x25519-chacha20poly1305:v1";

/// Errors from the encryption token path.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The AEAD layer refused to encrypt (e.g. absurd plaintext length).
    #[error("encryption failed")]
    Encrypt,
    /// The token was shorter than the fixed framing.
    #[error("token truncated: need at least {MIN_TOKEN_LENGTH} bytes, got {0}")]
    Truncated(usize),
    /// Authentication/decryption failed (tampered ciphertext or wrong key).
    #[error("decryption failed")]
    Decrypt,
}

/// Sign `message` with `identity`'s Ed25519 key.
pub fn sign(identity: &Identity, message: &[u8]) -> [u8; SIGNATURE_LENGTH] {
    identity.sign(message)
}

/// Verify a detached Ed25519 `signature` over `message` against `identity`.
pub fn verify(
    identity: &PublicIdentity,
    message: &[u8],
    signature: &[u8; SIGNATURE_LENGTH],
) -> bool {
    identity.verify(message, signature)
}

/// Derive the 32-byte AEAD key from a raw ECDH shared secret and the salt.
fn derive_key(shared: &[u8; 32], salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared);
    let mut okm = [0u8; 32];
    // `expand` only fails when the output length exceeds 255*HashLen; 32 is fine.
    hk.expand(HKDF_INFO, &mut okm)
        .expect("HKDF-SHA256 expand of 32 bytes is always valid");
    okm
}

/// Encrypt `plaintext` for `recipient`, producing a self-contained token.
pub fn encrypt(recipient: &PublicIdentity, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let rng = rand::rngs::OsRng;
    let ephemeral = EphemeralSecret::random_from_rng(rng);
    let ephemeral_public = XPublicKey::from(&ephemeral);

    let recipient_x = recipient.x25519_public();
    let their_public = XPublicKey::from(recipient_x);
    let shared = ephemeral.diffie_hellman(&their_public);

    let key = derive_key(&shared.to_bytes(), &recipient_x);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));

    let mut nonce_bytes = [0u8; NONCE_LENGTH];
    fill_random(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| CryptoError::Encrypt)?;

    let mut token = Vec::with_capacity(MIN_TOKEN_LENGTH + plaintext.len());
    token.extend_from_slice(ephemeral_public.as_bytes());
    token.extend_from_slice(&nonce_bytes);
    token.extend_from_slice(&ciphertext);
    Ok(token)
}

/// Decrypt a token addressed to `recipient` (whose secret keys we hold).
pub fn decrypt(recipient: &Identity, token: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if token.len() < MIN_TOKEN_LENGTH {
        return Err(CryptoError::Truncated(token.len()));
    }
    let mut eph = [0u8; EPHEMERAL_PUBLIC_LENGTH];
    eph.copy_from_slice(&token[..EPHEMERAL_PUBLIC_LENGTH]);
    let nonce_bytes = &token[EPHEMERAL_PUBLIC_LENGTH..EPHEMERAL_PUBLIC_LENGTH + NONCE_LENGTH];
    let ciphertext = &token[EPHEMERAL_PUBLIC_LENGTH + NONCE_LENGTH..];

    let ephemeral_public = XPublicKey::from(eph);
    let shared = recipient.dh(&ephemeral_public);

    // Salt is our own X25519 public key — the same value the sender used.
    let key = derive_key(&shared, &recipient.x25519_public());
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));

    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_wrappers() {
        let id = Identity::generate();
        let sig = sign(&id, b"mesh");
        assert!(verify(&id.public_identity(), b"mesh", &sig));
        assert!(!verify(&id.public_identity(), b"nesh", &sig));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let recipient = Identity::generate();
        let msg = b"reach the warren over the mesh";
        let token = encrypt(&recipient.public_identity(), msg).unwrap();
        let plain = decrypt(&recipient, &token).unwrap();
        assert_eq!(plain, msg);
    }

    #[test]
    fn empty_plaintext_roundtrip() {
        let recipient = Identity::generate();
        let token = encrypt(&recipient.public_identity(), b"").unwrap();
        assert_eq!(token.len(), MIN_TOKEN_LENGTH);
        assert_eq!(decrypt(&recipient, &token).unwrap(), b"");
    }

    #[test]
    fn wrong_recipient_cannot_decrypt() {
        let recipient = Identity::generate();
        let other = Identity::generate();
        let token = encrypt(&recipient.public_identity(), b"secret").unwrap();
        assert_eq!(decrypt(&other, &token), Err(CryptoError::Decrypt));
    }

    #[test]
    fn tampered_ciphertext_rejected() {
        let recipient = Identity::generate();
        let mut token = encrypt(&recipient.public_identity(), b"secret").unwrap();
        let last = token.len() - 1;
        token[last] ^= 0xFF;
        assert_eq!(decrypt(&recipient, &token), Err(CryptoError::Decrypt));
    }

    #[test]
    fn tampered_nonce_rejected() {
        let recipient = Identity::generate();
        let mut token = encrypt(&recipient.public_identity(), b"secret").unwrap();
        token[EPHEMERAL_PUBLIC_LENGTH] ^= 0xFF;
        assert_eq!(decrypt(&recipient, &token), Err(CryptoError::Decrypt));
    }

    #[test]
    fn ciphertext_differs_across_calls() {
        // Ephemeral key + random nonce ⇒ distinct tokens for the same plaintext.
        let recipient = Identity::generate();
        let a = encrypt(&recipient.public_identity(), b"same").unwrap();
        let b = encrypt(&recipient.public_identity(), b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn decrypt_truncated_never_panics() {
        let recipient = Identity::generate();
        let token = encrypt(&recipient.public_identity(), b"data").unwrap();
        for len in 0..token.len() {
            let res = decrypt(&recipient, &token[..len]);
            if len < MIN_TOKEN_LENGTH {
                assert!(matches!(res, Err(CryptoError::Truncated(_))));
            } else {
                // Valid framing length but truncated body ⇒ auth failure, no panic.
                assert!(res.is_err());
            }
        }
    }

    #[test]
    fn decrypt_arbitrary_never_panics() {
        let recipient = Identity::generate();
        let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
        for _ in 0..3000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 56) as usize % 128;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 40) as u8);
            }
            let _ = decrypt(&recipient, &buf);
        }
    }
}
