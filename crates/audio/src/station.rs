//! Station fan-out: one live source, many listeners, lag never blocks.
//!
//! A [`Station`] is the transport-agnostic heart of a radio mount: the live
//! source (DJ pipe or playlist engine) pushes [`Frame`]s and [`NowPlaying`]
//! updates in, and every subscribed [`Listener`] receives them as
//! [`StationEvent`]s. Built on [`tokio::sync::broadcast`], so delivery is
//! fire-and-forget: a listener that stops polling simply falls behind, has
//! the overwritten events counted as lag, and resumes from the oldest
//! retained event — the source never blocks on anyone. Actual transports
//! (QUIC streams, HTTP/ICY mounts) subscribe like any other listener in
//! later slices.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::Frame;

/// Track metadata for the currently playing item on a station.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NowPlaying {
    /// Track title.
    pub title: String,
    /// Track artist.
    pub artist: String,
    /// Name of the DJ (or automation) sourcing the stream.
    pub dj: String,
}

/// One event delivered to station listeners.
///
/// Frames are shared via [`Arc`] so fanning out to N listeners clones a
/// pointer, not a sample buffer.
#[derive(Clone, Debug)]
pub enum StationEvent {
    /// One 20 ms PCM frame of station audio.
    Frame(Arc<Frame>),
    /// The station's track metadata changed.
    NowPlaying(NowPlaying),
}

/// A broadcast hub: one live source feeding frames to many listeners.
///
/// Created with a bounded event capacity; when a listener falls more than
/// that many events behind, its oldest events are overwritten (skipped and
/// counted, never blocking the source).
#[derive(Debug)]
pub struct Station {
    name: String,
    tx: broadcast::Sender<StationEvent>,
    now_playing: Mutex<Option<NowPlaying>>,
}

impl Station {
    /// Creates a station retaining up to `capacity` events per listener.
    ///
    /// `capacity` is clamped to at least 1. At 50 frames per second, a
    /// capacity of 64 retains roughly 1.3 s of audio for slow listeners.
    pub fn new(name: impl Into<String>, capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self {
            name: name.into(),
            tx,
            now_playing: Mutex::new(None),
        }
    }

    /// The station's name (its mount identity for later transports).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of currently subscribed listeners.
    pub fn listener_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Subscribes a new listener; it receives events sent from now on.
    pub fn subscribe(&self) -> Listener {
        Listener {
            rx: self.tx.subscribe(),
            lagged: 0,
        }
    }

    /// Broadcasts one PCM frame to all listeners.
    ///
    /// Returns the number of listeners the frame was queued for (0 when
    /// nobody is tuned in — never an error; dead air is cheap).
    pub fn broadcast_frame(&self, frame: Frame) -> usize {
        self.tx
            .send(StationEvent::Frame(Arc::new(frame)))
            .unwrap_or(0)
    }

    /// Updates the station's metadata and broadcasts it as an event.
    ///
    /// Returns the number of listeners the update was queued for.
    pub fn set_now_playing(&self, now_playing: NowPlaying) -> usize {
        *self.now_playing.lock().expect("now_playing lock poisoned") = Some(now_playing.clone());
        self.tx
            .send(StationEvent::NowPlaying(now_playing))
            .unwrap_or(0)
    }

    /// The most recently set metadata, if any (for late tuners and status
    /// lines that poll rather than subscribe).
    pub fn now_playing(&self) -> Option<NowPlaying> {
        self.now_playing
            .lock()
            .expect("now_playing lock poisoned")
            .clone()
    }
}

/// One listener's view of a [`Station`]: an event stream plus a lag count.
#[derive(Debug)]
pub struct Listener {
    rx: broadcast::Receiver<StationEvent>,
    lagged: u64,
}

