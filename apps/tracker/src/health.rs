//! Per-server health/uptime observation — **verifiable, not authoritative**.
//!
//! Every slot the tracker has ever accepted a registration for gets a
//! [`HealthLog`]: a ring of [`NUM_BUCKETS`] fifteen-minute observation
//! windows covering the last 24 hours ([`OBSERVATION_WINDOW`]), each counting
//! heartbeats **seen** against the number a healthy server would have sent
//! ([`EXPECTED_PER_BUCKET`], calibrated to the classic five-minute heartbeat
//! cadence). From the ring the tracker derives `uptime_24h`, `first_seen` /
//! `last_seen`, and a **flap count** (live→expired transitions, counted by
//! the registry's lazy expiry sweeps). The status-port `INDEX` and `HEALTH`
//! verbs serve the results ([`crate::service`]).
//!
//! ## Verifiable, not authoritative
//!
//! Everything in this module is the **tracker's own local observation** —
//! plain bookkeeping about packets this process saw. None of it is signed,
//! and none of it crosses the gossip mesh: an entry learned from a peer
//! starts a **fresh** log the moment it arrives here, and a peer's uptime
//! numbers are never imported (a tracker can only vouch for what it watched
//! itself). Clients MUST present these numbers as "as observed by tracker
//! X", never as a property the server proved. What a client *can* verify is
//! the signed descriptor behind an entry: the `INDEX` verb carries the
//! descriptor's key prefix and generation/attestation timestamp
//! ([`crate::descriptor`] — the descriptor's `timestamp` doubles as both),
//! so the client can pull the full signed document (e.g. a gossip `Want` to
//! the tracker's gossip port, see [`crate::gossip`]) and check the signature
//! offline instead of trusting the directory line.
//!
//! ## Ring anchoring (the monotonic-epoch problem)
//!
//! The registry keeps time with [`Instant`], which is monotonic but has no
//! global zero — so there is no shared "bucket 0" across logs or restarts.
//! Each log therefore anchors its ring at its **own first observation**
//! (`epoch == first_seen`):
//!
//! ```text
//! bucket k covers [epoch + k*15 min, epoch + (k+1)*15 min)
//!
//!             epoch                                     now
//!               v                                        v
//! time  ────────┼────────┼────────┼──── … ────┼──────────┼───→
//! bucket        0        1        2           k-1        k (partial)
//! ring slot     0        1        2        (k-1)%96    k%96
//! ```
//!
//! Slots store the absolute bucket index they were written for, so after a
//! full lap of the ring a stale slot is detected and reset **lazily** — no
//! timer ever runs. Instants earlier than the epoch saturate to bucket 0;
//! nothing here can panic.
//!
//! ## Uptime math
//!
//! `uptime_24h` averages per-bucket credit over the window from
//! `max(epoch, now − 24 h)` to `now`: each completed bucket contributes
//! `min(seen, 3) / 3`, and the current partial bucket is judged only against
//! the heartbeats due *so far* (`1 + elapsed_in_bucket / 5 min`, capped at
//! 3) so a freshly registered, healthy server reads 100% instead of being
//! punished for windows that have not happened yet. Buckets the server was
//! silent through contribute zero — missed windows lower the number. The
//! result is integer per-mille (0..=1000), rendered as a percent by
//! [`format_permille`].

use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Width of one observation bucket: 15 minutes.
pub const BUCKET_LEN: Duration = Duration::from_secs(15 * 60);

/// Buckets in the ring: 96 × 15 min = 24 hours.
pub const NUM_BUCKETS: usize = 96;

/// The full observation window (also the retention horizon for idle logs).
pub const OBSERVATION_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// The heartbeat cadence the expectation is calibrated to (classic trackers:
/// one heartbeat every five minutes).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Heartbeats a healthy server sends per full bucket
/// (`BUCKET_LEN / HEARTBEAT_INTERVAL`).
pub const EXPECTED_PER_BUCKET: u32 = 3;

/// One ring slot: the absolute bucket index it was last written for, plus
/// the heartbeats counted in that bucket. `index == u64::MAX` marks a slot
/// that has never been written (real indices stay far below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bucket {
    index: u64,
    seen: u32,
}

const EMPTY_BUCKET: Bucket = Bucket {
    index: u64::MAX,
    seen: 0,
};

