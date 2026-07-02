//! Rotation engine: an ordered track list with a play cursor.
//!
//! A [`Playlist`] owns the *default* programming for a station — what plays
//! when no listener request is pending. It supports three deterministic
//! [`RotationMode`]s and a cursor that wraps around the end of the list. All
//! randomness (shuffle) is seeded and reproducible: the same tracks and seed
//! always yield the same order, so tests and replays are exact.
//!
//! The cursor starts *before* the first track: [`Playlist::current`] is `None`
//! until the first [`Playlist::advance`], which returns the opening track. This
//! lets a controller treat "start playing" and "advance to next" as the same
//! call.

use crate::track::Track;

/// How a [`Playlist`] moves from one track to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationMode {
    /// Play tracks in list order, wrapping from the last back to the first.
    Sequential,
    /// Play a deterministic shuffle of the list, wrapping around.
    ///
    /// The `seed` fully determines the order; the RNG is injected as this
    /// value rather than drawn from the environment, keeping rotation
    /// reproducible.
    Shuffle {
        /// Seed for the deterministic shuffle permutation.
        seed: u64,
    },
    /// Repeat the current track forever; [`Playlist::advance`] is a no-op.
    RepeatOne,
}

/// An ordered list of tracks plus a wrapping play cursor.
#[derive(Clone, Debug)]
pub struct Playlist {
    tracks: Vec<Track>,
    mode: RotationMode,
    /// Play order as indices into `tracks` (identity for sequential/repeat,
    /// a seeded permutation for shuffle).
    order: Vec<usize>,
    /// Position within `order`; `None` means "not started yet".
    cursor: Option<usize>,
}

impl Playlist {
    /// Builds a playlist from tracks and a rotation mode.
    ///
    /// The cursor starts unset, so [`Playlist::current`] is `None` until the
    /// first [`Playlist::advance`].
    pub fn new(tracks: Vec<Track>, mode: RotationMode) -> Self {
        let order = compute_order(&tracks, mode);
        Self {
            tracks,
            mode,
            order,
            cursor: None,
        }
    }

    /// The active rotation mode.
    pub fn mode(&self) -> RotationMode {
        self.mode
    }

    /// Changes the rotation mode, recomputing the play order and resetting the
    /// cursor to "not started".
    pub fn set_mode(&mut self, mode: RotationMode) {
        self.mode = mode;
        self.order = compute_order(&self.tracks, mode);
        self.cursor = None;
    }

    /// Number of tracks in the playlist.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether the playlist has no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Appends a track, rebuilding the play order and resetting the cursor.
    pub fn push(&mut self, track: Track) {
        self.tracks.push(track);
        self.order = compute_order(&self.tracks, self.mode);
        self.cursor = None;
    }

    /// The track under the cursor, or `None` if the playlist is empty or has
    /// not been started with [`Playlist::advance`] yet.
    pub fn current(&self) -> Option<&Track> {
        let cursor = self.cursor?;
        let idx = *self.order.get(cursor)?;
        self.tracks.get(idx)
    }

    /// Advances the cursor and returns the newly current track.
    ///
    /// The first call starts playout at the opening track. Sequential and
    /// shuffle modes wrap from the end back to the start; [`RotationMode::RepeatOne`]
    /// holds on the current track. Returns `None` only when the playlist is
    /// empty.
    pub fn advance(&mut self) -> Option<&Track> {
        if self.order.is_empty() {
            return None;
        }
        let next = match (self.cursor, self.mode) {
            (None, _) => 0,
            (Some(c), RotationMode::RepeatOne) => c,
            (Some(c), _) => (c + 1) % self.order.len(),
        };
        self.cursor = Some(next);
        self.current()
    }
}

/// Builds the play order for a set of tracks under a rotation mode.
fn compute_order(tracks: &[Track], mode: RotationMode) -> Vec<usize> {
    let mut order: Vec<usize> = (0..tracks.len()).collect();
    if let RotationMode::Shuffle { seed } = mode {
        shuffle(&mut order, seed);
    }
    order
}

