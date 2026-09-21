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

/// Request: what is on the air here, and where do I tune in? Client → server.
///
/// The pushes above are deltas for someone already watching. A client that
/// just arrived needs the whole picture: every station, the address its audio
/// is served from, and what it has been playing. Before this message the
/// client had to *ask the person* for the stream address, which is a fact the
/// server has and the person usually does not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioStationsRequest;

impl Message for RadioStationsRequest {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 3;
}

/// A track a station played, for its recently-played list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioPlayed {
    pub title: String,
    /// May be empty.
    pub artist: String,
    /// When it started, Unix milliseconds (0 when unknown).
    pub started_unix_ms: u64,
}

impl RadioPlayed {
    pub fn new(title: impl Into<String>, artist: impl Into<String>, started_unix_ms: u64) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            started_unix_ms,
        }
    }
}

/// One station, as the listing reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioStationInfo {
    /// Mount slug, also the path the audio is served at (`/<station>`).
    pub station: String,
    /// The station's display name.
    pub name: String,
    pub description: String,
    /// Current track title (empty when nothing is playing).
    pub title: String,
    pub artist: String,
    /// The source name: a live DJ, or the automation label.
    pub dj: String,
    pub listeners: u32,
    pub live: bool,
    /// Whether audio can be had right now. A rotation with no encoder behind
    /// it has now-playing but nothing to listen to, and the client should not
    /// offer a Listen button that 404s.
    pub streaming: bool,
    /// Cover art for the current track, as a blob id the client fetches with
    /// `BlobGet`. `None` when the station has none.
    pub cover: Option<[u8; 32]>,
    /// What played before the current track, newest first, at most ten.
    pub recent: Vec<RadioPlayed>,
}

impl RadioStationInfo {
    /// A station by slug and display name, silent and empty. The struct is
    /// `#[non_exhaustive]`, so other crates build one through these.
    pub fn new(station: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            station: station.into(),
            name: name.into(),
            description: String::new(),
            title: String::new(),
            artist: String::new(),
            dj: String::new(),
            listeners: 0,
            live: false,
            streaming: false,
            cover: None,
            recent: Vec::new(),
        }
    }

    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// What is on now, and who is sourcing it.
    pub fn playing(
        mut self,
        title: impl Into<String>,
        artist: impl Into<String>,
        dj: impl Into<String>,
    ) -> Self {
        self.title = title.into();
        self.artist = artist.into();
        self.dj = dj.into();
        self
    }

    /// Who is listening, whether a DJ is live, and whether there is audio.
    pub fn on_air(mut self, listeners: u32, live: bool, streaming: bool) -> Self {
        self.listeners = listeners;
        self.live = live;
        self.streaming = streaming;
        self
    }

    pub fn with_cover(mut self, cover: Option<[u8; 32]>) -> Self {
        self.cover = cover;
        self
    }

    pub fn with_recent(mut self, recent: Vec<RadioPlayed>) -> Self {
        self.recent = recent;
        self
    }
}

/// Reply: the stations and where their audio lives. Server → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioStations {
    /// The public base URL audio is served under, when the operator set one
    /// (`radio_public_base`, e.g. `https://radio.example.org`). Empty when not
    /// set, in which case the client derives `http://<the host it dialled>:<port>`.
    pub stream_base: String,
    /// The port the burrow's own stream listener is bound to. 0 when the
    /// listener is off, in which case nothing here can be tuned in to.
    pub port: u16,
    pub stations: Vec<RadioStationInfo>,
}

impl RadioStations {
    pub fn new(stream_base: impl Into<String>, port: u16, stations: Vec<RadioStationInfo>) -> Self {
        Self {
            stream_base: stream_base.into(),
            port,
            stations,
        }
    }
}

impl Message for RadioStations {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 4;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn the_station_listing_roundtrips_as_a_reply() {
        let req = Frame::request(crate::RequestId(7), &RadioStationsRequest).unwrap();
        assert_eq!(req.family, Family::RADIO);
        assert_eq!(req.message_type, 3);
        assert!(req.decode::<RadioStationsRequest>().unwrap().is_ok());

