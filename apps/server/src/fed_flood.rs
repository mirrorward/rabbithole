//! Shared state for Wave 9 board-event flood-fill: the origin-key registry.
//!
//! A federated board event is signed twice — by the author and by its **origin
//! server**. To verify the origin signature a receiver needs the origin
//! server's Ed25519 key. That is trivial one hop from the origin (its own
//! server key, or a direct peer's proven handshake key) but not two or more
//! hops out: on an `A ← B ← C` chain, `A` never peered with `C`, yet must
//! verify `C`'s origin signature on a post relayed through `B`.
//!
//! So the origin key rides the wire alongside each event (see
//! [`crate::federation`]'s `MT_EVENTS`), and every burrow **pins** the first
//! key it verifies for a given origin id in this registry. Subsequent events
//! claiming the same origin must present the same key or they are rejected as a
//! spoof (key continuity, mirroring the pinned-peer discipline in
//! [`crate::fed_catalog::ingest_peer_catalog`]). Relaying a stored event onward
//! resolves its origin key back out of here.
//!
//! Pins **persist across restart** to `<data_dir>/federation/origin_keys.json`
//! (via [`FloodState::load`]). This closes a hijack window a reboot would
//! otherwise reopen: without persistence, after a restart the first key seen
//! for a known origin re-pins it, so a spoofer racing the legitimate server
//! could steal the origin. Reloading the pins keeps continuity across reboots.
//!
//! The map is bounded by a hard cap on distinct origins (a burrow federates
//! with a small, human-scale set of servers, not per-event state), so it can
//! never grow without limit.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use parking_lot::{Mutex, RwLock};

/// Largest number of distinct `origin -> server key` pins retained. A burrow
/// peers with a human-scale set of servers; past the cap we stop learning new
/// origins (already-pinned ones keep verifying), so memory stays bounded even
/// under a hostile flood of fabricated origin names.
const MAX_ORIGINS: usize = 4096;

/// The origin-key registry: `origin id` (a server's `origin_name`) → its pinned
/// Ed25519 server key. First verified key wins; a later conflicting key for the
/// same origin is a spoof and is refused by the ingest path.
pub struct FloodState {
    origins: RwLock<HashMap<String, [u8; 32]>>,
    /// Where pins persist across restart. `None` = in-memory only (tests /
    /// non-federation callers).
    path: Option<PathBuf>,
    /// Serialises persistence so two concurrent new-origin pins don't lose each
    /// other in the file (they never block readers — [`resolve`] takes only
    /// the `origins` read lock).
    ///
    /// [`resolve`]: Self::resolve
    persist_lock: Mutex<()>,
}

impl Default for FloodState {
    fn default() -> Self {
        Self {
            origins: RwLock::new(HashMap::new()),
            path: None,
            persist_lock: Mutex::new(()),
        }
    }
}

impl FloodState {
    /// In-memory only (no persistence) — tests and non-federation callers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load pins from `<data_dir>/federation/origin_keys.json` (empty when
    /// absent/corrupt) and persist subsequent [`note`]s back to it, so pinned
    /// origin keys survive a restart.
    ///
    /// [`note`]: Self::note
    pub fn load(data_dir: &Path) -> Self {
        let path = pins_path(data_dir);
        Self {
            origins: RwLock::new(read_pins(&path)),
            path: Some(path),
            persist_lock: Mutex::new(()),
        }
    }

    /// The pinned key for `origin`, if we have verified one before.
    pub fn resolve(&self, origin: &str) -> Option<[u8; 32]> {
        self.origins.read().get(origin).copied()
    }