/// In-place Fisher–Yates shuffle driven by a seeded SplitMix64 stream, so the
/// permutation is a pure function of `(slice contents length, seed)`.
fn shuffle(order: &mut [usize], seed: u64) {
    let mut state = seed;
    // Fisher–Yates from the top down.
    for i in (1..order.len()).rev() {
        let j = (next_u64(&mut state) % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
}

/// One step of the SplitMix64 PRNG — a tiny, allocation-free, deterministic
/// generator (avoids pulling in an RNG crate and keeps shuffle reproducible).
fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::{BlobId, TrackId};

    fn track(id: u64) -> Track {
        Track::new(TrackId(id), format!("t{id}"), "artist", 1_000, BlobId::ZERO)
    }

    fn ids(tracks: &[Track]) -> Vec<u64> {
        tracks.iter().map(|t| t.id.0).collect()
    }

    #[test]
    fn sequential_wraps_around() {
        let mut pl = Playlist::new(vec![track(1), track(2), track(3)], RotationMode::Sequential);
        assert!(pl.current().is_none());
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(pl.advance().unwrap().id.0);
        }
        assert_eq!(seen, vec![1, 2, 3, 1, 2]);
    }

    #[test]
    fn repeat_one_holds_position() {
        let mut pl = Playlist::new(vec![track(10), track(11)], RotationMode::RepeatOne);
        assert_eq!(pl.advance().unwrap().id.0, 10);
        assert_eq!(pl.advance().unwrap().id.0, 10);
        assert_eq!(pl.advance().unwrap().id.0, 10);
    }

    #[test]
    fn shuffle_is_deterministic_for_a_seed() {
        let tracks = || vec![track(1), track(2), track(3), track(4), track(5)];
        let mut a = Playlist::new(tracks(), RotationMode::Shuffle { seed: 42 });
        let mut b = Playlist::new(tracks(), RotationMode::Shuffle { seed: 42 });
        let mut c = Playlist::new(tracks(), RotationMode::Shuffle { seed: 7 });

        let order_a: Vec<u64> = (0..5).map(|_| a.advance().unwrap().id.0).collect();
        let order_b: Vec<u64> = (0..5).map(|_| b.advance().unwrap().id.0).collect();
        let order_c: Vec<u64> = (0..5).map(|_| c.advance().unwrap().id.0).collect();

        assert_eq!(order_a, order_b, "same seed => same order");
        assert_ne!(order_a, order_c, "different seed => different order");
        // A permutation: every original track appears exactly once per cycle.
        let mut sorted = order_a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn shuffle_wraps_over_the_same_permutation() {
        let mut pl = Playlist::new(
            vec![track(1), track(2), track(3)],
            RotationMode::Shuffle { seed: 99 },
        );
        let first: Vec<u64> = (0..3).map(|_| pl.advance().unwrap().id.0).collect();
        let second: Vec<u64> = (0..3).map(|_| pl.advance().unwrap().id.0).collect();
        assert_eq!(first, second, "cursor wraps over the same shuffle order");
    }

    #[test]
    fn empty_playlist_never_advances() {
        let mut pl = Playlist::new(Vec::new(), RotationMode::Sequential);
        assert!(pl.is_empty());
        assert!(pl.advance().is_none());
        assert!(pl.current().is_none());
    }

    #[test]
    fn push_and_set_mode_rebuild_order() {
        let mut pl = Playlist::new(vec![track(1)], RotationMode::Sequential);
        pl.push(track(2));
        assert_eq!(pl.len(), 2);
        pl.set_mode(RotationMode::Sequential);
        assert_eq!(ids(&pl.tracks), vec![1, 2]);
        assert_eq!(pl.advance().unwrap().id.0, 1);
    }
}
