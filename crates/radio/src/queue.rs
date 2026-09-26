//! Listener request queue with upvoting.
//!
//! Between rotation picks, listeners steer a station by *requesting* tracks and
//! *upvoting* the requests they want to hear next. The scheduler drains the
//! highest-voted request first (see [`StationController`](crate::StationController)),
//! falling back to the playlist only when the queue is empty.
//!
//! Two rules keep the queue fair and deterministic:
//!
//! - **Dedupe.** A track already queued cannot be enqueued again; the duplicate
//!   request is rejected so a single track can't hog multiple slots.
//! - **One vote per listener per request.** Votes are counted as a set of
//!   listener ids, so a listener double-clicking upvote (or re-requesting)
//!   never inflates the tally. The initial request counts as its requester's
//!   first vote.
//!
//! Ties break by insertion order (oldest request wins), so ordering is total
//! and reproducible.

use std::collections::HashSet;

use crate::error::RadioError;
use crate::track::{Track, TrackId};

/// A pending request: a track, its voters, and its arrival order.
#[derive(Clone, Debug)]
pub struct QueuedRequest {
    track: Track,
    voters: HashSet<String>,
    /// Who asked for it, as opposed to who joined in afterwards: what a
    /// fair-use cap counts.
    requester: String,
    seq: u64,
}

impl QueuedRequest {
    /// The requested track.
    pub fn track(&self) -> &Track {
        &self.track
    }

    /// Current vote count (number of distinct listeners backing it).
    pub fn votes(&self) -> u32 {
        self.voters.len() as u32
    }

    /// Insertion order; lower values were requested earlier.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Who asked for it.
    pub fn requester(&self) -> &str {
        &self.requester
    }

    /// Whether `listener` asked for it or has voted for it since.
    pub fn backed_by(&self, listener: &str) -> bool {
        self.voters.contains(listener)
    }
}

/// A vote-ranked queue of listener track requests.
#[derive(Clone, Debug, Default)]
pub struct RequestQueue {
    requests: Vec<QueuedRequest>,
    next_seq: u64,
}

impl RequestQueue {
    /// Creates an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of pending requests.
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    /// Whether the queue holds no requests.
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Whether a track is already queued.
    pub fn contains(&self, id: TrackId) -> bool {
        self.requests.iter().any(|r| r.track.id == id)
    }

    /// Enqueues a new track request from `listener`, counting as its first
    /// vote.
    ///
    /// Returns [`RadioError::TrackAlreadyQueued`] if the track is already in
    /// the queue (dedupe): re-requesting an existing track does not add a slot.
    /// To back an existing request, call [`RequestQueue::upvote`] instead.
    pub fn enqueue(&mut self, track: Track, listener: impl Into<String>) -> Result<(), RadioError> {
        if self.contains(track.id) {
            return Err(RadioError::TrackAlreadyQueued(track.id));
        }
        let requester = listener.into();
        let mut voters = HashSet::new();
        voters.insert(requester.clone());
        let seq = self.next_seq;
        self.next_seq += 1;
        self.requests.push(QueuedRequest {
            track,
            voters,
            requester,
            seq,
        });
        Ok(())
    }

    /// How many requests waiting in the queue `listener` asked for — not
    /// the ones they only voted for. What a station counts before letting
    /// one person ask for more.
    pub fn requests_by(&self, listener: &str) -> usize {
        self.requests
            .iter()
            .filter(|r| r.requester == listener)
            .count()
    }

    /// Every request waiting, in the order they will play: most votes
    /// first, and the earlier of two equally wanted.
    pub fn in_play_order(&self) -> Vec<&QueuedRequest> {
        let mut all: Vec<&QueuedRequest> = self.requests.iter().collect();
        all.sort_by(|a, b| b.votes().cmp(&a.votes()).then_with(|| a.seq.cmp(&b.seq)));
        all
    }

    /// Adds `listener`'s vote to an already-queued track and returns its new
    /// vote count.
    ///
    /// Idempotent per listener: a listener who has already voted leaves the
    /// tally unchanged. Returns [`RadioError::TrackNotQueued`] if the track is
    /// not in the queue.
    pub fn upvote(&mut self, id: TrackId, listener: impl Into<String>) -> Result<u32, RadioError> {
        let listener = listener.into();
        let request = self
            .requests
            .iter_mut()
            .find(|r| r.track.id == id)
            .ok_or(RadioError::TrackNotQueued(id))?;
        request.voters.insert(listener);
        Ok(request.votes())
    }

