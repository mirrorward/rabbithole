//! Reticulum destination naming and hashing.
//!
//! A Reticulum *destination* is named by an `app_name` and zero or more
//! *aspects* (see <https://reticulum.network/manual/understanding.html>). Two
//! hashes derive from that name and an owning [`Identity`](crate::Identity):
//!
//! - the **name hash** — `SHA-256(full_name)` truncated to the first 10 bytes
//!   (`NAME_HASH_LENGTH`, 80 bits), and
//! - the **destination hash** — `SHA-256(name_hash || identity_hash)` truncated
//!   to the first 16 bytes (`TRUNCATED_HASHLENGTH`, 128 bits), where
//!   `identity_hash = SHA-256(public_identity)[..16]`.
//!
//! The full name is `app_name` followed by each aspect, joined with `.`
//! (e.g. `app_name = "rabbithole"`, `aspects = ["burrow", "control"]` →
//! `"rabbithole.burrow.control"`), matching `RNS.Destination.expand_name`.

use sha2::{Digest, Sha256};

use crate::identity::{PublicIdentity, IDENTITY_HASH_LENGTH};

/// Length in bytes of a Reticulum name hash (`NAME_HASH_LENGTH`, 80 bits).
pub const NAME_HASH_LENGTH: usize = 10;
/// Length in bytes of a destination hash (`TRUNCATED_HASHLENGTH`, 128 bits).
pub const DESTINATION_HASH_LENGTH: usize = 16;

/// Compute the full Reticulum name: `app_name` joined with each aspect by `.`.
pub fn full_name(app_name: &str, aspects: &[&str]) -> String {
    let mut name = String::from(app_name);
    for aspect in aspects {
        name.push('.');
        name.push_str(aspect);
    }
    name
}

/// Compute the 10-byte name hash of a full name string:
/// `SHA-256(full_name)[..NAME_HASH_LENGTH]`.
pub fn name_hash(app_name: &str, aspects: &[&str]) -> [u8; NAME_HASH_LENGTH] {
    let full = full_name(app_name, aspects);
    let digest = Sha256::digest(full.as_bytes());
    let mut out = [0u8; NAME_HASH_LENGTH];
    out.copy_from_slice(&digest[..NAME_HASH_LENGTH]);
    out
}

/// Compute the 16-byte destination hash from a name hash and an identity hash:
/// `SHA-256(name_hash || identity_hash)[..DESTINATION_HASH_LENGTH]`.
pub fn destination_hash(
    name_hash: &[u8; NAME_HASH_LENGTH],
    identity_hash: &[u8; IDENTITY_HASH_LENGTH],
) -> [u8; DESTINATION_HASH_LENGTH] {
    let mut hasher = Sha256::new();
    hasher.update(name_hash);
    hasher.update(identity_hash);
    let digest = hasher.finalize();
    let mut out = [0u8; DESTINATION_HASH_LENGTH];
    out.copy_from_slice(&digest[..DESTINATION_HASH_LENGTH]);
    out
}

/// A named Reticulum destination bound to a specific public identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    full_name: String,
    name_hash: [u8; NAME_HASH_LENGTH],
    identity: PublicIdentity,
    hash: [u8; DESTINATION_HASH_LENGTH],
}

impl Destination {
    /// Build a destination for `identity` named by `app_name` + `aspects`.
    pub fn new(identity: PublicIdentity, app_name: &str, aspects: &[&str]) -> Self {
        let full = full_name(app_name, aspects);
        let nh = name_hash(app_name, aspects);
        let hash = destination_hash(&nh, &identity.identity_hash());
        Self {
            full_name: full,
            name_hash: nh,
            identity,
            hash,
        }
    }

    /// The expanded full name (`app_name.aspect1.aspect2…`).
    pub fn full_name(&self) -> &str {
        &self.full_name
    }

    /// The 10-byte name hash.
    pub fn name_hash(&self) -> [u8; NAME_HASH_LENGTH] {
        self.name_hash
    }

    /// The owning public identity.
    pub fn identity(&self) -> &PublicIdentity {
        &self.identity
    }

    /// The 16-byte destination hash (the on-mesh address).
    pub fn hash(&self) -> [u8; DESTINATION_HASH_LENGTH] {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn full_name_joins_with_dots() {
        assert_eq!(full_name("rabbithole", &[]), "rabbithole");
        assert_eq!(
            full_name("rabbithole", &["burrow", "control"]),
            "rabbithole.burrow.control"
        );
    }

    #[test]
    fn name_hash_length_and_determinism() {
        let a = name_hash("rabbithole", &["burrow", "control"]);
        let b = name_hash("rabbithole", &["burrow", "control"]);
        assert_eq!(a.len(), NAME_HASH_LENGTH);
        assert_eq!(a, b);
        // Different aspects → different hash.
        assert_ne!(a, name_hash("rabbithole", &["burrow", "text"]));
    }

    #[test]
    fn name_hash_matches_sha256_truncation() {
        let full = full_name("rabbithole", &["burrow"]);
        let expected = &Sha256::digest(full.as_bytes())[..NAME_HASH_LENGTH];
        assert_eq!(&name_hash("rabbithole", &["burrow"]), expected);
    }

    #[test]
    fn destination_hash_length_and_determinism() {
        let id = Identity::generate();
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow", "control"]);
        assert_eq!(dest.hash().len(), DESTINATION_HASH_LENGTH);
        let again = Destination::new(id.public_identity(), "rabbithole", &["burrow", "control"]);
        assert_eq!(dest.hash(), again.hash());
    }

    #[test]
    fn destination_hash_matches_manual_formula() {
        let id = Identity::generate();
        let nh = name_hash("rabbithole", &["burrow"]);
        let expected = destination_hash(&nh, &id.identity_hash());
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow"]);
        assert_eq!(dest.hash(), expected);
    }

    #[test]
    fn different_identity_yields_different_destination() {
        let a = Identity::generate();
        let b = Identity::generate();
        let da = Destination::new(a.public_identity(), "rabbithole", &["burrow"]);
        let db = Destination::new(b.public_identity(), "rabbithole", &["burrow"]);
        assert_eq!(da.name_hash(), db.name_hash());
        assert_ne!(da.hash(), db.hash());
    }
}
