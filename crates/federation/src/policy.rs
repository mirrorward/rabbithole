//! Ingest-defense policy primitives — pure logic, no I/O.
//!
//! Two independent gates a federation ingest path composes:
//!
//! - [`RateLimiter`]: a per-peer token bucket. Each peer key gets its own
//!   bucket that refills continuously at a fixed rate up to a cap; a request
//!   is admitted only if a token is available. This bounds how fast any one
//!   peer can push work at us, without a background timer — refill is
//!   computed lazily from the caller-supplied clock.
//! - [`PeerPolicy`]: a static allow/deny set over peer keys, for operator
//!   admission control and defederation.
//!
//! Both are clock- and store-free: callers pass `now_ms`, and hold the
//! structures wherever they like. Nothing here does I/O, so it is trivially
//! testable and wasm-friendly.

use std::collections::{HashMap, HashSet};

/// Per-peer token-bucket rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    /// Maximum tokens a bucket can hold (burst size).
    capacity: f64,
    /// Tokens added per millisecond.
    refill_per_ms: f64,
    /// Per-peer bucket state.
    buckets: HashMap<[u8; 32], Bucket>,
    max_peers: usize,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_ms: i64,
}

impl RateLimiter {
    /// Create a limiter where each peer may burst up to `capacity` requests
    /// and sustain `refill_per_sec` requests per second thereafter.
    ///
    /// `capacity` is clamped to at least 1 and `refill_per_sec` to at least
    /// 0 (a zero refill yields a pure one-time burst allowance).
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        RateLimiter {
            capacity: capacity.max(1) as f64,
            refill_per_ms: finite_rate(refill_per_sec) / 1000.0,
            buckets: HashMap::new(),
            max_peers: usize::MAX,
        }
    }

    /// Attempt to spend one token for `peer` at time `now_ms`. Returns `true`
    /// if admitted (a token was available), `false` if the peer is currently
    /// rate-limited. New peers start with a full bucket.
    pub fn try_acquire(&mut self, peer: &[u8; 32], now_ms: i64) -> bool {
        self.try_acquire_many(peer, 1, now_ms)
    }

    /// Bound state without evicting a peer that still carries debt. A full
    /// table refuses new identities until an existing bucket refills.
    pub fn bounded(capacity: u32, refill_per_sec: f64, max_peers: usize) -> Self {
        let mut limiter = Self::new(capacity, refill_per_sec);
        limiter.capacity = f64::from(capacity);
        limiter.max_peers = max_peers;
        limiter
    }

    /// Apply a live policy at `now_ms`. Elapsed time uses the OLD rate;
    /// increasing a capacity never grants a fresh burst. Zero capacity denies.
    pub fn update_policy(&mut self, capacity: u32, refill_per_sec: f64, now_ms: i64) {
        for bucket in self.buckets.values_mut() {
            refill(bucket, self.capacity, self.refill_per_ms, now_ms);
            bucket.tokens = bucket.tokens.min(f64::from(capacity));
        }
        self.capacity = f64::from(capacity);
        self.refill_per_ms = finite_rate(refill_per_sec) / 1000.0;
    }

    /// Spend a bounded weighted cost (bytes/events). Insufficient funds do
    /// not partially spend the bucket. Caller must authenticate/admit peer
    /// identities before calling this method.
    pub fn try_acquire_many(&mut self, peer: &[u8; 32], cost: u32, now_ms: i64) -> bool {
        if cost == 0 {
            return true;
        }
        if f64::from(cost) > self.capacity {
            return false;
        }
        if !self.buckets.contains_key(peer) && self.buckets.len() >= self.max_peers {
            let capacity = self.capacity;
            let refill_per_ms = self.refill_per_ms;
            self.buckets.retain(|_, bucket| {
                refill(bucket, capacity, refill_per_ms, now_ms);
                bucket.tokens < capacity
            });
            if self.buckets.len() >= self.max_peers {
                return false;
            }
        }
        let bucket = self.buckets.entry(*peer).or_insert(Bucket {
            tokens: self.capacity,
            last_ms: now_ms,
        });
        refill(bucket, self.capacity, self.refill_per_ms, now_ms);
        if bucket.tokens >= f64::from(cost) {
            bucket.tokens -= f64::from(cost);
            true
        } else {
            false
        }
    }

    /// Forget a peer's bucket (e.g. on defederation), reclaiming its state.
    pub fn forget(&mut self, peer: &[u8; 32]) {
        self.buckets.remove(peer);
    }

    /// Number of peers currently tracked.
    pub fn tracked_peers(&self) -> usize {
        self.buckets.len()
    }
}