impl Listener {
    /// Receives the next event, waiting if none is ready.
    ///
    /// If this listener fell behind and events were overwritten, the skipped
    /// count is added to [`Listener::lag_count`] and reception resumes from
    /// the oldest retained event. Returns `None` once the station is dropped
    /// and all buffered events are drained.
    pub async fn recv(&mut self) -> Option<StationEvent> {
        loop {
            match self.rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.lagged += skipped;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Non-blocking receive: `None` when no event is ready right now (or
    /// the station is gone). Lag is accounted exactly as in
    /// [`Listener::recv`].
    pub fn try_recv(&mut self) -> Option<StationEvent> {
        loop {
            match self.rx.try_recv() {
                Ok(event) => return Some(event),
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    self.lagged += skipped;
                }
                Err(broadcast::error::TryRecvError::Empty)
                | Err(broadcast::error::TryRecvError::Closed) => return None,
            }
        }
    }

    /// Total events this listener has skipped by falling behind.
    pub fn lag_count(&self) -> u64 {
        self.lagged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioFormat;

    fn mono8k() -> AudioFormat {
        AudioFormat::new(8_000, 1).unwrap()
    }

    fn tone(value: i16) -> Frame {
        let f = mono8k();
        Frame::from_samples(f, vec![value; f.samples_per_frame()]).unwrap()
    }

    fn metadata(title: &str) -> NowPlaying {
        NowPlaying {
            title: title.into(),
            artist: "The Lagomorphs".into(),
            dj: "dj_hole".into(),
        }
    }

    #[tokio::test]
    async fn two_listeners_get_the_same_frames_and_metadata() {
        let station = Station::new("wrbt", 16);
        let mut a = station.subscribe();
        let mut b = station.subscribe();
        assert_eq!(station.listener_count(), 2);

        assert_eq!(station.broadcast_frame(tone(1)), 2);
        assert_eq!(station.set_now_playing(metadata("Carrot Top 40")), 2);
        assert_eq!(station.broadcast_frame(tone(2)), 2);

        for listener in [&mut a, &mut b] {
            match listener.recv().await {
                Some(StationEvent::Frame(f)) => assert_eq!(f.samples()[0], 1),
                other => panic!("expected frame, got {other:?}"),
            }
            match listener.recv().await {
                Some(StationEvent::NowPlaying(np)) => assert_eq!(np.title, "Carrot Top 40"),
                other => panic!("expected metadata, got {other:?}"),
            }
            match listener.recv().await {
                Some(StationEvent::Frame(f)) => assert_eq!(f.samples()[0], 2),
                other => panic!("expected frame, got {other:?}"),
            }
            assert_eq!(listener.lag_count(), 0);
        }
        assert_eq!(station.now_playing(), Some(metadata("Carrot Top 40")));
    }

    #[tokio::test]
    async fn slow_listener_skips_and_counts_lag_without_stalling_the_source() {
        let station = Station::new("wrbt", 4);
        let mut slow = station.subscribe();

        // The source pushes 20 frames while the listener never polls; every
        // send returns immediately (broadcast never blocks the sender).
        for i in 0..20 {
            assert_eq!(station.broadcast_frame(tone(i)), 1);
        }

        // The listener wakes up: 16 events were overwritten, and it resumes
        // at the oldest retained frame (16..=19).
        let first = slow.recv().await.expect("station still live");
        assert_eq!(slow.lag_count(), 16);
        match first {
            StationEvent::Frame(f) => assert_eq!(f.samples()[0], 16),
            other => panic!("expected frame, got {other:?}"),
        }
        for expected in 17..20 {
            match slow.recv().await {
                Some(StationEvent::Frame(f)) => assert_eq!(f.samples()[0], expected),
                other => panic!("expected frame, got {other:?}"),
            }
        }
        assert!(slow.try_recv().is_none());
    }

    #[tokio::test]
    async fn recv_ends_when_station_is_dropped() {
        let station = Station::new("wrbt", 4);
        let mut listener = station.subscribe();
        station.broadcast_frame(tone(7));
        drop(station);
        assert!(matches!(
            listener.recv().await,
            Some(StationEvent::Frame(_))
        ));
        assert!(listener.recv().await.is_none());
    }

    #[test]
    fn broadcasting_with_no_listeners_is_fine() {
        let station = Station::new("wrbt", 4);
        assert_eq!(station.broadcast_frame(tone(0)), 0);
        assert_eq!(station.set_now_playing(metadata("Dead Air")), 0);
        assert_eq!(station.now_playing(), Some(metadata("Dead Air")));
        assert_eq!(station.name(), "wrbt");
    }
}
