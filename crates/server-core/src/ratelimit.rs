//! Token-bucket rate limiting (Wave 13): one shared, thread-safe limiter
//! covering every surface the burrow exposes.
//!
//! A [`RateLimiter`] holds lazily-created buckets keyed by
//! [`LimitKey`] — a scope (client IP or account id) plus an endpoint
//! *class* (one of [`class`]'s constants). Each class carries a
//! [`Policy`]: the bucket capacity (burst) and a steady refill rate.
//! [`RateLimiter::check`]/[`RateLimiter::check_with`] consume one token and
//! answer with a [`Decision`]; [`RateLimiter::peek_with`] probes without
//! consuming (used to gate expensive attempts, e.g. logins, whose *failures*
//! are what drain the bucket).
//!
//! Design points:
//!
//! - **Injectable clock**: every call takes `now_ms` (monotonic
//!   milliseconds); production callers pass [`now_ms`], tests pass whatever
//!   they like.
//! - **Zero capacity = always limited** (the operator said "none, ever").
//!   By contrast a *rate knob of 0 in config disables the class entirely* —
//!   [`Policy::for_class`] returns `None` and no check happens.
//! - **Sparse audit**: a refusal sets `Decision::Limited { audit: true }` at
//!   most once per key per [`NOTE_INTERVAL_MS`] (one minute), so callers can
//!   audit-log refusals without a flood writing a flood of audit rows.
//! - **Expiry sweep**: buckets that have refilled to full (indistinguishable
//!   from fresh) and whose audit note is stale are dropped, opportunistically
//!   every [`SWEEP_EVERY`] checks or explicitly via [`RateLimiter::sweep`].
//! - **Never panics**: saturating/clamped arithmetic throughout (`f64 as u64`
//!   casts saturate; `u64` deltas use `saturating_sub`).

use std::collections::HashMap;
use std::net::IpAddr;

use parking_lot::Mutex;

use crate::config::ServerConfig;

/// The fixed set of endpoint classes. Each maps to a pair of config knobs
/// (`ratelimit_<class>_per_{sec,min}` + `ratelimit_<class>_burst`).
pub mod class {
    /// New connections per client IP (enforced at the accept loops).
    pub const CONN: &str = "conn";
    /// Failed login attempts per client IP (native + every legacy surface).
    pub const AUTH: &str = "auth";
    /// Chat lines / DM sends per account.
    pub const MSG: &str = "msg";
    /// Board posts per account (native, NNTP `POST`, Hotline news).
    pub const POST: &str = "post";
    /// File-transfer opens per account.
    pub const TRANSFER: &str = "transfer";
    /// Coarse per-command budget per client IP on the legacy surfaces
    /// (telnet, Hotline, NNTP reader + feed).
    pub const LEGACY: &str = "legacy";
}

/// Refusals are flagged for audit at most this often per key (1 minute).
pub const NOTE_INTERVAL_MS: u64 = 60_000;

/// Opportunistic sweep cadence: every N checks the bucket map is pruned.
const SWEEP_EVERY: u64 = 1024;

/// Who a bucket belongs to: a client address or an authenticated account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Ip(IpAddr),
    Account(i64),
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::Ip(ip) => write!(f, "ip={ip}"),
            Scope::Account(id) => write!(f, "account={id}"),
        }
    }
}

/// A bucket key: one scope in one endpoint class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LimitKey {
    pub scope: Scope,
    pub class: &'static str,
}

/// A class policy: burst capacity + steady refill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// Bucket size (tokens available in a burst). `0` = refuse everything.
    pub capacity: u32,
    /// Steady-state tokens regained per second.
    pub refill_per_sec: f64,
}

impl Policy {
    /// Resolve the live policy for `class` from config. `None` means the
    /// class is **disabled** (its rate knob is 0, or the class is unknown)
    /// and no check should be made.
    pub fn for_class(cfg: &ServerConfig, class: &str) -> Option<Policy> {
        let (rate, per_secs, burst) = match class {
            class::CONN => (cfg.ratelimit_conn_per_min, 60.0, cfg.ratelimit_conn_burst),
            class::AUTH => (cfg.ratelimit_auth_per_min, 60.0, cfg.ratelimit_auth_burst),
            class::MSG => (cfg.ratelimit_msg_per_sec, 1.0, cfg.ratelimit_msg_burst),
            class::POST => (cfg.ratelimit_post_per_min, 60.0, cfg.ratelimit_post_burst),
            class::TRANSFER => (
                cfg.ratelimit_transfer_per_min,
                60.0,
                cfg.ratelimit_transfer_burst,
            ),
            class::LEGACY => (
                cfg.ratelimit_legacy_per_sec,
                1.0,
                cfg.ratelimit_legacy_burst,
            ),
            _ => return None,
        };
        if rate == 0 {
            return None; // 0 disables the class
        }
        Some(Policy {
            capacity: burst,
            refill_per_sec: f64::from(rate) / per_secs,
        })
    }
}

