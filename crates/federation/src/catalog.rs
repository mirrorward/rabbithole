//! Signed server file-catalogs.
//!
//! A [`Catalog`] is a server's public statement of the files it advertises for
//! federated search and pull: a flat listing of [`CatalogEntry`] rows, each
//! carrying a file's name, size, blake3 content hash, area/path, mime type and
//! timestamp. The whole listing is serialized canonically (postcard) and
//! Ed25519-signed with the server's identity key, yielding a [`SignedCatalog`]
//! any peer can fetch and verify offline — the same self-certifying discipline
//! as the peering [`crate::handshake::PeerDescriptor`].
//!
//! Catalogs are *incremental*. Each carries a monotonically increasing
//! [`Catalog::generation`] and an optional [`Catalog::prev_id`] linking back to
//! the [`catalog_id`](SignedCatalog::catalog_id) of the previous generation, so
//! a peer holding an older copy can cheaply detect staleness
//! ([`SignedCatalog::supersedes`]) and know it should pull a fresher one.
//!
//! Signatures cover **domain-separated** canonical bytes ([`CATALOG_CONTEXT`])
//! so a catalog signature can never be replayed onto another signed surface,
//! and every decoder goes through `postcard` — arbitrary bytes yield an error,
//! never a panic.

use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

/// Domain separator for signed-catalog signatures.
pub const CATALOG_CONTEXT: &[u8] = b"rhp-fed-catalog-v1";

/// One advertised file in a [`Catalog`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// File name as displayed (e.g. `"cool-demo.zip"`).
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// blake3 content hash of the file's bytes — the cross-server dedupe key.
    pub hash: [u8; 32],
    /// File area / library slug the file lives in (e.g. `"warez"`).
    pub area: String,
    /// Folder path within the area (`""` = area root).
    pub path: String,
    /// MIME type (e.g. `"application/zip"`); may be empty if unknown.
    pub mime: String,
    /// Publish time, unix milliseconds.
    pub timestamp: i64,
}

impl CatalogEntry {
    /// Construct an entry; string-ish fields accept anything `Into<String>`.
    pub fn new(
        name: impl Into<String>,
        size: u64,
        hash: [u8; 32],
        area: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            size,
            hash,
            area: area.into(),
            path: path.into(),
            mime: String::new(),
            timestamp: 0,
        }
    }

    /// Builder: set the MIME type.
    pub fn with_mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = mime.into();
        self
    }

    /// Builder: set the publish timestamp (unix ms).
    pub fn with_timestamp(mut self, timestamp: i64) -> Self {
        self.timestamp = timestamp;
        self
    }
}

/// The signable core of a catalog (everything the signature and the
/// [`catalog_id`](SignedCatalog::catalog_id) cover).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// The advertising server's Ed25519 public identity key. Stamped from the
    /// signing key by [`Catalog::sign`] so the document self-certifies.
    pub server_key: [u8; 32],
    /// Monotonically increasing version. A higher generation from the same
    /// server is strictly newer.
    pub generation: u64,
    /// `catalog_id` of the immediately previous generation, if any. `None`
    /// marks the first (genesis) catalog. Lets peers verify continuity.
    pub prev_id: Option<[u8; 32]>,
    /// Issuance time, unix milliseconds.
    pub issued_at: i64,
    /// The advertised files, in author-chosen order (order is part of the
    /// canonical bytes, hence part of the id and signature).
    pub entries: Vec<CatalogEntry>,
}

/// Why a catalog failed to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// The signature does not verify under the supplied key.
    #[error("catalog signature does not verify")]
    BadSignature,
    /// The supplied key does not match the key declared in the catalog body.
    #[error("catalog server key mismatch")]
    KeyMismatch,
    /// The body could not be canonicalized for signing/verification.
    #[error("catalog body does not encode")]
    Encoding,
}

impl Catalog {
    /// Start an empty catalog for `server_key` at `generation`, linking to
    /// `prev_id` (`None` for the genesis catalog).
    pub fn new(server_key: [u8; 32], generation: u64, prev_id: Option<[u8; 32]>) -> Self {
        Self {
            server_key,
            generation,
            prev_id,
            issued_at: 0,
            entries: Vec::new(),
        }
    }