fn finite_rate(rate: f64) -> f64 {
    if rate.is_finite() {
        rate.max(0.0)
    } else {
        0.0
    }
}

fn refill(bucket: &mut Bucket, capacity: f64, rate: f64, now_ms: i64) {
    let elapsed = now_ms.saturating_sub(bucket.last_ms).max(0) as f64;
    bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
    // A backward clock must not count the same interval twice later.
    bucket.last_ms = bucket.last_ms.max(now_ms);
}

/// How a [`PeerPolicy`]'s entry set is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    /// Only listed peers are permitted (default-deny).
    AllowList,
    /// All peers are permitted except those listed (default-allow).
    DenyList,
}

/// Static admission control over peer keys.
#[derive(Debug, Clone)]
pub struct PeerPolicy {
    /// Whether `entries` is an allow-list or a deny-list.
    pub mode: PolicyMode,
    /// The set of peer keys the mode applies to.
    pub entries: HashSet<[u8; 32]>,
}

impl PeerPolicy {
    /// An allow-list seeded with `peers` (only these are permitted).
    pub fn allow(peers: impl IntoIterator<Item = [u8; 32]>) -> Self {
        PeerPolicy {
            mode: PolicyMode::AllowList,
            entries: peers.into_iter().collect(),
        }
    }

    /// A deny-list seeded with `peers` (all but these are permitted).
    pub fn deny(peers: impl IntoIterator<Item = [u8; 32]>) -> Self {
        PeerPolicy {
            mode: PolicyMode::DenyList,
            entries: peers.into_iter().collect(),
        }
    }

    /// Whether `peer` is permitted under this policy.
    pub fn permits(&self, peer: &[u8; 32]) -> bool {
        match self.mode {
            PolicyMode::AllowList => self.entries.contains(peer),
            PolicyMode::DenyList => !self.entries.contains(peer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_admits_burst_then_limits() {
        let mut rl = RateLimiter::new(2, 1.0); // burst 2, 1/sec
        let peer = [1u8; 32];
        assert!(rl.try_acquire(&peer, 0)); // full: 2 -> 1
        assert!(rl.try_acquire(&peer, 0)); // 1 -> 0
        assert!(!rl.try_acquire(&peer, 0), "burst exhausted");
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut rl = RateLimiter::new(2, 1.0);
        let peer = [1u8; 32];
        assert!(rl.try_acquire(&peer, 0));
        assert!(rl.try_acquire(&peer, 0));
        assert!(!rl.try_acquire(&peer, 0));
        // One second later: exactly one token refilled.
        assert!(rl.try_acquire(&peer, 1000));
        assert!(!rl.try_acquire(&peer, 1000), "only one refilled");
        // Long idle refills to the cap, not beyond.
        assert!(rl.try_acquire(&peer, 100_000));
        assert!(rl.try_acquire(&peer, 100_000));
        assert!(!rl.try_acquire(&peer, 100_000), "capped at capacity");
    }

    #[test]
    fn buckets_are_per_peer() {
        let mut rl = RateLimiter::new(1, 0.0);
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert!(rl.try_acquire(&a, 0));
        assert!(!rl.try_acquire(&a, 0));
        // b has its own full bucket.
        assert!(rl.try_acquire(&b, 0));
        assert_eq!(rl.tracked_peers(), 2);
        rl.forget(&a);
        assert_eq!(rl.tracked_peers(), 1);
    }

    #[test]
    fn clock_going_backwards_does_not_add_tokens() {
        let mut rl = RateLimiter::new(1, 1.0);
        let peer = [1u8; 32];
        assert!(rl.try_acquire(&peer, 1000));
        // Earlier timestamp must not refill (elapsed clamped to 0).
        assert!(!rl.try_acquire(&peer, 0));
    }

    #[test]
    fn allow_list_only_permits_members() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let policy = PeerPolicy::allow([a]);
        assert!(policy.permits(&a));
        assert!(!policy.permits(&b));
    }

    #[test]
    fn deny_list_permits_all_but_members() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let policy = PeerPolicy::deny([a]);
        assert!(!policy.permits(&a));
        assert!(policy.permits(&b));
    }

    #[test]
    fn empty_allow_list_denies_everyone() {
        let policy = PeerPolicy::allow([]);
        assert!(!policy.permits(&[9u8; 32]));
    }
}
