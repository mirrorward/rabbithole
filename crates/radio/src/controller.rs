//! The controller: a playlist + vote queue driving one audio station.
//!
//! [`StationController`] is where scheduling logic meets the audio core. It owns
//! a [`Playlist`](crate::Playlist), a [`RequestQueue`](crate::RequestQueue), and
//! an audio [`Station`](rabbithole_audio::Station), and it answers two
//! questions: *what is playing now?* ([`now_playing`](StationController::now_playing) /
//! [`station_meta`](StationController::station_meta)) and *what plays next?*
//! ([`on_track_finished`](StationController::on_track_finished)).
//!
//! # The seam (what this does *not* do)
//!
//! This controller schedules and publishes **metadata**; it never produces or
//! pushes **audio frames**. When a track changes it updates the audio station's
//! now-playing text (a metadata event), but turning
//! [`Track::source`](crate::Track::source) into
//! [`Frame`](rabbithole_audio::Frame)s and calling
//! [`Station::broadcast_frame`](rabbithole_audio::Station::broadcast_frame) is
//! the encoder/transport slice's job. That slice reads
//! [`current_source`](StationController::current_source) to know which blob to
//! decode and subscribes to [`station`](StationController::station) to fan the
//! frames out. Keeping the seam here means scheduling stays pure and testable
//! with no codec or runtime in the loop.
//!
//! # Clock injection
//!
//! Scheduling needs a monotonic time source, but `Instant::now` may be
//! unavailable in this environment, so the caller passes a monotonic
//! millisecond timestamp (`now_ms`) into every time-sensitive call. The
//! controller stores no wall clock of its own.

use rabbithole_audio::{NowPlaying, Station};

use crate::playlist::Playlist;
use crate::queue::RequestQueue;
use crate::track::{BlobId, Track};

/// A directory-style metadata view of a station, mapped from the currently
/// playing track and live presence.
///
/// This is the radio layer's own metadata shape (a Wave-11 `legacy-icecast`
/// `StationMeta` crate does not yet exist); it mirrors the fields an ICY/HTTP
/// mount would advertise and is trivial to project onto one later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StationMeta {
    /// Mount path, e.g. `"/wrbt"`.
    pub mount: String,
    /// Human-readable station name.
    pub name: String,
    /// Longer description.
    pub description: String,
    /// Current track metadata, if anything is playing.
    pub now_playing: Option<NowPlaying>,
    /// Listener count as seen by the audio station's fan-out.
    pub listeners: usize,
}

/// Ties a [`Playlist`] and a [`RequestQueue`] to one audio [`Station`].
#[derive(Debug)]
pub struct StationController {
    station: Station,
    playlist: Playlist,
    queue: RequestQueue,
    description: String,
    dj: String,
    current: Option<Track>,
    started_at_ms: u64,
}

impl StationController {
    /// Builds a controller around an audio station and its default playlist.
    ///
    /// `dj` is the source name surfaced in [`NowPlaying::dj`]; nothing is
    /// playing until the first [`on_track_finished`](StationController::on_track_finished).
    pub fn new(
        station: Station,
        playlist: Playlist,
        description: impl Into<String>,
        dj: impl Into<String>,
    ) -> Self {
        Self {
            station,
            playlist,
            queue: RequestQueue::new(),
            description: description.into(),
            dj: dj.into(),
            current: None,
            started_at_ms: 0,
        }
    }

    /// Borrows the underlying audio station (so a transport can subscribe).
    pub fn station(&self) -> &Station {
        &self.station
    }

    /// Mutable access to the request queue (enqueue/upvote listener requests).
    pub fn queue_mut(&mut self) -> &mut RequestQueue {
        &mut self.queue
    }

    /// Read access to the request queue.
    pub fn queue(&self) -> &RequestQueue {
        &self.queue
    }

    /// The track currently playing, if any.
    pub fn current(&self) -> Option<&Track> {
        self.current.as_ref()
    }

    /// The opaque media handle of the current track (the blob the transport
    /// slice must decode). `None` when nothing is playing.
    pub fn current_source(&self) -> Option<BlobId> {
        self.current.as_ref().map(|t| t.source)
    }

    /// Now-playing metadata derived from the current track and DJ name.
    pub fn now_playing(&self) -> Option<NowPlaying> {
        self.current.as_ref().map(|t| NowPlaying {
            title: t.title.clone(),
            artist: t.artist.clone(),
            dj: self.dj.clone(),
        })
    }

    /// A [`StationMeta`] snapshot for directory/UX surfaces.
    pub fn station_meta(&self) -> StationMeta {
        StationMeta {
            mount: format!("/{}", self.station.name()),
            name: self.station.name().to_string(),
            description: self.description.clone(),
            now_playing: self.now_playing(),
            listeners: self.station.listener_count(),
        }
    }

