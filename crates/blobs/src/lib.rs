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
use std::io::{Read, Seek, SeekFrom, Write};
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

    /// The size in bytes of a stored blob.
    pub fn size(&self, id: &BlobId) -> Result<u64, BlobError> {
        fs::metadata(self.blob_path(id))
            .map(|m| m.len())
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BlobError::NotFound(*id),
                _ => BlobError::Io(e),
            })
    }

    /// Read a byte range `[offset, offset+len)` of a blob without loading
    /// the whole file — the download-serving primitive (Wave 4.2). Reads are
    /// clamped to the end of the blob, so a short read at EOF returns fewer
    /// bytes than requested. Integrity is the whole-file root check the
    /// downloader performs on completion; the store serves its own trusted
    /// content.
    pub fn read_range(&self, id: &BlobId, offset: u64, len: usize) -> Result<Vec<u8>, BlobError> {
        let mut f = fs::File::open(self.blob_path(id)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BlobError::NotFound(*id),
            _ => BlobError::Io(e),
        })?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len];
        let mut read = 0;
        while read < len {
            match f.read(&mut buf[read..])? {
                0 => break, // EOF
                n => read += n,
            }
        }
        buf.truncate(read);
        Ok(buf)
    }

    /// Commit a staged file into the store, verifying its content hashes to
    /// `expected` — the upload-finalize primitive (Wave 4.2). Hashes the file
    /// incrementally (no whole-file buffer), then renames it into the
    /// content-addressed layout. On mismatch the staged file is left for the
    /// caller to clean up. A no-op (removing the stage) if already present.
    pub fn put_verified(&self, staged: &Path, expected: &BlobId) -> Result<BlobId, BlobError> {
        let mut hasher = blake3::Hasher::new();
        {
            let mut f = fs::File::open(staged)?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }
        let actual = BlobId(*hasher.finalize().as_bytes());
        if actual != *expected {
            return Err(BlobError::HashMismatch {
                expected: *expected,
                actual,
            });
        }
        let path = self.blob_path(expected);
        if path.exists() {
            let _ = fs::remove_file(staged);
            return Ok(*expected);
        }
        fs::create_dir_all(path.parent().expect("blob path has parent"))?;
        fs::rename(staged, &path)?;
        Ok(*expected)
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

    /// Cache policy: bound the disk used by **unreferenced** blobs to
    /// `max_bytes`, evicting oldest-first (by modified time) until the total
    /// fits. Referenced blobs — library content the database layer holds —
    /// are never evicted, so this only trims the swarm/transient cache. A
    /// `max_bytes` of 0 evicts every unreferenced blob ("none" policy);
    /// a "mirror" policy simply never calls this. Returns the evicted ids.
    pub fn evict_unreferenced_over(&self, max_bytes: u64) -> Result<Vec<BlobId>, BlobError> {
        // Gather (id, size, mtime) for unreferenced blobs.
        let mut cache: Vec<(BlobId, u64, std::time::SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        for path in walk(&self.root)? {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = BlobId::from_hex(name) else {
                continue; // skip .refs / .tmp sidecars
            };
            if self.read_refs(&id)? != 0 {
                continue; // referenced: never a cache eviction candidate
            }
            let meta = fs::metadata(&path)?;
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            total += meta.len();
            cache.push((id, meta.len(), mtime));
        }
        if total <= max_bytes {
            return Ok(Vec::new());
        }
        // Oldest first.
        cache.sort_by_key(|(_, _, mtime)| *mtime);
        let mut removed = Vec::new();
        for (id, size, _) in cache {
            if total <= max_bytes {
                break;
            }
            fs::remove_file(self.blob_path(&id))?;
            let _ = fs::remove_file(self.refs_path(&id));
            total = total.saturating_sub(size);
            removed.push(id);
        }
        Ok(removed)
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

    #[test]
    fn read_range_serves_slices() {
        let (_dir, store) = store();
        let id = store.put(b"0123456789").unwrap();
        assert_eq!(store.size(&id).unwrap(), 10);
        assert_eq!(store.read_range(&id, 0, 4).unwrap(), b"0123");
        assert_eq!(store.read_range(&id, 4, 3).unwrap(), b"456");
        // Reads past EOF clamp to what's there.
        assert_eq!(store.read_range(&id, 8, 100).unwrap(), b"89");
        assert_eq!(store.read_range(&id, 10, 5).unwrap(), b"");
    }

    #[test]
    fn evicts_oldest_unreferenced_over_cap_but_spares_referenced() {
        let (_dir, store) = store();
        // Three ~unreferenced cache blobs, distinct mtimes (oldest first).
        let a = store.put(&vec![b'a'; 1000]).unwrap();
        let b = store.put(&vec![b'b'; 1000]).unwrap();
        let c = store.put(&vec![b'c'; 1000]).unwrap();
        let older = std::time::SystemTime::now() - std::time::Duration::from_secs(300);
        let mid = std::time::SystemTime::now() - std::time::Duration::from_secs(200);
        filetime_set(&store.blob_path(&a), older);
        filetime_set(&store.blob_path(&b), mid);
        // c keeps ~now. Pin `a` with a reference: it must survive despite
        // being oldest.
        store.add_ref(&a).unwrap();

        // Cap of 1500 bytes: only ~1000 of unreferenced cache may remain, so
        // one of {b,c} is evicted — the older (b).
        let removed = store.evict_unreferenced_over(1500).unwrap();
        assert_eq!(removed, vec![b], "oldest unreferenced blob evicted");
        assert!(store.contains(&a), "referenced blob spared");
        assert!(!store.contains(&b));
        assert!(store.contains(&c));
    }

    #[test]
    fn evict_zero_cap_clears_all_unreferenced() {
        let (_dir, store) = store();
        let a = store.put(b"cache one").unwrap();
        let b = store.put(b"cache two").unwrap();
        let pinned = store.put(b"library file").unwrap();
        store.add_ref(&pinned).unwrap();

        let removed = store.evict_unreferenced_over(0).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!store.contains(&a));
        assert!(!store.contains(&b));
        assert!(store.contains(&pinned), "referenced content never evicted");
    }

    /// Set a file's modified time (test helper; no external crate).
    fn filetime_set(path: &Path, t: std::time::SystemTime) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn put_verified_commits_and_rejects_mismatch() {
        let (dir, store) = store();
        let content = b"a staged upload body";
        let expected = BlobId::for_bytes(content);

        // A correct stage commits and becomes retrievable; the stage is gone.
        let stage = dir.path().join("stage.part");
        std::fs::write(&stage, content).unwrap();
        assert_eq!(store.put_verified(&stage, &expected).unwrap(), expected);
        assert_eq!(store.get(&expected).unwrap(), content);
        assert!(!stage.exists());

        // A stage whose bytes don't match the declared id is refused.
        let bad = dir.path().join("bad.part");
        std::fs::write(&bad, b"different bytes").unwrap();
        assert!(matches!(
            store.put_verified(&bad, &expected),
            Err(BlobError::HashMismatch { .. })
        ));
        assert!(!store.contains(&BlobId::for_bytes(b"different bytes")));
    }
}
