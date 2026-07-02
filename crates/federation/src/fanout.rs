//! Pull fan-out planning.
//!
//! Once files are deduped across servers ([`crate::dedupe`]), a client that
//! wants one still has to decide *which* of its N sources to pull from, and in
//! what order. [`plan_fetch`] turns a single [`DedupedMatch`] into an ordered
//! [`FetchPlan`] for the transfer layer to consume; [`plan_fetch_batch`] plans
//! many at once while round-robining the *primary* source across servers so a
//! multi-file grab spreads its initial load instead of hammering one peer.
//!
//! Everything here is pure ordering over the in-memory deduped view — no
//! sockets, no fetching. The transfer layer walks the ordered sources: use the
//! first, fall back to the next on failure, or stripe chunks across all of
//! them swarm-style.
//!
//! ## Ordering strategy
//!
//! - [`FetchStrategy::FreshestFirst`] (default) orders sources by catalog
//!   generation, newest first. The server whose catalog is freshest is the one
//!   least likely to have dropped the file since it advertised it — the
//!   closest thing to "fewest hops to a live copy" this sans-store layer can
//!   know. Ties break by `server_key` for determinism.
//! - [`FetchStrategy::StableByKey`] ignores freshness and orders purely by
//!   `server_key`, giving a load-neutral, fully reproducible order.

use serde::{Deserialize, Serialize};

use crate::dedupe::{DedupedMatch, ServerRef};

/// How [`plan_fetch`] orders a match's sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FetchStrategy {
    /// Newest catalog generation first, ties broken by `server_key`.
    #[default]
    FreshestFirst,
    /// Deterministic order by `server_key` only.
    StableByKey,
}

/// Tuning for the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FetchPolicy {
    /// Source ordering strategy.
    pub strategy: FetchStrategy,
    /// Cap on how many sources to include in a plan. `0` means "all".
    pub max_sources: usize,
}

impl FetchPolicy {
    /// A policy with the given strategy and no source cap.
    pub fn new(strategy: FetchStrategy) -> Self {
        Self {
            strategy,
            max_sources: 0,
        }
    }

    /// Builder: cap the number of sources per plan (`0` = uncapped).
    pub fn with_max_sources(mut self, max: usize) -> Self {
        self.max_sources = max;
        self
    }
}

/// An ordered set of sources to fetch one file from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchPlan {
    /// blake3 content hash of the file being fetched.
    pub hash: [u8; 32],
    /// File size in bytes.
    pub size: u64,
    /// Sources in the order the transfer layer should prefer them. The first
    /// is the primary; the rest are fallbacks / swarm peers.
    pub sources: Vec<ServerRef>,
}

/// Plan a fetch for one deduped match: order its sources by `policy.strategy`
/// and cap to `policy.max_sources`.
pub fn plan_fetch(deduped: &DedupedMatch, policy: &FetchPolicy) -> FetchPlan {
    let mut sources = deduped.sources.clone();
    order_sources(&mut sources, policy.strategy);
    if policy.max_sources != 0 {
        sources.truncate(policy.max_sources);
    }
    FetchPlan {
        hash: deduped.hash,
        size: deduped.size,
        sources,
    }
}

/// Plan fetches for many deduped matches, round-robining which source is made
/// primary so a batch grab spreads its first connections across servers.
///
/// Each match's sources are first ordered by `policy.strategy`; then the
/// ordered list for the *i*-th match is rotated left by `i` (modulo its source
/// count) so consecutive files start on different servers. The `max_sources`
/// cap is applied after rotation.
pub fn plan_fetch_batch(deduped: &[DedupedMatch], policy: &FetchPolicy) -> Vec<FetchPlan> {
    deduped
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let mut sources = d.sources.clone();
            order_sources(&mut sources, policy.strategy);
            let len = sources.len();
            if len != 0 {
                sources.rotate_left(i % len);
            }
            if policy.max_sources != 0 {
                sources.truncate(policy.max_sources);
            }
            FetchPlan {
                hash: d.hash,
                size: d.size,
                sources,
            }
        })
        .collect()
}

