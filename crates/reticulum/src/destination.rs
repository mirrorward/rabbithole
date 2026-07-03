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
//!
//! [`DestinationHash`] wraps the 16-byte destination hash as an ordered,
//! hashable value type with lowercase-hex [`Display`](core::fmt::Display) and
//! a total [`FromStr`](core::str::FromStr) — the currency higher layers
//! (announce caches, link tables, future `rabbit://` links carrying RNS
//! destination hashes) trade in.

use sha2::{Digest, Sha256};

use crate::identity::{hex_16, PublicIdentity, IDENTITY_HASH_LENGTH};

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

/// A 16-byte Reticulum destination hash — the on-mesh address of a
/// destination — as a first-class value type.
///
/// This is the same 16 bytes produced by [`destination_hash`] /
/// [`Destination::hash`]; the newtype exists so higher layers (announce
/// caches, link tables, `rabbit://` links gaining RNS destination hashes in a
/// later swarm-crate slice) can key on it, print it, and parse it back:
///
/// - [`Display`](core::fmt::Display) renders the conventional lowercase-hex
///   form used by RNS tooling (32 hex characters, no separators).
/// - [`FromStr`](core::str::FromStr) is **total**: any string that is not
///   exactly 32 hex characters (either case) yields a
///   [`DestinationHashError`], never a panic.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DestinationHash(pub [u8; DESTINATION_HASH_LENGTH]);

/// Errors produced while parsing a [`DestinationHash`] from hex.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DestinationHashError {
    /// The input was not exactly `2 * DESTINATION_HASH_LENGTH` characters.
    #[error(
        "destination hash hex must be {expected} characters, got {got}",
        expected = DESTINATION_HASH_LENGTH * 2,
        got = .0
    )]
    BadLength(usize),
    /// The input contained a non-hex character at the given byte offset.
    #[error("destination hash hex has a non-hex character at offset {0}")]
    BadChar(usize),
}

impl DestinationHash {
    /// Wrap a hash from exactly 16 bytes; `None` on any other length.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; DESTINATION_HASH_LENGTH] = bytes.try_into().ok()?;
        Some(Self(arr))
    }

    /// The raw 16 hash bytes.
    pub fn as_bytes(&self) -> &[u8; DESTINATION_HASH_LENGTH] {
        &self.0
    }
}

impl From<[u8; DESTINATION_HASH_LENGTH]> for DestinationHash {
    fn from(bytes: [u8; DESTINATION_HASH_LENGTH]) -> Self {
        Self(bytes)
    }
}

impl From<DestinationHash> for [u8; DESTINATION_HASH_LENGTH] {
    fn from(hash: DestinationHash) -> Self {
        hash.0
    }
}

impl From<&Destination> for DestinationHash {
    fn from(destination: &Destination) -> Self {
        Self(destination.hash())
    }
}

impl core::fmt::Display for DestinationHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&hex_16(&self.0))
    }
}

impl core::fmt::Debug for DestinationHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DestinationHash({self})")
    }
}

impl core::str::FromStr for DestinationHash {
    type Err = DestinationHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != DESTINATION_HASH_LENGTH * 2 {
            return Err(DestinationHashError::BadLength(bytes.len()));
        }
        let mut out = [0u8; DESTINATION_HASH_LENGTH];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = hex_val(pair[0]).ok_or(DestinationHashError::BadChar(i * 2))?;
            let lo = hex_val(pair[1]).ok_or(DestinationHashError::BadChar(i * 2 + 1))?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

/// Decode one hex digit (either case); `None` for anything else.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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

    /// A fixed identity for the pinned derivation vectors below.
    fn pinned_identity() -> Identity {
        Identity::from_private_bytes(&[0x11; 32], &[0x22; 32])
    }

    #[test]
    fn pinned_name_hash_vector() {
        // Pins the byte layout (SHA-256 of the UTF-8 full name, first 10
        // bytes) so a later interop pass can adjust derivation in one place.
        assert_eq!(
            hex::encode(name_hash("rabbithole", &["burrow", "control"])),
            "e06f6d4a397697d3aa71"
        );
    }

    #[test]
    fn pinned_identity_and_destination_hash_vectors() {
        let id = pinned_identity();
        assert_eq!(
            hex::encode(id.identity_hash()),
            "4cd0cc45a7405dbd5cf9b5be1ef92f10"
        );
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow", "control"]);
        assert_eq!(hex::encode(dest.hash()), "d598cf8ca7dfc18bd8091f045d60184b");
        assert_eq!(
            DestinationHash::from(&dest).to_string(),
            "d598cf8ca7dfc18bd8091f045d60184b"
        );
    }

    #[test]
    fn destination_hash_display_fromstr_roundtrip() {
        let id = Identity::generate();
        let dest = Destination::new(id.public_identity(), "rabbithole", &["burrow"]);
        let hash = DestinationHash::from(&dest);
        let text = hash.to_string();
        assert_eq!(text.len(), DESTINATION_HASH_LENGTH * 2);
        let parsed: DestinationHash = text.parse().unwrap();
        assert_eq!(parsed, hash);
        // Uppercase parses to the same value.
        let upper: DestinationHash = text.to_uppercase().parse().unwrap();
        assert_eq!(upper, hash);
        // Debug embeds the hex form.
        assert!(format!("{hash:?}").contains(&text));
    }

    #[test]
    fn destination_hash_parse_is_total() {
        use core::str::FromStr;
        // Wrong lengths.
        assert_eq!(
            DestinationHash::from_str(""),
            Err(DestinationHashError::BadLength(0))
        );
        assert_eq!(
            DestinationHash::from_str("ab"),
            Err(DestinationHashError::BadLength(2))
        );
        assert_eq!(
            DestinationHash::from_str(&"a".repeat(33)),
            Err(DestinationHashError::BadLength(33))
        );
        // Bad character position is reported in byte offsets.
        let mut s = "00".repeat(DESTINATION_HASH_LENGTH);
        s.replace_range(5..6, "g");
        assert_eq!(
            DestinationHash::from_str(&s),
            Err(DestinationHashError::BadChar(5))
        );
        // Multi-byte UTF-8 never panics (length is counted in bytes).
        assert!(DestinationHash::from_str(&"é".repeat(16)).is_err());
        let mut t = "00".repeat(DESTINATION_HASH_LENGTH - 1);
        t.push('é'); // 2 bytes → 32 bytes total, non-hex tail
        assert_eq!(t.len(), DESTINATION_HASH_LENGTH * 2);
        assert_eq!(
            DestinationHash::from_str(&t),
            Err(DestinationHashError::BadChar(30))
        );
    }

    #[test]
    fn destination_hash_byte_conversions() {
        let arr = [0xA5u8; DESTINATION_HASH_LENGTH];
        let hash = DestinationHash::from(arr);
        assert_eq!(hash.as_bytes(), &arr);
        assert_eq!(<[u8; DESTINATION_HASH_LENGTH]>::from(hash), arr);
        assert_eq!(DestinationHash::from_bytes(&arr), Some(hash));
        assert_eq!(DestinationHash::from_bytes(&arr[..15]), None);
        assert_eq!(DestinationHash::from_bytes(&[0u8; 17]), None);
    }
}