/// The answer to one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    Limited {
        /// Milliseconds until one token will be available (`u64::MAX` when
        /// the bucket never refills, i.e. zero capacity or zero refill).
        retry_after_ms: u64,
        /// `true` at most once per key per [`NOTE_INTERVAL_MS`]: the caller
        /// should audit-log this refusal. Keeps a flood from flooding the
        /// audit log too.
        audit: bool,
    },
}

impl Decision {
    pub fn is_limited(&self) -> bool {
        matches!(self, Decision::Limited { .. })
    }
}

/// Monotonic milliseconds since the first call (the production clock).
pub fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    /// Snapshot of the policy last applied (live overrides update it), so
    /// the sweep can tell when a bucket has refilled to full.
    capacity: f64,
    refill_per_sec: f64,
    updated_ms: u64,
    /// When this key last flagged a refusal for audit (`None` = never).
    noted_ms: Option<u64>,
}

impl Bucket {
    fn fresh(policy: Policy, now_ms: u64) -> Bucket {
        Bucket {
            tokens: f64::from(policy.capacity),
            capacity: f64::from(policy.capacity),
            refill_per_sec: policy.refill_per_sec,
            updated_ms: now_ms,
            noted_ms: None,
        }
    }

    /// Advance the bucket to `now`, applying (and snapshotting) `policy`.
    fn refill(&mut self, policy: Policy, now_ms: u64) {
        let cap = f64::from(policy.capacity);
        let dt = now_ms.saturating_sub(self.updated_ms);
        if policy.refill_per_sec > 0.0 && dt > 0 {
            self.tokens += (dt as f64 / 1000.0) * policy.refill_per_sec;
        }
        self.tokens = self.tokens.min(cap).max(0.0);
        self.capacity = cap;
        self.refill_per_sec = policy.refill_per_sec;
        self.updated_ms = now_ms;
    }

    /// Flag a refusal for audit at most once per [`NOTE_INTERVAL_MS`].
    fn note(&mut self, now_ms: u64) -> bool {
        let stale = self
            .noted_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= NOTE_INTERVAL_MS);
        if stale {
            self.noted_ms = Some(now_ms);
        }
        stale
    }

    /// Whether the bucket carries no state worth keeping at `now`: refilled
    /// to full (indistinguishable from a fresh one) and its audit note is
    /// stale.
    fn expired(&self, now_ms: u64) -> bool {
        let dt = now_ms.saturating_sub(self.updated_ms) as f64 / 1000.0;
        let full = self.tokens + dt * self.refill_per_sec >= self.capacity;
        let note_stale = self
            .noted_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= NOTE_INTERVAL_MS);
        full && note_stale
    }
}

#[derive(Default)]
struct Inner {
    policies: HashMap<&'static str, Policy>,
    buckets: HashMap<LimitKey, Bucket>,
    checks: u64,
}

/// The shared token-bucket limiter. Cheap to check (one mutex, one hash
/// lookup); buckets are created lazily and swept when idle.
#[derive(Default)]
pub struct RateLimiter {
    inner: Mutex<Inner>,
}

impl RateLimiter {
    pub fn new() -> RateLimiter {
        RateLimiter::default()
    }

    /// Install (or replace) the policy `check` uses for `class`.
    pub fn set_policy(&self, class: &'static str, policy: Policy) {
        self.inner.lock().policies.insert(class, policy);
    }

    /// Consume one token for `key` using the installed policy for its class.
    /// A class with no installed policy is unrestricted.
    pub fn check(&self, key: LimitKey, now_ms: u64) -> Decision {
        let policy = self.inner.lock().policies.get(key.class).copied();
        match policy {
            Some(p) => self.check_with(key, p, now_ms),
            None => Decision::Allowed,
        }
    }

    /// Consume one token for `key` under an explicitly supplied policy
    /// (callers with live-reloadable config resolve the policy themselves).
    pub fn check_with(&self, key: LimitKey, policy: Policy, now_ms: u64) -> Decision {
        self.decide(key, policy, now_ms, true)
    }