/// Order `sources` in place according to `strategy`.
fn order_sources(sources: &mut [ServerRef], strategy: FetchStrategy) {
    match strategy {
        FetchStrategy::FreshestFirst => sources.sort_by(|a, b| {
            b.generation
                .cmp(&a.generation)
                .then(a.server_key.cmp(&b.server_key))
        }),
        FetchStrategy::StableByKey => sources.sort_by_key(|a| a.server_key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(server: u8, generation: u64) -> ServerRef {
        ServerRef {
            server_key: [server; 32],
            generation,
            area: "warez".into(),
            path: String::new(),
            name: "f.zip".into(),
        }
    }

    fn deduped(hash: u8, sources: Vec<ServerRef>) -> DedupedMatch {
        DedupedMatch {
            hash: [hash; 32],
            size: 100,
            sources,
        }
    }

    #[test]
    fn freshest_first_orders_by_generation_desc() {
        let d = deduped(1, vec![src(1, 3), src(2, 9), src(3, 5)]);
        let plan = plan_fetch(&d, &FetchPolicy::default());
        let gens: Vec<u64> = plan.sources.iter().map(|s| s.generation).collect();
        assert_eq!(gens, vec![9, 5, 3]);
        assert_eq!(plan.hash, [1u8; 32]);
        assert_eq!(plan.size, 100);
    }

    #[test]
    fn freshest_first_breaks_ties_by_key() {
        let d = deduped(1, vec![src(3, 5), src(1, 5), src(2, 5)]);
        let plan = plan_fetch(&d, &FetchPolicy::default());
        let keys: Vec<u8> = plan.sources.iter().map(|s| s.server_key[0]).collect();
        assert_eq!(keys, vec![1, 2, 3]);
    }

    #[test]
    fn stable_by_key_ignores_generation() {
        let d = deduped(1, vec![src(3, 9), src(1, 1), src(2, 5)]);
        let plan = plan_fetch(&d, &FetchPolicy::new(FetchStrategy::StableByKey));
        let keys: Vec<u8> = plan.sources.iter().map(|s| s.server_key[0]).collect();
        assert_eq!(keys, vec![1, 2, 3]);
    }

    #[test]
    fn max_sources_caps_the_plan() {
        let d = deduped(1, vec![src(1, 3), src(2, 9), src(3, 5)]);
        let plan = plan_fetch(&d, &FetchPolicy::default().with_max_sources(2));
        assert_eq!(plan.sources.len(), 2);
        // Still the two freshest, in order.
        let gens: Vec<u64> = plan.sources.iter().map(|s| s.generation).collect();
        assert_eq!(gens, vec![9, 5]);
    }

    #[test]
    fn batch_round_robins_the_primary_source() {
        // Three files, each on the same three servers with identical freshness
        // so the base order is [1, 2, 3]. Rotation makes each file start on a
        // different server.
        let base = || vec![src(1, 1), src(2, 1), src(3, 1)];
        let matches = vec![deduped(1, base()), deduped(2, base()), deduped(3, base())];
        let plans = plan_fetch_batch(&matches, &FetchPolicy::default());
        let primaries: Vec<u8> = plans.iter().map(|p| p.sources[0].server_key[0]).collect();
        assert_eq!(primaries, vec![1, 2, 3]);
        // Rotation preserves the full fallback set.
        assert_eq!(plans[1].sources.len(), 3);
        let second: Vec<u8> = plans[1].sources.iter().map(|s| s.server_key[0]).collect();
        assert_eq!(second, vec![2, 3, 1]);
    }

    #[test]
    fn planning_a_sourceless_match_is_empty_not_a_panic() {
        let plan = plan_fetch(&deduped(1, vec![]), &FetchPolicy::default());
        assert!(plan.sources.is_empty());
        let batch = plan_fetch_batch(&[deduped(1, vec![])], &FetchPolicy::default());
        assert!(batch[0].sources.is_empty());
    }
}