        let msg = RadioStations::new(
            "https://radio.example.org",
            8000,
            vec![RadioStationInfo {
                station: "live".into(),
                name: "Warren FM".into(),
                description: "Pirate radio".into(),
                title: "Down the Hole".into(),
                artist: "The Lagomorphs".into(),
                dj: "Robin".into(),
                listeners: 7,
                live: true,
                streaming: true,
                cover: Some([9u8; 32]),
                recent: vec![RadioPlayed::new(
                    "Carrot Cake",
                    "The Lagomorphs",
                    1_700_000_000_000,
                )],
            }],
        );
        let frame = Frame::reply_to(&req, &msg).unwrap();
        assert_eq!(frame.message_type, 4);
        assert_eq!(frame.decode::<RadioStations>().unwrap().unwrap(), msg);
    }

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

/// Ask what each station is doing, for an operator: what is on, who is
/// listening, and what the rotation could not play. Operators only
/// (`CONFIG_ADMIN`); everyone else uses [`RadioStationsRequest`], which says
/// what is on without the housekeeping. → [`RadioStatus`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioStatusRequest;

impl Message for RadioStatusRequest {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 5;
}

/// One track a station could not play, and why. A rotation is a file area,
/// and an area holds whatever was put in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LeftOut {
    pub title: String,
    /// Said for a person: "not audio this burrow can stream", "not what this
    /// station is sending (Ogg)".
    pub reason: String,
    pub at_unix_ms: u64,
}

impl LeftOut {
    pub fn new(title: impl Into<String>, reason: impl Into<String>, at_unix_ms: u64) -> Self {
        Self {
            title: title.into(),
            reason: reason.into(),
            at_unix_ms,
        }
    }
}

/// One station, as its operator needs to see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioStationStatus {
    pub station: String,
    pub name: String,
    /// The file area its rotation comes from, when it is a library station.
    pub area: String,
    /// What it is sending: `audio/mpeg`, `audio/ogg`, or empty when it has
    /// not started.
    pub content_type: String,
    pub title: String,
    pub artist: String,
    pub listeners: u32,
    pub live: bool,
    /// Tracks in the rotation, and how many of them it can play.
    pub tracks: u32,
    /// The most recent tracks it had to leave out, newest first.
    pub left_out: Vec<LeftOut>,
}

impl RadioStationStatus {
    pub fn new(station: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            station: station.into(),
            name: name.into(),
            area: String::new(),
            content_type: String::new(),
            title: String::new(),
            artist: String::new(),
            listeners: 0,
            live: false,
            tracks: 0,
            left_out: Vec::new(),
        }
    }

    /// Where its rotation comes from and what it is sending.
    pub fn of_area(mut self, area: impl Into<String>, content_type: impl Into<String>) -> Self {
        self.area = area.into();
        self.content_type = content_type.into();
        self
    }

    /// What is on, and who is hearing it.
    pub fn on_air(
        mut self,
        title: impl Into<String>,
        artist: impl Into<String>,
        listeners: u32,
        live: bool,
    ) -> Self {
        self.title = title.into();
        self.artist = artist.into();
        self.listeners = listeners;
        self.live = live;
        self
    }

    /// How big the rotation is, and what it could not play.
    pub fn with_rotation(mut self, tracks: u32, left_out: Vec<LeftOut>) -> Self {
        self.tracks = tracks;
        self.left_out = left_out;
        self
    }
}

/// Reply: every station this burrow runs. Server → operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioStatus {
    pub stations: Vec<RadioStationStatus>,
}

impl RadioStatus {
    pub fn new(stations: Vec<RadioStationStatus>) -> Self {
        Self { stations }
    }
}

impl Message for RadioStatus {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 6;
}

/// A track a listener may ask a station for: something in its rotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RequestableTrack {
    pub id: u64,
    pub title: String,
    pub artist: String,
}

impl RequestableTrack {
    pub fn new(id: u64, title: impl Into<String>, artist: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            artist: artist.into(),
        }
    }
}

/// A request waiting to be played, in the order it will be: most wanted
/// first, and the earlier of two equally wanted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct QueuedTrack {
    pub id: u64,
    pub title: String,
    pub artist: String,
    /// How many people want it, the one who asked included.
    pub votes: u32,
    /// Whether the person asking is one of them — so a pane does not offer
    /// a vote they have already cast.
    pub mine: bool,
}

