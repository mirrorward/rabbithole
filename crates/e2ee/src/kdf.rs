//! Key-derivation functions, built on BLAKE3 in derive-key mode.
//!
//! Each function uses a distinct, hardcoded context string. In BLAKE3's derive-key
//! mode the context provides domain separation (the role HKDF's `info` plays), so
//! outputs of these functions can never collide even when fed identical key
//! material. See the crate-level docs for the rationale behind choosing BLAKE3 over
//! HKDF-SHA256.

/// Context strings are globally unique per KDF role (BLAKE3 best practice: hardcode,
/// application-specific, and rarely-changing). Versioned so the wire format can
/// evolve without silent key collisions.
const ROOT_CTX: &str = "RabbitHole-E2EE v1 double-ratchet root-kdf";
const CHAIN_STEP_CTX: &str = "RabbitHole-E2EE v1 double-ratchet chain-step";
const MESSAGE_KEY_CTX: &str = "RabbitHole-E2EE v1 double-ratchet message-key";
const AEAD_KEY_CTX: &str = "RabbitHole-E2EE v1 aead key";
const AEAD_NONCE_CTX: &str = "RabbitHole-E2EE v1 aead nonce";
const X3DH_CTX: &str = "RabbitHole-E2EE v1 x3dh-lite shared-secret";
const SEALED_CTX: &str = "RabbitHole-E2EE v1 sealed-sender message-key";

/// Root-key KDF (`KDF_RK` in the Double Ratchet spec).
///
/// Mixes the current root key with a fresh DH output, producing the next root key
/// and a new chain key.
pub(crate) fn kdf_rk(root_key: &[u8; 32], dh_out: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut hasher = blake3::Hasher::new_derive_key(ROOT_CTX);
    hasher.update(root_key);
    hasher.update(dh_out);
    let mut out = [0u8; 64];
    hasher.finalize_xof().fill(&mut out);
    let mut new_root = [0u8; 32];
    let mut chain = [0u8; 32];
    new_root.copy_from_slice(&out[..32]);
    chain.copy_from_slice(&out[32..]);
    (new_root, chain)
}

/// Symmetric chain-key KDF (`KDF_CK` in the Double Ratchet spec).
///
/// Deterministically ratchets a chain key forward, yielding the next chain key and
/// the message key for the current step. The two outputs use different contexts,
/// so knowing one reveals nothing about the other.
pub(crate) fn kdf_ck(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let next_chain = blake3::derive_key(CHAIN_STEP_CTX, chain_key);
    let message_key = blake3::derive_key(MESSAGE_KEY_CTX, chain_key);
    (next_chain, message_key)
}

/// Derive the ChaCha20-Poly1305 key + 96-bit nonce from a one-time message key.
///
/// Safe to use a deterministic nonce because every message key is unique.
pub(crate) fn aead_key_nonce(message_key: &[u8; 32]) -> ([u8; 32], [u8; 12]) {
    let key = blake3::derive_key(AEAD_KEY_CTX, message_key);
    let nonce_full = blake3::derive_key(AEAD_NONCE_CTX, message_key);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_full[..12]);
    (key, nonce)
}

/// Derive the X3DH-lite shared secret from the concatenated DH outputs.
pub(crate) fn kdf_x3dh(dh_concat: &[u8]) -> [u8; 32] {
    blake3::derive_key(X3DH_CTX, dh_concat)
}

/// Derive a sealed-sender message key from an ephemeral DH output.
pub(crate) fn kdf_sealed(dh_out: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(SEALED_CTX, dh_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdf_ck_outputs_differ_and_are_deterministic() {
        let ck = [7u8; 32];
        let (next1, mk1) = kdf_ck(&ck);
        let (next2, mk2) = kdf_ck(&ck);
        assert_eq!((next1, mk1), (next2, mk2));
        assert_ne!(next1, mk1);
        assert_ne!(next1, ck);
    }

    #[test]
    fn kdf_rk_domain_separates_root_and_chain() {
        let (rk, ck) = kdf_rk(&[1u8; 32], &[2u8; 32]);
        assert_ne!(rk, ck);
    }

    #[test]
    fn aead_key_and_nonce_are_distinct() {
        let (k, n) = aead_key_nonce(&[3u8; 32]);
        assert_ne!(&k[..12], &n[..]);
    }
}