    /// Builder: set the issuance timestamp (unix ms).
    pub fn with_issued_at(mut self, issued_at: i64) -> Self {
        self.issued_at = issued_at;
        self
    }

    /// Builder: append an entry.
    pub fn with_entry(mut self, entry: CatalogEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Canonical bytes for hashing/signing: `postcard(self)`.
    fn canonical(&self) -> Result<Vec<u8>, CatalogError> {
        postcard::to_allocvec(self).map_err(|_| CatalogError::Encoding)
    }

    /// The stable content id of this catalog body: `blake3(canonical)`.
    ///
    /// Note this depends on `server_key` being the real key, which
    /// [`Catalog::sign`] stamps before computing anything.
    pub fn id(&self) -> Result<[u8; 32], CatalogError> {
        Ok(*blake3::hash(&self.canonical()?).as_bytes())
    }

    /// Sign this catalog. The declared `server_key` is overwritten with
    /// `key`'s public key so the document always self-certifies.
    pub fn sign(mut self, key: &IdentityKey) -> Result<SignedCatalog, CatalogError> {
        self.server_key = key.public().0;
        let msg = signed_bytes(&self)?;
        let sig = key.sign(&msg);
        Ok(SignedCatalog { catalog: self, sig })
    }
}

/// A [`Catalog`] plus the origin server's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCatalog {
    /// The signed listing.
    pub catalog: Catalog,
    /// Ed25519 signature over [`CATALOG_CONTEXT`] ‖ postcard(catalog), by the
    /// key named in `catalog.server_key`.
    pub sig: Signature,
}

impl SignedCatalog {
    /// The stable content id: `blake3` of the catalog's canonical bytes. Peers
    /// use this as the `prev_id` link target across generations.
    pub fn catalog_id(&self) -> Result<[u8; 32], CatalogError> {
        self.catalog.id()
    }

    /// Verify the signature against `pubkey`, which must also equal the key
    /// declared inside the catalog (self-certification). On success the caller
    /// may trust every entry as authentically advertised by `pubkey`.
    pub fn verify(&self, pubkey: &PublicKey) -> Result<(), CatalogError> {
        if self.catalog.server_key != pubkey.0 {
            return Err(CatalogError::KeyMismatch);
        }
        let msg = signed_bytes(&self.catalog)?;
        if pubkey.verify(&msg, &self.sig) {
            Ok(())
        } else {
            Err(CatalogError::BadSignature)
        }
    }

    /// Whether this catalog is a strictly-newer successor of `prev`: it comes
    /// from the same server, carries a higher generation, and links back to
    /// `prev`'s id. A peer holding `prev` seeing this returns `true` should
    /// pull the fresher copy.
    ///
    /// Returns `false` (never errors) if either id cannot be computed.
    pub fn supersedes(&self, prev: &SignedCatalog) -> bool {
        if self.catalog.server_key != prev.catalog.server_key {
            return false;
        }
        if self.catalog.generation <= prev.catalog.generation {
            return false;
        }
        match (self.catalog.prev_id, prev.catalog_id()) {
            (Some(link), Ok(prev_id)) => link == prev_id,
            _ => false,
        }
    }

    /// Wire form (postcard) for serving or relaying.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("signed catalog serializes")
    }

    /// Decode from bytes; `None` on malformed input (never panics). The caller
    /// must still [`verify`](Self::verify).
    pub fn from_bytes(bytes: &[u8]) -> Option<SignedCatalog> {
        postcard::from_bytes(bytes).ok()
    }
}

