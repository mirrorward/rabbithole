//! Reticulum [`Identity`] — the cryptographic root of a Reticulum peer.
//!
//! A Reticulum identity holds two keypairs (see
//! <https://reticulum.network/manual/understanding.html>):
//!
//! - an **X25519** keypair used for key exchange / encryption, and
//! - an **Ed25519** keypair used for signing.
//!
//! The *public identity* published on the mesh is the 64-byte concatenation of
//! the two public keys, in the order used by upstream `RNS`:
//! `x25519_public(32) || ed25519_public(32)`.
//!
//! The *identity hash* — the stable short handle for a peer — is
//! `SHA-256(public_identity)` truncated to the first 16 bytes
//! (`TRUNCATED_HASHLENGTH = 128 bits`).

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

/// Length in bytes of an Ed25519 or X25519 public key.
pub const KEY_LENGTH: usize = 32;
/// Length in bytes of the published public identity (X25519 || Ed25519 public).
pub const PUBLIC_IDENTITY_LENGTH: usize = KEY_LENGTH * 2;
/// Length in bytes of an identity hash (`TRUNCATED_HASHLENGTH`, 128 bits).
pub const IDENTITY_HASH_LENGTH: usize = 16;
/// Length in bytes of an Ed25519 signature.
pub const SIGNATURE_LENGTH: usize = 64;

/// A full Reticulum identity: both private keys plus their public halves.
///
/// Private key material is zeroized on drop: the Ed25519 [`SigningKey`] via
/// `ed25519-dalek`'s `zeroize` feature, and the X25519 [`StaticSecret`] via its
/// own `Drop` (`x25519-dalek`'s `zeroize` feature, enabled by default).
pub struct Identity {
    signing: SigningKey,
    encryption: StaticSecret,
}

impl Identity {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut rng = rand::rngs::OsRng;
        let signing = SigningKey::generate(&mut rng);
        let encryption = StaticSecret::random_from_rng(rng);
        Self {
            signing,
            encryption,
        }
    }

    /// Reconstruct an identity from its two 32-byte private seeds
    /// (`ed25519_seed`, `x25519_secret`). Both are copied and the inputs are
    /// left to the caller to zeroize.
    pub fn from_private_bytes(ed25519_seed: &[u8; 32], x25519_secret: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(ed25519_seed),
            encryption: StaticSecret::from(*x25519_secret),
        }
    }

    /// The Ed25519 signing seed (handle with care — this is secret material).
    pub fn ed25519_seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The X25519 secret scalar (handle with care — this is secret material).
    pub fn x25519_secret(&self) -> [u8; 32] {
        self.encryption.to_bytes()
    }

    /// The X25519 (encryption) public key.
    pub fn x25519_public(&self) -> [u8; KEY_LENGTH] {
        XPublicKey::from(&self.encryption).to_bytes()
    }

    /// The Ed25519 (signing) public key.
    pub fn ed25519_public(&self) -> [u8; KEY_LENGTH] {
        self.signing.verifying_key().to_bytes()
    }

    /// The 64-byte published public identity: `x25519_public || ed25519_public`.
    pub fn public_identity(&self) -> PublicIdentity {
        let mut bytes = [0u8; PUBLIC_IDENTITY_LENGTH];
        bytes[..KEY_LENGTH].copy_from_slice(&self.x25519_public());
        bytes[KEY_LENGTH..].copy_from_slice(&self.ed25519_public());
        PublicIdentity(bytes)
    }

    /// The 16-byte identity hash: `SHA-256(public_identity)[..16]`.
    pub fn identity_hash(&self) -> [u8; IDENTITY_HASH_LENGTH] {
        self.public_identity().identity_hash()
    }

    /// Sign `message` with the Ed25519 key, returning a detached 64-byte
    /// signature.
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LENGTH] {
        self.signing.sign(message).to_bytes()
    }

    /// The X25519 static secret, exposed to the [`crypto`](crate::crypto) module
    /// for Diffie-Hellman. Not part of the public API surface.
    pub(crate) fn dh(&self, their_public: &XPublicKey) -> [u8; 32] {
        self.encryption.diffie_hellman(their_public).to_bytes()
    }
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print private key material.
        f.debug_struct("Identity")
            .field("identity_hash", &hex_16(&self.identity_hash()))
            .finish_non_exhaustive()
    }
}

/// A published 64-byte public identity (`x25519_public || ed25519_public`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicIdentity(pub [u8; PUBLIC_IDENTITY_LENGTH]);