impl QueuedTrack {
    pub fn new(
        id: u64,
        title: impl Into<String>,
        artist: impl Into<String>,
        votes: u32,
        mine: bool,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            artist: artist.into(),
            votes,
            mine,
        }
    }
}

/// Ask what a station's listeners have asked for. → [`RadioRequests`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioRequestsRequest {
    pub station: String,
}

impl RadioRequestsRequest {
    pub fn new(station: impl Into<String>) -> Self {
        Self {
            station: station.into(),
        }
    }
}

impl Message for RadioRequestsRequest {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 7;
}

/// A station's queue of requests, in the order they will play. The answer
/// to asking about it, requesting and voting alike. Small by construction:
/// a station holds a bounded number of requests. What can be asked for is
/// [`RadioOffer`], which is looked through rather than sent whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioRequests {
    pub station: String,
    /// Whether the station takes requests at all. A mount a DJ streams to
    /// has no rotation to ask from; it answers with an empty queue and
    /// `false` rather than a refusal, since nobody asked for anything.
    pub requestable: bool,
    /// A DJ has the air: requests wait until they leave.
    pub dj_live: bool,
    pub queue: Vec<QueuedTrack>,
}

impl RadioRequests {
    pub fn new(
        station: impl Into<String>,
        requestable: bool,
        dj_live: bool,
        queue: Vec<QueuedTrack>,
    ) -> Self {
        Self {
            station: station.into(),
            requestable,
            dj_live,
            queue,
        }
    }
}

impl Message for RadioRequests {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 8;
}

/// Ask for a track to play next. A signed-in account, not a guest: a guest
/// could come back under another name and ask again. → [`RadioRequests`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioRequest {
    pub station: String,
    pub track: u64,
}

impl RadioRequest {
    pub fn new(station: impl Into<String>, track: u64) -> Self {
        Self {
            station: station.into(),
            track,
        }
    }
}

impl Message for RadioRequest {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 9;
}

/// Add your vote to a request already waiting. One vote each, however many
/// times it is sent. → [`RadioRequests`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioRequestVote {
    pub station: String,
    pub track: u64,
}

impl RadioRequestVote {
    pub fn new(station: impl Into<String>, track: u64) -> Self {
        Self {
            station: station.into(),
            track,
        }
    }
}

impl Message for RadioRequestVote {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 10;
}

/// The most of a search a station looks for, and says back in
/// [`RadioOffer::search`]. A client keeps the same much of what it sent, so
/// the answer can be matched to the question.
pub const OFFER_SEARCH_CHARS: usize = 200;

/// Look through what a station can be asked for: tracks whose title or
/// artist holds `search` (all of them when it is empty; the first
/// [`OFFER_SEARCH_CHARS`] of it, trimmed), a page at a time.
/// → [`RadioOffer`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioOfferRequest {
    pub station: String,
    pub search: String,
}

impl RadioOfferRequest {
    pub fn new(station: impl Into<String>, search: impl Into<String>) -> Self {
        Self {
            station: station.into(),
            search: search.into(),
        }
    }
}

impl Message for RadioOfferRequest {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 11;
}

/// What a station can be asked for, as far as `search` narrows it: only
/// what it can actually play, and not what is playing now. A library can
/// hold more than one reply should carry, so this is the first of them and
/// how many more there are; a person narrows the search to reach the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadioOffer {
    pub station: String,
    /// The search this answers, so a late answer to an older one can be
    /// told apart from the answer to what the person is typing now.
    pub search: String,
    pub tracks: Vec<RequestableTrack>,
    /// How many more matched than are here.
    pub more: u32,
}

impl RadioOffer {
    pub fn new(
        station: impl Into<String>,
        search: impl Into<String>,
        tracks: Vec<RequestableTrack>,
        more: u32,
    ) -> Self {
        Self {
            station: station.into(),
            search: search.into(),
            tracks,
            more,
        }
    }
}

impl Message for RadioOffer {
    const FAMILY: Family = Family::RADIO;
    const MESSAGE_TYPE: u16 = 12;
}
