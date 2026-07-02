//! Per-account push replay log.
//!
//! Every push delivered to an account gets a monotonically increasing
//! sequence number (carried in the frame's `id` field). A bounded ring of
//! recent pushes is kept per account so a reconnecting client presenting
//! its replay cursor receives what it missed. In-memory by design: replay
//! is best-effort across short drops, not a durable mailbox (offline
//! delivery for DMs arrives in Wave 2 with its own store).

use std::collections::HashMap;

use parking_lot::Mutex;
use rabbithole_proto::Frame;

/// Retained pushes per account.
const RING_CAPACITY: usize = 256;

#[derive(Default)]
struct AccountLog {
    next_seq: u64,
    ring: std::collections::VecDeque<(u64, Frame)>,
}

#[derive(Default)]
pub struct PushLog {
    inner: Mutex<HashMap<i64, AccountLog>>,
}

impl PushLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamp `frame` with the account's next sequence number and record it.
    /// Returns the stamped frame ready to send.
    pub fn stamp(&self, account_id: i64, mut frame: Frame) -> Frame {
        let mut inner = self.inner.lock();
        let log = inner.entry(account_id).or_default();
        log.next_seq += 1;
        frame.id = rabbithole_proto::RequestId(log.next_seq);
        if log.ring.len() == RING_CAPACITY {
            log.ring.pop_front();
        }
        log.ring.push_back((log.next_seq, frame.clone()));
        frame
    }

    /// Pushes newer than `cursor`, oldest first.
    pub fn since(&self, account_id: i64, cursor: u64) -> Vec<Frame> {
        let inner = self.inner.lock();
        match inner.get(&account_id) {
            Some(log) => log
                .ring
                .iter()
                .filter(|(seq, _)| *seq > cursor)
                .map(|(_, f)| f.clone())
                .collect(),
            None => Vec::new(),
        }
    }

    /// Drop an account's log (e.g. on logout/ban).
    pub fn forget(&self, account_id: i64) {
        self.inner.lock().remove(&account_id);
    }

    /// Accounts that have a log (i.e. have connected at least once since
    /// boot). Used by the offline-replay recorder.
    pub fn known_accounts(&self) -> Vec<i64> {
        self.inner.lock().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::session::Welcome;

    fn push(text: &str) -> Frame {
        Frame::push(&Welcome::new(text, None)).unwrap()
    }

    #[test]
    fn stamps_monotonic_and_replays_since() {
        let log = PushLog::new();
        let a = log.stamp(1, push("a"));
        let b = log.stamp(1, push("b"));
        let c = log.stamp(1, push("c"));
        assert_eq!((a.id.0, b.id.0, c.id.0), (1, 2, 3));

        let replay = log.since(1, 1);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].id.0, 2);
        assert_eq!(replay[1].id.0, 3);

        assert!(log.since(1, 3).is_empty());
        assert!(log.since(99, 0).is_empty());
    }

    #[test]
    fn ring_is_bounded() {
        let log = PushLog::new();
        for i in 0..300 {
            log.stamp(7, push(&i.to_string()));
        }
        let all = log.since(7, 0);
        assert_eq!(all.len(), RING_CAPACITY);
        // Oldest retained is 300 - 256 + 1 = 45.
        assert_eq!(all[0].id.0, 45);
    }

    #[test]
    fn accounts_are_isolated() {
        let log = PushLog::new();
        log.stamp(1, push("one"));
        log.stamp(2, push("two"));
        assert_eq!(log.since(1, 0).len(), 1);
        log.forget(1);
        assert!(log.since(1, 0).is_empty());
        assert_eq!(log.since(2, 0).len(), 1);
    }
}
