//! The shared dupe/seen subsystem — core infrastructure for federation
//! (W9) and syndication (W10, where an echo storm that loops can get a new
//! FidoNet node excommunicated). Every network's message identity form —
//! blake3 event id, FTN MSGID, Usenet Message-ID, QWK number+conf — folds
//! into one namespaced key, checked against a time-windowed seen set.
//!
//! In-memory with a bounded, time-ordered ring so it can't grow without
//! limit; the durable stores (posts table, syndication tables) remain the
//! permanent record. This is the fast "have I processed this already?"
//! gate that prevents reprocessing and rebroadcast loops.

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

/// A message identity, namespaced by the network it came from so ids from
/// different networks can never collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SeenKey {
    /// Native content id (blake3 of a signed event). This is also the key the
    /// Wave 9 board-event flood-fill dedupes on: a federated `FedEvent` id *is*
    /// the blake3 of its signed board event, so no distinct `FedEvent` variant
    /// is needed — the same id gates local minting and cross-server ingestion
    /// under one namespace, and an event echoed back over a second edge is a
    /// no-op replay here.
    Event([u8; 32]),
    /// FidoNet MSGID: origin address + serial.
    Ftn(String),
    /// Usenet/NNTP Message-ID.
    MessageId(String),
    /// QWK: conference number + message number.
    Qwk { conference: u16, number: u32 },
    /// QWK `.REP` reply upload: the blake3 *content* digest of an uploaded
    /// reply (`rabbithole-legacy-qwk`'s `content_hash` — conference, routing,
    /// subject, body; volatile header bookkeeping excluded), so a re-uploaded
    /// reply packet does not double-post.
    QwkReply([u8; 32]),
    /// Syndicated feed item: the stable `legacy-syndication` dedup id
    /// (blake3 of guid/link/title+date, 64 hex chars).
    Syndication(String),
}

struct Inner {
    /// key → first-seen unix ms.
    seen: HashMap<SeenKey, i64>,
    /// (seen_at_ms, key) in insertion order for windowed eviction.
    order: VecDeque<(i64, SeenKey)>,
    window_ms: i64,
    capacity: usize,
}

/// A time-windowed, capacity-bounded seen set.
pub struct DedupStore {
    inner: Mutex<Inner>,
}

impl DedupStore {
    /// `window_ms`: entries older than this are eligible for eviction.
    /// `capacity`: hard cap on retained entries (oldest evicted first).
    pub fn new(window_ms: i64, capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                seen: HashMap::new(),
                order: VecDeque::new(),
                window_ms,
                capacity,
            }),
        }
    }

    /// A sane default: a 30-day window, up to 1M entries.
    pub fn with_defaults() -> Self {
        Self::new(1000 * 60 * 60 * 24 * 30, 1_000_000)
    }

    /// Record a key as seen at `now_ms`. Returns `true` if it was **new**
    /// (act on it), `false` if already seen (drop it — a dupe/loop).
    pub fn check_and_record(&self, key: SeenKey, now_ms: i64) -> bool {
        let mut inner = self.inner.lock();
        Self::evict(&mut inner, now_ms);
        if inner.seen.contains_key(&key) {
            return false;
        }
        inner.seen.insert(key.clone(), now_ms);
        inner.order.push_back((now_ms, key));
        // Capacity backstop even within the window.
        while inner.order.len() > inner.capacity {
            if let Some((_, old)) = inner.order.pop_front() {
                inner.seen.remove(&old);
            }
        }
        true
    }

    /// Non-mutating check.
    pub fn seen(&self, key: &SeenKey) -> bool {
        self.inner.lock().seen.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn evict(inner: &mut Inner, now_ms: i64) {
        let cutoff = now_ms - inner.window_ms;
        while let Some((ts, _)) = inner.order.front() {
            if *ts >= cutoff {
                break;
            }
            if let Some((_, key)) = inner.order.pop_front() {
                inner.seen.remove(&key);
            }
        }
    }
}

impl Default for DedupStore {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_then_duplicate() {
        let d = DedupStore::new(10_000, 100);
        let k = SeenKey::Event([1; 32]);
        assert!(d.check_and_record(k.clone(), 1000), "first sighting is new");
        assert!(!d.check_and_record(k.clone(), 1001), "second is a dupe");
        assert!(d.seen(&k));
    }

    #[test]
    fn namespaces_never_collide() {
        let d = DedupStore::default();
        assert!(d.check_and_record(SeenKey::Ftn("1:2/3 abcd".into()), 0));
        assert!(d.check_and_record(SeenKey::MessageId("<x@host>".into()), 0));
        assert!(d.check_and_record(
            SeenKey::Qwk {
                conference: 1,
                number: 42
            },
            0
        ));
        assert!(d.check_and_record(SeenKey::QwkReply([7; 32]), 0));
        // Same textual content, different network → distinct keys.
        assert!(d.check_and_record(SeenKey::MessageId("1:2/3 abcd".into()), 0));
        // Same 32 bytes, different namespace → distinct keys.
        assert!(d.check_and_record(SeenKey::Event([7; 32]), 0));
        assert_eq!(d.len(), 6);
    }

    #[test]
    fn window_eviction_allows_reprocess() {
        let d = DedupStore::new(1000, 100);
        let k = SeenKey::Event([7; 32]);
        assert!(d.check_and_record(k.clone(), 0));
        assert!(!d.check_and_record(k.clone(), 500)); // still in window
                                                      // Well past the window: the old entry is evicted, so it's "new" again.
        assert!(d.check_and_record(k.clone(), 5000));
    }

    #[test]
    fn capacity_backstop() {
        let d = DedupStore::new(i64::MAX, 3);
        for i in 0..5u8 {
            d.check_and_record(SeenKey::Event([i; 32]), i as i64);
        }
        assert_eq!(d.len(), 3, "capacity capped");
        // The 3 newest survive; the 2 oldest were evicted.
        assert!(!d.seen(&SeenKey::Event([0; 32])));
        assert!(d.seen(&SeenKey::Event([4; 32])));
    }

    #[test]
    fn loop_scenario_a_message_seen_twice_is_dropped_once() {
        // Simulate an echomail message arriving via two paths.
        let d = DedupStore::default();
        let msgid = SeenKey::Ftn("2:250/1 deadbeef".into());
        assert!(
            d.check_and_record(msgid.clone(), 100),
            "toss the first copy"
        );
        assert!(
            !d.check_and_record(msgid, 200),
            "second copy from a loop is dropped"
        );
    }
}
