//! Pure, host-tested connection-lifecycle logic.
//!
//! This module holds **no** `web_sys`, `wasm_bindgen`, or timer types so the
//! reconnect schedule and the connection-state model can be unit-tested on the
//! host with `cargo test`. The wasm-only glue that actually arms a timer and
//! opens a socket lives in [`crate::ws`] and calls straight into
//! [`backoff_delay`] here.
//!
//! # The connection-state model
//!
//! [`ConnState`] is the four-state lifecycle the [`StatusBar`] surfaces:
//! `Offline → Connecting → Online`, and, when a live socket drops,
//! `Reconnecting` until it comes back (or the user gives up). It is folded into
//! [`UiState`](crate::state::UiState) so the header reflects the transport.
//!
//! # The backoff seam
//!
//! [`backoff_delay`] is the pure schedule: capped exponential growth plus an
//! *equal-jitter* term. The randomness is a caller-supplied seam — the wasm
//! transport passes `js_sys::Math::random()`, tests pass a fixed fraction — so
//! the math is fully deterministic under test.

use std::time::Duration;

/// The connection lifecycle surfaced to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnState {
    /// No socket, and none wanted (fresh, or a user-requested disconnect).
    #[default]
    Offline,
    /// A socket is opening (or the very first connect is in flight).
    Connecting,
    /// The handshake completed; the session is live.
    Online,
    /// A live socket dropped and a backoff-scheduled reconnect is pending.
    Reconnecting,
}

impl ConnState {
    /// A short, human label for the header/status line.
    pub fn label(self) -> &'static str {
        match self {
            ConnState::Offline => "Offline",
            ConnState::Connecting => "Connecting\u{2026}",
            ConnState::Online => "Online",
            ConnState::Reconnecting => "Reconnecting\u{2026}",
        }
    }

    /// Whether the transport is live (the header shows a lit indicator).
    pub fn is_live(self) -> bool {
        matches!(self, ConnState::Online)
    }

    /// Whether a connection attempt is in flight (the initial open or a retry).
    pub fn is_pending(self) -> bool {
        matches!(self, ConnState::Connecting | ConnState::Reconnecting)
    }
}

/// Base delay before the first reconnect attempt, in milliseconds.
pub const BACKOFF_BASE_MS: u64 = 500;
/// Ceiling the backoff delay never exceeds, in milliseconds.
pub const BACKOFF_CAP_MS: u64 = 30_000;
/// Attempt index past which the exponential term is already saturated; clamped
/// so `2^attempt` can never overflow.
const MAX_SHIFT: u32 = 32;

/// The reconnect delay for a 0-based `attempt`, using the default base and cap.
///
/// `jitter` is the randomness seam in `[0.0, 1.0)`: the wasm transport passes
/// `Math::random()`, tests pass a fixed value. See [`backoff_delay_with`] for
/// the schedule.
pub fn backoff_delay(attempt: u32, jitter: f64) -> Duration {
    backoff_delay_with(attempt, jitter, BACKOFF_BASE_MS, BACKOFF_CAP_MS)
}

/// The *equal-jitter* exponential backoff schedule.
///
/// The uncapped exponential term is `base_ms * 2^attempt`, capped at `cap_ms`.
/// The returned delay is `capped / 2 + jitter * (capped / 2)`, so it always
/// lands in `[capped/2, capped)` — half fixed, half randomised — which spreads
/// a thundering herd of reconnecting clients without ever collapsing to zero.
///
/// All arithmetic saturates, so absurd attempt counts simply pin to `cap_ms`.
pub fn backoff_delay_with(attempt: u32, jitter: f64, base_ms: u64, cap_ms: u64) -> Duration {
    let exp = base_ms.saturating_mul(1u64.checked_shl(attempt.min(MAX_SHIFT)).unwrap_or(u64::MAX));
    let capped = exp.min(cap_ms);
    let half = capped / 2;
    let jitter = jitter.clamp(0.0, 1.0);
    let extra = (half as f64 * jitter) as u64;
    Duration::from_millis(half.saturating_add(extra))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conn_state_labels_and_predicates() {
        assert_eq!(ConnState::default(), ConnState::Offline);
        assert!(ConnState::Online.is_live());
        assert!(!ConnState::Reconnecting.is_live());
        assert!(ConnState::Connecting.is_pending());
        assert!(ConnState::Reconnecting.is_pending());
        assert!(!ConnState::Online.is_pending());
        assert_eq!(ConnState::Offline.label(), "Offline");
        assert!(ConnState::Reconnecting.label().starts_with("Reconnecting"));
    }

    #[test]
    fn zero_jitter_gives_half_the_capped_exponential() {
        // attempt 0 → base 500 → half = 250.
        assert_eq!(backoff_delay(0, 0.0), Duration::from_millis(250));
        // attempt 1 → 1000 → half = 500.
        assert_eq!(backoff_delay(1, 0.0), Duration::from_millis(500));
        // attempt 2 → 2000 → half = 1000.
        assert_eq!(backoff_delay(2, 0.0), Duration::from_millis(1000));
    }

    #[test]
    fn full_jitter_stays_below_the_capped_exponential() {
        // attempt 0: [250, 500). jitter just under 1.0 approaches but never
        // reaches the full 500.
        let d = backoff_delay(0, 0.999);
        assert!(d >= Duration::from_millis(250));
        assert!(d < Duration::from_millis(500));
    }

    #[test]
    fn delay_is_capped() {
        // A large attempt pins the exponential to the cap; the delay is then in
        // [cap/2, cap] (jitter comes from `[0.0, 1.0)`, so a real draw never
        // quite reaches the cap, but the ceiling still holds).
        let d = backoff_delay(20, 0.999);
        assert!(d >= Duration::from_millis(BACKOFF_CAP_MS / 2));
        assert!(d <= Duration::from_millis(BACKOFF_CAP_MS));
    }

    #[test]
    fn absurd_attempt_counts_saturate_to_the_cap() {
        // No overflow panic; still bounded by the cap.
        let d = backoff_delay(u32::MAX, 0.0);
        assert_eq!(d, Duration::from_millis(BACKOFF_CAP_MS / 2));
    }

    #[test]
    fn jitter_is_monotonic_in_the_fraction() {
        let lo = backoff_delay(3, 0.1);
        let mid = backoff_delay(3, 0.5);
        let hi = backoff_delay(3, 0.9);
        assert!(lo <= mid);
        assert!(mid <= hi);
    }

    #[test]
    fn custom_base_and_cap_are_honoured() {
        // base 100, cap 800: attempt 4 → 1600 → capped 800 → half 400.
        assert_eq!(
            backoff_delay_with(4, 0.0, 100, 800),
            Duration::from_millis(400)
        );
    }
}
