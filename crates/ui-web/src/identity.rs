//! Portable identity: a persisted Ed25519 keypair that names **you** across
//! every burrow. The secret seed lives in the browser's `localStorage`; only the
//! public key + a short fingerprint are ever shown or shared. This is the basis
//! for a "You" hub and (with a small additive proto delta) *verified-key* People
//! de-dup — so two humans who both pick the handle "rabbit" stay distinct by key.
//!
//! The crypto core ([`Identity`]) is deterministic + host-tested; generation and
//! persistence are wasm-only (browser CSPRNG + `localStorage`).

use ed25519_dalek::SigningKey;

/// A portable identity — the local Ed25519 keypair.
#[derive(Clone)]
pub struct Identity {
    seed: [u8; 32],
    public: [u8; 32],
}

impl Identity {
    /// Reconstruct from a 32-byte secret seed. Deterministic — no RNG — so the
    /// same seed always yields the same public key.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        let public = signing.verifying_key().to_bytes();
        Self { seed, public }
    }

    /// The public key (32 bytes) — the stable, shareable identifier.
    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// The secret seed. Handle with care (it *is* the private key).
    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    /// The public key as lowercase hex (64 chars).
    pub fn public_hex(&self) -> String {
        hex::encode(self.public)
    }

    /// A short human-readable fingerprint: the first 8 bytes of the public key's
    /// blake3 hash, as 16 hex chars. Short enough to read aloud, long enough that
    /// a collision is not a practical concern for de-dup.
    pub fn fingerprint(&self) -> String {
        hex::encode(&blake3::hash(&self.public).as_bytes()[..8])
    }

    /// The public face of this identity (no secret), for the UI.
    pub fn you(&self) -> You {
        You {
            fingerprint: self.fingerprint(),
            public_hex: self.public_hex(),
        }
    }
}

/// The shareable, secret-free view of the local identity — what the "You" hub
/// shows and what (later) rides on presence/profile for verified de-dup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct You {
    /// The short fingerprint (16 hex chars).
    pub fingerprint: String,
    /// The full public key (64 hex chars).
    pub public_hex: String,
}

/// The short fingerprint for a public key given as hex — the same 8-byte
/// blake3 digest [`Identity::fingerprint`] shows for the local key, so a remote
/// person's mark reads identically to your own. Falls back to a truncated copy
/// of the input if it isn't valid 32-byte hex.
pub fn short_fingerprint(pubkey_hex: &str) -> String {
    match hex::decode(pubkey_hex) {
        Ok(bytes) if bytes.len() == 32 => hex::encode(&blake3::hash(&bytes).as_bytes()[..8]),
        _ => pubkey_hex.chars().take(16).collect(),
    }
}

#[cfg(target_arch = "wasm32")]
mod persist {
    use super::Identity;

    /// `localStorage` key holding the hex-encoded secret seed.
    const SEED_KEY: &str = "rh.identity.seed";

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    /// Load the persisted identity, or mint + persist a fresh one on first run.
    pub fn load_or_create() -> Identity {
        if let Some(seed) = load_seed() {
            return Identity::from_seed(seed);
        }
        let seed = random_seed();
        save_seed(&seed);
        Identity::from_seed(seed)
    }

    fn load_seed() -> Option<[u8; 32]> {
        let hex = storage()?.get_item(SEED_KEY).ok()??;
        hex::decode(hex).ok()?.try_into().ok()
    }

    fn save_seed(seed: &[u8; 32]) {
        if let Some(s) = storage() {
            let _ = s.set_item(SEED_KEY, &hex::encode(seed));
        }
    }

    /// 32 bytes from the browser CSPRNG (`crypto.getRandomValues`).
    fn random_seed() -> [u8; 32] {
        let mut seed = [0u8; 32];
        if let Some(crypto) = web_sys::window().and_then(|w| w.crypto().ok()) {
            let _ = crypto.get_random_values_with_u8_array(&mut seed);
        }
        seed
    }
}

#[cfg(target_arch = "wasm32")]
pub use persist::load_or_create;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_deterministic_and_distinct() {
        let a = Identity::from_seed([7; 32]);
        let b = Identity::from_seed([7; 32]);
        // Same seed → same key + fingerprint.
        assert_eq!(a.public(), b.public());
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.public_hex().len(), 64);
        // A fingerprint is 8 bytes = 16 hex chars.
        assert_eq!(a.fingerprint().len(), 16);
        // A different seed → different identity.
        let c = Identity::from_seed([8; 32]);
        assert_ne!(a.public(), c.public());
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn short_fingerprint_matches_the_you_hub() {
        let id = Identity::from_seed([3; 32]);
        // Same 8-byte digest whether computed from the Identity or from its hex.
        assert_eq!(short_fingerprint(&id.public_hex()), id.fingerprint());
        // Invalid hex degrades gracefully to a truncated echo.
        assert_eq!(short_fingerprint("nothex"), "nothex");
    }

    #[test]
    fn public_key_matches_ed25519() {
        // The public key really is the Ed25519 point for this seed (guards against
        // a future refactor silently changing the derivation).
        let id = Identity::from_seed([1; 32]);
        let expected = ed25519_dalek::SigningKey::from_bytes(&[1; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(id.public(), expected);
    }
}