/// The tracker's local health observations for one `(ip, port)` slot.
///
/// Fed by every *accepted* registration (unsigned heartbeat, signed announce,
/// gossip) and by the registry's lazy expiry sweeps; queried by the `INDEX`
/// and `HEALTH` status verbs. All methods take an injected `now` so tests
/// are deterministic — see the module docs for the ring-anchoring design.
#[derive(Debug, Clone)]
pub struct HealthLog {
    /// Ring origin — the first observation (`first_seen`), see module docs.
    epoch: Instant,
    first_seen: Instant,
    last_seen: Instant,
    live: bool,
    flap_count: u32,
    buckets: [Bucket; NUM_BUCKETS],
}

impl HealthLog {
    /// Starts a log whose ring is anchored at `now` (the first observation).
    /// The log is not `live` until the first [`record_heartbeat`].
    ///
    /// [`record_heartbeat`]: Self::record_heartbeat
    pub fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            first_seen: now,
            last_seen: now,
            live: false,
            flap_count: 0,
            buckets: [EMPTY_BUCKET; NUM_BUCKETS],
        }
    }

    /// Records one accepted registration (heartbeat, announce, or gossip
    /// arrival) at `now`: the server is live, and its current bucket gains a
    /// heartbeat.
    pub fn record_heartbeat(&mut self, now: Instant) {
        self.last_seen = self.last_seen.max(now);
        self.live = true;
        let bucket = bucket_index(self.epoch, now);
        let slot = &mut self.buckets[(bucket % NUM_BUCKETS as u64) as usize];
        if slot.index != bucket {
            // Lazy ring-lap reset: this slot last served an older bucket.
            *slot = Bucket {
                index: bucket,
                seen: 0,
            };
        }
        slot.seen = slot.seen.saturating_add(1);
    }

    /// Marks the slot expired (called by the registry's lazy expiry sweep).
    /// A live→expired transition counts one flap; repeated sweeps over an
    /// already-expired slot do not.
    pub fn mark_expired(&mut self, _now: Instant) {
        if self.live {
            self.live = false;
            self.flap_count = self.flap_count.saturating_add(1);
        }
    }

    /// Whether the slot was live at the last sweep/heartbeat.
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Live→expired transitions observed so far.
    pub fn flap_count(&self) -> u32 {
        self.flap_count
    }

    /// When this log first saw the server (also the ring epoch).
    pub fn first_seen(&self) -> Instant {
        self.first_seen
    }

    /// When this log last saw an accepted registration.
    pub fn last_seen(&self) -> Instant {
        self.last_seen
    }

    /// Observed uptime over the last 24 h (or since `first_seen` if
    /// younger), in integer per-mille (0..=1000). See the module docs for
    /// the exact math; missed windows lower the number.
    pub fn uptime_24h_permille(&self, now: Instant) -> u32 {
        let (start, now_bucket, elapsed) = self.window(now);
        let mut credit: u64 = 0;
        let mut buckets: u64 = 0;
        for bucket in start..=now_bucket {
            let expected = expected_in(bucket, now_bucket, elapsed);
            let seen = u64::from(self.seen_in(bucket).min(expected));
            credit += seen * 1000 / u64::from(expected);
            buckets += 1;
        }
        (credit / buckets.max(1)) as u32
    }

    /// ASCII sparkline of the observation window, oldest bucket first:
    /// `#` = all expected heartbeats seen, `+` = some, `.` = silent.
    /// At most [`NUM_BUCKETS`] characters; shorter for young logs.
    pub fn sparkline(&self, now: Instant) -> String {
        let (start, now_bucket, elapsed) = self.window(now);
        (start..=now_bucket)
            .map(|bucket| {
                let expected = expected_in(bucket, now_bucket, elapsed);
                match self.seen_in(bucket) {
                    0 => '.',
                    seen if seen >= expected => '#',
                    _ => '+',
                }
            })
            .collect()
    }

    /// Snapshot of everything the `HEALTH` verb reports, taken at `now`.
    pub fn report(&self, addr: SocketAddr, now: Instant) -> HealthReport {
        HealthReport {
            addr,
            live: self.live,
            uptime_permille: self.uptime_24h_permille(now),
            first_seen_secs: now.saturating_duration_since(self.first_seen).as_secs(),
            last_seen_secs: now.saturating_duration_since(self.last_seen).as_secs(),
            flap_count: self.flap_count,
            sparkline: self.sparkline(now),
        }
    }

    /// The window to judge: `(first bucket, current bucket, elapsed since
    /// epoch)`. At most [`NUM_BUCKETS`] buckets, never before the epoch.
    fn window(&self, now: Instant) -> (u64, u64, Duration) {
        let elapsed = now.saturating_duration_since(self.epoch);
        let now_bucket = elapsed.as_secs() / BUCKET_LEN.as_secs();
        let start = now_bucket.saturating_sub(NUM_BUCKETS as u64 - 1);
        (start, now_bucket, elapsed)
    }

    /// Heartbeats recorded in absolute `bucket` (0 if the slot has lapped).
    fn seen_in(&self, bucket: u64) -> u32 {
        let slot = self.buckets[(bucket % NUM_BUCKETS as u64) as usize];
        if slot.index == bucket {
            slot.seen
        } else {
            0
        }
    }
}