    /// Pin `key` as `origin`'s server key on **first** sighting (idempotent;
    /// never overwrites an existing pin, and stops inserting once the origin
    /// cap is reached), then persist the registry. Conflicts are detected by
    /// callers via [`resolve`] before verification, not here.
    ///
    /// [`resolve`]: Self::resolve
    pub fn note(&self, origin: String, key: [u8; 32]) {
        {
            let mut m = self.origins.write();
            if m.contains_key(&origin) || m.len() >= MAX_ORIGINS {
                return;
            }
            m.insert(origin, key);
        }
        // Persist the current full map, serialised so concurrent new-origin
        // pins each snapshot the latest state (never a partial one).
        if let Some(path) = &self.path {
            let _guard = self.persist_lock.lock();
            let snapshot = self.origins.read().clone();
            if let Err(e) = persist_pins(path, &snapshot) {
                tracing::warn!(path = %path.display(), "federation: origin-key persist failed: {e}");
            }
        }
    }

    /// Number of pinned origins (for tests / status).
    pub fn len(&self) -> usize {
        self.origins.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The origin-key pin file under a burrow's data dir.
fn pins_path(data_dir: &Path) -> PathBuf {
    data_dir.join("federation").join("origin_keys.json")
}

/// Read pinned `origin -> key` pairs (empty when absent/corrupt), capped so a
/// hand-edited file can't exceed the in-memory bound.
fn read_pins(path: &Path) -> HashMap<String, [u8; 32]> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(map) = serde_json::from_slice::<BTreeMap<String, String>>(&bytes) else {
        tracing::warn!(path = %path.display(), "federation: origin_keys.json unreadable");
        return HashMap::new();
    };
    map.into_iter()
        .filter_map(|(origin, h)| Some((origin, <[u8; 32]>::try_from(hex::decode(h).ok()?).ok()?)))
        .take(MAX_ORIGINS)
        .collect()
}

/// Atomically persist the pins as a sorted `{origin: hexkey}` object
/// (tmp + rename, so a crash never leaves a torn file).
fn persist_pins(path: &Path, map: &HashMap<String, [u8; 32]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sorted: BTreeMap<String, String> = map
        .iter()
        .map(|(o, k)| (o.clone(), hex::encode(k)))
        .collect();
    let bytes = serde_json::to_vec_pretty(&sorted)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_key_wins_and_is_stable() {
        let s = FloodState::new();
        assert!(s.resolve("warren-c").is_none());
        s.note("warren-c".into(), [1u8; 32]);
        assert_eq!(s.resolve("warren-c"), Some([1u8; 32]));
        // A later conflicting note never overwrites the pin.
        s.note("warren-c".into(), [2u8; 32]);
        assert_eq!(s.resolve("warren-c"), Some([1u8; 32]));
    }

    #[test]
    fn origins_are_bounded() {
        let s = FloodState::new();
        for i in 0..(MAX_ORIGINS + 100) {
            s.note(format!("origin-{i}"), [(i % 256) as u8; 32]);
        }
        assert_eq!(s.len(), MAX_ORIGINS, "distinct-origin count is capped");
    }

    #[test]
    fn pins_persist_and_reload_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = FloodState::load(dir.path());
            s.note("warren-b".into(), [7u8; 32]);
            s.note("warren-c".into(), [9u8; 32]);
            // A conflicting re-note is a no-op on disk too.
            s.note("warren-c".into(), [3u8; 32]);
        }
        // A fresh instance (a "restart") reloads the pins.
        let s2 = FloodState::load(dir.path());
        assert_eq!(s2.resolve("warren-b"), Some([7u8; 32]));
        assert_eq!(s2.resolve("warren-c"), Some([9u8; 32]), "first key kept");
        assert_eq!(s2.len(), 2);
    }

    #[test]
    fn absent_or_corrupt_file_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        // Absent: loads empty.
        assert!(FloodState::load(dir.path()).is_empty());
        // Corrupt: loads empty, and a subsequent note still works + overwrites.
        std::fs::create_dir_all(dir.path().join("federation")).unwrap();
        std::fs::write(pins_path(dir.path()), b"{not valid json").unwrap();
        let s = FloodState::load(dir.path());
        assert!(s.is_empty());
        s.note("warren-a".into(), [1u8; 32]);
        assert_eq!(
            FloodState::load(dir.path()).resolve("warren-a"),
            Some([1u8; 32])
        );
    }
}
