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
        let mut voters = HashSet::new();
        voters.insert(listener.into());
        let seq = self.next_seq;
        self.next_seq += 1;
        self.requests.push(QueuedRequest { track, voters, seq });
        Ok(())
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
}
