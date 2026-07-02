//! Swarm manifests: a content-addressed catalog of a fileset.
//!
//! A manifest lists the files distributed as a unit — each with its path,
//! size, and blake3 root (the same root the blob store and Bao verification
//! use). The manifest's own id is `blake3` over its canonical encoding, so
//! identical contents always hash to the same id. That id is what a
//! [`rabbit://`](crate::link) link pins, and what a fetcher checks a
//! downloaded manifest against before trusting any of its file roots.
//!
//! Encoding is postcard (the RHP wire codec): compact and deterministic for a
//! fixed schema, which is exactly what content-addressing needs. Files are
//! kept sorted by path so a manifest built from the same set in any order
//! yields identical bytes. (A self-describing CBOR encoding could be added
//! later as an alternate for non-Rust swarm clients; the id is defined over
//! whatever canonical bytes ship.)

use serde::{Deserialize, Serialize};

/// Swarm chunk size (also the Bao verification granularity): 1 MiB.
pub const CHUNK_SIZE: u32 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("decode: {0}")]
    Decode(#[from] postcard::Error),
}

/// One file in a [`Manifest`]. Folders are implied by `/` in `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// `/`-joined path relative to the manifest root (never leading `/`).
    pub path: String,
    /// File length in bytes.
    pub size: u64,
    /// blake3 root over the file's 1 MiB chunks (== its blob id).
    pub root: [u8; 32],
    /// MIME type (may be empty).
    pub mime: String,
}

impl ManifestFile {
    pub fn new(
        path: impl Into<String>,
        size: u64,
        root: [u8; 32],
        mime: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            size,
            root,
            mime: mime.into(),
        }
    }

    /// Number of `CHUNK_SIZE` chunks this file spans (0-byte files → 0).
    pub fn chunk_count(&self) -> u64 {
        self.size.div_ceil(CHUNK_SIZE as u64)
    }
}

/// A content-addressed catalog of files distributed as a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Human-readable label (not part of identity beyond its bytes).
    pub name: String,
    /// Chunk size these roots were computed with.
    pub chunk_size: u32,
    /// Files, kept sorted by `path` for a canonical encoding.
    pub files: Vec<ManifestFile>,
}

impl Manifest {
    /// Build a manifest, sorting files by path so the encoding is canonical
    /// regardless of the order they were supplied in.
    pub fn new(name: impl Into<String>, mut files: Vec<ManifestFile>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            name: name.into(),
            chunk_size: CHUNK_SIZE,
            files,
        }
    }

    /// Canonical bytes (postcard). Files are already path-sorted by [`new`].
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("manifest serializes")
    }

    /// Parse canonical bytes back into a manifest.
    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestError> {
        Ok(postcard::from_bytes(bytes)?)
    }

    /// The manifest id: `blake3` over the canonical encoding.
    pub fn id(&self) -> [u8; 32] {
        *blake3::hash(&self.encode()).as_bytes()
    }

    /// Total bytes across all files.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Total chunk count across all files.
    pub fn total_chunks(&self) -> u64 {
        self.files.iter().map(|f| f.chunk_count()).sum()
    }

    /// Look a file up by its path.
    pub fn file(&self, path: &str) -> Option<&ManifestFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn roundtrips_and_ids_are_stable() {
        let m = Manifest::new(
            "demo",
            vec![
                ManifestFile::new("a.bin", 10, root(1), "application/octet-stream"),
                ManifestFile::new("sub/b.txt", 20, root(2), "text/plain"),
            ],
        );
        let bytes = m.encode();
        let back = Manifest::decode(&bytes).unwrap();
        assert_eq!(m, back);
        assert_eq!(m.id(), back.id());
    }

    #[test]
    fn id_is_order_independent() {
        let files = vec![
            ManifestFile::new("z.bin", 1, root(9), ""),
            ManifestFile::new("a.bin", 1, root(1), ""),
            ManifestFile::new("m.bin", 1, root(5), ""),
        ];
        let mut rev = files.clone();
        rev.reverse();
        // Same set, different input order → identical id (files are sorted).
        assert_eq!(Manifest::new("x", files).id(), Manifest::new("x", rev).id());
    }

    #[test]
    fn different_content_different_id() {
        let a = Manifest::new("x", vec![ManifestFile::new("f", 1, root(1), "")]);
        let b = Manifest::new("x", vec![ManifestFile::new("f", 1, root(2), "")]);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn chunk_math() {
        // exact multiple, one over, empty
        assert_eq!(
            ManifestFile::new("f", CHUNK_SIZE as u64, root(1), "").chunk_count(),
            1
        );
        assert_eq!(
            ManifestFile::new("f", CHUNK_SIZE as u64 + 1, root(1), "").chunk_count(),
            2
        );
        assert_eq!(ManifestFile::new("f", 0, root(1), "").chunk_count(), 0);

        let m = Manifest::new(
            "x",
            vec![
                ManifestFile::new("a", CHUNK_SIZE as u64 + 1, root(1), ""), // 2 chunks
                ManifestFile::new("b", 5, root(2), ""),                     // 1 chunk
            ],
        );
        assert_eq!(m.total_size(), CHUNK_SIZE as u64 + 6);
        assert_eq!(m.total_chunks(), 3);
    }
}
