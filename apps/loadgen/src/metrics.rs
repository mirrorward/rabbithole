//! Run metrics: atomic counters plus sorted-vec latency percentiles.
//!
//! Latencies are recorded as raw microsecond samples and sorted once at
//! report time. Even the 10k-session hardware target only produces a few
//! million samples (tens of MB) — no HDR bucket structure or extra
//! dependencies needed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Collects raw latency samples (microseconds).
#[derive(Debug, Default)]
pub struct LatencyRecorder {
    samples_us: Mutex<Vec<u64>>,
}

impl LatencyRecorder {
    pub fn record(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.samples_us.lock().expect("not poisoned").push(us);
    }

    pub fn summary(&self) -> LatencySummary {
        let mut v = self.samples_us.lock().expect("not poisoned").clone();
        v.sort_unstable();
        LatencySummary::from_sorted(&v)
    }
}

/// Percentile summary of one latency series, in milliseconds.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LatencySummary {
    pub count: u64,
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl LatencySummary {
    /// Build from an ascending-sorted microsecond series.
    fn from_sorted(sorted_us: &[u64]) -> Self {
        let ms = |us: u64| us as f64 / 1000.0;
        let pct = |p: f64| -> f64 {
            if sorted_us.is_empty() {
                return 0.0;
            }
            // Nearest-rank on a zero-based index.
            let idx = ((p / 100.0) * (sorted_us.len() - 1) as f64).round() as usize;
            ms(sorted_us[idx.min(sorted_us.len() - 1)])
        };
        Self {
            count: sorted_us.len() as u64,
            min_ms: sorted_us.first().copied().map_or(0.0, ms),
            p50_ms: pct(50.0),
            p95_ms: pct(95.0),
            p99_ms: pct(99.0),
            max_ms: sorted_us.last().copied().map_or(0.0, ms),
        }
    }
}

/// Shared run counters. All monotonic except `sessions_active` (a gauge).
#[derive(Debug, Default)]
pub struct Metrics {
    /// Sessions the ramp has spawned.
    pub sessions_started: AtomicU64,
    /// Successful transport connects (includes reconnects).
    pub sessions_connected: AtomicU64,
    /// Successful logins (auth + welcome; includes reconnect re-logins).
    pub sessions_logged_in: AtomicU64,
    /// Sessions currently up (gauge).
    pub sessions_active: AtomicU64,
    /// Session tasks that have fully finished (drained or given up).
    pub sessions_completed: AtomicU64,
    /// Distinct sessions that saw at least one of their own chat echoes.
    pub sessions_echoed: AtomicU64,
    /// Chat lines sent.
    pub msgs_sent: AtomicU64,
    /// Own-echo pushes matched back to a send.
    pub echoes_seen: AtomicU64,
    /// Sends whose echo never arrived within the echo timeout.
    pub echo_timeouts: AtomicU64,
    /// All pushes observed (chat from others, presence, ...).
    pub pushes_seen: AtomicU64,
    /// Errors of any kind (connect/auth/send failures, echo timeouts).
    pub errors: AtomicU64,
    /// Sessions dropped by the server or transport mid-run.
    pub disconnects: AtomicU64,
    /// Bounded reconnect attempts made after a drop.
    pub reconnects: AtomicU64,

    pub connect_latency: LatencyRecorder,
    pub login_latency: LatencyRecorder,
    pub chat_rtt: LatencyRecorder,
}

