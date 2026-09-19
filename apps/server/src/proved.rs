//! Proved ranges of the files this burrow stores: the Bao stream for a byte
//! range, which whoever takes it checks block by block against the file's
//! blake3 root (its blob id). What lets the burrow be one of the sources a
//! swarm fetch takes units from, beside people's peers, with every chunk it
//! sends as checkable as theirs.
//!
//! The proofs of a file (its Bao outboard, about 64 bytes per 16 KiB) are
//! computed the first time a range of it is asked for, streamed from the
//! file to disk, and kept beside it in the blob store (`<blob>.obao`), which
//! removes them with it. Building them reads the whole file, so it happens
//! in the background, once per file at a time and at most
//! [`BUILDS_AT_ONCE`] files at once: an ask that comes before they are
//! ready is told so at once ([`Unproved::Building`]) and asks again,
//! holding nothing here while it waits. A file that does not prove out is
//! left alone for [`FAILED_FOR`] rather than read again on every ask.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rabbithole_blobs::{BlobId, BlobStore};

use crate::Shared;

/// Files whose proofs are being built at once, for the whole burrow.
const BUILDS_AT_ONCE: usize = 2;
/// How long a file that did not prove out is left alone.
const FAILED_FOR: Duration = Duration::from_secs(600);

/// Why a range was not sent proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unproved {
    /// The file is not stored here.
    NotHere,
    /// Its proofs are being made: ask again shortly.
    Building,
    /// It does not prove out (it changed on disk, or its proofs cannot be
    /// made here): sent the plain way, checked whole, if at all.
    Failed,
}

#[derive(Default)]
struct Books {
    /// Proofs files being built now.
    building: HashSet<PathBuf>,
    /// Proofs files whose file did not prove out, and when.
    failed: HashMap<PathBuf, Instant>,
}

struct Builds {
    books: parking_lot::Mutex<Books>,
    slots: Arc<tokio::sync::Semaphore>,
}

fn builds() -> &'static Builds {
    static BUILDS: OnceLock<Builds> = OnceLock::new();
    BUILDS.get_or_init(|| Builds {
        books: parking_lot::Mutex::new(Books::default()),
        slots: Arc::new(tokio::sync::Semaphore::new(BUILDS_AT_ONCE)),
    })
}

/// A build's place in the books, given up however the build ends (done,
/// failed, or dropped with its runtime), recording a failure first.
struct Building {
    proofs: PathBuf,
    built: bool,
}

impl Drop for Building {
    fn drop(&mut self) {
        let mut books = builds().books.lock();
        if !self.built {
            books.failed.insert(self.proofs.clone(), Instant::now());
        }
        books.building.remove(&self.proofs);
    }
}

/// The stored file `root` and its proofs, if they are ready. If not, their
/// build is started (unless it is under way, or failed a while ago).
fn proofs_ready(blobs: &Arc<BlobStore>, root: [u8; 32]) -> Result<(PathBuf, PathBuf), Unproved> {
    let id = BlobId(root);
    let (data, proofs) = (blobs.file_path(&id), blobs.outboard_path(&id));
    if proofs.exists() {
        return Ok((data, proofs));
    }
    let b = builds();
    {
        let mut books = b.books.lock();
        books.failed.retain(|_, at| at.elapsed() < FAILED_FOR);
        if books.failed.contains_key(&proofs) {
            return Err(Unproved::Failed);
        }
        if !books.building.insert(proofs.clone()) {
            return Err(Unproved::Building);
        }
        // A build that ended between the first look and now left them.
        if proofs.exists() {
            books.building.remove(&proofs);
            return Ok((data, proofs));
        }
    }
    let place = Building {
        proofs: proofs.clone(),
        built: false,
    };
    let slots = b.slots.clone();
    tokio::spawn(async move {
        // The whole of it, so it is given up when the build ends, not before.
        let mut place = place;
        let Ok(_slot) = slots.acquire_owned().await else {
            return;
        };
        let built = tokio::task::spawn_blocking(move || {
            rabbithole_swarm::write_outboard(&data, root, &proofs)
        })
        .await;
        place.built = matches!(built, Ok(Ok(())));
    });
    Err(Unproved::Building)
}

/// The Bao stream for `[offset, offset + len)` of the stored file `root`
/// (`size` bytes). Proofs that no longer match the file are removed and
/// made again, in case they were stale or damaged.
pub async fn proved_range(
    shared: &Shared,
    root: [u8; 32],
    size: u64,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, Unproved> {
    let blobs = shared.blobs.clone();
    if !blobs.contains(&BlobId(root)) {
        return Err(Unproved::NotHere);
    }
    let (data, proofs) = proofs_ready(&blobs, root)?;
    let at = proofs.clone();
    let encoded = tokio::task::spawn_blocking(move || {
        rabbithole_swarm::encode_proved(&data, &at, root, size, offset, len)
    })
    .await;
    match encoded {
        Ok(Ok(stream)) => Ok(stream),
        // The proofs no longer match the file: made again, and asked for
        // again once they are.
        Ok(Err(rabbithole_swarm::PeerError::Verify(_))) => {
            let _ = std::fs::remove_file(&proofs);
            Err(proofs_ready(&blobs, root)
                .err()
                .unwrap_or(Unproved::Building))
        }
        // Something else went wrong reading them (no file handles left, a
        // disk hiccup): they are good as far as anyone knows, so they are
        // left alone and asked for again.
        _ => Err(Unproved::Building),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn settled(blobs: &Arc<BlobStore>, root: [u8; 32]) -> Result<(), Unproved> {
        for _ in 0..200 {
            match proofs_ready(blobs, root) {
                Err(Unproved::Building) => tokio::time::sleep(Duration::from_millis(10)).await,
                other => return other.map(|_| ()),
            }
        }
        panic!("the build never ended");
    }

    #[tokio::test]
    async fn proofs_are_built_once_in_the_background_and_a_failure_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = Arc::new(BlobStore::open(dir.path().join("blobs")).unwrap());
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 249) as u8).collect();
        let id = blobs.put(&body).unwrap();

        // The first ask starts the build and is told to ask again; so is
        // every ask while it runs.
        assert_eq!(proofs_ready(&blobs, id.0).unwrap_err(), Unproved::Building);
        assert_eq!(settled(&blobs, id.0).await, Ok(()));
        assert!(blobs.outboard_path(&id).exists());

        // A file that changed on disk does not prove out, and is not read
        // again for a while, whoever asks.
        let other = blobs.put(b"another file entirely").unwrap();
        std::fs::write(blobs.file_path(&other), b"changed since").unwrap();
        assert_eq!(settled(&blobs, other.0).await, Err(Unproved::Failed));
        assert_eq!(proofs_ready(&blobs, other.0), Err(Unproved::Failed));
        assert!(!blobs.outboard_path(&other).exists());
    }
}
