//! Fetch-scheduling state machine (conditional GET + backoff).
//!
//! A [`PollState`] models one feed's polling lifecycle: the HTTP validators
//! (`ETag` / `Last-Modified`) to replay on the next conditional request, a
//! consecutive-failure counter, and the unix time the feed is next due. It is
//! deliberately **pure and clockless** — every transition takes `now` as an
//! argument and returns the *next* state plus a [`PollDecision`], so a later
//! server slice can drive it with a real fetcher and a real clock while the
//! logic itself stays host-testable and deterministic.
//!
//! The three responses a poll can produce:
//! - **304 Not Modified** → [`PollDecision::NotModified`]: validators kept,
//!   failure count reset, reschedule at the base interval.
//! - **2xx with a body** → [`PollDecision::Modified`]: validators refreshed
//!   from the response, failure count reset, reschedule at the base interval —
//!   the caller parses and ingests the body.
//! - **anything else, or a transport error** → [`PollDecision::Failed`]:
//!   validators kept, failure count incremented, reschedule with exponential
//!   backoff.
//!
//! Scheduling honors a feed-declared minimum interval when one is present:
//! RSS `<ttl>` (minutes) or the `sy:updatePeriod`/`sy:updateFrequency` pair.
//! This crate does not parse those out of the document (that stays with the
//! parser/server wiring), but [`ttl_minutes_to_secs`] and
//! [`sy_update_period_secs`] turn the raw values into the `feed_ttl_secs`
//! argument these transitions accept.

/// Tuning for [`PollState`] scheduling. [`Default`] is a 1-hour base with a
/// 5-minute floor, a 1-day ceiling, and interval-doubling backoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollConfig {
    /// Nominal interval between successful polls, in seconds.
    pub base_interval_secs: i64,
    /// Never schedule sooner than this many seconds out (politeness floor).
    pub min_interval_secs: i64,
    /// Never schedule further than this many seconds out (backoff ceiling).
    pub max_interval_secs: i64,
    /// Backoff base raised to the failure count (e.g. `2` doubles each time).
    pub backoff_base: u32,
    /// When set, a feed-declared TTL raises the effective base interval.
    pub respect_feed_ttl: bool,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            base_interval_secs: 3_600,
            min_interval_secs: 300,
            max_interval_secs: 86_400,
            backoff_base: 2,
            respect_feed_ttl: true,
        }
    }
}

/// The outcome of feeding a response into [`PollState::on_response`] (or a
/// transport error into [`PollState::on_transport_error`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollDecision {
    /// 304: nothing changed since the last poll.
    NotModified,
    /// 2xx: a fresh body is available — parse and ingest it.
    Modified,
    /// Error / transport failure: back off and retry later.
    Failed,
}

/// One feed's polling state. Cheap to clone; equality is structural so tests
/// can assert exact transitions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollState {
    /// Last-seen `ETag`, to send as `If-None-Match`.
    pub etag: Option<String>,
    /// Last-seen `Last-Modified`, to send as `If-Modified-Since`.
    pub last_modified: Option<String>,
    /// Consecutive failures since the last success (drives backoff).
    pub failures: u32,
    /// Unix seconds at which this feed is next due to be polled.
    pub next_poll_at: i64,
}

impl PollState {
    /// A fresh state that is due immediately at `now` (no validators yet).
    pub fn initial(now: i64) -> Self {
        Self {
            next_poll_at: now,
            ..Self::default()
        }
    }

    /// Is the feed due to be polled at `now`?
    pub fn is_due(&self, now: i64) -> bool {
        now >= self.next_poll_at
    }