impl Metrics {
    pub fn add(&self, counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Snapshot everything into a [`Report`].
    pub fn report(
        &self,
        scenario: &str,
        sessions_target: u64,
        wall: Duration,
        aborted: Option<String>,
    ) -> Report {
        Report {
            scenario: scenario.to_owned(),
            wall_secs: wall.as_secs_f64(),
            sessions_target,
            sessions_started: Self::get(&self.sessions_started),
            sessions_connected: Self::get(&self.sessions_connected),
            sessions_logged_in: Self::get(&self.sessions_logged_in),
            sessions_completed: Self::get(&self.sessions_completed),
            sessions_echoed: Self::get(&self.sessions_echoed),
            msgs_sent: Self::get(&self.msgs_sent),
            echoes_seen: Self::get(&self.echoes_seen),
            echo_timeouts: Self::get(&self.echo_timeouts),
            pushes_seen: Self::get(&self.pushes_seen),
            errors: Self::get(&self.errors),
            disconnects: Self::get(&self.disconnects),
            reconnects: Self::get(&self.reconnects),
            connect: self.connect_latency.summary(),
            login: self.login_latency.summary(),
            chat_rtt: self.chat_rtt.summary(),
            aborted,
        }
    }
}

/// Final run report: counts + latency percentiles.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    pub scenario: String,
    pub wall_secs: f64,
    pub sessions_target: u64,
    pub sessions_started: u64,
    pub sessions_connected: u64,
    pub sessions_logged_in: u64,
    pub sessions_completed: u64,
    pub sessions_echoed: u64,
    pub msgs_sent: u64,
    pub echoes_seen: u64,
    pub echo_timeouts: u64,
    pub pushes_seen: u64,
    pub errors: u64,
    pub disconnects: u64,
    pub reconnects: u64,
    pub connect: LatencySummary,
    pub login: LatencySummary,
    pub chat_rtt: LatencySummary,
    /// `Some(reason)` when the circuit breaker cut the run short.
    pub aborted: Option<String>,
}

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serializes")
    }

    /// The plain-text REPORT block printed at the end of a run.
    pub fn render_text(&self) -> String {
        fn lat(name: &str, s: &LatencySummary) -> String {
            format!(
                "{name:<14} p50 {:>8.1}  p95 {:>8.1}  p99 {:>8.1}  max {:>8.1}  (n={})",
                s.p50_ms, s.p95_ms, s.p99_ms, s.max_ms, s.count
            )
        }
        let mut out = String::new();
        out.push_str("==== warren-stampede REPORT ====\n");
        out.push_str(&format!("scenario:      {}\n", self.scenario));
        out.push_str(&format!("wall time:     {:.1}s\n", self.wall_secs));
        out.push_str(&format!(
            "sessions:      {} target / {} started / {} logged in / {} completed\n",
            self.sessions_target,
            self.sessions_started,
            self.sessions_logged_in,
            self.sessions_completed
        ));
        out.push_str(&format!(
            "chat:          {} sent / {} echoed / {} echo timeouts / {} sessions echoed\n",
            self.msgs_sent, self.echoes_seen, self.echo_timeouts, self.sessions_echoed
        ));
        out.push_str(&format!("pushes seen:   {}\n", self.pushes_seen));
        out.push_str(&format!(
            "errors:        {} (disconnects {}, reconnects {})\n",
            self.errors, self.disconnects, self.reconnects
        ));
        out.push_str(&format!("{}\n", lat("connect ms:", &self.connect)));
        out.push_str(&format!("{}\n", lat("login ms:", &self.login)));
        out.push_str(&format!("{}\n", lat("chat rtt ms:", &self.chat_rtt)));
        match &self.aborted {
            Some(reason) => out.push_str(&format!("ABORTED:       {reason}\n")),
            None => out.push_str("aborted:       no\n"),
        }
        out.push_str("================================");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_is_zeroed() {
        let r = LatencyRecorder::default();
        let s = r.summary();
        assert_eq!(s.count, 0);
        assert_eq!(s.p95_ms, 0.0);
        assert_eq!(s.max_ms, 0.0);
    }

    #[test]
    fn percentiles_from_known_series() {
        let r = LatencyRecorder::default();
        // 1..=100 ms, recorded out of order.
        for ms in (1..=100u64).rev() {
            r.record(Duration::from_millis(ms));
        }
        let s = r.summary();
        assert_eq!(s.count, 100);
        assert_eq!(s.min_ms, 1.0);
        assert_eq!(s.max_ms, 100.0);
        assert!((s.p50_ms - 51.0).abs() < 1.5, "p50 = {}", s.p50_ms);
        assert!((s.p95_ms - 95.0).abs() < 1.5, "p95 = {}", s.p95_ms);
        assert!((s.p99_ms - 99.0).abs() < 1.5, "p99 = {}", s.p99_ms);
    }

    #[test]
    fn report_renders_both_forms() {
        let m = Metrics::default();
        m.add(&m.msgs_sent, 3);
        let rep = m.report("chat", 10, Duration::from_secs(5), None);
        assert!(rep.render_text().contains("REPORT"));
        let json: serde_json::Value = serde_json::from_str(&rep.to_json()).unwrap();
        assert_eq!(json["msgs_sent"], 3);
        assert_eq!(json["sessions_target"], 10);
    }
}
