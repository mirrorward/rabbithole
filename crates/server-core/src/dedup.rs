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
    /// key → first-seen time and unique insertion generation.
    seen: HashMap<SeenKey, (i64, u64)>,
    /// (seen_at_ms, key, generation) in insertion order for windowed eviction.
    order: VecDeque<(i64, SeenKey, u64)>,
    window_ms: i64,
    capacity: usize,
    next_generation: u64,
}

/// A time-windowed, capacity-bounded seen set.
pub struct DedupStore {
    inner: Mutex<Inner>,
}

/// A temporary duplicate gate around a fallible ingest. Commit after durable
/// success; failure or cancellation drops only this insertion's generation.
#[must_use]
pub struct SeenReservation<'a> {
    store: &'a DedupStore,
    key: SeenKey,
    generation: u64,
    committed: bool,
}

impl SeenReservation<'_> {
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SeenReservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut inner = self.store.inner.lock();
        if inner
            .seen
            .get(&self.key)
            .is_some_and(|(_, generation)| *generation == self.generation)
        {
            inner.seen.remove(&self.key);
        }
        // The bounded ring entry can remain until normal eviction. Generation
        // comparisons ensure it can never evict a later insertion of this key.
    }
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
                next_generation: 0,
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
        self.record(key, now_ms).is_some()
    }

    /// Reserve processing once across concurrent callers, rolling back on
    /// dropped futures and failures. Existing permanent-record callers keep
    /// using `check_and_record` unchanged.
    pub fn reserve(&self, key: SeenKey, now_ms: i64) -> Option<SeenReservation<'_>> {
        let generation = self.record(key.clone(), now_ms)?;
        Some(SeenReservation {
            store: self,
            key,
            generation,
            committed: false,
        })
    }

    fn record(&self, key: SeenKey, now_ms: i64) -> Option<u64> {
        let mut inner = self.inner.lock();
        Self::evict(&mut inner, now_ms);
        if inner.seen.contains_key(&key) {
            return None;
        }
        let generation = inner.next_generation.checked_add(1)?;
        inner.next_generation = generation;
        inner.seen.insert(key.clone(), (now_ms, generation));
        inner.order.push_back((now_ms, key, generation));
        // Capacity backstop even within the window.
        while inner.order.len() > inner.capacity {
            if let Some((_, old, old_generation)) = inner.order.pop_front() {
                if inner
                    .seen
                    .get(&old)
                    .is_some_and(|(_, current)| *current == old_generation)
                {
                    inner.seen.remove(&old);
                }
            }
        }
        Some(generation)
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
        while let Some((ts, _, _)) = inner.order.front() {
            if *ts >= cutoff {
                break;
            }
            if let Some((_, key, generation)) = inner.order.pop_front() {
                if inner
                    .seen
                    .get(&key)
                    .is_some_and(|(_, current)| *current == generation)
                {
                    inner.seen.remove(&key);
                }
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
    #[test]
    fn reservation_rollback_and_commit_are_distinct() {
        let d = DedupStore::new(1000, 100);
        let key = SeenKey::Event([1; 32]);
        let pending = d.reserve(key.clone(), 0).unwrap();
        assert!(d.reserve(key.clone(), 0).is_none());
        drop(pending);
        assert!(!d.seen(&key));
        d.reserve(key.clone(), 1).unwrap().commit();
        assert!(d.seen(&key));
        assert!(d.reserve(key, 2).is_none());
    }

    #[test]
    fn expired_or_evicted_reservations_cannot_remove_new_generations() {
        for capacity in [1, 100] {
            let d = DedupStore::new(10, capacity);
            let key = SeenKey::Event([1; 32]);
            let old = d.reserve(key.clone(), 0).unwrap();
            d.check_and_record(SeenKey::Event([2; 32]), 0);
            d.reserve(key.clone(), 20).unwrap().commit();
            drop(old);
            assert!(d.seen(&key), "old rollback must not remove renewed key");
        }
        let d = DedupStore::new(10, 100);
        let key = SeenKey::Event([3; 32]);
        drop(d.reserve(key.clone(), 0).unwrap());
        d.reserve(key.clone(), 5).unwrap().commit();
        d.check_and_record(SeenKey::Event([4; 32]), 11);
        assert!(
            d.seen(&key),
            "expired ghost entry must not evict renewed key"
        );
    }

    #[tokio::test]
    async fn cancelled_ingest_releases_its_reservation() {
        let d = std::sync::Arc::new(DedupStore::default());
        let key = SeenKey::Event([5; 32]);
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let task_store = d.clone();
        let task_key = key.clone();
        let task = tokio::spawn(async move {
            let _pending = task_store.reserve(task_key, 0).unwrap();
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        waiting.await.unwrap();
        assert!(d.seen(&key));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(d.reserve(key, 1).is_some(), "next delivery can retry");
    }
}
