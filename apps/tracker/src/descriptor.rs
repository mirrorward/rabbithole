//! Signed server descriptors — the tracker's authenticated registration.
//!
//! A [`Descriptor`] is a server's public statement of what it is: name,
//! description, the address it *declares* clients should connect to, category
//! tags, occupancy, software version and a timestamp. The whole document is
//! serialized canonically (postcard) and Ed25519-signed with the server's
//! identity key, yielding a [`SignedDescriptor`] any tracker can verify
//! offline — the same self-certifying, domain-separated discipline as the
//! federation catalogs (`rabbithole-federation::catalog`).
//!
//! Contrast with the classic HTRK heartbeat ([`crate::htrk::Registration`]):
//! the unsigned heartbeat proves nothing beyond "someone at this UDP source
//! sent bytes", so the tracker keys it by the *observed* source IP. A signed
//! descriptor instead *declares* its address and backs the claim with a
//! signature, which is what makes it safe to relay tracker-to-tracker via
//! gossip ([`crate::gossip`]) — a receiving tracker that never saw the origin
//! server can still verify exactly what the server said about itself.
//!
//! `timestamp` doubles as the descriptor's **generation** for gossip: a
//! descriptor with a newer timestamp for the same server supersedes an older
//! one, and replaying a stale capture is rejected ([`crate::registry`]).
//!
//! Signatures cover **domain-separated** canonical bytes
//! ([`DESCRIPTOR_CONTEXT`]) so a descriptor signature can never be replayed
//! onto another signed surface, and every decoder goes through `postcard` —
//! arbitrary bytes yield `None`/`Err`, never a panic.

use std::net::SocketAddr;

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain separator for signed-descriptor signatures.
pub const DESCRIPTOR_CONTEXT: &[u8] = b"rhp-trk-descriptor-v1";

/// Longest accepted server name, in bytes (matches the HTRK pascal limit).
pub const MAX_NAME_LEN: usize = 255;
/// Longest accepted description, in bytes (matches the HTRK pascal limit).
pub const MAX_DESCRIPTION_LEN: usize = 255;
/// Longest accepted software version string, in bytes.
pub const MAX_SOFTWARE_LEN: usize = 64;
/// Most category tags a descriptor may carry.
pub const MAX_CATEGORIES: usize = 8;
/// Longest accepted category tag, in bytes.
pub const MAX_CATEGORY_LEN: usize = 32;

/// Why a descriptor failed to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// The signature does not verify under the descriptor's declared key.
    #[error("descriptor signature does not verify")]
    BadSignature,
    /// A field exceeds the tracker's size limits (or a category is empty).
    #[error("descriptor exceeds field limits")]
    Limits,
    /// The body could not be canonicalized for signing/verification.
    #[error("descriptor body does not encode")]
    Encoding,
}

/// The signable core of a registration (everything the signature covers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    /// The server's Ed25519 public identity key. Stamped from the signing
    /// key by [`Descriptor::sign`] so the document self-certifies.
    pub server_key: [u8; 32],
    /// Server display name.
    pub name: String,
    /// One-line server description.
    pub description: String,
    /// The address the server **declares** clients should connect to. Unlike
    /// the unsigned heartbeat path this is taken from the document, not the
    /// packet source — the signature makes it an authenticated *claim*
    /// (first verified claimant holds the listing slot; see
    /// [`crate::registry`] for the conflict policy).
    pub addr: SocketAddr,
    /// Category tags for directory filtering (e.g. `"chat"`, `"warez"`).
    pub categories: Vec<String>,
    /// Users currently online.
    pub users_online: u16,
    /// Maximum users, `0` = unknown/unlimited.
    pub capacity: u16,
    /// Software version string (e.g. `"rabbithole-server 0.36"`).
    pub software: String,
    /// Issuance time, unix milliseconds. Doubles as the gossip generation:
    /// higher supersedes lower for the same server key.
    pub timestamp: i64,
}

