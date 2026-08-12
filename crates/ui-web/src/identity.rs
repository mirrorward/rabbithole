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

    /// Sign `message` with the identity key, returning the 64-byte Ed25519
    /// signature — used to prove possession of the key against a server challenge.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        SigningKey::from_bytes(&self.seed).sign(message).to_bytes()
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

/// Recovery-document format version.
pub const BACKUP_VERSION: u32 = 1;

/// Serialise this identity as a recovery document: everything needed to become
/// you again on another machine, and nothing else.
///
/// The seed **is** the private key, so this is a credential, not a settings
/// export. Plain JSON on purpose — a recovery file you can't read is one you
/// can't check — and it carries its own warning so the danger travels with the
/// bytes rather than living only in the UI that produced them.
pub fn backup_json(id: &Identity) -> String {
    format!(
        "{{\n  \"rabbithole_identity\": {BACKUP_VERSION},\n  \"warning\": \"{}\",\n  \"fingerprint\": \"{}\",\n  \"public_key\": \"{}\",\n  \"secret_seed\": \"{}\"\n}}\n",
        "This file contains your private key. Anyone who has it can be you on every burrow. Keep it somewhere only you can read.",
        id.fingerprint(),
        id.public_hex(),
        hex::encode(id.seed()),
    )
}

/// Parse a recovery document back into an identity.
///
/// Lenient about *shape* (any JSON carrying a 64-hex `secret_seed` restores,
/// so a hand-edited or re-serialised file still works) and strict about
/// *substance*: the seed must be 32 bytes, and when the document also carries
/// a public key it must be the one that seed derives — a mismatch means the
/// file was altered, and restoring it would hand you a different identity than
/// the one you backed up.
pub fn restore_from_backup(text: &str) -> Result<Identity, String> {
    let seed_hex = json_string_field(text, "secret_seed").ok_or_else(|| {
        "No secret_seed in that file \u{2014} is it a RabbitHole identity backup?".to_string()
    })?;
    let bytes =
        hex::decode(seed_hex.trim()).map_err(|_| "The secret_seed isn't valid hex.".to_string())?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("A seed is 32 bytes; that one is {}.", bytes.len()))?;
    let id = Identity::from_seed(seed);
    if let Some(claimed) = json_string_field(text, "public_key") {
        if !claimed.trim().eq_ignore_ascii_case(&id.public_hex()) {
            return Err(
                "That file's public key doesn't match its seed \u{2014} it has been altered."
                    .to_string(),
            );
        }
    }
    Ok(id)
}

/// The value of a `"name": "value"` pair — enough for a four-field document,
/// without pulling a JSON parser into the wasm bundle for it.
fn json_string_field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("\"{name}\"");
    let after = &text[text.find(&key)? + key.len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let rest = after.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
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
    /// Replace the stored identity — the restore half of a backup.
    ///
    /// Everything keyed to the old identity (friendships you signed, the marks
    /// people see beside your name) belongs to that key, not this app, so a
    /// restore is a *change of person*, not a settings tweak. The caller warns
    /// before this runs.
    pub fn adopt(id: &Identity) {
        save_seed(&id.seed());
    }

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
pub use persist::{adopt, load_or_create};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_round_trips_into_the_same_identity() {
        let id = Identity::from_seed([3u8; 32]);
        let doc = backup_json(&id);
        let back = restore_from_backup(&doc).expect("round-trips");
        assert_eq!(back.public_hex(), id.public_hex());
        assert_eq!(back.seed(), id.seed());
        // The danger travels with the bytes, not just the UI that made them.
        assert!(doc.contains("private key"), "the file warns about itself");
        assert!(doc.contains(&id.fingerprint()), "readable enough to check");
    }

    #[test]
    fn a_tampered_backup_is_refused_rather_than_silently_restoring() {
        let id = Identity::from_seed([4u8; 32]);
        let other = Identity::from_seed([5u8; 32]);
        // A file whose public key doesn't match its seed would hand you a
        // DIFFERENT identity than the one you thought you backed up.
        let doc = backup_json(&id).replace(&id.public_hex(), &other.public_hex());
        let err = match restore_from_backup(&doc) {
            Err(e) => e,
            Ok(_) => panic!("a tampered backup must not restore"),
        };
        assert!(err.contains("altered"), "{err}");
    }

    #[test]
    fn junk_is_rejected_with_a_reason_a_person_can_act_on() {
        for (input, want) in [
            ("", "No secret_seed"),
            ("{}", "No secret_seed"),
            (r#"{"secret_seed":"nothex!!"}"#, "valid hex"),
            (r#"{"secret_seed":"aabb"}"#, "32 bytes"),
        ] {
            let err = match restore_from_backup(input) {
                Err(e) => e,
                Ok(_) => panic!("{input:?} must not restore"),
            };
            assert!(err.contains(want), "{input:?} => {err}");
        }
    }

    #[test]
    fn a_backup_without_a_public_key_still_restores() {
        // Hand-edited or re-serialised files are common; only the seed is
        // load-bearing, so shape is lenient where substance is strict.
        let id = Identity::from_seed([6u8; 32]);
        let doc = format!(r#"{{"secret_seed": "{}"}}"#, hex::encode(id.seed()));
        assert_eq!(
            restore_from_backup(&doc).unwrap().public_hex(),
            id.public_hex()
        );
    }

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
    fn sign_produces_a_signature_the_public_key_verifies() {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let id = Identity::from_seed([11; 32]);
        let msg = b"challenge-nonce";
        let sig = id.sign(msg);
        let vk = VerifyingKey::from_bytes(&id.public()).unwrap();
        assert!(
            vk.verify(msg, &Signature::from_bytes(&sig)).is_ok(),
            "own key verifies"
        );
        // A different message does not verify against this signature.
        assert!(vk.verify(b"other", &Signature::from_bytes(&sig)).is_err());
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
