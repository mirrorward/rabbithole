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
//! The map is bounded by a hard cap on distinct origins (a burrow federates
//! with a small, human-scale set of servers, not per-event state), so it can
//! never grow without limit.

use std::collections::HashMap;

use parking_lot::RwLock;

/// Largest number of distinct `origin -> server key` pins retained. A burrow
/// peers with a human-scale set of servers; past the cap we stop learning new
/// origins (already-pinned ones keep verifying), so memory stays bounded even
/// under a hostile flood of fabricated origin names.
const MAX_ORIGINS: usize = 4096;

/// The origin-key registry: `origin id` (a server's `origin_name`) → its pinned
/// Ed25519 server key. First verified key wins; a later conflicting key for the
/// same origin is a spoof and is refused by the ingest path.
#[derive(Default)]
pub struct FloodState {
    origins: RwLock<HashMap<String, [u8; 32]>>,
}

impl FloodState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pinned key for `origin`, if we have verified one before.
    pub fn resolve(&self, origin: &str) -> Option<[u8; 32]> {
        self.origins.read().get(origin).copied()
    }

    /// Pin `key` as `origin`'s server key on **first** sighting (idempotent;
    /// never overwrites an existing pin, and stops inserting once the origin
    /// cap is reached). Conflicts are detected by callers via [`resolve`]
    /// before verification, not here.
    ///
    /// [`resolve`]: Self::resolve
    pub fn note(&self, origin: String, key: [u8; 32]) {
        let mut m = self.origins.write();
        if m.contains_key(&origin) || m.len() >= MAX_ORIGINS {
            return;
        }
        m.insert(origin, key);
    }

    /// Number of pinned origins (for tests / status).
    pub fn len(&self) -> usize {
        self.origins.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
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
}