impl Descriptor {
    /// Start a descriptor for a server at `addr`. The key is stamped by
    /// [`Descriptor::sign`]; remaining fields default to empty/zero.
    pub fn new(name: impl Into<String>, addr: SocketAddr) -> Self {
        Self {
            server_key: [0u8; 32],
            name: name.into(),
            description: String::new(),
            addr,
            categories: Vec::new(),
            users_online: 0,
            capacity: 0,
            software: String::new(),
            timestamp: 0,
        }
    }

    /// Builder: set the one-line description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Builder: append a category tag.
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.categories.push(category.into());
        self
    }

    /// Builder: set the current user count.
    pub fn with_users(mut self, users_online: u16) -> Self {
        self.users_online = users_online;
        self
    }

    /// Builder: set the capacity (`0` = unknown/unlimited).
    pub fn with_capacity(mut self, capacity: u16) -> Self {
        self.capacity = capacity;
        self
    }

    /// Builder: set the software version string.
    pub fn with_software(mut self, software: impl Into<String>) -> Self {
        self.software = software.into();
        self
    }

    /// Builder: set the issuance timestamp (unix ms; the gossip generation).
    pub fn with_timestamp(mut self, timestamp: i64) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Whether the descriptor carries `category` (ASCII-case-insensitive).
    pub fn has_category(&self, category: &str) -> bool {
        self.categories
            .iter()
            .any(|c| c.eq_ignore_ascii_case(category))
    }

    /// Canonical bytes for signing: `postcard(self)`.
    fn canonical(&self) -> Result<Vec<u8>, DescriptorError> {
        postcard::to_allocvec(self).map_err(|_| DescriptorError::Encoding)
    }

    /// Field-limit sanity: the tracker sits on the open internet, so a
    /// descriptor may not claim unbounded strings or category lists.
    fn within_limits(&self) -> bool {
        self.name.len() <= MAX_NAME_LEN
            && self.description.len() <= MAX_DESCRIPTION_LEN
            && self.software.len() <= MAX_SOFTWARE_LEN
            && self.categories.len() <= MAX_CATEGORIES
            && self
                .categories
                .iter()
                .all(|c| !c.is_empty() && c.len() <= MAX_CATEGORY_LEN)
    }

    /// Sign this descriptor. The declared `server_key` is overwritten with
    /// `key`'s public key so the document always self-certifies.
    pub fn sign(mut self, key: &IdentityKey) -> Result<SignedDescriptor, DescriptorError> {
        self.server_key = key.public().0;
        let msg = signed_bytes(&self)?;
        let sig = key.sign(&msg);
        Ok(SignedDescriptor {
            descriptor: self,
            sig,
        })
    }
}

/// A [`Descriptor`] plus the origin server's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedDescriptor {
    /// The signed registration document.
    pub descriptor: Descriptor,
    /// Ed25519 signature over [`DESCRIPTOR_CONTEXT`] ‖ postcard(descriptor),
    /// by the key named in `descriptor.server_key`.
    pub sig: Signature,
}

impl SignedDescriptor {
    /// The verified public key this descriptor certifies itself under.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.descriptor.server_key)
    }

    /// Verify the descriptor against its **own** declared key (trackers have
    /// no prior key knowledge — trust is per-`(ip, port)` slot, first
    /// verified claimant wins; see [`crate::registry`]). Also enforces field
    /// limits so a verified descriptor is always safe to store and relay.
    pub fn verify(&self) -> Result<(), DescriptorError> {
        if !self.descriptor.within_limits() {
            return Err(DescriptorError::Limits);
        }
        let msg = signed_bytes(&self.descriptor)?;
        if self.public_key().verify(&msg, &self.sig) {
            Ok(())
        } else {
            Err(DescriptorError::BadSignature)
        }
    }

    /// Wire form (postcard) for announcing or gossiping.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("signed descriptor serializes")
    }

    /// Decode from bytes; `None` on malformed input (never panics). The
    /// caller must still [`verify`](Self::verify).
    pub fn from_bytes(bytes: &[u8]) -> Option<SignedDescriptor> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The exact bytes signed: context ‖ postcard(descriptor).