    /// Milliseconds the current track has been playing, given `now_ms`.
    ///
    /// `0` when nothing is playing or `now_ms` predates the start.
    pub fn position_ms(&self, now_ms: u64) -> u64 {
        if self.current.is_none() {
            return 0;
        }
        now_ms.saturating_sub(self.started_at_ms)
    }

    /// Whether the current track has played for at least its full duration.
    pub fn is_finished(&self, now_ms: u64) -> bool {
        match &self.current {
            Some(track) => self.position_ms(now_ms) >= track.duration_ms,
            None => false,
        }
    }

    /// Selects the next track (highest-voted request first, else rotation),
    /// makes it current at `now_ms`, publishes its metadata to the audio
    /// station, and returns it.
    ///
    /// This is the single "advance" entry point: the first call starts playout.
    /// It does **not** push audio frames — see the module seam docs. Returns
    /// `None` only when both the queue and playlist are empty.
    pub fn on_track_finished(&mut self, now_ms: u64) -> Option<Track> {
        let next = self
            .queue
            .pop_next()
            .map(|request| request.track().clone())
            .or_else(|| self.playlist.advance().cloned());

        self.current = next.clone();
        self.started_at_ms = now_ms;

        if let Some(now_playing) = self.now_playing() {
            // Metadata event only; frames are the transport slice's job.
            self.station.set_now_playing(now_playing);
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::RotationMode;
    use crate::track::{BlobId, TrackId};

    fn track(id: u64, dur: u64) -> Track {
        Track::new(
            TrackId(id),
            format!("song {id}"),
            "The Lagomorphs",
            dur,
            BlobId([id as u8; 32]),
        )
    }

    fn controller() -> StationController {
        let station = Station::new("wrbt", 16);
        let playlist = Playlist::new(
            vec![track(1, 1_000), track(2, 2_000), track(3, 1_500)],
            RotationMode::Sequential,
        );
        StationController::new(station, playlist, "pirate radio", "dj_hole")
    }

    #[test]
    fn first_finish_starts_playout() {
        let mut c = controller();
        assert!(c.current().is_none());
        assert!(c.now_playing().is_none());

        let next = c.on_track_finished(0).unwrap();
        assert_eq!(next.id, TrackId(1));
        let np = c.now_playing().unwrap();
        assert_eq!(np.title, "song 1");
        assert_eq!(np.dj, "dj_hole");
        // Metadata was published to the audio station too.
        assert_eq!(c.station().now_playing(), Some(np));
    }

    #[test]
    fn rotation_advances_and_wraps() {
        let mut c = controller();
        let ids: Vec<u64> = (0..4)
            .map(|_| c.on_track_finished(0).unwrap().id.0)
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 1]);
    }

    #[test]
    fn requests_preempt_rotation() {
        let mut c = controller();
        // Start rotation at track 1.
        assert_eq!(c.on_track_finished(0).unwrap().id, TrackId(1));

        // A listener requests track 3; it should play before rotation's 2.
        c.queue_mut().enqueue(track(3, 1_500), "alice").unwrap();
        assert_eq!(c.on_track_finished(1_000).unwrap().id, TrackId(3));
        // Queue drained: rotation resumes at track 2.
        assert_eq!(c.on_track_finished(2_500).unwrap().id, TrackId(2));
    }

    #[test]
    fn position_and_finish_track_the_clock() {
        let mut c = controller();
        c.on_track_finished(1_000); // track 1, 1000 ms long, started at 1000
        assert_eq!(c.position_ms(1_400), 400);
        assert!(!c.is_finished(1_900));
        assert!(c.is_finished(2_000));
        assert!(c.is_finished(5_000));
    }

    #[test]
    fn station_meta_maps_current_track() {
        let mut c = controller();
        let empty = c.station_meta();
        assert_eq!(empty.mount, "/wrbt");
        assert_eq!(empty.name, "wrbt");
        assert!(empty.now_playing.is_none());

        c.on_track_finished(0);
        let meta = c.station_meta();
        assert_eq!(meta.now_playing.unwrap().title, "song 1");
        assert_eq!(c.current_source(), Some(BlobId([1u8; 32])));
    }

    #[test]
    fn empty_program_yields_nothing() {
        let station = Station::new("dead", 4);
        let playlist = Playlist::new(Vec::new(), RotationMode::Sequential);
        let mut c = StationController::new(station, playlist, "", "auto");
        assert!(c.on_track_finished(0).is_none());
        assert!(c.now_playing().is_none());
    }
}