    /// The `If-None-Match` value to send, if any.
    pub fn if_none_match(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// The `If-Modified-Since` value to send, if any.
    pub fn if_modified_since(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// Apply an HTTP `status` (with any returned validators) observed at `now`,
    /// honoring an optional feed-declared TTL. Returns the next state and the
    /// [`PollDecision`] the caller should act on. Pure: no clock, no I/O.
    pub fn on_response(
        &self,
        cfg: &PollConfig,
        status: u16,
        etag: Option<&str>,
        last_modified: Option<&str>,
        feed_ttl_secs: Option<i64>,
        now: i64,
    ) -> (PollState, PollDecision) {
        if status == 304 {
            // Not modified: refresh validators if the server re-sent them,
            // otherwise keep what we had; success resets backoff.
            let next = PollState {
                etag: keep_or_update(&self.etag, etag),
                last_modified: keep_or_update(&self.last_modified, last_modified),
                failures: 0,
                next_poll_at: next_poll_at(cfg, 0, feed_ttl_secs, now),
            };
            return (next, PollDecision::NotModified);
        }
        if (200..300).contains(&status) {
            let next = PollState {
                etag: keep_or_update(&self.etag, etag),
                last_modified: keep_or_update(&self.last_modified, last_modified),
                failures: 0,
                next_poll_at: next_poll_at(cfg, 0, feed_ttl_secs, now),
            };
            return (next, PollDecision::Modified);
        }
        // Any other status (1xx, 3xx≠304, 4xx, 5xx) is a failure.
        self.failed(cfg, feed_ttl_secs, now)
    }

    /// A transport-level failure (connection refused, timeout, DNS…) with no
    /// HTTP status. Backs off exactly like an error status.
    pub fn on_transport_error(
        &self,
        cfg: &PollConfig,
        feed_ttl_secs: Option<i64>,
        now: i64,
    ) -> (PollState, PollDecision) {
        self.failed(cfg, feed_ttl_secs, now)
    }

    /// Shared failure transition: keep validators, bump the counter, back off.
    fn failed(
        &self,
        cfg: &PollConfig,
        feed_ttl_secs: Option<i64>,
        now: i64,
    ) -> (PollState, PollDecision) {
        let failures = self.failures.saturating_add(1);
        let next = PollState {
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
            failures,
            next_poll_at: next_poll_at(cfg, failures, feed_ttl_secs, now),
        };
        (next, PollDecision::Failed)
    }
}

/// Keep the existing validator unless the response supplied a new one.
fn keep_or_update(existing: &Option<String>, fresh: Option<&str>) -> Option<String> {
    match fresh {
        Some(v) => Some(v.to_string()),
        None => existing.clone(),
    }
}

/// The next poll time: `now` plus [`poll_interval_secs`], saturating so a huge
/// `now` can never overflow.
pub fn next_poll_at(cfg: &PollConfig, failures: u32, feed_ttl_secs: Option<i64>, now: i64) -> i64 {
    now.saturating_add(poll_interval_secs(cfg, failures, feed_ttl_secs))
}

/// The interval (seconds) to wait before the next poll, given the consecutive
/// `failures` count and an optional feed-declared TTL.
///
/// The effective base is `base_interval_secs`, raised to the feed's TTL when
/// [`PollConfig::respect_feed_ttl`] is set (a publisher-requested minimum).
/// Each consecutive failure multiplies the interval by `backoff_base`. The
/// result is finally clamped into `[min_interval_secs, max_interval_secs]`
/// (with the bounds themselves sanitized so the clamp can never panic).
pub fn poll_interval_secs(cfg: &PollConfig, failures: u32, feed_ttl_secs: Option<i64>) -> i64 {
    let mut base = cfg.base_interval_secs.max(1);
    if cfg.respect_feed_ttl {
        if let Some(ttl) = feed_ttl_secs {
            if ttl > 0 {
                base = base.max(ttl);
            }
        }
    }
    let interval = if failures == 0 {
        base
    } else {
        let factor = i64::from(cfg.backoff_base.max(1)).saturating_pow(failures);
        base.saturating_mul(factor)
    };
    // Sanitize bounds so `clamp` never panics on a misconfigured lo > hi.
    let lo = cfg.min_interval_secs.max(1);
    let hi = cfg.max_interval_secs.max(lo);
    interval.clamp(lo, hi)
}

/// Convert an RSS `<ttl>` (minutes, as text) into seconds. `None` for
/// non-numeric or non-positive values.
pub fn ttl_minutes_to_secs(ttl: &str) -> Option<i64> {
    let n: i64 = ttl.trim().parse().ok()?;
    if n <= 0 {
        return None;
    }
    Some(n.saturating_mul(60))
}

/// Convert an RSS 1.0 syndication module hint into a poll interval in seconds:
/// `sy:updatePeriod` (`hourly`/`daily`/`weekly`/`monthly`/`yearly`, the RFC
/// default being `daily`) divided by `sy:updateFrequency` (times per period,
/// defaulting to 1). Unknown periods yield `None`.
pub fn sy_update_period_secs(period: &str, frequency: u32) -> Option<i64> {
    let unit = match period.trim().to_ascii_lowercase().as_str() {
        "hourly" => 3_600,
        "daily" => 86_400,
        "weekly" => 604_800,
        "monthly" => 2_592_000, // 30 days
        "yearly" => 31_536_000, // 365 days
        _ => return None,
    };
    let freq = i64::from(frequency.max(1));
    Some((unit / freq).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PollConfig {
        PollConfig {
            base_interval_secs: 1_000,
            min_interval_secs: 100,
            max_interval_secs: 100_000,
            backoff_base: 2,
            respect_feed_ttl: true,
        }
    }

    #[test]
    fn initial_is_due_now() {
        let s = PollState::initial(500);
        assert!(s.is_due(500));
        assert!(s.is_due(600));
        assert!(!s.is_due(499));
        assert_eq!(s.failures, 0);
        assert!(s.etag.is_none());
    }

    #[test]
    fn success_stores_validators_and_schedules_base() {
        let s = PollState::initial(0);
        let (next, decision) = s.on_response(
            &cfg(),
            200,
            Some("\"abc\""),
            Some("Wed, 02 Jul 2003 05:00:00 GMT"),
            None,
            10_000,
        );
        assert_eq!(decision, PollDecision::Modified);
        assert_eq!(next.etag.as_deref(), Some("\"abc\""));
        assert_eq!(
            next.last_modified.as_deref(),
            Some("Wed, 02 Jul 2003 05:00:00 GMT")
        );
        assert_eq!(next.failures, 0);
        assert_eq!(next.next_poll_at, 10_000 + 1_000);
        // Conditional-GET headers surface the stored validators.
        assert_eq!(next.if_none_match(), Some("\"abc\""));
        assert_eq!(
            next.if_modified_since(),
            Some("Wed, 02 Jul 2003 05:00:00 GMT")
        );
    }

    #[test]
    fn not_modified_keeps_validators_and_resets_failures() {
        let mut s = PollState::initial(0);
        s.etag = Some("\"keep\"".into());
        s.failures = 3;
        let (next, decision) = s.on_response(&cfg(), 304, None, None, None, 500);
        assert_eq!(decision, PollDecision::NotModified);
        assert_eq!(next.etag.as_deref(), Some("\"keep\""), "validator retained");
        assert_eq!(next.failures, 0, "success resets backoff");
        assert_eq!(next.next_poll_at, 500 + 1_000);
    }

    #[test]
    fn success_refreshes_validator_when_server_resends() {
        let mut s = PollState::initial(0);
        s.etag = Some("\"old\"".into());
        let (next, _) = s.on_response(&cfg(), 304, Some("\"new\""), None, None, 0);
        assert_eq!(next.etag.as_deref(), Some("\"new\""));
    }

    #[test]
    fn errors_back_off_exponentially_and_keep_validators() {
        let mut s = PollState::initial(0);
        s.etag = Some("\"v\"".into());
        let c = cfg();

        let (s1, d1) = s.on_response(&c, 500, None, None, None, 0);
        assert_eq!(d1, PollDecision::Failed);
        assert_eq!(s1.failures, 1);
        assert_eq!(s1.next_poll_at, 1_000 * 2, "one failure doubles");
        assert_eq!(s1.etag.as_deref(), Some("\"v\""), "validator preserved");

        let (s2, _) = s1.on_response(&c, 503, None, None, None, 0);
        assert_eq!(s2.failures, 2);
        assert_eq!(s2.next_poll_at, 1_000 * 4, "two failures quadruple");

        let (s3, _) = s2.on_response(&c, 404, None, None, None, 0);
        assert_eq!(s3.failures, 3);
        assert_eq!(s3.next_poll_at, 1_000 * 8);
    }

    #[test]
    fn recovery_resets_backoff() {
        let mut s = PollState::initial(0);
        s.failures = 5;
        let (next, d) = s.on_response(&cfg(), 200, None, None, None, 42);
        assert_eq!(d, PollDecision::Modified);
        assert_eq!(next.failures, 0);
        assert_eq!(next.next_poll_at, 42 + 1_000);
    }

    #[test]
    fn transport_error_backs_off_like_a_status_error() {
        let s = PollState::initial(0);
        let (next, d) = s.on_transport_error(&cfg(), None, 0);
        assert_eq!(d, PollDecision::Failed);
        assert_eq!(next.failures, 1);
        assert_eq!(next.next_poll_at, 2_000);
    }

    #[test]
    fn backoff_is_capped_at_max_interval() {
        let c = cfg();
        // Enough failures that the raw interval would blow past the ceiling.
        let capped = poll_interval_secs(&c, 30, None);
        assert_eq!(capped, c.max_interval_secs);
    }

    #[test]
    fn interval_never_below_min() {
        let c = PollConfig {
            base_interval_secs: 10,
            min_interval_secs: 300,
            max_interval_secs: 100_000,
            backoff_base: 2,
            respect_feed_ttl: true,
        };
        assert_eq!(poll_interval_secs(&c, 0, None), 300, "base floored to min");
    }

    #[test]
    fn feed_ttl_raises_the_base_interval() {
        let c = cfg(); // base 1000
                       // TTL of 2 hours > base → success schedules at the TTL.
        assert_eq!(poll_interval_secs(&c, 0, Some(7_200)), 7_200);
        // A TTL smaller than the base does not lower it.
        assert_eq!(poll_interval_secs(&c, 0, Some(60)), 1_000);
        // Disable the honoring: base wins regardless.
        let mut c2 = c.clone();
        c2.respect_feed_ttl = false;
        assert_eq!(poll_interval_secs(&c2, 0, Some(7_200)), 1_000);
    }

    #[test]
    fn ttl_also_participates_in_backoff() {
        let c = cfg();
        // Effective base = 7200 (TTL), one failure doubles it.
        assert_eq!(poll_interval_secs(&c, 1, Some(7_200)), 14_400);
    }

    #[test]
    fn misconfigured_bounds_do_not_panic() {
        // min > max: clamp must not panic; hi is raised to lo.
        let c = PollConfig {
            base_interval_secs: 50,
            min_interval_secs: 10_000,
            max_interval_secs: 100,
            backoff_base: 2,
            respect_feed_ttl: true,
        };
        let v = poll_interval_secs(&c, 3, None);
        assert_eq!(v, 10_000);
    }

    #[test]
    fn scheduling_saturates_instead_of_overflowing() {
        let c = cfg();
        assert_eq!(next_poll_at(&c, 0, None, i64::MAX), i64::MAX);
    }

    #[test]
    fn ttl_minutes_parsing() {
        assert_eq!(ttl_minutes_to_secs("60"), Some(3_600));
        assert_eq!(ttl_minutes_to_secs("  15 "), Some(900));
        assert_eq!(ttl_minutes_to_secs("0"), None);
        assert_eq!(ttl_minutes_to_secs("-5"), None);
        assert_eq!(ttl_minutes_to_secs("soon"), None);
        assert_eq!(ttl_minutes_to_secs(""), None);
    }

    #[test]
    fn sy_update_period_parsing() {
        assert_eq!(sy_update_period_secs("hourly", 1), Some(3_600));
        assert_eq!(sy_update_period_secs("Daily", 1), Some(86_400));
        assert_eq!(sy_update_period_secs("daily", 4), Some(21_600), "4x/day");
        assert_eq!(sy_update_period_secs("weekly", 1), Some(604_800));
        // updateFrequency of 0 is treated as 1 (never divide by zero).
        assert_eq!(sy_update_period_secs("hourly", 0), Some(3_600));
        assert_eq!(sy_update_period_secs("fortnightly", 1), None);
    }

    #[test]
    fn full_lifecycle_200_then_304_then_error_then_recover() {
        let c = cfg();
        let s = PollState::initial(0);
        // First fetch: 200 with an ETag.
        let (s, d) = s.on_response(&c, 200, Some("\"e1\""), None, None, 1_000);
        assert_eq!(d, PollDecision::Modified);
        // Next poll: 304, ETag replayed, still nothing new.
        let (s, d) = s.on_response(&c, 304, None, None, None, 5_000);
        assert_eq!(d, PollDecision::NotModified);
        assert_eq!(s.if_none_match(), Some("\"e1\""));
        // Then the server errors twice.
        let (s, _) = s.on_response(&c, 500, None, None, None, 9_000);
        let (s, d) = s.on_response(&c, 500, None, None, None, 9_000);
        assert_eq!(d, PollDecision::Failed);
        assert_eq!(s.failures, 2);
        assert_eq!(s.next_poll_at, 9_000 + 4_000);
        // Recovery: 200 clears the backoff and keeps a working schedule.
        let (s, d) = s.on_response(&c, 200, Some("\"e2\""), None, None, 20_000);
        assert_eq!(d, PollDecision::Modified);
        assert_eq!(s.failures, 0);
        assert_eq!(s.if_none_match(), Some("\"e2\""));
        assert_eq!(s.next_poll_at, 21_000);
    }
}