    /// Index of the winning request: most votes, ties broken by earliest
    /// insertion.
    fn winner_index(&self) -> Option<usize> {
        self.requests
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.votes()
                    .cmp(&b.votes())
                    // Fewer/earlier seq wins ties, so invert the seq compare.
                    .then_with(|| b.seq.cmp(&a.seq))
            })
            .map(|(i, _)| i)
    }

    /// Keeps only the requests for tracks still in `tracks`, and brings the
    /// ones that stay up to date with them (a renamed file is asked for
    /// under its new name). Returns how many were dropped: a request for a
    /// file that has gone cannot be played, and left in place it would sit
    /// at the head of the queue for good.
    pub fn keep_only(&mut self, tracks: &[Track]) -> usize {
        let before = self.requests.len();
        self.requests
            .retain_mut(|r| match tracks.iter().find(|t| t.id == r.track.id) {
                Some(t) => {
                    r.track = t.clone();
                    true
                }
                None => false,
            });
        before - self.requests.len()
    }

    /// Returns the highest-voted request without removing it.
    pub fn peek(&self) -> Option<&QueuedRequest> {
        self.winner_index().map(|i| &self.requests[i])
    }

    /// Removes and returns the highest-voted request (ties: oldest first).
    pub fn pop_next(&mut self) -> Option<QueuedRequest> {
        let index = self.winner_index()?;
        Some(self.requests.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::BlobId;

    fn track(id: u64) -> Track {
        Track::new(TrackId(id), format!("t{id}"), "artist", 1_000, BlobId::ZERO)
    }

    #[test]
    fn requests_follow_the_rotation() {
        let mut q = RequestQueue::new();
        q.enqueue(track(1), "alice").unwrap();
        q.enqueue(track(2), "bob").unwrap();
        let mut renamed = track(1);
        renamed.title = "One (live)".into();
        // 2 went; 1 was renamed.
        assert_eq!(q.keep_only(&[renamed, track(3)]), 1);
        assert_eq!(q.len(), 1);
        assert_eq!(q.peek().unwrap().track().title, "One (live)");
    }

    #[test]
    fn highest_voted_request_pops_first() {
        let mut q = RequestQueue::new();
        q.enqueue(track(1), "alice").unwrap();
        q.enqueue(track(2), "bob").unwrap();
        q.enqueue(track(3), "carol").unwrap();

        // Track 2 gathers the most votes.
        q.upvote(TrackId(2), "dave").unwrap();
        q.upvote(TrackId(2), "erin").unwrap();
        q.upvote(TrackId(3), "frank").unwrap();

        assert_eq!(q.peek().unwrap().track().id, TrackId(2));
        assert_eq!(q.pop_next().unwrap().track().id, TrackId(2));
        // Then track 3 (2 votes) beats track 1 (1 vote).
        assert_eq!(q.pop_next().unwrap().track().id, TrackId(3));
        assert_eq!(q.pop_next().unwrap().track().id, TrackId(1));
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn ties_break_by_insertion_order() {
        let mut q = RequestQueue::new();
        q.enqueue(track(10), "a").unwrap();
        q.enqueue(track(11), "b").unwrap();
        // Both have exactly one vote; the earlier request wins.
        assert_eq!(q.pop_next().unwrap().track().id, TrackId(10));
        assert_eq!(q.pop_next().unwrap().track().id, TrackId(11));
    }

    #[test]
    fn a_queued_track_is_deduped() {
        let mut q = RequestQueue::new();
        q.enqueue(track(1), "alice").unwrap();
        let err = q.enqueue(track(1), "bob").unwrap_err();
        assert_eq!(err, RadioError::TrackAlreadyQueued(TrackId(1)));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn votes_are_one_per_listener() {
        let mut q = RequestQueue::new();
        q.enqueue(track(1), "alice").unwrap(); // alice's first vote
        assert_eq!(q.peek().unwrap().votes(), 1);
        // Alice voting again changes nothing.
        assert_eq!(q.upvote(TrackId(1), "alice").unwrap(), 1);
        // A distinct listener bumps the tally.
        assert_eq!(q.upvote(TrackId(1), "bob").unwrap(), 2);
    }

    #[test]
    fn upvoting_a_missing_track_errors() {
        let mut q = RequestQueue::new();
        let err = q.upvote(TrackId(99), "alice").unwrap_err();
        assert_eq!(err, RadioError::TrackNotQueued(TrackId(99)));
    }

    #[test]
    fn a_queue_knows_who_asked_and_plays_in_the_order_it_says() {
        let mut q = RequestQueue::new();
        q.enqueue(track(1), "alice").unwrap();
        q.enqueue(track(2), "alice").unwrap();
        q.enqueue(track(3), "bob").unwrap();
        q.upvote(TrackId(3), "carol").unwrap();
        q.upvote(TrackId(1), "bob").unwrap();

        // Asking is not the same as voting: bob backs two, asked for one.
        assert_eq!(q.requests_by("alice"), 2);
        assert_eq!(q.requests_by("bob"), 1);
        assert_eq!(q.requests_by("carol"), 0);

        // The order the list is shown in is the order it will play.
        let order: Vec<u64> = q.in_play_order().iter().map(|r| r.track().id.0).collect();
        assert_eq!(
            order,
            vec![1, 3, 2],
            "two votes each, then the earlier; one vote last"
        );
        assert_eq!(
            q.peek().unwrap().track().id.0,
            order[0],
            "and it agrees with what plays"
        );

        let first = &q.in_play_order()[0];
        assert_eq!(first.requester(), "alice");
        assert!(first.backed_by("bob"), "bob voted for it");
        assert!(!first.backed_by("carol"));
    }
}
