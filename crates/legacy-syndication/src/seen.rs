//! Dedup / seen-set for feed ingestion.
//!
//! Feeds are polled forever, so every poll re-parses items the board already
//! stored. This module is the pure, sans-IO counterpart of the shared dupe
//! subsystem (the "have I processed this already?" gate): given the set of
//! [`dedup_id`](crate::dedup::dedup_id)s already ingested, it partitions a
//! freshly-parsed batch into the genuinely new items and the duplicates.
//!
//! Two shapes are offered over the same logic:
//! - [`partition_fresh`] — the free function, taking a plain `&HashSet<ItemId>`.
//! - [`SeenSet`] — a thin owning wrapper that also lets callers *record* ids as
//!   they are ingested and [`partition`](SeenSet::partition) in one call.
//!
//! Both are total and deterministic. Input order is preserved, and a batch
//! that repeats the same id twice counts the second (and later) occurrence as a
//! duplicate — so an intra-feed loop can't slip a double-post past the gate.

use std::collections::HashSet;

use crate::dedup::dedup_id;
use crate::feed::FeedItem;

/// A stable feed-item id, as produced by [`dedup_id`]: 64 lowercase hex chars.
pub type ItemId = String;

/// The two halves of a partitioned batch: items not seen before, and items
/// that were already known (or repeated within the same batch).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Partition {
    /// Items whose id was absent from the seen set (act on these).
    pub fresh: Vec<FeedItem>,
    /// Items whose id was already seen — dupes to drop.
    pub duplicates: Vec<FeedItem>,
}

/// Split `items` into `(fresh, duplicates)` against the `seen` id set.
///
/// Order is preserved within each half. An id repeated inside `items` lands in
/// `fresh` on first sight and in `duplicates` thereafter, so re-partitioning
/// the same batch (after recording the fresh ids) is a no-op. The caller's
/// `seen` set is not mutated.
pub fn partition_fresh(items: &[FeedItem], seen: &HashSet<ItemId>) -> Partition {
    let mut out = Partition::default();
    // Ids already emitted to `fresh` in this batch, so an in-feed repeat is a
    // duplicate too — without touching the caller's set.
    let mut batch: HashSet<ItemId> = HashSet::new();
    for item in items {
        let id = dedup_id(item);
        if seen.contains(&id) || !batch.insert(id) {
            out.duplicates.push(item.clone());
        } else {
            out.fresh.push(item.clone());
        }
    }
    out
}

/// An owning set of ingested item ids: the pure, in-memory record of "already
/// posted" that the ingest loop grows as it stores drafts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeenSet {
    ids: HashSet<ItemId>,
}

impl SeenSet {
    /// An empty seen set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from an iterator of ids (e.g. loaded from the durable store).
    pub fn from_ids<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<ItemId>,
    {
        Self {
            ids: ids.into_iter().map(Into::into).collect(),
        }
    }

    /// Is this id already recorded?
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Has this item (by its [`dedup_id`]) already been recorded?
    pub fn contains_item(&self, item: &FeedItem) -> bool {
        self.ids.contains(&dedup_id(item))
    }

    /// Record an id. Returns `true` if it was newly inserted, `false` if it
    /// was already present.
    pub fn insert(&mut self, id: impl Into<ItemId>) -> bool {
        self.ids.insert(id.into())
    }

    /// Record an item by its [`dedup_id`]. Returns `true` if newly inserted.
    pub fn record(&mut self, item: &FeedItem) -> bool {
        self.ids.insert(dedup_id(item))
    }

    /// Record every item of a batch; returns how many were newly inserted.
    pub fn record_all<'a, I>(&mut self, items: I) -> usize
    where
        I: IntoIterator<Item = &'a FeedItem>,
    {
        items.into_iter().filter(|it| self.record(it)).count()
    }

    /// Partition a batch against this set (see [`partition_fresh`]). Does not
    /// mutate the set — record the fresh half explicitly if desired.
    pub fn partition(&self, items: &[FeedItem]) -> Partition {
        partition_fresh(items, &self.ids)
    }

    /// Number of recorded ids.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Is the set empty?
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(guid: &str) -> FeedItem {
        FeedItem {
            guid: guid.into(),
            title: format!("post {guid}"),
            ..FeedItem::default()
        }
    }

    #[test]
    fn all_fresh_against_empty_set() {
        let items = vec![item("a"), item("b"), item("c")];
        let p = partition_fresh(&items, &HashSet::new());
        assert_eq!(p.fresh.len(), 3);
        assert!(p.duplicates.is_empty());
    }

    #[test]
    fn known_ids_are_duplicates_and_order_is_preserved() {
        let items = vec![item("a"), item("b"), item("c")];
        let mut seen = HashSet::new();
        seen.insert(dedup_id(&item("b")));
        let p = partition_fresh(&items, &seen);
        assert_eq!(p.fresh, vec![item("a"), item("c")]);
        assert_eq!(p.duplicates, vec![item("b")]);
    }

    #[test]
    fn intra_batch_repeat_is_a_duplicate() {
        let items = vec![item("a"), item("a"), item("b"), item("a")];
        let p = partition_fresh(&items, &HashSet::new());
        // First "a" is fresh; the two later "a"s are dupes.
        assert_eq!(p.fresh, vec![item("a"), item("b")]);
        assert_eq!(p.duplicates, vec![item("a"), item("a")]);
    }

    #[test]
    fn re_partition_after_recording_is_a_no_op() {
        let items = vec![item("a"), item("b")];
        let mut set = SeenSet::new();
        let first = set.partition(&items);
        assert_eq!(first.fresh.len(), 2);
        let inserted = set.record_all(first.fresh.iter());
        assert_eq!(inserted, 2);
        // Second poll of the identical feed: nothing fresh.
        let second = set.partition(&items);
        assert!(second.fresh.is_empty());
        assert_eq!(second.duplicates.len(), 2);
    }

    #[test]
    fn seen_set_record_and_contains() {
        let mut set = SeenSet::new();
        assert!(set.is_empty());
        let it = item("x");
        assert!(set.record(&it), "first record is new");
        assert!(!set.record(&it), "second record is a dupe");
        assert!(set.contains_item(&it));
        assert!(set.contains(&dedup_id(&it)));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn from_ids_seeds_the_set() {
        let it = item("seed");
        let set = SeenSet::from_ids([dedup_id(&it)]);
        assert!(set.contains_item(&it));
        let p = set.partition(&[it.clone(), item("new")]);
        assert_eq!(p.fresh, vec![item("new")]);
        assert_eq!(p.duplicates, vec![it]);
    }

    #[test]
    fn empty_batch_is_total() {
        let p = partition_fresh(&[], &HashSet::new());
        assert!(p.fresh.is_empty() && p.duplicates.is_empty());
        assert!(SeenSet::new().partition(&[]).fresh.is_empty());
    }
}
