//! Room-scoped history correlation and overlap reconciliation, shared by web
//! and desktop. The wire carries no message id: compare multiplicities, never
//! a set, so older servers' identical same-millisecond lines survive.

use std::collections::{HashMap, HashSet, VecDeque};

use rabbithole_proto::{Frame, FrameKind, RequestId};

use crate::state::ChatLine;

pub const HISTORY_LIMIT: usize = 500;
const MAX_PENDING: usize = 32;
const MAX_OVERLAP: usize = HISTORY_LIMIT * MAX_PENDING;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomRequest {
    /// History is fetched only after the server accepts this join/create.
    Open(String),
    History(String),
}

impl RoomRequest {
    fn room(&self) -> &str {
        match self {
            Self::Open(room) | Self::History(room) => room,
        }
    }
}

/// One tracker per transport, never shared between burrows. Only replies on
/// the current socket generation may consume a request (push ids can collide).
#[derive(Debug, Default)]
pub struct HistoryRequests {
    generation: u64,
    authenticated: bool,
    joined: HashSet<String>,
    pending: HashMap<RequestId, RoomRequest>,
}

impl HistoryRequests {
    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.authenticated = false;
        self.joined.clear();
        self.pending.clear();
    }

    pub fn authenticated(&mut self) {
        self.authenticated = true;
        self.joined(crate::client::LOBBY);
    }

    /// Record only an accepted canonical RoomInfo, not an optimistic UI tab.
    pub fn joined(&mut self, room: &str) {
        self.joined.insert(room.trim().to_lowercase());
    }

    pub fn begin(&mut self, generation: u64, id: RequestId, request: RoomRequest) -> bool {
        if generation != self.generation
            || !self.authenticated
            || matches!(&request, RoomRequest::History(room) if !self.joined.contains(&room.trim().to_lowercase()))
            || self.pending.len() >= MAX_PENDING
            || self.pending.contains_key(&id)
            || self.pending.values().any(|p| p == &request)
        {
            return false;
        }
        self.pending.insert(id, request);
        true
    }

    pub fn take(&mut self, generation: u64, frame: &Frame) -> Option<RoomRequest> {
        if generation != self.generation || frame.kind != FrameKind::Reply {
            return None;
        }
        self.pending.remove(&frame.id)
    }

    pub fn forget(&mut self, room: &str) {
        let room = room.trim().to_lowercase();
        self.joined.remove(&room);
        self.pending
            .retain(|_, request| request.room().trim().to_lowercase() != room);
    }
}

/// Credits represent history lines not yet received on this socket, whose live
/// push may still arrive. They are consumed once per occurrence and cleared
/// on a connection boundary. Current servers assign increasing timestamps
/// within each room, distinguishing a later identical send from this overlap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryOverlap {
    pending: VecDeque<ChatLine>,
    received: VecDeque<ChatLine>,
}

impl HistoryOverlap {
    pub fn reset(&mut self) {
        self.pending.clear();
        self.received.clear();
    }

    /// Stable multiset union: preserve the greater count of each exact line,
    /// add older missing lines, and retain server order for equal timestamps.
    /// Unrelated room payloads are rejected by the wire decoder as well.
    pub fn merge(
        &mut self,
        messages: &mut Vec<ChatLine>,
        room: &str,
        history: Vec<ChatLine>,
    ) -> usize {
        let history: Vec<_> = history
            .into_iter()
            .filter(|line| line.room == room)
            .collect();
        let mut existing = HashMap::<ChatLine, usize>::new();
        let mut received = HashMap::<ChatLine, usize>::new();
        for line in messages.iter().filter(|line| line.room == room) {
            *existing.entry(line.clone()).or_default() += 1;
        }
        for line in self.received.iter().filter(|line| line.room == room) {
            *received.entry(line.clone()).or_default() += 1;
        }
        self.pending.retain(|line| line.room != room);
        let before = messages.len();
        // History's order wins equal-timestamp ties, including when the tail
        // was already seen live before an older line was backfilled.
        let mut merged = history.clone();
        for line in history {
            let remaining = existing.entry(line.clone()).or_default();
            *remaining = remaining.saturating_sub(1);
            let live = received.entry(line.clone()).or_default();
            if *live > 0 {
                *live -= 1;
            } else {
                self.pending.push_back(line);
            }
        }
        for line in messages.drain(..) {
            if line.room != room {
                merged.push(line);
            } else {
                let remaining = existing.entry(line.clone()).or_default();
                if *remaining > 0 {
                    *remaining -= 1;
                    merged.push(line);
                }
            }
        }
        while self.pending.len() > MAX_OVERLAP {
            self.pending.pop_front();
        }
        merged.sort_by_key(|line| line.at_unix_ms);
        *messages = merged;
        messages.len() - before
    }

