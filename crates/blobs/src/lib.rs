//! Content-addressed blob store.
//!
//! Files, avatars, banner art, and theme-bundle assets are stored once,
//! keyed by their BLAKE3 hash, under `blobs/ab/cd/<hex>` (two levels of
//! fan-out to keep directories small). References are counted in a
//! sidecar `refs` file per blob; [`BlobStore::gc`] removes blobs whose
//! count has reached zero.
//!
//! This is deliberately a plain synchronous filesystem implementation:
//! callers run it via `spawn_blocking` (server) or use it directly (CLI).
//! Reference counts here are advisory bookkeeping owned by the database
//! layer in later waves; the store itself only enforces content identity.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A blob identity: the BLAKE3 hash of its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(pub [u8; 32]);

impl BlobId {
    pub fn for_bytes(bytes: &[u8]) -> Self {
        BlobId(*blake3::hash(bytes).as_bytes())
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let raw = hex::decode(s).ok()?;
        Some(BlobId(raw.try_into().ok()?))
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("blob not found: {0}")]
    NotFound(BlobId),
    #[error("content hash mismatch (store corruption?): expected {expected}, got {actual}")]
    HashMismatch { expected: BlobId, actual: BlobId },
}

/// A filesystem-backed content-addressed store.
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Open (creating if needed) a store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BlobError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn blob_path(&self, id: &BlobId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(&hex)
    }

    fn refs_path(&self, id: &BlobId) -> PathBuf {
        self.blob_path(id).with_extension("refs")
    }

    /// Store bytes, returning their id. Idempotent: storing existing
    /// content is a no-op (and does not touch its refcount).
    pub fn put(&self, bytes: &[u8]) -> Result<BlobId, BlobError> {
        let id = BlobId::for_bytes(bytes);
        let path = self.blob_path(&id);
        if path.exists() {
            return Ok(id);
        }
        fs::create_dir_all(path.parent().expect("blob path has parent"))?;
        // Write via temp file + rename for crash atomicity.
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(id)
    }

    /// Read a blob fully, verifying its hash.
    pub fn get(&self, id: &BlobId) -> Result<Vec<u8>, BlobError> {
        let path = self.blob_path(id);
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BlobError::NotFound(*id),
                _ => BlobError::Io(e),
            })?
            .read_to_end(&mut bytes)?;
        let actual = BlobId::for_bytes(&bytes);
        if actual != *id {
            return Err(BlobError::HashMismatch {
                expected: *id,
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn contains(&self, id: &BlobId) -> bool {
        self.blob_path(id).exists()
    }

    /// Increment the reference count, returning the new count.
    pub fn add_ref(&self, id: &BlobId) -> Result<u64, BlobError> {
        if !self.contains(id) {
            return Err(BlobError::NotFound(*id));
        }
        let count = self.read_refs(id)? + 1;
        self.write_refs(id, count)?;
        Ok(count)
    }

    /// Decrement the reference count (saturating at zero), returning the
    /// new count. The blob is not removed until [`Self::gc`].
    pub fn release(&self, id: &BlobId) -> Result<u64, BlobError> {
        let count = self.read_refs(id)?.saturating_sub(1);
        self.write_refs(id, count)?;
        Ok(count)
    }

    fn read_refs(&self, id: &BlobId) -> Result<u64, BlobError> {
        match fs::read_to_string(self.refs_path(id)) {
            Ok(s) => Ok(s.trim().parse().unwrap_or(0)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    fn write_refs(&self, id: &BlobId, count: u64) -> Result<(), BlobError> {
        fs::write(self.refs_path(id), count.to_string())?;
        Ok(())
    }

    /// Remove every blob with a zero reference count. Returns the ids
    /// removed. Blobs with no refs file are treated as zero (unreferenced).
    pub fn gc(&self) -> Result<Vec<BlobId>, BlobError> {
        let mut removed = Vec::new();
        for entry in walk(&self.root)? {
            let path = entry;
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = BlobId::from_hex(name) else {
                continue;
            };
            if self.read_refs(&id)? == 0 {
                fs::remove_file(&path)?;
                let _ = fs::remove_file(self.refs_path(&id));
                removed.push(id);
            }
        }
        Ok(removed)
    }
}

fn walk(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BlobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path().join("blobs")).unwrap();
        (dir, store)
    }

    #[test]
    fn put_get_roundtrip() {
        let (_dir, store) = store();
        let id = store.put(b"hello warren").unwrap();
        assert_eq!(store.get(&id).unwrap(), b"hello warren");
        assert!(store.contains(&id));
    }

    #[test]
    fn put_is_idempotent() {
        let (_dir, store) = store();
        let a = store.put(b"same").unwrap();
        let b = store.put(b"same").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn get_missing_is_not_found() {
        let (_dir, store) = store();
        let id = BlobId::for_bytes(b"never stored");
        assert!(matches!(store.get(&id), Err(BlobError::NotFound(_))));
    }

    #[test]
    fn refcount_and_gc() {
        let (_dir, store) = store();
        let keep = store.put(b"keep me").unwrap();
        let drop_ = store.put(b"drop me").unwrap();
        store.add_ref(&keep).unwrap();
        store.add_ref(&drop_).unwrap();
        store.release(&drop_).unwrap();

        let removed = store.gc().unwrap();
        assert_eq!(removed, vec![drop_]);
        assert!(store.contains(&keep));
        assert!(!store.contains(&drop_));
    }

    #[test]
    fn detects_corruption() {
        let (_dir, store) = store();
        let id = store.put(b"pristine").unwrap();
        std::fs::write(store.blob_path(&id), b"tampered!").unwrap();
        assert!(matches!(
            store.get(&id),
            Err(BlobError::HashMismatch { .. })
        ));
    }

    #[test]
    fn hex_roundtrip() {
        let id = BlobId::for_bytes(b"x");
        assert_eq!(BlobId::from_hex(&id.to_hex()), Some(id));
        assert_eq!(BlobId::from_hex("zz"), None);
    }
}