/// The absolute bucket index of `now` on a ring anchored at `epoch`
/// (instants before the epoch saturate to bucket 0).
fn bucket_index(epoch: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(epoch).as_secs() / BUCKET_LEN.as_secs()
}

/// Heartbeats due in `bucket` by the time `elapsed` has passed: full buckets
/// expect [`EXPECTED_PER_BUCKET`]; the current partial bucket only what is
/// due so far (so a fresh healthy server reads 100%).
fn expected_in(bucket: u64, now_bucket: u64, elapsed: Duration) -> u32 {
    if bucket < now_bucket {
        return EXPECTED_PER_BUCKET;
    }
    let into_bucket = elapsed.as_secs() - now_bucket * BUCKET_LEN.as_secs();
    let due = 1 + (into_bucket / HEARTBEAT_INTERVAL.as_secs()) as u32;
    due.min(EXPECTED_PER_BUCKET)
}

/// Renders per-mille as a percent with one decimal (`1000` → `"100.0"`,
/// `66` → `"6.6"`).
pub fn format_permille(permille: u32) -> String {
    format!("{}.{}", permille / 10, permille % 10)
}

/// Everything the `HEALTH <ip:port>` verb reports for one slot, at one
/// instant. `*_secs` fields are "seconds ago" — the tracker has no wall
/// clock in this path (see the module docs on `Instant`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// The slot this report describes.
    pub addr: SocketAddr,
    /// Whether the slot was live at the report's sweep.
    pub live: bool,
    /// Observed 24 h uptime, integer per-mille (0..=1000).
    pub uptime_permille: u32,
    /// Seconds since this tracker first saw the server.
    pub first_seen_secs: u64,
    /// Seconds since the last accepted registration.
    pub last_seen_secs: u64,
    /// Live→expired transitions observed.
    pub flap_count: u32,
    /// Bucket sparkline, oldest first (see [`HealthLog::sparkline`]).
    pub sparkline: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Injected-time helper: `at(base, secs)` is `base + secs` — all test
    /// instants are in the future relative to `Instant::now()`, so they are
    /// valid on any platform (backdating 24 h can underflow on fresh VMs).
    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn constants_are_consistent() {
        assert_eq!(
            BUCKET_LEN.as_secs(),
            u64::from(EXPECTED_PER_BUCKET) * HEARTBEAT_INTERVAL.as_secs()
        );
        assert_eq!(
            OBSERVATION_WINDOW.as_secs(),
            BUCKET_LEN.as_secs() * NUM_BUCKETS as u64
        );
    }

    #[test]
    fn fresh_heartbeat_reads_full_uptime() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        assert!(!log.is_live());
        // Before any heartbeat: one bucket, one due, none seen.
        assert_eq!(log.uptime_24h_permille(base), 0);

        log.record_heartbeat(base);
        assert!(log.is_live());
        assert_eq!(log.uptime_24h_permille(base), 1000);
        assert_eq!(log.sparkline(base), "#");
        assert_eq!(log.flap_count(), 0);
    }

    #[test]
    fn partial_bucket_expectation_grows_with_time() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        log.record_heartbeat(base);
        // One of one due at t0 → 100%; one of two due at +5 min → 50%;
        // one of three due at +10 min → 33.3%.
        assert_eq!(log.uptime_24h_permille(base), 1000);
        assert_eq!(log.uptime_24h_permille(at(base, 300)), 500);
        assert_eq!(log.uptime_24h_permille(at(base, 600)), 333);
        assert_eq!(log.sparkline(at(base, 600)), "+");
    }

    #[test]
    fn steady_heartbeats_hold_full_uptime_across_24h() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        // One heartbeat every five minutes for a full day.
        for k in 0..=(OBSERVATION_WINDOW.as_secs() / HEARTBEAT_INTERVAL.as_secs()) {
            log.record_heartbeat(at(base, k * HEARTBEAT_INTERVAL.as_secs()));
        }
        let now = at(base, OBSERVATION_WINDOW.as_secs());
        assert_eq!(log.uptime_24h_permille(now), 1000);
        let spark = log.sparkline(now);
        assert_eq!(spark.len(), NUM_BUCKETS);
        assert!(spark.chars().all(|c| c == '#'));
    }

    #[test]
    fn missed_windows_lower_uptime() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        log.record_heartbeat(base);
        // One heartbeat, then silence for an hour: bucket 0 has 1 of 3,
        // buckets 1–3 are empty, the partial bucket 4 owes 1 and has 0.
        // (333 + 0 + 0 + 0 + 0) / 5 = 66 per-mille.
        let now = at(base, 3600);
        assert_eq!(log.uptime_24h_permille(now), 66);
        assert_eq!(log.sparkline(now), "+....");
    }

    #[test]
    fn ring_lap_lazily_invalidates_stale_buckets() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        log.record_heartbeat(base);
        // A day later the ring has lapped: bucket 96 reuses slot 0, and the
        // window (buckets 1..=96) holds exactly one fresh heartbeat.
        let now = at(base, OBSERVATION_WINDOW.as_secs());
        log.record_heartbeat(now);
        // 1000 (bucket 96, one of one due) / 96 buckets = 10 per-mille.
        assert_eq!(log.uptime_24h_permille(now), 10);
        let spark = log.sparkline(now);
        assert_eq!(spark.len(), NUM_BUCKETS);
        assert!(spark.starts_with('.'));
        assert!(spark.ends_with('#'));
        assert_eq!(spark.matches('#').count(), 1);
    }

    #[test]
    fn flaps_count_live_to_expired_transitions_once() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        log.record_heartbeat(base);

        log.mark_expired(at(base, 400));
        assert!(!log.is_live());
        assert_eq!(log.flap_count(), 1);
        // Repeated sweeps over an already-expired slot are not new flaps.
        log.mark_expired(at(base, 500));
        assert_eq!(log.flap_count(), 1);

        // Coming back and dying again is a second flap.
        log.record_heartbeat(at(base, 600));
        assert!(log.is_live());
        log.mark_expired(at(base, 1000));
        assert_eq!(log.flap_count(), 2);
    }

    #[test]
    fn instants_before_the_epoch_saturate_instead_of_panicking() {
        let base = Instant::now() + Duration::from_secs(3600);
        let mut log = HealthLog::new(base);
        let earlier = base - Duration::from_secs(1800);
        // Both recording and querying earlier than the epoch land on
        // bucket 0 — total, never a panic.
        log.record_heartbeat(earlier);
        assert_eq!(log.uptime_24h_permille(earlier), 1000);
        assert_eq!(log.sparkline(earlier), "#");
        assert!(log.last_seen() >= log.first_seen());
    }

    #[test]
    fn report_snapshots_everything_at_one_instant() {
        let base = Instant::now();
        let mut log = HealthLog::new(base);
        log.record_heartbeat(base);
        log.mark_expired(at(base, 400));
        let addr: SocketAddr = ([10, 0, 0, 1], 5500).into();
        let report = log.report(addr, at(base, 900));
        assert_eq!(report.addr, addr);
        assert!(!report.live);
        assert_eq!(report.first_seen_secs, 900);
        assert_eq!(report.last_seen_secs, 900);
        assert_eq!(report.flap_count, 1);
        // Bucket 0: 1 of 3 → 333; bucket 1 (partial, 1 due): 0 → (333+0)/2.
        assert_eq!(report.uptime_permille, 166);
        assert_eq!(report.sparkline, "+.");
    }

    #[test]
    fn format_permille_renders_percent_with_one_decimal() {
        assert_eq!(format_permille(1000), "100.0");
        assert_eq!(format_permille(500), "50.0");
        assert_eq!(format_permille(66), "6.6");
        assert_eq!(format_permille(0), "0.0");
    }
}