/// The exact bytes signed: context ‖ postcard(catalog).
fn signed_bytes(catalog: &Catalog) -> Result<Vec<u8>, CatalogError> {
    let mut msg = CATALOG_CONTEXT.to_vec();
    msg.extend(catalog.canonical()?);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, hash: u8, size: u64) -> CatalogEntry {
        CatalogEntry::new(name, size, [hash; 32], "warez", "demos")
            .with_mime("application/zip")
            .with_timestamp(1_700_000_000_000)
    }

    fn catalog() -> Catalog {
        Catalog::new([0u8; 32], 1, None)
            .with_issued_at(1_700_000_000_000)
            .with_entry(entry("a.zip", 1, 100))
            .with_entry(entry("b.zip", 2, 200))
    }

    #[test]
    fn sign_verify_roundtrip_and_wire_form() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let signed = catalog().sign(&key).unwrap();
        // signing stamped the real public key into the body.
        assert_eq!(signed.catalog.server_key, key.public().0);
        assert_eq!(signed.verify(&key.public()), Ok(()));

        let back = SignedCatalog::from_bytes(&signed.to_bytes()).unwrap();
        assert_eq!(back, signed);
        assert_eq!(back.verify(&key.public()), Ok(()));
    }

    #[test]
    fn catalog_id_is_stable_and_content_addressed() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let a = catalog().sign(&key).unwrap();
        let b = catalog().sign(&key).unwrap();
        assert_eq!(a.catalog_id().unwrap(), b.catalog_id().unwrap());

        // A different listing hashes differently.
        let c = Catalog::new([0u8; 32], 1, None)
            .with_issued_at(1_700_000_000_000)
            .with_entry(entry("a.zip", 1, 100))
            .sign(&key)
            .unwrap();
        assert_ne!(a.catalog_id().unwrap(), c.catalog_id().unwrap());
    }

    #[test]
    fn tampered_entry_fails_verification() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let mut signed = catalog().sign(&key).unwrap();
        signed.catalog.entries.push(entry("evil.exe", 9, 9));
        assert_eq!(
            signed.verify(&key.public()),
            Err(CatalogError::BadSignature)
        );
    }

    #[test]
    fn wrong_key_is_rejected_as_mismatch() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let other = IdentityKey::from_seed(&[9u8; 32]);
        let signed = catalog().sign(&key).unwrap();
        assert_eq!(
            signed.verify(&other.public()),
            Err(CatalogError::KeyMismatch)
        );
    }

    #[test]
    fn impersonating_key_fails_verification() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let mut signed = catalog().sign(&key).unwrap();
        // Claim a different server key without a matching signature; verifying
        // against that claimed key fails on the signature.
        let impostor = IdentityKey::from_seed(&[9u8; 32]).public();
        signed.catalog.server_key = impostor.0;
        assert_eq!(signed.verify(&impostor), Err(CatalogError::BadSignature));
    }

    #[test]
    fn supersedes_detects_fresher_generation() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let gen1 = catalog().sign(&key).unwrap();
        let gen2 = Catalog::new(key.public().0, 2, Some(gen1.catalog_id().unwrap()))
            .with_entry(entry("c.zip", 3, 300))
            .sign(&key)
            .unwrap();
        assert!(gen2.supersedes(&gen1));
        assert!(!gen1.supersedes(&gen2));
    }

    #[test]
    fn supersedes_requires_matching_prev_link() {
        let key = IdentityKey::from_seed(&[3u8; 32]);
        let gen1 = catalog().sign(&key).unwrap();
        // Higher generation but a wrong/missing prev_id link is not a valid
        // successor.
        let unlinked = Catalog::new(key.public().0, 2, Some([0xaa; 32]))
            .sign(&key)
            .unwrap();
        assert!(!unlinked.supersedes(&gen1));
    }

    #[test]
    fn supersedes_rejects_different_server() {
        let a = IdentityKey::from_seed(&[3u8; 32]);
        let b = IdentityKey::from_seed(&[4u8; 32]);
        let gen1 = catalog().sign(&a).unwrap();
        let other = Catalog::new(b.public().0, 2, Some(gen1.catalog_id().unwrap()))
            .sign(&b)
            .unwrap();
        assert!(!other.supersedes(&gen1));
    }

    #[test]
    fn decoder_never_panics_on_garbage() {
        assert!(SignedCatalog::from_bytes(&[0xff; 5]).is_none());
        assert!(SignedCatalog::from_bytes(&[]).is_none());
    }
}