    /// A delayed push already represented by history is not a new message.
    pub fn push(&mut self, messages: &mut Vec<ChatLine>, line: ChatLine) -> bool {
        self.received.push_back(line.clone());
        if self.received.len() > MAX_OVERLAP {
            self.received.pop_front();
        }
        if let Some(index) = self.pending.iter().position(|pending| pending == &line) {
            self.pending.remove(index);
            return false;
        }
        // Inserting after equal timestamps preserves the order in which a
        // legacy server delivered distinct lines within the same millisecond.
        messages.sort_by_key(|old| old.at_unix_ms);
        let index = messages.partition_point(|old| old.at_unix_ms <= line.at_unix_ms);
        messages.insert(index, line);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::chat::{ChatHistory, ChatHistoryRequest};
    use rabbithole_proto::ErrorCode;

    fn line(room: &str, text: &str, at: i64) -> ChatLine {
        ChatLine {
            room: room.into(),
            from: "rabbit".into(),
            text: text.into(),
            at_unix_ms: at,
        }
    }

    fn requests() -> HistoryRequests {
        let mut requests = HistoryRequests::default();
        requests.reset(1);
        requests.authenticated();
        requests
    }

    fn reply(id: u64) -> Frame {
        Frame::reply_to(
            &Frame::request(RequestId(id), &ChatHistoryRequest::new("unused", 500)).unwrap(),
            &ChatHistory::default(),
        )
        .unwrap()
    }

    #[test]
    fn empty_out_of_order_replies_keep_the_origin_room_and_burrow() {
        let mut a = requests();
        let mut b = requests();
        a.joined("music");
        b.joined("private");
        assert!(a.begin(1, RequestId(1), RoomRequest::History("lobby".into())));
        assert!(a.begin(1, RequestId(2), RoomRequest::History("music".into())));
        assert!(b.begin(1, RequestId(1), RoomRequest::History("private".into())));
        assert_eq!(
            a.take(1, &reply(2)),
            Some(RoomRequest::History("music".into()))
        );
        assert_eq!(
            b.take(1, &reply(1)),
            Some(RoomRequest::History("private".into()))
        );
        assert_eq!(
            a.take(1, &reply(1)),
            Some(RoomRequest::History("lobby".into()))
        );
        assert_eq!(a.take(1, &reply(1)), None);
    }

    #[test]
    fn pushes_errors_reconnects_and_closed_rooms_cannot_misroute_history() {
        let mut requests = requests();
        let room = RoomRequest::Open("private".into());
        assert!(requests.begin(1, RequestId(1), room.clone()));
        let mut push = reply(1);
        push.kind = FrameKind::Push;
        assert_eq!(requests.take(1, &push), None);
        let refused = Frame::error_reply(&reply(1), ErrorCode::NotFound);
        assert_eq!(requests.take(1, &refused), Some(room.clone()));
        assert!(requests.begin(1, RequestId(2), room.clone()));
        requests.reset(2);
        assert_eq!(requests.take(1, &reply(2)), None);
        assert_eq!(requests.take(2, &reply(2)), None);
        assert!(!requests.begin(2, RequestId(3), room.clone()));
        requests.authenticated();
        assert!(requests.begin(2, RequestId(3), room));
        requests.forget("private");
        assert_eq!(requests.take(2, &reply(3)), None);
    }

    #[test]
    fn unanswered_requests_are_bounded_and_coalesced() {
        let mut requests = requests();
        for id in 0..MAX_PENDING {
            requests.joined(&id.to_string());
            assert!(requests.begin(
                1,
                RequestId(id as u64),
                RoomRequest::History(id.to_string())
            ));
        }
        requests.joined("extra");
        assert!(!requests.begin(1, RequestId(100), RoomRequest::History("extra".into())));
        requests.take(1, &reply(0));
        assert!(!requests.begin(1, RequestId(100), RoomRequest::History("1".into())));
        assert!(requests.begin(1, RequestId(100), RoomRequest::History("extra".into())));
    }

    #[test]
    fn a_leave_cancels_history_queued_after_an_accepted_join() {
        let mut requests = requests();
        requests.joined("Music Room");
        requests.forget(" music room ");
        assert!(!requests.begin(1, RequestId(1), RoomRequest::History("Music Room".into())));
        // A subsequent real join admits it again, on this socket only.
        requests.joined("Music Room");
        assert!(requests.begin(1, RequestId(2), RoomRequest::History("Music Room".into())));
        requests.reset(2);
        requests.authenticated();
        assert!(!requests.begin(2, RequestId(3), RoomRequest::History("Music Room".into())));
        assert!(requests.begin(2, RequestId(4), RoomRequest::History("lobby".into())));
    }

    #[test]
    fn backfill_merges_live_overlap_and_sorts_without_losing_repeated_lines() {
        let first = line("lobby", "same", 10);
        let live = line("lobby", "live", 30);
        let other = line("music", "elsewhere", 20);
        let mut messages = vec![];
        let mut overlap = HistoryOverlap::default();
        for line in [first.clone(), other.clone(), live.clone()] {
            overlap.push(&mut messages, line);
        }
        assert_eq!(
            overlap.merge(
                &mut messages,
                "lobby",
                vec![first.clone(), first.clone(), live.clone()]
            ),
            1
        );
        assert_eq!(messages, vec![first.clone(), first.clone(), other, live]);
        assert!(!overlap.push(&mut messages, first.clone()));
        // A third identical live occurrence survives: this is a multiset,
        // not a permanent deduplication set.
        assert!(overlap.push(&mut messages, first));
        assert_eq!(messages.iter().filter(|m| m.text == "same").count(), 3);
    }

    #[test]
    fn history_before_push_is_idempotent_and_a_new_identical_send_survives() {
        let old = line("lobby", "hello", 10);
        let mut messages = vec![];
        let mut overlap = HistoryOverlap::default();
        assert_eq!(
            overlap.merge(&mut messages, "lobby", vec![old.clone(), old.clone()]),
            2
        );
        assert_eq!(
            overlap.merge(&mut messages, "lobby", vec![old.clone(), old.clone()]),
            0
        );
        assert!(!overlap.push(&mut messages, old.clone()));
        assert!(!overlap.push(&mut messages, old));
        assert!(overlap.push(&mut messages, line("lobby", "hello", 11)));
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn unrelated_rooms_and_connection_boundaries_never_consume_overlap() {
        let mut messages = vec![];
        let mut overlap = HistoryOverlap::default();
        let old = line("lobby", "hello", 10);
        overlap.merge(
            &mut messages,
            "lobby",
            vec![old.clone(), line("private", "secret", 10)],
        );
        assert_eq!(messages, vec![old.clone()]);
        assert!(overlap.push(&mut messages, line("music", "hello", 10)));
        overlap.reset();
        assert!(overlap.push(&mut messages, old));
    }

    #[test]
    fn backfill_restores_server_order_for_equal_timestamp_tail_seen_live_first() {
        let a = line("lobby", "first", 10);
        let b = line("lobby", "second", 10);
        let mut messages = vec![];
        let mut overlap = HistoryOverlap::default();
        overlap.push(&mut messages, b.clone());
        overlap.merge(&mut messages, "lobby", vec![a.clone(), b.clone()]);
        assert_eq!(messages, vec![a, b]);
    }

    #[test]
    fn reconnect_history_credits_existing_lines_without_inventing_new_rows() {
        let old = line("lobby", "seen before disconnect", 10);
        let mut messages = vec![old.clone()];
        let mut overlap = HistoryOverlap::default();
        assert_eq!(overlap.merge(&mut messages, "lobby", vec![old.clone()]), 0);
        assert!(!overlap.push(&mut messages, old.clone()));
        assert_eq!(overlap.merge(&mut messages, "lobby", vec![old.clone()]), 0);
        assert!(overlap.push(&mut messages, line("lobby", &old.text, 11)));
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn notices_and_seeded_rows_cannot_break_sorted_live_insertion() {
        let mut state = crate::state::UiState {
            messages: vec![line("lobby", "future", i64::MAX), line("music", "past", 1)],
            ..Default::default()
        };
        state.push_notice("server", "local clock notice");
        state.push_chat(line("lobby", "new live", i64::MAX - 1));
        state.merge_chat_history("lobby", vec![line("lobby", "old history", 2)]);
        assert_eq!(state.messages.len(), 5);
        assert!(state
            .messages
            .windows(2)
            .all(|w| w[0].at_unix_ms <= w[1].at_unix_ms));
        let chat: Vec<_> = state
            .messages
            .iter()
            .filter(|line| !line.from.starts_with("! "))
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(chat, vec!["past", "old history", "new live", "future"]);
        assert_eq!(state.messages[4].text, "future");
    }
}
