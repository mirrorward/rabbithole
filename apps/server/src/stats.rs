//! Wave 10 — live gateway/feed activity counters.
//!
//! A single [`GatewayStats`] lives on [`crate::Shared`]. The syndication
//! fetcher records per-feed poll outcomes into it; each legacy surface bumps
//! a named counter at its existing success/failure points (a cheap map
//! insert under a short-held lock — these events fire at human, not packet,
//! rate). The admin family exposes a snapshot ([`snapshot`]) over the wire
//! (`GatewayStatsRequest` → `GatewayStatsReply`) and `burrow ctl
//! gateway-stats`, filling the "live stats land with a future server slice"
//! seam the SPA syndication panel documented.
//!
//! Counters are **in-memory** — they reset on restart, like presence and the
//! rooms. They are activity meters, not durable accounting.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use rabbithole_proto::admin as padm;

/// Per-feed outcome the fetcher already computes, kept for the monitor.
#[derive(Debug, Clone, Default)]
struct FeedStat {
    last_poll_ms: i64,
    last_status: String,
    items_seen: u64,
    items_posted: u64,
    dupes_dropped: u64,
}

/// Live activity counters for the syndication fetcher and every legacy
/// gateway. Cheap, lock-guarded, in-memory.
#[derive(Default)]
pub struct GatewayStats {
    /// `"gateway.counter"` → value (e.g. `"nntp.sessions"`).
    counters: Mutex<BTreeMap<String, u64>>,
    /// Feed URL → its latest poll stats.
    feeds: Mutex<BTreeMap<String, FeedStat>>,
}

impl GatewayStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment `"<gateway>.<counter>"` by one.
    pub fn incr(&self, gateway: &str, counter: &str) {
        self.add(gateway, counter, 1);
    }

    /// Add `n` to `"<gateway>.<counter>"`.
    pub fn add(&self, gateway: &str, counter: &str, n: u64) {
        if n == 0 {
            return;
        }
        let mut map = self.counters.lock();
        *map.entry(format!("{gateway}.{counter}")).or_insert(0) += n;
    }

    /// Set `"<gateway>.<counter>"` to `n` (for gauges like current listeners
    /// where "add" would be wrong).
    pub fn set(&self, gateway: &str, counter: &str, n: u64) {
        let mut map = self.counters.lock();
        map.insert(format!("{gateway}.{counter}"), n);
    }

    /// Record a feed poll outcome (status = `"ok"` | `"not_modified"` |
    /// `"error"`). Ingest deltas are added separately via [`feed_ingest`].
    pub fn feed_poll(&self, url: &str, now_ms: i64, status: &str) {
        let mut map = self.feeds.lock();
        let e = map.entry(url.to_string()).or_default();
        e.last_poll_ms = now_ms;
        e.last_status = status.to_string();
    }

    /// Add per-poll ingest deltas for a feed.
    pub fn feed_ingest(&self, url: &str, seen: u64, posted: u64, dupes: u64) {
        let mut map = self.feeds.lock();
        let e = map.entry(url.to_string()).or_default();
        e.items_seen += seen;
        e.items_posted += posted;
        e.dupes_dropped += dupes;
    }

    /// Ensure a feed row exists (so the monitor lists a configured-but-never-
    /// yet-polled feed) without disturbing existing stats.
    pub fn feed_register(&self, url: &str) {
        self.feeds.lock().entry(url.to_string()).or_default();
    }

    /// Build the wire snapshot. `enabled_gateways` names the surfaces that
    /// are on right now, so a gateway with zero counters still shows as
    /// enabled (and a disabled one with residual counters shows as off).
    pub fn snapshot(
        &self,
        generated_at_ms: i64,
        enabled: &[(&str, bool)],
    ) -> padm::GatewayStatsReply {
        let counters = self.counters.lock();
        let feeds = self.feeds.lock();

        let feed_rows = feeds
            .iter()
            .map(|(url, s)| padm::FeedStat {
                url: url.clone(),
                last_poll_ms: s.last_poll_ms,
                last_status: s.last_status.clone(),
                items_seen: s.items_seen,
                items_posted: s.items_posted,
                dupes_dropped: s.dupes_dropped,
            })
            .collect();

        // Group the flat "gateway.counter" map back into per-gateway rows.
        let mut by_gateway: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
        for (key, &val) in counters.iter() {
            if let Some((gw, ctr)) = key.split_once('.') {
                by_gateway
                    .entry(gw.to_string())
                    .or_default()
                    .push((ctr.to_string(), val));
            }
        }
        // Fold in enabled gateways that may have no counters yet, and record
        // the enabled flag.
        let enabled_map: BTreeMap<&str, bool> = enabled.iter().copied().collect();
        for (name, _) in enabled {
            by_gateway.entry((*name).to_string()).or_default();
        }
        let gateway_rows = by_gateway
            .into_iter()
            .map(|(name, counters)| padm::GatewayStat {
                enabled: enabled_map.get(name.as_str()).copied().unwrap_or(false),
                name,
                counters,
            })
            .collect();

        padm::GatewayStatsReply {
            generated_at_ms,
            feeds: feed_rows,
            gateways: gateway_rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_group() {
        let s = GatewayStats::new();
        s.incr("nntp", "sessions");
        s.incr("nntp", "sessions");
        s.add("nntp", "posts", 3);
        s.set("radio", "listeners", 5);
        s.add("radio", "listeners", 0); // no-op

        let snap = s.snapshot(1000, &[("nntp", true), ("hotline", true)]);
        assert_eq!(snap.generated_at_ms, 1000);

        let nntp = snap.gateways.iter().find(|g| g.name == "nntp").unwrap();
        assert!(nntp.enabled);
        assert_eq!(
            nntp.counters,
            vec![("posts".into(), 3), ("sessions".into(), 2)]
        );

        // hotline is enabled with no counters -> present, empty, enabled.
        let hl = snap.gateways.iter().find(|g| g.name == "hotline").unwrap();
        assert!(hl.enabled);
        assert!(hl.counters.is_empty());

        // radio has counters but wasn't in the enabled list -> off.
        let radio = snap.gateways.iter().find(|g| g.name == "radio").unwrap();
        assert!(!radio.enabled);
        assert_eq!(radio.counters, vec![("listeners".into(), 5)]);
    }

    #[test]
    fn feed_stats_track_poll_and_ingest() {
        let s = GatewayStats::new();
        s.feed_register("https://a.example/feed");
        s.feed_poll("https://a.example/feed", 42, "ok");
        s.feed_ingest("https://a.example/feed", 10, 7, 3);
        s.feed_ingest("https://a.example/feed", 5, 2, 1);
        s.feed_poll("https://a.example/feed", 99, "not_modified");

        let snap = s.snapshot(1, &[]);
        assert_eq!(snap.feeds.len(), 1);
        let f = &snap.feeds[0];
        assert_eq!(f.url, "https://a.example/feed");
        assert_eq!(f.last_poll_ms, 99);
        assert_eq!(f.last_status, "not_modified");
        assert_eq!(f.items_seen, 15);
        assert_eq!(f.items_posted, 9);
        assert_eq!(f.dupes_dropped, 4);
    }
}
