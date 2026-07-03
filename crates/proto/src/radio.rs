//! RADIO family (9): now-playing pushes for the pirate-radio stations
//! (Wave 11).
//!
//! These replace the interim `[radio]` [`crate::session::ServerNotice`] bridge:
//! a station's now-playing rides a typed [`RadioNowPlaying`] push, and a
//! sign-off rides [`RadioOff`], instead of a pipe-delimited string smuggled
//! through the generic notice channel. Both are **push-only** and ephemeral —
//! the server never offline-replays them (stale now-playing is meaningless).
//! Clients fold them into the same station model they render.

use serde::{Deserialize, Serialize};

use crate::{Family, Message};

/// Push: a station's now-playing changed (a track rotated, a DJ took over, or
/// the listener count moved). Server → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioNowPlaying {
    /// Station mount slug (e.g. `"live"`).
    pub station: String,
    /// Current track title (or the station name before the first track).
    pub title: String,
    /// Current track artist (may be empty).
    pub artist: String,
    /// The source name: a live DJ, or the automation label.
    pub dj: String,
    /// Listeners currently tuned in.
    pub listeners: u32,
    /// Whether a live DJ is sourcing the mount (vs. playlist automation).
    pub live: bool,
}

impl RadioNowPlaying {
    pub fn new(
        station: impl Into<String>,
        title: impl Into<String>,
        artist: impl Into<String>,
        dj: impl Into<String>,
        listeners: u32,
        live: bool,
    ) -> Self {
        Self {
            station: station.into(),
            title: title.into(),
            artist: artist.into(),
            dj: dj.into(),
            listeners,
            live,
        }
    }
}

impl Message for RadioNowPlaying {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 1;
}

/// Push: a station went off the air (its mount closed and no playlist took
/// over). Server → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioOff {
    /// Station mount slug that went silent.
    pub station: String,
}

impl RadioOff {
    pub fn new(station: impl Into<String>) -> Self {
        Self {
            station: station.into(),
        }
    }
}

impl Message for RadioOff {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 2;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn now_playing_roundtrips_as_a_push() {
        let msg =
            RadioNowPlaying::new("live", "Down the Hole", "The Lagomorphs", "Robin", 12, true);
        let frame = Frame::push(&msg).unwrap();
        assert_eq!(frame.family, Family::RADIO);
        assert_eq!(frame.message_type, 1);
        assert_eq!(frame.decode::<RadioNowPlaying>().unwrap().unwrap(), msg);
    }

    #[test]
    fn off_roundtrips_as_a_push() {
        let msg = RadioOff::new("ambient");
        let frame = Frame::push(&msg).unwrap();
        assert_eq!(frame.family, Family::RADIO);
        assert_eq!(frame.message_type, 2);
        assert_eq!(frame.decode::<RadioOff>().unwrap().unwrap(), msg);
    }
}