fn signed_bytes(descriptor: &Descriptor) -> Result<Vec<u8>, DescriptorError> {
    let mut msg = DESCRIPTOR_CONTEXT.to_vec();
    msg.extend(descriptor.canonical()?);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> Descriptor {
        Descriptor::new("Wonderland", ([203, 0, 113, 7], 5500).into())
            .with_description("Down the rabbit hole")
            .with_category("chat")
            .with_category("warez")
            .with_users(12)
            .with_capacity(64)
            .with_software("rabbithole-server 0.36")
            .with_timestamp(1_700_000_000_000)
    }

    #[test]
    fn sign_verify_roundtrip_and_wire_form() {
        let key = IdentityKey::from_seed(&[7u8; 32]);
        let signed = descriptor().sign(&key).unwrap();
        // Signing stamped the real public key into the body.
        assert_eq!(signed.descriptor.server_key, key.public().0);
        assert_eq!(signed.verify(), Ok(()));

        let back = SignedDescriptor::from_bytes(&signed.to_bytes()).unwrap();
        assert_eq!(back, signed);
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn tampered_fields_fail_verification() {
        let key = IdentityKey::from_seed(&[7u8; 32]);

        let mut signed = descriptor().sign(&key).unwrap();
        signed.descriptor.name = "Evil Twin".into();
        assert_eq!(signed.verify(), Err(DescriptorError::BadSignature));

        let mut signed = descriptor().sign(&key).unwrap();
        signed.descriptor.addr = ([198, 51, 100, 66], 5500).into();
        assert_eq!(signed.verify(), Err(DescriptorError::BadSignature));

        let mut signed = descriptor().sign(&key).unwrap();
        signed.descriptor.categories.push("bonus".into());
        assert_eq!(signed.verify(), Err(DescriptorError::BadSignature));
    }

    #[test]
    fn impersonating_key_fails_verification() {
        let key = IdentityKey::from_seed(&[7u8; 32]);
        let mut signed = descriptor().sign(&key).unwrap();
        // Claim a different key without a matching signature.
        signed.descriptor.server_key = IdentityKey::from_seed(&[9u8; 32]).public().0;
        assert_eq!(signed.verify(), Err(DescriptorError::BadSignature));
    }

    #[test]
    fn oversized_fields_are_rejected() {
        let key = IdentityKey::from_seed(&[7u8; 32]);
        let cases = [
            descriptor().with_software("v".repeat(MAX_SOFTWARE_LEN + 1)),
            Descriptor::new("x".repeat(MAX_NAME_LEN + 1), ([10, 0, 0, 1], 1).into()),
            descriptor().with_description("d".repeat(MAX_DESCRIPTION_LEN + 1)),
            descriptor().with_category("c".repeat(MAX_CATEGORY_LEN + 1)),
            descriptor().with_category(""),
            (0..=MAX_CATEGORIES).fold(Descriptor::new("n", ([10, 0, 0, 1], 1).into()), |d, i| {
                d.with_category(format!("c{i}"))
            }),
        ];
        for case in cases {
            let signed = case.sign(&key).unwrap();
            assert_eq!(signed.verify(), Err(DescriptorError::Limits));
        }
    }

    #[test]
    fn category_matching_ignores_ascii_case() {
        let d = descriptor();
        assert!(d.has_category("chat"));
        assert!(d.has_category("CHAT"));
        assert!(!d.has_category("board"));
    }

    #[test]
    fn decoder_never_panics_on_garbage() {
        assert!(SignedDescriptor::from_bytes(&[]).is_none());
        assert!(SignedDescriptor::from_bytes(&[0xff; 7]).is_none());
        assert!(SignedDescriptor::from_bytes(&[0x00; 300]).is_none());
    }
}