impl PublicIdentity {
    /// Parse a public identity from exactly 64 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; PUBLIC_IDENTITY_LENGTH] = bytes.try_into().ok()?;
        Some(Self(arr))
    }

    /// The X25519 (encryption) public key half.
    pub fn x25519_public(&self) -> [u8; KEY_LENGTH] {
        let mut k = [0u8; KEY_LENGTH];
        k.copy_from_slice(&self.0[..KEY_LENGTH]);
        k
    }

    /// The Ed25519 (signing) public key half.
    pub fn ed25519_public(&self) -> [u8; KEY_LENGTH] {
        let mut k = [0u8; KEY_LENGTH];
        k.copy_from_slice(&self.0[KEY_LENGTH..]);
        k
    }

    /// The 16-byte identity hash: `SHA-256(public_identity)[..16]`.
    pub fn identity_hash(&self) -> [u8; IDENTITY_HASH_LENGTH] {
        let digest = Sha256::digest(self.0);
        let mut out = [0u8; IDENTITY_HASH_LENGTH];
        out.copy_from_slice(&digest[..IDENTITY_HASH_LENGTH]);
        out
    }

    /// Verify a detached Ed25519 `signature` over `message` against this
    /// identity's signing key. Returns `false` on any malformed key/signature.
    pub fn verify(&self, message: &[u8], signature: &[u8; SIGNATURE_LENGTH]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.ed25519_public()) else {
            return false;
        };
        vk.verify(message, &ed25519_dalek::Signature::from_bytes(signature))
            .is_ok()
    }
}

impl core::fmt::Debug for PublicIdentity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PublicIdentity({})", hex_16(&self.identity_hash()))
    }
}

/// Small internal hex helper (avoids a dependency for `Debug`/`Display`
/// output). Shared with [`destination`](crate::destination) and
/// [`link`](crate::link) for their 16-byte hash newtypes.
pub(crate) fn hex_16(bytes: &[u8; IDENTITY_HASH_LENGTH]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Fill `buf` with cryptographically secure random bytes from the OS CSPRNG.
pub(crate) fn fill_random(buf: &mut [u8]) {
    rand::rngs::OsRng.fill_bytes(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_identity_is_64_bytes_xthen_ed() {
        let id = Identity::generate();
        let pi = id.public_identity();
        assert_eq!(pi.0.len(), PUBLIC_IDENTITY_LENGTH);
        assert_eq!(pi.x25519_public(), id.x25519_public());
        assert_eq!(pi.ed25519_public(), id.ed25519_public());
    }

    #[test]
    fn identity_hash_is_16_bytes_and_deterministic() {
        let id = Identity::generate();
        assert_eq!(id.identity_hash().len(), IDENTITY_HASH_LENGTH);
        assert_eq!(id.identity_hash(), id.identity_hash());
        // Matches the hash computed from the public identity.
        assert_eq!(id.identity_hash(), id.public_identity().identity_hash());
    }

    #[test]
    fn private_roundtrip_preserves_public_identity() {
        let id = Identity::generate();
        let restored = Identity::from_private_bytes(&id.ed25519_seed(), &id.x25519_secret());
        assert_eq!(id.public_identity(), restored.public_identity());
        assert_eq!(id.identity_hash(), restored.identity_hash());
    }

    #[test]
    fn sign_verify_roundtrip_and_tamper() {
        let id = Identity::generate();
        let sig = id.sign(b"through the looking glass");
        let pi = id.public_identity();
        assert!(pi.verify(b"through the looking glass", &sig));
        assert!(!pi.verify(b"through the looking-glass", &sig));
    }

    #[test]
    fn wrong_identity_rejects_signature() {
        let a = Identity::generate();
        let b = Identity::generate();
        let sig = a.sign(b"msg");
        assert!(!b.public_identity().verify(b"msg", &sig));
    }

    #[test]
    fn public_identity_from_bytes_rejects_wrong_length() {
        assert!(PublicIdentity::from_bytes(&[0u8; 63]).is_none());
        assert!(PublicIdentity::from_bytes(&[0u8; 65]).is_none());
        assert!(PublicIdentity::from_bytes(&[0u8; 64]).is_some());
    }

    #[test]
    fn debug_never_leaks_secret() {
        let id = Identity::generate();
        let dbg = format!("{id:?}");
        assert!(dbg.contains("identity_hash"));
        // The signing seed must not appear.
        assert!(!dbg.contains(&hex_16(&{
            let mut a = [0u8; 16];
            a.copy_from_slice(&id.ed25519_seed()[..16]);
            a
        })));
    }
}