    /// Probe without consuming: would a request on `key` be allowed now?
    pub fn peek_with(&self, key: LimitKey, policy: Policy, now_ms: u64) -> Decision {
        self.decide(key, policy, now_ms, false)
    }

    fn decide(&self, key: LimitKey, policy: Policy, now_ms: u64, consume: bool) -> Decision {
        let mut inner = self.inner.lock();
        inner.checks = inner.checks.wrapping_add(1);
        if inner.checks % SWEEP_EVERY == 0 {
            inner.buckets.retain(|_, b| !b.expired(now_ms));
        }
        if policy.capacity == 0 {
            // Always limited; the bucket exists only to pace audit notes.
            let b = inner
                .buckets
                .entry(key)
                .or_insert_with(|| Bucket::fresh(policy, now_ms));
            b.refill(policy, now_ms);
            return Decision::Limited {
                retry_after_ms: u64::MAX,
                audit: b.note(now_ms),
            };
        }
        if !consume && !inner.buckets.contains_key(&key) {
            return Decision::Allowed; // fresh bucket is full; nothing to record
        }
        let b = inner
            .buckets
            .entry(key)
            .or_insert_with(|| Bucket::fresh(policy, now_ms));
        b.refill(policy, now_ms);
        if b.tokens >= 1.0 {
            if consume {
                b.tokens -= 1.0;
            }
            Decision::Allowed
        } else {
            let retry_after_ms = if policy.refill_per_sec > 0.0 {
                ((1.0 - b.tokens) * 1000.0 / policy.refill_per_sec).ceil() as u64
            } else {
                u64::MAX
            };
            Decision::Limited {
                retry_after_ms,
                audit: b.note(now_ms),
            }
        }
    }

    /// Drop every bucket that is refilled-to-full with a stale audit note
    /// (also runs opportunistically every [`SWEEP_EVERY`] checks).
    pub fn sweep(&self, now_ms: u64) {
        self.inner.lock().buckets.retain(|_, b| !b.expired(now_ms));
    }

