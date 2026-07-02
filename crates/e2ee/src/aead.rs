//! Authenticated encryption with ChaCha20-Poly1305 ([RFC 8439]).
//!
//! Keyed by a one-time message key; the AEAD key and nonce are derived from it via
//! [`crate::kdf::aead_key_nonce`], so a deterministic (zero-transmission) nonce is
//! safe — each message key is used exactly once.
//!
//! [RFC 8439]: https://www.rfc-editor.org/rfc/rfc8439

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};

use crate::kdf::aead_key_nonce;
use crate::{Error, Result};

/// Encrypt `plaintext` under a one-time `message_key`, binding `associated_data`.
///
/// Returns the ciphertext with the Poly1305 tag appended.
pub(crate) fn seal(message_key: &[u8; 32], plaintext: &[u8], associated_data: &[u8]) -> Vec<u8> {
    let (key, nonce) = aead_key_nonce(message_key);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is always valid");
    cipher
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: plaintext,
                aad: associated_data,
            },
        )
        .expect("ChaCha20-Poly1305 encryption is infallible for in-memory buffers")
}

/// Decrypt and verify `ciphertext`, checking `associated_data`.
///
/// Returns [`Error::Decrypt`] on any authentication failure (wrong key, tampered
/// ciphertext, or mismatched associated data).
pub(crate) fn open(
    message_key: &[u8; 32],
    ciphertext: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>> {
    let (key, nonce) = aead_key_nonce(message_key);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is always valid");
    cipher
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: ciphertext,
                aad: associated_data,
            },
        )
        .map_err(|_| Error::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mk = [9u8; 32];
        let ct = seal(&mk, b"hello", b"ad");
        assert_eq!(open(&mk, &ct, b"ad").unwrap(), b"hello");
    }

    #[test]
    fn wrong_ad_fails() {
        let mk = [9u8; 32];
        let ct = seal(&mk, b"hello", b"ad");
        assert!(matches!(open(&mk, &ct, b"other"), Err(Error::Decrypt)));
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let mk = [9u8; 32];
        let mut ct = seal(&mk, b"hello", b"ad");
        ct[0] ^= 0xff;
        assert!(matches!(open(&mk, &ct, b"ad"), Err(Error::Decrypt)));
    }

    #[test]
    fn wrong_key_fails() {
        let ct = seal(&[1u8; 32], b"hello", b"ad");
        assert!(matches!(open(&[2u8; 32], &ct, b"ad"), Err(Error::Decrypt)));
    }
}