    /// Live buckets (test/introspection hook).
    pub fn bucket_count(&self) -> usize {
        self.inner.lock().buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip_key(last_octet: u8, class: &'static str) -> LimitKey {
        LimitKey {
            scope: Scope::Ip(IpAddr::from([127, 0, 0, last_octet])),
            class,
        }
    }

    #[test]
    fn burst_then_drain_then_refill() {
        let rl = RateLimiter::new();
        let p = Policy {
            capacity: 3,
            refill_per_sec: 1.0,
        };
        let k = ip_key(1, class::MSG);

        // The full burst is allowed…
        for _ in 0..3 {
            assert_eq!(rl.check_with(k, p, 0), Decision::Allowed);
        }
        // …then the bucket is empty and reports when to retry.
        match rl.check_with(k, p, 0) {
            Decision::Limited { retry_after_ms, .. } => assert_eq!(retry_after_ms, 1000),
            d => panic!("expected limited, got {d:?}"),
        }
        // One second later exactly one token has refilled.
        assert_eq!(rl.check_with(k, p, 1000), Decision::Allowed);
        assert!(rl.check_with(k, p, 1000).is_limited());
        // A long idle period refills to capacity, not beyond.
        for _ in 0..3 {
            assert_eq!(rl.check_with(k, p, 60_000), Decision::Allowed);
        }
        assert!(rl.check_with(k, p, 60_000).is_limited());
    }

    #[test]
    fn per_key_isolation() {
        let rl = RateLimiter::new();
        let p = Policy {
            capacity: 1,
            refill_per_sec: 0.1,
        };
        // Draining one IP leaves another untouched…
        assert_eq!(
            rl.check_with(ip_key(1, class::AUTH), p, 0),
            Decision::Allowed
        );
        assert!(rl.check_with(ip_key(1, class::AUTH), p, 0).is_limited());
        assert_eq!(
            rl.check_with(ip_key(2, class::AUTH), p, 0),
            Decision::Allowed
        );
        // …and the same scope in a different class is a different bucket.
        assert_eq!(
            rl.check_with(ip_key(1, class::MSG), p, 0),
            Decision::Allowed
        );
        // Accounts are scoped separately from IPs entirely.
        let acct = LimitKey {
            scope: Scope::Account(7),
            class: class::AUTH,
        };
        assert_eq!(rl.check_with(acct, p, 0), Decision::Allowed);
    }

    #[test]
    fn zero_capacity_always_limited() {
        let rl = RateLimiter::new();
        let p = Policy {
            capacity: 0,
            refill_per_sec: 100.0,
        };
        let k = ip_key(1, class::TRANSFER);
        for now in [0u64, 1000, 1_000_000] {
            match rl.check_with(k, p, now) {
                Decision::Limited { retry_after_ms, .. } => assert_eq!(retry_after_ms, u64::MAX),
                d => panic!("zero capacity must limit, got {d:?}"),
            }
        }
    }

    #[test]
    fn peek_does_not_consume() {
        let rl = RateLimiter::new();
        let p = Policy {
            capacity: 1,
            refill_per_sec: 0.001,
        };
        let k = ip_key(1, class::AUTH);
        // Probing repeatedly never drains the bucket…
        for _ in 0..5 {
            assert_eq!(rl.peek_with(k, p, 0), Decision::Allowed);
        }
        // …one real check does, and the probe then reports limited.
        assert_eq!(rl.check_with(k, p, 0), Decision::Allowed);
        assert!(rl.peek_with(k, p, 0).is_limited());
    }

    #[test]
    fn sweep_drops_idle_buckets_only() {
        let rl = RateLimiter::new();
        let p = Policy {
            capacity: 2,
            refill_per_sec: 1.0,
        };
        rl.check_with(ip_key(1, class::LEGACY), p, 0);
        rl.check_with(ip_key(2, class::LEGACY), p, 0);
        assert_eq!(rl.bucket_count(), 2);
        // Still draining: nothing is full yet, so nothing is dropped.
        rl.sweep(500);
        assert_eq!(rl.bucket_count(), 2);
        // After both refill to capacity they carry no state: dropped.
        rl.sweep(5_000);
        assert_eq!(rl.bucket_count(), 0);
        // Dropping the bucket did not grant extra burst.
        assert_eq!(
            rl.check_with(ip_key(1, class::LEGACY), p, 5_000),
            Decision::Allowed
        );
    }

    #[test]
    fn audit_flag_at_most_once_per_minute_per_key() {
        let rl = RateLimiter::new();
        let p = Policy {
            capacity: 0,
            refill_per_sec: 0.0,
        };
        let k = ip_key(1, class::AUTH);
        let audit_at = |now| match rl.check_with(k, p, now) {
            Decision::Limited { audit, .. } => audit,
            d => panic!("expected limited, got {d:?}"),
        };
        assert!(audit_at(0), "first refusal is audited");
        assert!(!audit_at(1), "immediate repeat is not");
        assert!(!audit_at(NOTE_INTERVAL_MS - 1));
        assert!(audit_at(NOTE_INTERVAL_MS), "audited again after a minute");
        // A different key audits independently.
        match rl.check_with(ip_key(2, class::AUTH), p, 0) {
            Decision::Limited { audit, .. } => assert!(audit),
            d => panic!("expected limited, got {d:?}"),
        }
    }

    #[test]
    fn policy_table_and_unknown_class() {
        let rl = RateLimiter::new();
        let k = ip_key(1, class::CONN);
        // No installed policy: unrestricted.
        assert_eq!(rl.check(k, 0), Decision::Allowed);
        rl.set_policy(
            class::CONN,
            Policy {
                capacity: 1,
                refill_per_sec: 0.5,
            },
        );
        assert_eq!(rl.check(k, 0), Decision::Allowed);
        assert!(rl.check(k, 0).is_limited());
    }

    #[test]
    fn policy_for_class_maps_config_knobs() {
        let cfg = ServerConfig::default();
        let auth = Policy::for_class(&cfg, class::AUTH).expect("auth enabled by default");
        assert_eq!(auth.capacity, 5);
        assert!((auth.refill_per_sec - 5.0 / 60.0).abs() < 1e-9);
        let msg = Policy::for_class(&cfg, class::MSG).expect("msg enabled by default");
        assert_eq!(msg.capacity, 20);
        assert!((msg.refill_per_sec - 10.0).abs() < 1e-9);

        // A rate knob of 0 disables the class outright.
        let cfg = ServerConfig {
            ratelimit_post_per_min: 0,
            ..ServerConfig::default()
        };
        assert!(Policy::for_class(&cfg, class::POST).is_none());
        assert!(Policy::for_class(&cfg, "no-such-class").is_none());
    }
}
