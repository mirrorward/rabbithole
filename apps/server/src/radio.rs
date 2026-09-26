//! Icecast/SHOUTcast (ICY) radio delivery listener (Wave 11.4): the transport
//! that finally moves radio bytes over a socket.
//!
//! The pure pieces already exist and are unit-tested in their own crates; this
//! module is the *bridge* that wires them to a live TCP listener and to
//! [`Shared`]:
//!
//! - [`rabbithole-legacy-icecast`](rabbithole_legacy_icecast) — the wire codec:
//!   [`parse_source_request`]/[`parse_listener_request`] decode the ICY/HTTP
//!   request heads, [`build_listener_response`] renders the `ICY 200 OK` head,
//!   and [`IcyMetaInterleaver`] splices `StreamTitle` metadata blocks into the
//!   audio at the negotiated `icy-metaint` boundary.
//! - [`rabbithole-radio`](rabbithole_radio) — [`StationRegistry`] is the
//!   station directory + per-mount listener accounting.
//! - [`rabbithole-audio`](rabbithole_audio) — [`NowPlaying`] is the shared
//!   now-playing vocabulary carried to listeners as `StreamTitle`.
//!
//! # DJ (source) auth
//!
//! A source authenticates with HTTP Basic credentials against
//! [`AuthService::login_password`](rabbithole_server_core::AuthService::login_password)
//! and must additionally hold [`Caps::BROADCAST`] on the `radio` resource — the
//! capability that already means "may broadcast" server-wide, so "may DJ" reuses
//! it rather than minting a new bit. Bad credentials get `401`; an authenticated
//! user without the capability (or a mount already in use) gets `403`.
//!
//! # Byte passthrough (this slice)
//!
//! The source body is fanned out to listeners **verbatim** over a per-mount
//! [`tokio::sync::broadcast`] channel — no decode, no transcode, no
//! [`StationController`](rabbithole_radio::StationController) playout. That is a
//! deliberate first slice: it makes any real Icecast source (a DJ pushing MP3
//! or Ogg) audible in any real player. Decoding the stream into
//! [`rabbithole_audio::Frame`]s and driving a scheduled playlist through the
//! audio [`Station`](rabbithole_audio::Station) is a documented follow-up. The
//! drop-behind fan-out here mirrors the audio `Station` semantics exactly: a
//! listener that falls behind skips ahead and never blocks the source.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use rabbithole_audio::{NowPlaying, Station};
use rabbithole_legacy_icecast::{
    build_listener_response, metadata_update_failed, metadata_update_ok,
    metadata_update_unauthorized, parse_listener_request, parse_metadata_update,
    parse_source_request, source_forbidden, source_ok, source_unauthorized, IcyMetaInterleaver,
    MetadataUpdate, StationMeta, DEFAULT_METAINT,
};
use rabbithole_radio::{
    BlobId, Playlist, RotationMode, StationConfig, StationController, StationRegistry, Track,
    TrackId,
};
use rabbithole_server_core::files::KIND_FILE;
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, RadioStatus, ServerEvent};
use rabbithole_store_server::repo6::FileNodeRow;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::Shared;

/// Broadcast-event capacity for a library station's audio fan-out. At 50
/// frames/second this retains roughly 1.3 s for a slow listener.
const STATION_CAPACITY: usize = 64;

/// Nominal per-track duration for library tracks whose real length is unknown
/// (no decode happens here). It only drives the rotation driver's finish
/// detection; a long value keeps automation from spinning.
const DEFAULT_TRACK_MS: u64 = 180_000;

/// The DJ label shown for playlist automation (no live human sourcing).
const AUTOMATION_DJ: &str = "auto";

/// How many raw audio chunks a listener may fall behind before the oldest are
/// overwritten (drop-behind). Chunks are read up to [`SOURCE_CHUNK`] bytes, so
/// this bounds a slow listener's backlog rather than stalling the source.
const BROADCAST_CAPACITY: usize = 512;

/// Largest raw source read pushed into the broadcast in one go.
const SOURCE_CHUNK: usize = 8 * 1024;

/// The permission resource DJ authorization is checked against.
const RADIO_RESOURCE: &str = "radio";

/// A mount's byte fan-out: what a source (a DJ, or a rotation's pump) sends
/// into and every listener reads from.
type Fanout = broadcast::Sender<Arc<[u8]>>;

/// The slot a mount's listeners read their stream title from.
type TitleSlot = Arc<Mutex<Option<NowPlaying>>>;

/// A live mount: its byte fan-out channel plus the metadata listeners need.
///
/// Only the source connection and this registry entry hold the [`broadcast::Sender`];
/// listeners hold only a [`broadcast::Receiver`] (via [`MountEntry::subscribe`]),
/// so dropping the entry when the source disconnects closes every listener
/// cleanly instead of leaving them parked forever.
struct MountEntry {
    /// Raw ICY audio-byte fan-out. Slow listeners are dropped-behind.
    tx: broadcast::Sender<Arc<[u8]>>,
    /// Station description advertised by the source (`icy-*` headers).
    meta: StationMeta,
    /// Stream content type (`audio/mpeg`, `audio/ogg`, …).
    content_type: String,
    /// Current now-playing metadata, shared live with listeners for the
    /// `StreamTitle` blocks (updatable without keeping the sender alive).
    now_playing: Arc<Mutex<Option<NowPlaying>>>,
    /// Held by a library rotation's pump rather than a live source. A DJ may
    /// take such a mount over; a mount another DJ holds is refused.
    program_owned: bool,
    /// What a listener needs before any of the audio makes sense, sent to
    /// each one as they arrive. Empty for MP3, whose frames each say what
    /// they are; a `fLaC` magic and one STREAMINFO for a FLAC mount, which
    /// says it once for the whole night.
    lead: Arc<[u8]>,
}

/// A listener's view of a mount: an event receiver plus the head it needs to
/// render the ICY response, none of which keeps the source's sender alive.
struct MountHandle {
    rx: broadcast::Receiver<Arc<[u8]>>,
    meta: StationMeta,
    content_type: String,
    now_playing: Arc<Mutex<Option<NowPlaying>>>,
    /// The head of the stream, for a listener who joined in the middle of
    /// it. Written before anything that comes over the air.
    lead: Arc<[u8]>,
}

impl MountEntry {
    fn subscribe(&self) -> MountHandle {
        MountHandle {
            rx: self.tx.subscribe(),
            meta: self.meta.clone(),
            content_type: self.content_type.clone(),
            now_playing: self.now_playing.clone(),
            lead: self.lead.clone(),
        }
    }
}

/// The server-wide radio state: the station directory and the live mounts.
///
/// A field of [`Shared`] alongside `swarm`/`transfers`. The [`StationRegistry`]
/// owns the directory + listener accounting (what a UI lists); `mounts` owns the
/// live byte-fan-out channels (what the transport moves).
pub struct Stations {
    /// Station directory + per-mount listener counts.
    pub registry: StationRegistry,
    /// Live source mounts, keyed by bare slug (no leading `/`).
    mounts: Mutex<HashMap<String, MountEntry>>,
    /// Library-backed programs (playlist engine per station), keyed by slug.
    programs: Mutex<HashMap<String, Program>>,
    /// Monotonic listener-id source for registry accounting.
    next_listener: AtomicU64,
    /// What each station is playing and what it played before, so a client
    /// that just arrived can be shown more than the present moment.
    history: Mutex<HashMap<String, StationHistory>>,
    /// Cover art per station: track title to the blob id of its image.
    covers: Mutex<HashMap<String, HashMap<String, [u8; 32]>>>,
    /// What each station could not play, newest first, for its operator.
    left_out: Mutex<HashMap<String, Vec<rabbithole_proto::radio::LeftOut>>>,
    /// The port the stream listener actually bound (0 = not listening), which
    /// is what gets advertised: the configured port may have been 0 ("any").
    listen_port: AtomicU16,
}

/// How many tracks a station remembers having played.
pub const RECENT_TRACKS: usize = 10;

/// A track on the air, and when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    pub title: String,
    pub artist: String,
    pub started_unix_ms: u64,
}

/// A station's current track and the ones before it, newest first.
#[derive(Debug, Default)]
struct StationHistory {
    current: Option<Played>,
    recent: VecDeque<Played>,
}

impl StationHistory {
    /// A now-playing report arrived. Only a *change of track* moves history:
    /// now-playing is republished whenever the listener count moves, and those
    /// repeats must not fill the list with one song ten times.
    fn note(&mut self, title: &str, artist: &str, now_ms: u64) {
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.title == title && c.artist == artist)
        {
            return;
        }
        self.retire();
        if !title.trim().is_empty() {
            self.current = Some(Played {
                title: title.to_string(),
                artist: artist.to_string(),
                started_unix_ms: now_ms,
            });
        }
    }

    /// The current track is over: it becomes the newest recent one.
    fn retire(&mut self) {
        if let Some(done) = self.current.take() {
            self.recent.push_front(done);
            self.recent.truncate(RECENT_TRACKS);
        }
    }
}

/// A library-backed station: a playlist engine ([`StationController`]) plus its
/// live-DJ takeover state. The playlist rotates on its own until a DJ goes
/// live; while live, the DJ's now-playing overrides the rotation and rotation
/// is paused (resuming when the DJ disconnects).
/// What bringing a station up to date with its folder did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// Nothing in the folder had changed.
    Unchanged,
    /// The rotation changed. `dropped` waiting requests asked after files
    /// that are gone; `started` says a station that had nothing to play
    /// has begun.
    Changed { dropped: usize, started: bool },
    /// The folder asked for a station that was not on the air, and now is.
    Installed,
}

struct Program {
    controller: StationController,
    /// `Some` while a DJ is live: their now-playing overrides the playlist's.
    live: Option<NowPlaying>,
    /// Bytes ingested from the current live DJ (observability + tests).
    source_bytes: u64,
    /// A pump is streaming this rotation's audio, and advances it when a
    /// track's audio actually ends. Otherwise the timer driver advances it on
    /// a nominal duration and the station is now-playing only.
    pumped: bool,
    /// What its tracks' names say they are, so the mount goes up as the
    /// right kind rather than guessing and being remade under whoever is
    /// already listening.
    expected: Option<Sound>,
    /// How many tracks the rotation holds.
    tracks: usize,
    /// Tracks in the rotation that are not what this station sends — audio
    /// of a kind this burrow cannot stream, or FLAC in another form than
    /// the mount's — kept here so the station can say it left them out.
    /// Never offered to a listener to ask for.
    unsendable: HashSet<TrackId>,
    /// Tracks the pump found it could not play when their turn came. Not
    /// offered until one does play: a file can be fixed, or the station can
    /// change what it sends.
    unplayable: HashSet<TrackId>,
    /// A DJ talked over the current track. It is moved past when they
    /// leave, not when they arrive: moving on takes the most wanted request
    /// off the queue, and nobody would hear it until then.
    owed: bool,
}

impl Program {
    fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// What is playing now, as far as a listener is concerned: nothing
    /// of the rotation's while a DJ has the air or has just talked over it.
    fn playing(&self) -> Option<TrackId> {
        if self.is_live() || self.owed {
            return None;
        }
        self.controller.current().map(|t| t.id)
    }

    /// Whether a listener may ask for `track`: in the rotation, something
    /// this station can send and has not found it could not play, and not
    /// what is playing now. Whether a moderator is holding it back is the
    /// caller's to say.
    fn offers(&self, track: &Track) -> bool {
        !self.unsendable.contains(&track.id)
            && !self.unplayable.contains(&track.id)
            && self.playing() != Some(track.id)
    }

    /// How many of `listener`'s requests are waiting, not counting any a
    /// moderator has since held back: those are not shown to them, so
    /// they must not use up what they may ask for either.
    fn waiting_for(&self, listener: &str, held: &impl Fn(&Track) -> bool) -> usize {
        self.controller
            .queue()
            .in_play_order()
            .into_iter()
            .filter(|r| r.requester() == listener && !held(r.track()))
            .count()
    }

    /// How many requests are waiting that anybody can see.
    fn waiting(&self, held: &impl Fn(&Track) -> bool) -> usize {
        self.controller
            .queue()
            .in_play_order()
            .into_iter()
            .filter(|r| !held(r.track()))
            .count()
    }

    /// Move to the next track, passing over what a moderator is holding
    /// back (`held`) so its name is never announced as playing. Once round
    /// the rotation and everything waiting, at most: a station held back
    /// entirely lands on something rather than going round for ever, and
    /// the pump, which checks again, does not play it.
    fn move_on(&mut self, now_ms: u64, held: &impl Fn(&Track) -> bool) {
        self.controller.on_track_finished(now_ms);
        let mut tries = self.tracks + self.controller.queue().len();
        while tries > 0 && self.controller.current().is_some_and(held) {
            self.controller.on_track_finished(now_ms);
            tries -= 1;
        }
    }

    /// The now-playing to surface: the live DJ's when live, else the playlist's.
    fn now_playing(&self) -> Option<NowPlaying> {
        self.live.clone().or_else(|| self.controller.now_playing())
    }
}

impl Default for Stations {
    fn default() -> Self {
        Self::new()
    }
}

impl Stations {
    pub fn new() -> Self {
        Self {
            registry: StationRegistry::new(),
            mounts: Mutex::new(HashMap::new()),
            programs: Mutex::new(HashMap::new()),
            next_listener: AtomicU64::new(1),
            history: Mutex::new(HashMap::new()),
            covers: Mutex::new(HashMap::new()),
            left_out: Mutex::new(HashMap::new()),
            listen_port: AtomicU16::new(0),
        }
    }

    /// Record the port the stream listener bound, so it can be advertised.
    pub fn set_listen_port(&self, port: u16) {
        self.listen_port.store(port, Ordering::Relaxed);
    }

    /// The port audio is served on, or 0 when the listener is off.
    pub fn listen_port(&self) -> u16 {
        self.listen_port.load(Ordering::Relaxed)
    }

    /// A station reported what it is playing.
    pub fn note_now_playing(&self, slug: &str, title: &str, artist: &str, now_ms: u64) {
        self.history
            .lock()
            .entry(slug.to_string())
            .or_default()
            .note(title, artist, now_ms);
    }

    /// A station went off the air: its last track joins the recent list, which
    /// is kept, so coming back on air does not start from an empty history.
    pub fn note_off_air(&self, slug: &str) {
        if let Some(h) = self.history.lock().get_mut(slug) {
            h.retire();
        }
    }

    /// What `slug` played before its current track, newest first.
    pub fn recent(&self, slug: &str) -> Vec<Played> {
        self.history
            .lock()
            .get(slug)
            .map(|h| h.recent.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Install a station's cover art: track title to image blob.
    pub fn set_covers(&self, slug: &str, covers: HashMap<String, [u8; 32]>) {
        self.covers.lock().insert(slug.to_string(), covers);
    }

    /// The cover for the track `slug` is playing, when it has one.
    pub fn cover_for(&self, slug: &str, title: &str) -> Option<[u8; 32]> {
        self.covers.lock().get(slug)?.get(title).copied()
    }

    /// Whether audio can be had from `slug` right now: a source is connected
    /// and its bytes are being fanned out.
    pub fn is_streaming(&self, slug: &str) -> bool {
        self.mounts.lock().contains_key(slug)
    }

    fn next_listener_id(&self) -> u64 {
        self.next_listener.fetch_add(1, Ordering::Relaxed)
    }

    /// Subscribe a listener to a mount, if a source is currently live there.
    fn subscribe(&self, slug: &str) -> Option<MountHandle> {
        self.mounts.lock().get(slug).map(MountEntry::subscribe)
    }

    /// Installs a library-backed program: a station whose default rotation is
    /// `tracks`, sending `expected`. Registers it in the directory and starts
    /// playout at the first track (now-playing is populated immediately,
    /// deterministically).
    ///
    /// `expected` is what the mount goes up as before the first track has
    /// been read, so a listener who tunes in during that moment is told the
    /// truth and is not cut off a second later when it turns out to be
    /// something else. A caller that sorted the tracks by kind knows it
    /// exactly and says so; one that did not asks [`sound_of_tracks`].
    pub fn install_program(
        &self,
        slug: &str,
        display_name: &str,
        description: &str,
        tracks: Vec<Track>,
        expected: Option<Sound>,
    ) {
        let station = Station::new(slug, STATION_CAPACITY);
        let track_count = tracks.len();
        let playlist = Playlist::new(tracks, RotationMode::Sequential);
        let mut controller = StationController::new(station, playlist, description, AUTOMATION_DJ);
        // Start playout at the opening track so now-playing is live at once,
        // on the clock the pump and the timer driver both advance it by.
        controller.on_track_finished(unix_ms());
        let _ = self.registry.create(StationConfig {
            slug: slug.to_string(),
            display_name: display_name.to_string(),
            description: description.to_string(),
            enabled: true,
        });
        let _ = self.registry.set_enabled(slug, true);
        // A format added to a live library can install its companion while
        // a DJ already owns that URL. Keep their metadata until departure.
        // Lock in source-then-program order so departure cannot clear the
        // live state before this newly installed program has received it.
        let mounts = self.mounts.lock();
        let live = mounts
            .get(slug)
            .filter(|m| !m.program_owned)
            .and_then(|m| m.now_playing.lock().clone());
        self.programs.lock().insert(
            slug.to_string(),
            Program {
                controller,
                live,
                source_bytes: 0,
                pumped: false,
                expected,
                tracks: track_count,
                unsendable: HashSet::new(),
                unplayable: HashSet::new(),
                owed: false,
            },
        );
    }

    /// Say which of `slug`'s tracks are not what it sends, so they are not
    /// offered to listeners to ask for. The rotation keeps them, so the
    /// station can say it left them out.
    pub fn set_unsendable(&self, slug: &str, tracks: impl IntoIterator<Item = TrackId>) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.unsendable = tracks.into_iter().collect();
        }
    }

    /// What `listener` sees of `slug`'s requests: the queue in the order it
    /// will play, without anything a moderator is holding back (`held`). A
    /// mount with no rotation — one a DJ streams to — takes no requests and
    /// says so rather than refusing, since nobody asked for anything. `None`
    /// for no station by that name at all.
    pub fn requests(
        &self,
        slug: &str,
        listener: &str,
        held: impl Fn(&Track) -> bool,
    ) -> Option<rabbithole_proto::radio::RadioRequests> {
        use rabbithole_proto::radio::{QueuedTrack, RadioRequests};
        let (queue, live) = {
            let programs = self.programs.lock();
            match programs.get(slug) {
                Some(p) => {
                    let queue = p
                        .controller
                        .queue()
                        .in_play_order()
                        .into_iter()
                        .filter(|r| !held(r.track()))
                        .map(|r| {
                            let t = r.track();
                            QueuedTrack::new(
                                t.id.0,
                                clip(&t.title),
                                clip(&t.artist),
                                r.votes(),
                                r.backed_by(listener),
                            )
                        })
                        .collect();
                    (Some(queue), p.is_live())
                }
                None => (None, false),
            }
        };
        let dj = live || self.dj_holds(slug);
        match queue {
            Some(queue) => Some(RadioRequests::new(slug, true, dj, queue)),
            None if dj || self.registry.get(slug).is_some() => {
                Some(RadioRequests::new(slug, false, dj, Vec::new()))
            }
            None => None,
        }
    }

    /// What `slug` can be asked for, as far as `search` narrows it: the
    /// first [`OFFER_SHOWN`] of what it can play and nobody is holding back,
    /// and how many more there are. `None` for a station with no rotation.
    pub fn offer(
        &self,
        slug: &str,
        search: &str,
        held: impl Fn(&Track) -> bool,
    ) -> Option<rabbithole_proto::radio::RadioOffer> {
        use rabbithole_proto::radio::{RadioOffer, RequestableTrack};
        let search: String = search
            .trim()
            .chars()
            .take(rabbithole_proto::radio::OFFER_SEARCH_CHARS)
            .collect();
        let needle = search.to_lowercase();
        let programs = self.programs.lock();
        let p = programs.get(slug)?;
        let mut tracks = Vec::new();
        let mut more = 0u32;
        for t in p.controller.rotation() {
            if !p.offers(t) || held(t) {
                continue;
            }
            if !needle.is_empty()
                && !t.title.to_lowercase().contains(&needle)
                && !t.artist.to_lowercase().contains(&needle)
            {
                continue;
            }
            if tracks.len() < OFFER_SHOWN {
                tracks.push(RequestableTrack::new(
                    t.id.0,
                    clip(&t.title),
                    clip(&t.artist),
                ));
            } else {
                more = more.saturating_add(1);
            }
        }
        Some(RadioOffer::new(slug, search, tracks, more))
    }

    /// `listener` asks for `track` on `slug`. Asking for something already
    /// waiting is a vote for it, which is what a person who asks for it
    /// means. Only what the station would offer can be asked for, so
    /// nothing waits that would be passed over without a word when its
    /// turn came.
    pub fn request(
        &self,
        slug: &str,
        track: u64,
        listener: &str,
        held: impl Fn(&Track) -> bool,
    ) -> Result<(), RequestRefused> {
        let mut programs = self.programs.lock();
        let p = programs
            .get_mut(slug)
            .ok_or(RequestRefused::NoSuchStation)?;
        let id = rabbithole_radio::TrackId(track);
        if p.playing() == Some(id) {
            return Err(RequestRefused::PlayingNow);
        }
        let found = p
            .controller
            .rotation()
            .iter()
            .find(|t| t.id == id)
            .filter(|t| p.offers(t) && !held(t))
            .cloned()
            .ok_or(RequestRefused::NotInRotation)?;
        if p.controller.queue().contains(id) {
            p.controller
                .queue_mut()
                .upvote(id, listener)
                .map_err(|_| RequestRefused::NotInRotation)?;
            return Ok(());
        }
        if p.waiting(&held) >= REQUESTS_WAITING {
            return Err(RequestRefused::Full);
        }
        if p.waiting_for(listener, &held) >= REQUESTS_EACH {
            return Err(RequestRefused::TooManyOfYours);
        }
        p.controller
            .queue_mut()
            .enqueue(found, listener)
            .map_err(|_| RequestRefused::NotInRotation)
    }

    /// `listener` adds a vote to a request already waiting on `slug`. One
    /// a moderator has since held back is not waiting as far as anybody
    /// can see, and takes no votes.
    pub fn vote_request(
        &self,
        slug: &str,
        track: u64,
        listener: &str,
        held: impl Fn(&Track) -> bool,
    ) -> Result<(), RequestRefused> {
        let mut programs = self.programs.lock();
        let p = programs
            .get_mut(slug)
            .ok_or(RequestRefused::NoSuchStation)?;
        let id = rabbithole_radio::TrackId(track);
        if p.playing() == Some(id) {
            return Err(RequestRefused::PlayingNow);
        }
        let hidden = p
            .controller
            .queue()
            .in_play_order()
            .into_iter()
            .any(|r| r.track().id == id && held(r.track()));
        if hidden {
            return Err(RequestRefused::NotWaiting);
        }
        p.controller
            .queue_mut()
            .upvote(id, listener)
            .map(|_| ())
            .map_err(|_| RequestRefused::NotWaiting)
    }

    /// A DJ took over `slug`: pause rotation and adopt the DJ's now-playing.
    /// Returns whether a library program existed (a pure-DJ mount with no
    /// playlist still fans bytes out via the mount channel).
    pub fn go_live(&self, slug: &str, now_playing: NowPlaying) -> bool {
        let mut programs = self.programs.lock();
        match programs.get_mut(slug) {
            Some(p) => {
                p.live = Some(now_playing);
                p.source_bytes = 0;
                true
            }
            None => false,
        }
    }

    /// Records bytes ingested from the live DJ on `slug` (best-effort; a
    /// program-less mount simply has no counter to bump).
    pub fn add_source_bytes(&self, slug: &str, n: u64) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.source_bytes = p.source_bytes.saturating_add(n);
        }
    }

    /// The DJ disconnected from `slug`: resume playlist rotation, moving
    /// past the track they talked over in the same breath, so nobody is
    /// told it is playing again.
    pub fn end_live(&self, slug: &str, held: impl Fn(&Track) -> bool) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.live = None;
            if std::mem::take(&mut p.owed) || (p.controller.current().is_none() && p.tracks > 0) {
                p.move_on(unix_ms(), &held);
            }
        }
    }

    /// Whether a DJ is currently sourcing `slug`.
    pub fn is_live(&self, slug: &str) -> bool {
        self.programs.lock().get(slug).is_some_and(Program::is_live)
    }

    /// Bytes ingested from the current live DJ on `slug`.
    pub fn source_bytes(&self, slug: &str) -> u64 {
        self.programs
            .lock()
            .get(slug)
            .map(|p| p.source_bytes)
            .unwrap_or(0)
    }

    /// The now-playing for `slug` (DJ's when live, else the playlist's).
    pub fn now_playing(&self, slug: &str) -> Option<NowPlaying> {
        self.programs
            .lock()
            .get(slug)
            .and_then(Program::now_playing)
    }

    /// Advances every non-live program whose current track has finished at
    /// `now_ms`, returning the slugs that rotated so the caller can republish
    /// now-playing. Live (DJ-sourced) programs are skipped — the DJ owns the air.
    ///
    /// A track a moderator is holding back (`held`) is passed over, the way
    /// the pump passes over it, so its name is not announced as playing.
    pub fn advance_finished(&self, now_ms: u64, held: impl Fn(&Track) -> bool) -> Vec<String> {
        let mut advanced = Vec::new();
        let mut programs = self.programs.lock();
        for (slug, p) in programs.iter_mut() {
            // A pumped rotation moves on when its audio ends, not on a timer.
            if p.is_live() || p.pumped {
                continue;
            }
            if p.controller.is_finished(now_ms) {
                p.move_on(now_ms, &held);
                advanced.push(slug.clone());
            }
        }
        advanced
    }

    /// Whether what `slug` is playing is something somebody asked for.
    pub fn current_was_requested(&self, slug: &str) -> bool {
        self.programs
            .lock()
            .get(slug)
            .is_some_and(|p| p.controller.current_was_requested())
    }

    /// The pump could not play `track` on `slug`: it is not offered to
    /// listeners until it does.
    pub fn cannot_play(&self, slug: &str, track: TrackId) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.unplayable.insert(track);
        }
    }

    /// `track` (`title`) is going out on `slug`: whatever was wrong with it
    /// is not any more — the install's guess from its name or form
    /// included — so it is offered again and no longer said to have been
    /// left out. Unless another file of the same name is still being left
    /// out: the operator's list is by name, and that one has not changed.
    pub fn can_play(&self, slug: &str, track: TrackId, title: &str) {
        let namesake_still_out = {
            let mut programs = self.programs.lock();
            let Some(p) = programs.get_mut(slug) else {
                return;
            };
            p.unplayable.remove(&track);
            p.unsendable.remove(&track);
            p.controller
                .rotation()
                .iter()
                .any(|t| t.id != track && t.title == title && p.unplayable.contains(&t.id))
        };
        if namesake_still_out {
            return;
        }
        if let Some(list) = self.left_out.lock().get_mut(slug) {
            list.retain(|l| l.title != title);
        }
    }

    /// A DJ talked over what `slug` was playing.
    pub fn owe_advance(&self, slug: &str) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.owed = true;
        }
    }

    /// The air is the rotation's again: if a DJ talked over its track, move
    /// on now, so what is announced next is what will actually play.
    /// Returns whether it moved.
    pub fn take_air_back(&self, slug: &str, held: impl Fn(&Track) -> bool) -> bool {
        let mut programs = self.programs.lock();
        match programs.get_mut(slug) {
            Some(p) if p.owed && !p.is_live() => {
                p.owed = false;
                p.move_on(unix_ms(), &held);
                true
            }
            _ => false,
        }
    }

    /// Whether a pump is streaming `slug`'s rotation.
    pub fn is_pumped(&self, slug: &str) -> bool {
        self.programs.lock().get(slug).is_some_and(|p| p.pumped)
    }

    /// Bring `slug` up to date with the folder behind it, or put it on the
    /// air if the folder now asks for a station that is not there yet (the
    /// first FLAC file in a library that was all MP3).
    ///
    /// The track on the air plays on and the rotation carries on from where
    /// it was: a song added to a station's folder takes its turn, and one
    /// taken out is not played again, without anybody listening being cut
    /// off. Waiting requests for files that have gone are dropped.
    pub fn refresh_program(
        &self,
        slug: &str,
        display_name: &str,
        description: &str,
        tracks: Vec<Track>,
        expected: Option<Sound>,
        unsendable: impl IntoIterator<Item = TrackId>,
    ) -> Refreshed {
        let unsendable: HashSet<TrackId> = unsendable.into_iter().collect();
        {
            let mounts = self.mounts.lock();
            let mut programs = self.programs.lock();
            if let Some(p) = programs.get_mut(slug) {
                p.live = mounts
                    .get(slug)
                    .filter(|m| !m.program_owned)
                    .and_then(|m| m.now_playing.lock().clone());
                if p.controller.rotation() == tracks.as_slice() && p.unsendable == unsendable {
                    return Refreshed::Unchanged;
                }
                let here: HashSet<TrackId> = tracks.iter().map(|t| t.id).collect();
                p.unplayable.retain(|id| here.contains(id));
                p.tracks = tracks.len();
                p.unsendable = unsendable;
                // A mount that is not up yet goes up as what its tracks say.
                // One that is up stays what its listeners were told.
                if !p.pumped {
                    p.expected = expected;
                }
                let dropped = p.controller.replace_rotation(tracks);
                // A station that had nothing to play starts now it has.
                let started = if p.controller.current().is_none() && p.tracks > 0 && !p.is_live() {
                    p.controller.on_track_finished(unix_ms());
                    true
                } else {
                    false
                };
                return Refreshed::Changed { dropped, started };
            }
        }
        self.install_program(slug, display_name, description, tracks, expected);
        self.set_unsendable(slug, unsendable);
        Refreshed::Installed
    }

    /// Hand a rotation to a pump: from now on it advances when told to.
    pub fn set_pumped(&self, slug: &str, pumped: bool) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.pumped = pumped;
        }
    }

    /// The track a rotation is on, with the blob its audio lives in.
    pub fn current_track(&self, slug: &str) -> Option<Track> {
        self.programs
            .lock()
            .get(slug)
            .and_then(|p| p.controller.current().cloned())
    }

    /// How many tracks a rotation has.
    pub fn track_count(&self, slug: &str) -> usize {
        self.programs.lock().get(slug).map_or(0, |p| p.tracks)
    }

    /// Move a rotation to its next track.
    pub fn advance(&self, slug: &str, now_ms: u64) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.controller.on_track_finished(now_ms);
        }
    }

    /// Whether a person (a live source) holds `slug`'s mount right now.
    pub fn dj_holds(&self, slug: &str) -> bool {
        self.mounts
            .lock()
            .get(slug)
            .is_some_and(|m| !m.program_owned)
    }

    /// The rotation's own mount: the existing one, or a fresh one. `None`
    /// while a DJ holds the slug. Returns the byte fan-out and the slot the
    /// listeners' stream titles are read from.
    fn program_mount(&self, slug: &str, sound: Sound) -> Option<(Fanout, TitleSlot)> {
        // The controller's own station record (name, description, what is on).
        let program = self
            .programs
            .lock()
            .get(slug)
            .map(|p| p.controller.station_meta())?;
        let display_name = self
            .registry
            .get(slug)
            .map(|i| i.display_name)
            .unwrap_or_else(|| program.name.clone());
        let mut mounts = self.mounts.lock();
        if let Some(existing) = mounts.get(slug) {
            return existing
                .program_owned
                .then(|| (existing.tx.clone(), existing.now_playing.clone()));
        }
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let now_playing = Arc::new(Mutex::new(program.now_playing.clone()));
        mounts.insert(
            slug.to_string(),
            MountEntry {
                tx: tx.clone(),
                meta: StationMeta {
                    name: display_name,
                    genre: program.description.clone(),
                    ..StationMeta::default()
                },
                content_type: sound.content_type().to_string(),
                now_playing: now_playing.clone(),
                program_owned: true,
                // Settled by the pump when it has read a track and knows
                // what to say about the stream.
                lead: Arc::from(&[][..]),
            },
        );
        Some((tx, now_playing))
    }

    /// Note that a station could not play a track, for its operator. The
    /// last few per station, newest first: a rotation of the wrong kind
    /// would otherwise be a silent station and a debug log.
    ///
    /// Returns whether this is news: a track not already left out for the
    /// same reason, which is worth saying in the log. A rotation passes a
    /// track it cannot play every time round, and saying so every time is
    /// a log of nothing else.
    pub fn left_out(&self, slug: &str, title: &str, reason: String, at_unix_ms: u64) -> bool {
        let mut left = self.left_out.lock();
        let list = left.entry(slug.to_string()).or_default();
        let known = list
            .iter()
            .any(|l: &rabbithole_proto::radio::LeftOut| l.title == title && l.reason == reason);
        list.retain(|l: &rabbithole_proto::radio::LeftOut| l.title != title);
        list.insert(
            0,
            rabbithole_proto::radio::LeftOut::new(title, reason, at_unix_ms),
        );
        list.truncate(LEFT_OUT_REMEMBERED);
        !known
    }

    /// What a station has had to leave out, newest first.
    pub fn left_out_for(&self, slug: &str) -> Vec<rabbithole_proto::radio::LeftOut> {
        self.left_out.lock().get(slug).cloned().unwrap_or_default()
    }

    /// What a rotation's mount is sending right now, if it is up and the
    /// rotation's own.
    pub fn program_content_type(&self, slug: &str) -> Option<String> {
        self.mounts
            .lock()
            .get(slug)
            .filter(|m| m.program_owned)
            .map(|m| m.content_type.clone())
    }

    /// What every listener who joins is given before the air: the head of
    /// the stream, for a mount that has one. Says whether it changed, so
    /// the pump sends it over the air once for whoever is already here.
    pub fn set_lead(&self, slug: &str, lead: Arc<[u8]>) -> bool {
        let mut mounts = self.mounts.lock();
        match mounts.get_mut(slug) {
            Some(mount) if mount.program_owned && mount.lead != lead => {
                mount.lead = lead;
                true
            }
            _ => false,
        }
    }

    /// Whether this mount has already told its listeners its stream is
    /// something other than `lead`. A mount that has said nothing yet has
    /// nothing to take back.
    pub fn lead_differs(&self, slug: &str, lead: &Arc<[u8]>) -> bool {
        self.mounts
            .lock()
            .get(slug)
            .is_some_and(|m| m.program_owned && !m.lead.is_empty() && &m.lead != lead)
    }

    /// Take one rotation's mount down, so it can go back up sending
    /// something else. A mount a DJ holds is theirs and is left alone.
    fn retire_program_mount(&self, slug: &str) {
        let mut mounts = self.mounts.lock();
        if mounts.get(slug).is_some_and(|m| m.program_owned) {
            mounts.remove(slug);
        }
    }

    /// The stream listener went away: take every rotation off the air. A
    /// mount a DJ holds is theirs and stays; it ends when their source does.
    /// Dropping the entry closes each listener's stream cleanly.
    pub fn retire_program_mounts(&self) {
        self.mounts.lock().retain(|_, m| !m.program_owned);
        for p in self.programs.lock().values_mut() {
            p.pumped = false;
        }
    }

    /// Whether the pump holding `tx` still has the air: its mount is there,
    /// is the rotation's, and is this very channel.
    fn owns_air(&self, slug: &str, tx: &Fanout) -> bool {
        self.mounts
            .lock()
            .get(slug)
            .is_some_and(|m| m.program_owned && m.tx.same_channel(tx))
    }

    /// What a station's tracks say it will be sending, before any of them
    /// has been read.
    pub fn expected_sound(&self, slug: &str) -> Option<Sound> {
        self.programs.lock().get(slug).and_then(|p| p.expected)
    }

    /// Slugs of all installed library programs, sorted (deterministic).
    pub fn program_slugs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.programs.lock().keys().cloned().collect();
        v.sort();
        v
    }

    /// The one live mount, when exactly one source is connected. Used to
    /// resolve the SHOUTcast `admin.cgi` updinfo form, which carries no mount
    /// (the station is implied by the port).
    pub fn sole_mount(&self) -> Option<String> {
        let mounts = self.mounts.lock();
        if mounts.len() == 1 {
            mounts.keys().next().cloned()
        } else {
            None
        }
    }

    /// Apply a mid-stream metadata (updinfo) title change to a live mount:
    /// listeners' `StreamTitle` blocks pick it up via the mount's shared
    /// now-playing, and a live program's DJ now-playing is updated in step
    /// (keeping the DJ name). Returns the updated [`NowPlaying`], or `None`
    /// when no source is live on `slug`.
    pub fn update_live_metadata(
        &self,
        slug: &str,
        title: &str,
        artist: &str,
    ) -> Option<NowPlaying> {
        let updated = {
            let mounts = self.mounts.lock();
            let entry = mounts.get(slug)?;
            let mut np = entry.now_playing.lock();
            let dj = np.as_ref().map(|n| n.dj.clone()).unwrap_or_default();
            let next = NowPlaying {
                title: title.to_string(),
                artist: artist.to_string(),
                dj,
            };
            *np = Some(next.clone());
            next
        };
        if let Some(p) = self.programs.lock().get_mut(slug) {
            if p.live.is_some() {
                p.live = Some(updated.clone());
            }
        }
        Some(updated)
    }
}

/// Split an updinfo `song` into `(artist, title)` on the conventional
/// `"Artist - Title"` form; a song with no `" - "` is all title.
fn split_song(song: &str) -> (String, String) {
    match song.split_once(" - ") {
        Some((artist, title)) => (artist.trim().to_string(), title.trim().to_string()),
        None => (String::new(), song.trim().to_string()),
    }
}

/// How many left-out tracks a station remembers for its operator. Enough to
/// see the shape of the problem, few enough to be a list rather than a log.
const LEFT_OUT_REMEMBERED: usize = 20;

/// Normalizes a mount target (`/live` or `live`) to a bare slug (`live`).
fn slug_of(mount: &str) -> &str {
    mount
        .strip_prefix('/')
        .unwrap_or(mount)
        .trim_end_matches('/')
}

/// Bind + serve the ICY radio surface. Returns the bound address (useful when
/// the config asked for port 0) and the accept-loop task handle. Mirrors the
/// telnet/finger/nntp spawn helpers.
pub async fn spawn_radio(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, peer)) = listener.accept().await else {
                break;
            };
            // Over the per-IP connection budget: drop it on the floor.
            if !shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = serve(sock, shared, Some(peer.ip())).await {
                    tracing::debug!("radio session error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// Reads the HTTP/ICY request head, returning `(head_bytes, leftover_body)`.
///
/// The head ends at the first blank line (`\r\n\r\n`, or a lone `\n\n`); any
/// bytes already read past it are the first of the request body (a source may
/// pipeline audio right behind its head, so we must not lose them).
async fn read_head<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some((end, body_start)) = find_head_end(&buf) {
            let body = buf[body_start..].to_vec();
            buf.truncate(end);
            return Ok((buf, body));
        }
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok((buf, Vec::new())); // EOF before a full head
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            bail!("request head too large");
        }
    }
}

/// Locates the header/body split: `(head_end, body_start)`.
fn find_head_end(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(i) = find_subslice(buf, b"\r\n\r\n") {
        return Some((i, i + 4));
    }
    find_subslice(buf, b"\n\n").map(|i| (i, i + 2))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Dispatch one connection: peek the method, then serve as a source or listener.
async fn serve(
    mut sock: tokio::net::TcpStream,
    shared: Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> Result<()> {
    let (mut rd, mut wr) = sock.split();
    let (head, body) = read_head(&mut rd).await?;
    if head.is_empty() {
        return Ok(()); // client hung up before sending anything
    }

    // A leading GET is a listener; SOURCE/PUT is a DJ. Anything else: 400.
    let is_get = head
        .split(|&b| b == b' ')
        .next()
        .map(|m| m.eq_ignore_ascii_case(b"GET"))
        .unwrap_or(false);

    if is_get {
        serve_listener(&head, &mut wr, &shared).await
    } else {
        serve_source(&head, body, &mut rd, &mut wr, &shared, peer_ip).await
    }
}

/// Serve a DJ source: authenticate, claim the mount, and fan the body out.
async fn serve_source<R, W>(
    head: &[u8],
    body: Vec<u8>,
    rd: &mut R,
    wr: &mut W,
    shared: &Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let req = match parse_source_request(head) {
        Ok(r) => r,
        Err(_) => {
            wr.write_all(source_forbidden().as_bytes()).await?;
            return Ok(());
        }
    };
    let slug = slug_of(&req.mount).to_string();
    if slug.is_empty() {
        wr.write_all(source_forbidden().as_bytes()).await?;
        return Ok(());
    }

    // Failed source logins drain the per-IP auth budget; an empty bucket
    // refuses the attempt before it is tried.
    if let Some(ip) = peer_ip {
        if !shared.rate_probe(Scope::Ip(ip), rl::AUTH) {
            wr.write_all(source_unauthorized().as_bytes()).await?;
            return Ok(());
        }
    }
    // Authenticate the Basic credentials, then require the broadcast capability.
    let authed = match shared.auth.login_password(&req.user, &req.pass, None).await {
        Ok(u) => u,
        Err(_) => {
            if let Some(ip) = peer_ip {
                let _ = shared.rate_allow(Scope::Ip(ip), rl::AUTH);
            }
            wr.write_all(source_unauthorized().as_bytes()).await?;
            return Ok(());
        }
    };
    if !shared
        .perms
        .allows(&authed.subject, RADIO_RESOURCE, Caps::BROADCAST)
    {
        wr.write_all(source_forbidden().as_bytes()).await?;
        return Ok(());
    }

    // Claim the mount (reject if a source is already live on it). The lock is
    // released before any `.await` so the guard never crosses a suspension.
    let claim = {
        let mut mounts = shared.radio.mounts.lock();
        // A live source holding the mount is refused; a rotation's mount gives
        // way. Replacing the entry closes the rotation's listeners, who
        // reconnect to the DJ (whose stream may well be another codec).
        if mounts.get(&slug).is_some_and(|m| !m.program_owned) {
            None
        } else {
            let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
            let now_playing = Arc::new(Mutex::new(Some(initial_now_playing(
                &req.metadata,
                &authed,
            ))));
            mounts.insert(
                slug.clone(),
                MountEntry {
                    tx: tx.clone(),
                    meta: req.metadata.clone(),
                    content_type: req.content_type.clone(),
                    now_playing: now_playing.clone(),
                    program_owned: false,
                    // A DJ's stream is whatever they are sending; whatever
                    // a listener needs is in it already.
                    lead: Arc::from(&[][..]),
                },
            );
            Some((tx, now_playing))
        }
    };
    let Some((tx, now_playing)) = claim else {
        wr.write_all(source_forbidden().as_bytes()).await?;
        return Ok(());
    };

    // Register in the directory for listings + listener accounting.
    let _ = shared.radio.registry.create(StationConfig {
        slug: slug.clone(),
        display_name: req.metadata.name.clone(),
        description: req.metadata.genre.clone(),
        enabled: true,
    });
    let _ = shared.radio.registry.set_enabled(&slug, true);

    wr.write_all(source_ok(req.method).as_bytes()).await?;
    tracing::info!(mount = %slug, dj = %authed.persona.screen_name, "radio source live");

    // Tell everyone watching that the station is on the air. This surface
    // used to go live in silence: listeners could tune in, but no client was
    // told there was anything to tune in to until the DJ's encoder happened to
    // send a title through the *other* port.
    if let Some(np) = now_playing.lock().clone() {
        let listeners = shared.radio.registry.listener_count(&slug).unwrap_or(0);
        publish_status(
            shared,
            RadioStatus {
                station: slug.clone(),
                title: np.title,
                artist: np.artist,
                dj: np.dj,
                listeners,
                live: true,
            },
        );
    }

    // Fan the body out verbatim until the source disconnects.
    if !body.is_empty() {
        let _ = tx.send(Arc::from(body.into_boxed_slice()));
    }
    let mut chunk = vec![0u8; SOURCE_CHUNK];
    loop {
        let n = match rd.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let _ = tx.send(Arc::from(&chunk[..n]));
    }

    // Source gone: drop the mount (closing every listener) and disable it.
    let _ = now_playing; // kept alive for the source's lifetime
    shared.radio.mounts.lock().remove(&slug);
    // And say so: a rotation behind the mount takes the air back, otherwise
    // the station is off it. What it announces is what it plays next, not
    // the song the DJ talked over.
    shared.radio.end_live(&slug, |t| is_held(shared, t));
    shared.radio.take_air_back(&slug, |t| is_held(shared, t));
    if shared.radio.now_playing(&slug).is_some() {
        publish_now_playing(shared, &slug, false);
    } else {
        shared.radio.note_off_air(&slug);
        shared.presence.clear_radio_now_playing(&slug);
        shared.bus.publish(ServerEvent::RadioOff {
            station: slug.clone(),
        });
        let _ = shared.radio.registry.set_enabled(&slug, false);
    }
    tracing::info!(mount = %slug, "radio source ended");
    Ok(())
}

/// Serve a listener: negotiate metadata, then stream the mount, drop-behind.
async fn serve_listener<W>(head: &[u8], wr: &mut W, shared: &Arc<Shared>) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let req = match parse_listener_request(head) {
        Ok(r) => r,
        Err(_) => {
            wr.write_all(b"HTTP/1.0 400 Bad Request\r\n\r\n").await?;
            return Ok(());
        }
    };
    let slug = slug_of(&req.mount).to_string();

    let Some(handle) = shared.radio.subscribe(&slug) else {
        wr.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n").await?;
        return Ok(());
    };

    let metaint = req.wants_metadata.then_some(DEFAULT_METAINT);
    let response = build_listener_response(&handle.meta, &handle.content_type, metaint);
    wr.write_all(response.as_bytes()).await?;

    // Account for the listener in the directory for its whole session.
    let listener_id = shared.radio.next_listener_id().to_string();
    let _ = shared.radio.registry.join(&slug, listener_id.clone());

    let result = stream_to_listener(handle, metaint, wr).await;

    let _ = shared.radio.registry.leave(&slug, &listener_id);
    result
}

/// The listener's fan-out loop: receive chunks, splice metadata if negotiated,
/// and write. A lagged listener skips ahead (drop-behind, never blocks the
/// source); a closed station or a write error ends the session.
async fn stream_to_listener<W>(
    mut handle: MountHandle,
    metaint: Option<usize>,
    wr: &mut W,
) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut weaver = metaint.map(IcyMetaInterleaver::new);
    // The head of the stream, for a listener who joined in the middle of a
    // song: a FLAC mount says what it is once, and this is how everybody
    // after the first person hears it.
    if !handle.lead.is_empty() {
        let lead = match weaver.as_mut() {
            Some(weaver) => weaver.push(&handle.lead),
            None => handle.lead.to_vec(),
        };
        if wr.write_all(&lead).await.is_err() {
            return Ok(());
        }
    }
    loop {
        let chunk = match handle.rx.recv().await {
            Ok(chunk) => chunk,
            Err(broadcast::error::RecvError::Lagged(_)) => continue, // drop-behind
            Err(broadcast::error::RecvError::Closed) => break,       // source gone
        };
        let out = match weaver.as_mut() {
            Some(weaver) => {
                if let Some(np) = handle.now_playing.lock().as_ref() {
                    weaver.set_title(stream_title(np));
                }
                weaver.push(&chunk)
            }
            None => chunk.to_vec(),
        };
        if wr.write_all(&out).await.is_err() {
            break; // listener disconnected
        }
    }
    Ok(())
}

/// The `StreamTitle` string for a now-playing item: `"Artist - Title"`, or just
/// the title when the artist is unknown.
fn stream_title(np: &NowPlaying) -> String {
    if np.artist.trim().is_empty() {
        np.title.clone()
    } else {
        format!("{} - {}", np.artist, np.title)
    }
}

/// The now-playing snapshot a source implies at connect: no track has played
/// yet, so we surface the station name as the title and the DJ persona.
fn initial_now_playing(
    meta: &StationMeta,
    authed: &rabbithole_server_core::AuthedUser,
) -> NowPlaying {
    let title = if meta.now_playing.trim().is_empty() {
        meta.name.clone()
    } else {
        meta.now_playing.clone()
    };
    NowPlaying {
        title,
        artist: String::new(),
        dj: authed.persona.screen_name.clone(),
    }
}

// ---------------------------------------------------------------------------
// Library-from-file-areas playlist source
// ---------------------------------------------------------------------------

/// Whether a library node looks like a playable audio file, by MIME first and
/// then by filename extension (many uploads carry a generic MIME).
fn is_audio(name: &str, mime: &str) -> bool {
    if mime.to_ascii_lowercase().starts_with("audio/") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    const EXTS: [&str; 8] = [
        ".mp3", ".ogg", ".oga", ".opus", ".flac", ".wav", ".aac", ".m4a",
    ];
    EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Maps one file node to a [`Track`], or `None` if it is not a playable audio
/// file (wrong kind, no blob, or non-audio type). Pure and testable.
fn track_from_node(node: &FileNodeRow) -> Option<Track> {
    if node.kind != KIND_FILE {
        return None;
    }
    let blob = node.blob_id?;
    if !is_audio(&node.name, &node.mime) {
        return None;
    }
    Some(Track::new(
        TrackId(node.id as u64),
        node.name.clone(),
        node.comment.clone(), // artist/notes, when the uploader supplied any
        DEFAULT_TRACK_MS,
        BlobId(blob),
    ))
}

/// Is this a picture a player could show as a cover?
fn is_image(name: &str, mime: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    mime.starts_with("image/")
        || [".jpg", ".jpeg", ".png", ".webp", ".gif"]
            .iter()
            .any(|ext| lower.ends_with(ext))
}

/// The name without its extension, lowercased: `"Down the Hole.mp3"` and
/// `"down the hole.JPG"` are the same stem.
fn stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => lower,
    }
}

/// Cover art for a file area's tracks, the way music folders have always done
/// it: an image named like the track wins, else the folder's `cover`,
/// `folder`, `front` or `album` image. Keyed by track title (the file name),
/// which is what now-playing carries. Tracks with neither get no entry.
pub fn covers_from_nodes(nodes: &[FileNodeRow]) -> HashMap<String, [u8; 32]> {
    const FOLDER_ART: [&str; 4] = ["cover", "folder", "front", "album"];
    let images: Vec<(&FileNodeRow, [u8; 32])> = nodes
        .iter()
        .filter(|n| n.kind == KIND_FILE && is_image(&n.name, &n.mime))
        .filter_map(|n| n.blob_id.map(|b| (n, b)))
        .collect();
    let mut covers = HashMap::new();
    for track in nodes {
        if track.kind != KIND_FILE || track.blob_id.is_none() || !is_audio(&track.name, &track.mime)
        {
            continue;
        }
        let beside = |n: &&(&FileNodeRow, [u8; 32])| n.0.parent_id == track.parent_id;
        let own = images
            .iter()
            .filter(beside)
            .find(|(img, _)| stem(&img.name) == stem(&track.name));
        let folder = FOLDER_ART.iter().find_map(|wanted| {
            images
                .iter()
                .filter(beside)
                .find(|(img, _)| stem(&img.name) == *wanted)
        });
        if let Some((_, blob)) = own.or(folder) {
            covers.insert(track.name.clone(), *blob);
        }
    }
    covers
}

/// Maps a file-area listing into a playlist track list, preserving order and
/// dropping non-audio nodes. This is the file-listing → track-list seam.
pub fn tracks_from_nodes(nodes: &[FileNodeRow]) -> Vec<Track> {
    nodes.iter().filter_map(track_from_node).collect()
}

/// What kind of sound a file's name and type say it is, without reading it.
/// A station is built from this; the pump checks the bytes themselves
/// before sending any, and leaves out anything that disagrees.
pub fn sound_of_name(name: &str, mime: &str) -> Option<Sound> {
    let lower = name.to_ascii_lowercase();
    let mime = mime.to_ascii_lowercase();
    if lower.ends_with(".mp3") || mime == "audio/mpeg" || mime == "audio/mp3" {
        return Some(Sound::Mpeg);
    }
    if lower.ends_with(".flac") || mime == "audio/flac" || mime == "audio/x-flac" {
        return Some(Sound::Flac(Form::default()));
    }
    if [".ogg", ".oga", ".opus"].iter().any(|e| lower.ends_with(e))
        || mime == "audio/ogg"
        || mime == "audio/opus"
        || mime == "application/ogg"
    {
        // The rate is read from the file itself when it plays; this only
        // says which mount it belongs on.
        return Some(Sound::Ogg(0));
    }
    None
}

/// What a rotation of `tracks` will send, worked out from their names alone.
/// The most of any one kind wins, and a tie goes the way a library hands out
/// its mounts: MP3, then Ogg, then FLAC. Files this burrow cannot send at all
/// are not counted — they would otherwise vote the mount up as MP3 and cut
/// every listener the moment the first real track was read.
///
/// A caller that has already sorted its tracks by kind knows better than this
/// and passes the kind straight to [`Stations::install_program`].
pub fn sound_of_tracks(tracks: &[Track]) -> Option<Sound> {
    let of = |want: fn(&Sound) -> bool| {
        tracks
            .iter()
            .filter(|t| sound_of_name(&t.title, "").as_ref().is_some_and(want))
            .count()
    };
    let mpeg = of(|s| matches!(s, Sound::Mpeg));
    let ogg = of(|s| matches!(s, Sound::Ogg(_)));
    let flac = of(|s| matches!(s, Sound::Flac(_)));
    match mpeg.max(ogg).max(flac) {
        0 => None,
        most if mpeg == most => Some(Sound::Mpeg),
        most if ogg == most => Some(Sound::Ogg(0)),
        _ => Some(Sound::Flac(Form::default())),
    }
}

/// Split a library's tracks by the kind of sound they are, so each kind can
/// have a mount of its own: the MP3 files on one, the Ogg files on another,
/// and nothing left out for being the wrong kind. Tracks that are neither
/// stay where they are — the pump says what it could not play.
pub fn split_by_sound(nodes: &[FileNodeRow]) -> Rotations {
    let mut out = Rotations::default();
    for node in nodes {
        let Some(track) = track_from_node(node) else {
            continue;
        };
        match sound_of_name(&node.name, &node.mime) {
            Some(Sound::Mpeg) => out.mpeg.push(track),
            Some(Sound::Ogg(_)) => out.ogg.push(track),
            Some(Sound::Flac(_)) => out.flac.push(track),
            None => out.other.push(track),
        }
    }
    out
}

/// A library's tracks, by the kind of sound they are.
#[derive(Debug, Default)]
pub struct Rotations {
    pub mpeg: Vec<Track>,
    pub ogg: Vec<Track>,
    pub flac: Vec<Track>,
    /// Audio this burrow cannot send as it is (an AAC file, say). They go
    /// with the first mount, which says what it could not play.
    pub other: Vec<Track>,
}

// ---------------------------------------------------------------------------
// DJ live source ingest + now-playing plumbing
// ---------------------------------------------------------------------------

/// The now-playing a source implies at connect, from its `ice-*` metadata.
fn now_playing_from_ice(meta: &StationMeta, dj: &str) -> NowPlaying {
    let title = if meta.now_playing.trim().is_empty() {
        meta.name.clone()
    } else {
        meta.now_playing.clone()
    };
    NowPlaying {
        title,
        artist: meta.genre.clone(),
        dj: dj.to_string(),
    }
}

/// Publishes a station's current now-playing (with the live listener count)
/// into presence, so status lines pick it up like away/idle status.
pub(crate) fn publish_now_playing(shared: &Arc<Shared>, slug: &str, live: bool) {
    let Some(np) = shared.radio.now_playing(slug) else {
        return;
    };
    let listeners = shared.radio.registry.listener_count(slug).unwrap_or(0);
    publish_status(
        shared,
        RadioStatus {
            station: slug.to_string(),
            title: np.title,
            artist: np.artist,
            dj: np.dj,
            listeners,
            live,
        },
    );
}

/// The one door a now-playing report leaves through: it is remembered (so the
/// station has a history to show someone who just arrived) and then published
/// to presence, which pushes it to everyone watching.
fn publish_status(shared: &Arc<Shared>, status: RadioStatus) {
    shared
        .radio
        .note_now_playing(&status.station, &status.title, &status.artist, unix_ms());
    shared.presence.set_radio_now_playing(status);
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The listing a client asks for on arrival: every station on the air, what
/// it is playing, what it played, its cover, and whether there is audio to be
/// had. Built from presence (the same statuses that get pushed), so the
/// listing and the pushes can never disagree about what is on.
pub fn station_listing(shared: &Arc<Shared>) -> Vec<rabbithole_proto::radio::RadioStationInfo> {
    use rabbithole_proto::radio::{RadioPlayed, RadioStationInfo};
    shared
        .presence
        .radio_now_playing()
        .into_iter()
        .map(|status| {
            let info = shared.radio.registry.get(&status.station);
            let name = info
                .as_ref()
                .map(|i| i.display_name.clone())
                .unwrap_or_else(|| status.station.clone());
            let recent = shared
                .radio
                .recent(&status.station)
                .into_iter()
                .map(|p| RadioPlayed::new(p.title, p.artist, p.started_unix_ms))
                .collect();
            RadioStationInfo::new(status.station.clone(), name)
                .described(info.map(|i| i.description).unwrap_or_default())
                .on_air(
                    status.listeners as u32,
                    status.live,
                    shared.radio.is_streaming(&status.station),
                )
                .with_cover(shared.radio.cover_for(&status.station, &status.title))
                .with_recent(recent)
                .playing(status.title, status.artist, status.dj)
        })
        .collect()
}

/// Every station this burrow runs, as its operator needs to see it: what is
/// on, who is listening, what it is sending, and what it could not play.
/// Unlike [`station_listing`], this includes stations that are installed but
/// silent — which is exactly when an operator comes looking.
pub fn station_status(shared: &Arc<Shared>) -> Vec<rabbithole_proto::radio::RadioStationStatus> {
    let areas = shared.config.read().radio_library_areas.clone();
    let mut out = Vec::new();
    for slug in shared.radio.program_slugs() {
        let info = shared.radio.registry.get(&slug);
        let now = shared.radio.now_playing(&slug);
        let name = info
            .as_ref()
            .map(|i| i.display_name.clone())
            .unwrap_or_else(|| slug.clone());
        out.push(
            rabbithole_proto::radio::RadioStationStatus::new(slug.clone(), name)
                .of_area(
                    // A companion mount (`<slug>.ogg`) plays the same area
                    // as the station it is beside.
                    areas
                        .get(&slug)
                        .or_else(|| slug.rsplit_once('.').and_then(|(base, _)| areas.get(base)))
                        .cloned()
                        .unwrap_or_default(),
                    shared.radio.program_content_type(&slug).unwrap_or_default(),
                )
                .on_air(
                    now.as_ref().map(|n| n.title.clone()).unwrap_or_default(),
                    now.as_ref().map(|n| n.artist.clone()).unwrap_or_default(),
                    info.as_ref().map(|i| i.listener_count as u32).unwrap_or(0),
                    shared.radio.is_live(&slug),
                )
                .with_rotation(
                    shared.radio.track_count(&slug) as u32,
                    shared.radio.left_out_for(&slug),
                ),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Rotation playout: a library station that actually streams
// ---------------------------------------------------------------------------

/// How much audio one send carries. Long enough that a track is a few
/// hundred sends rather than tens of thousands, short enough that a DJ taking
/// the air is noticed within a quarter of a second.
const PUMP_BATCH_MICROS: u64 = 250_000;

/// And no send is bigger than this, whatever the timing arithmetic says. A
/// quarter second of audio is far under it; a file whose frames claim less
/// time than they hold is what it is for, so no listener is ever handed a
/// whole track in one breath.
const PUMP_BATCH_BYTES: usize = 1 << 20;

/// How far ahead of the clock the pump runs, so a player that just tuned in
/// has something to buffer instead of starving on its first frame.
const PUMP_LEAD: Duration = Duration::from_secs(2);

/// A run of whole, contiguous frames and how long it plays for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub bytes: std::ops::Range<usize>,
    pub micros: u64,
}

/// Cut a track into sends: whole frames only (a listener's decoder should
/// never be handed half of one by us), contiguous bytes only (junk between
/// frames is not audio and is not sent), about a quarter second each.
pub fn batches(track: &[u8]) -> Vec<Batch> {
    let mut out: Vec<Batch> = Vec::new();
    let mut open: Option<Batch> = None;
    for frame in rabbithole_radio::mp3::frames(track) {
        let end = frame.offset + frame.len;
        match open.as_mut() {
            Some(b)
                if b.bytes.end == frame.offset
                    && b.micros < PUMP_BATCH_MICROS
                    && b.bytes.len() < PUMP_BATCH_BYTES =>
            {
                b.bytes.end = end;
                b.micros += frame.micros();
            }
            _ => {
                out.extend(open.take());
                open = Some(Batch {
                    bytes: frame.offset..end,
                    micros: frame.micros(),
                });
            }
        }
    }
    out.extend(open);
    out
}

/// Cut an Ogg track (Opus, Vorbis) into sends, the same way and for the same
/// reasons as [`batches`]: whole pages only, contiguous bytes only, about a
/// quarter second each. A page that completes no packet carries no time of
/// its own; it goes out with the page that finishes what it started.
pub fn ogg_batches(track: &[u8], rate: u32) -> Vec<Batch> {
    use rabbithole_radio::ogg;
    if rate == 0 {
        return Vec::new();
    }
    let mut out: Vec<Batch> = Vec::new();
    let mut open: Option<Batch> = None;
    let mut played = 0u64; // samples the stream has reached
    for page in ogg::pages(track) {
        let end = page.offset + page.len;
        // The headers, and any page that finishes no packet, are worth no
        // time; a page that finishes one is worth the samples it added.
        let micros = match page.granule {
            Some(granule) if granule > played => {
                let micros = (granule - played) * 1_000_000 / u64::from(rate);
                played = granule;
                micros
            }
            _ => 0,
        };
        match open.as_mut() {
            Some(b)
                if b.bytes.end == page.offset
                    && b.micros < PUMP_BATCH_MICROS
                    && b.bytes.len() < PUMP_BATCH_BYTES =>
            {
                b.bytes.end = end;
                b.micros += micros;
            }
            _ => {
                out.extend(open.take());
                open = Some(Batch {
                    bytes: page.offset..end,
                    micros,
                });
            }
        }
    }
    out.extend(open);
    out
}

/// A FLAC track, written again as the next stretch of a mount's stream:
/// whole frames only, about a quarter second to a send, each one renumbered
/// to carry on from `from` — the samples the mount has already sent.
///
/// The file's own headers stay behind. A mount says what it is once, at the
/// head of its stream, and a listener who joins later is given that as they
/// arrive; a fresh `fLaC` magic between two songs is what stops a native
/// FLAC player dead at the end of the first one.
pub fn flac_stream(track: &[u8], from: u64) -> Cut {
    use rabbithole_radio::flac;
    // Headers and no frames is not a track: sending them alone would take
    // no time at all, and a station would race through its rotation. One
    // walk of the file, because walking it is the expensive part.
    let frames = flac::frames(track);
    let mut audio = Vec::with_capacity(track.len());
    let mut sends: Vec<Batch> = Vec::new();
    let mut open: Option<Batch> = None;
    let mut played = from;
    for frame in frames {
        let bytes = &track[frame.offset..frame.offset + frame.len];
        // A frame that cannot be written again is one the mount cannot
        // send: skipping it is a click, where sending it as it came would
        // be a decoder told to go back to the beginning of another song.
        let Some(again) = flac::renumber(bytes, frame.header, frame.number, played) else {
            continue;
        };
        let start = audio.len();
        audio.extend_from_slice(&again);
        let end = audio.len();
        played += u64::from(frame.samples);
        match open.as_mut() {
            Some(b)
                if b.bytes.end == start
                    && b.micros < PUMP_BATCH_MICROS
                    && b.bytes.len() < PUMP_BATCH_BYTES =>
            {
                b.bytes.end = end;
                b.micros += frame.micros();
            }
            _ => {
                sends.extend(open.take());
                open = Some(Batch {
                    bytes: start..end,
                    micros: frame.micros(),
                });
            }
        }
    }
    sends.extend(open);
    Cut {
        audio,
        sends,
        samples: played - from,
    }
}

/// How many requests one person may have waiting on a station at once.
/// Enough to ask for a few songs; not enough to fill the evening.
pub const REQUESTS_EACH: usize = 3;

/// How many requests a station holds at all. A queue longer than this is
/// hours of other people's asking, and nobody's request would ever come up.
pub const REQUESTS_WAITING: usize = 50;

/// How many tracks one look through a station's offer brings back. A
/// library can hold tens of thousands; a person narrows the search to find
/// the one they want, and a reply stays well inside a frame.
pub const OFFER_SHOWN: usize = 100;

/// The most of a title or an artist a listener is sent. A file's name is
/// short; its comment, which stands for the artist, is whatever an uploader
/// typed.
const WORDS_SENT: usize = 200;

/// `words`, cut to what a listener is sent.
fn clip(words: &str) -> String {
    words.chars().take(WORDS_SENT).collect()
}

/// Why a station would not take a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestRefused {
    /// No station by that name has a rotation to ask of.
    NoSuchStation,
    /// Nothing by that id is in the station's rotation that it can play:
    /// not there, of a kind it cannot send, left out, or held back.
    NotInRotation,
    /// A vote for something nobody has asked for.
    NotWaiting,
    /// It is what is playing now.
    PlayingNow,
    /// This person already has [`REQUESTS_EACH`] waiting.
    TooManyOfYours,
    /// The station already holds [`REQUESTS_WAITING`].
    Full,
}

/// What a station is sending, and how a track of that kind is paced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// MPEG audio, `audio/mpeg`.
    Mpeg,
    /// Ogg (Opus or Vorbis), `audio/ogg`, paced by granule at this rate.
    Ogg(u32),
    /// FLAC, `audio/flac`, paced by the samples each frame holds, in the
    /// one form this mount's listeners were told about.
    Flac(Form),
}

/// What a FLAC stream is: the three things a listener's decoder is told
/// once, at the head of the stream, and cannot be told again part-way
/// through. A mount sends one form of FLAC, so a track of another is left
/// out with a word about why rather than stopping every decoder on it.
///
/// A form of zeros is one nobody has looked up yet — a name that ends in
/// `.flac` says this much and no more — and it matches whatever it meets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Form {
    pub rate: u32,
    pub channels: u8,
    pub bits: u8,
}

impl Form {
    /// Whether a mount sending `self` can send `other` without its
    /// listeners being told something new.
    fn same_as(self, other: Form) -> bool {
        self == other || self == Form::default() || other == Form::default()
    }

    /// The form, for a person: what the console says a station is sending.
    fn say(self) -> String {
        if self == Form::default() {
            return "audio/flac".to_string();
        }
        format!(
            "audio/flac, {} Hz, {}, {} bit",
            self.rate,
            match self.channels {
                1 => "mono".to_string(),
                2 => "stereo".to_string(),
                n => format!("{n} channels"),
            },
            self.bits
        )
    }
}

impl Sound {
    /// What an ICY listener is told this mount is.
    pub fn content_type(self) -> &'static str {
        match self {
            Sound::Mpeg => "audio/mpeg",
            Sound::Ogg(_) => "audio/ogg",
            Sound::Flac(_) => "audio/flac",
        }
    }

    /// What this mount is sending, for a person: the content type, and for
    /// FLAC the form of it, because two FLAC tracks are not interchangeable.
    pub fn say(self) -> String {
        match self {
            Sound::Flac(form) => form.say(),
            other => other.content_type().to_string(),
        }
    }

    /// What this track is, if it is anything a station can send as it is.
    pub fn of(track: &[u8]) -> Option<Sound> {
        if rabbithole_radio::mp3::looks_like_mp3(track) {
            return Some(Sound::Mpeg);
        }
        if let Some(info) = rabbithole_radio::flac::playable(track) {
            return Some(Sound::Flac(Form {
                rate: info.sample_rate,
                channels: info.channels,
                bits: info.bits_per_sample,
            }));
        }
        rabbithole_radio::ogg::codec(track).map(|(_, rate)| Sound::Ogg(rate))
    }

    /// The track, cut into sends. `from` is how many samples this mount has
    /// sent already, which only FLAC has any use for.
    pub fn cut(self, track: Vec<u8>, from: u64) -> Cut {
        match self {
            Sound::Mpeg => Cut::of(batches(&track), track),
            Sound::Ogg(rate) => Cut::of(ogg_batches(&track, rate), track),
            Sound::Flac(_) => flac_stream(&track, from),
        }
    }

    /// Whether a mount sending `self` can send `other` next: only the same
    /// kind of sound, because a listener's decoder is not told to start
    /// again mid-stream.
    pub fn same_as(self, other: Sound) -> bool {
        match (self, other) {
            (Sound::Mpeg, Sound::Mpeg) | (Sound::Ogg(_), Sound::Ogg(_)) => true,
            // Two FLAC tracks are not interchangeable the way two MP3s are:
            // the rate, the channels and the depth are in the one STREAMINFO
            // the mount's listeners were given, and cannot be taken back.
            (Sound::Flac(a), Sound::Flac(b)) => a.same_as(b),
            _ => false,
        }
    }
}

/// A track, ready to go out: the bytes a mount will send and the sends that
/// point into them.
///
/// For MP3 and Ogg the bytes are the track's own, untouched. A FLAC mount
/// sends a rewrite instead, because a stream is one stream and not a file
/// after a file: the file's own headers stay behind (the mount said what it
/// is once, at the top) and every frame is renumbered to carry on from the
/// track before.
pub struct Cut {
    /// What goes out, which the sends below index into.
    pub audio: Vec<u8>,
    pub sends: Vec<Batch>,
    /// How many samples this adds to what the mount has sent.
    pub samples: u64,
}

impl Cut {
    /// A track that goes out as it is.
    fn of(sends: Vec<Batch>, audio: Vec<u8>) -> Cut {
        Cut {
            audio,
            sends,
            samples: 0,
        }
    }
}

/// Start streaming a library station: one task that plays its rotation out
/// loud, for as long as the burrow runs.
pub fn spawn_program_pump(shared: Arc<Shared>, slug: String) -> JoinHandle<()> {
    shared.radio.set_pumped(&slug, true);
    // The station is on the air from this line, not from whenever the pump has
    // finished loading its first track: a listener who tuned in during that
    // moment was told 404 by a station that was about to play.
    // The mount goes up as what this station's tracks say it is, so a
    // listener who tunes in before the first track has been read is told
    // the truth and is not cut off when it is.
    let expected = shared.radio.expected_sound(&slug).unwrap_or(Sound::Mpeg);
    let _ = shared.radio.program_mount(&slug, expected);
    tokio::spawn(program_pump(shared, slug))
}

/// Where a mount's sample count goes after a track: back to the start, once
/// it has gone far enough that the next song might run past what a number
/// can hold.
///
/// Between songs is the place to do it. A jump backwards in a stream with no
/// length is something a decoder takes in its stride; one in the middle of a
/// song is a click in the middle of a song. Half of what a number can hold
/// is about six and three quarter hours of playing at 44.1 kHz, an hour and
/// a half at 192 kHz, and no one track is that long.
///
/// Nobody hears it — every decoder tried plays straight through, and the
/// audio is the same bytes either way — but a recording made with
/// `ffmpeg -c copy` across the moment carries no usable timeline after it:
/// the CLI drops the timestamp of every later frame, so the container says
/// it is only as long as the part before. There is nowhere else to go: the
/// seven-byte number that would avoid it is the one libavcodec refuses
/// outright, which is silence rather than a wrong duration field.
fn next_number(played: u64) -> u64 {
    // Where the count actually is: a track longer than half of what a
    // number can hold wraps inside itself, and starting the next one at
    // zero would hand out numbers it has just written.
    let played = played % rabbithole_radio::flac::NUMBER_LIMIT;
    if played >= rabbithole_radio::flac::NUMBER_LIMIT / 2 {
        0
    } else {
        played
    }
}

/// Play a rotation: load the current track, send it at the speed it plays,
/// move on when its audio ends. A station plays whether or not anyone is
/// listening, so the clock runs regardless and only the sends are skipped.
///
/// A DJ may take the air at any moment (the mount is replaced under us). The
/// pump notices within one batch, stands down, and picks the rotation back up
/// with the next track when the DJ leaves.
async fn program_pump(shared: Arc<Shared>, slug: String) {
    // One clock for the station, not one per track: restarting it per track
    // would push the lead again each time, and a listener's buffer would grow
    // by two seconds a song.
    let mut clock: Option<(std::time::Instant, Duration)> = None;
    // Turns of the rotation in a row that could not be played. A request
    // that could not be played is not a turn of the rotation.
    let mut passed = 0usize;
    // Whether any of those were held back by a moderator. A rotation passed
    // over because of holds says nothing about whether the library guessed
    // right about what the station sends.
    let mut passed_held = false;
    // What this station is sending. The library already said, if it was
    // able to look: a mount says what its stream is once, so which form of
    // FLAC it is must not depend on which track came up first.
    let mut sending: Option<Sound> = match shared.radio.expected_sound(&slug) {
        Some(Sound::Flac(form)) if form != Form::default() => Some(Sound::Flac(form)),
        _ => None,
    };
    // Whether that is still only the library's word for it. It read the
    // front of every file to work it out, and the front of a file can be
    // the only part of it that is any good — so until a track has actually
    // gone out, it is a guess, and a guess that leaves a whole rotation
    // out is worse than no guess at all.
    let mut unproven = sending.is_some();
    // How many samples this mount has sent. A FLAC stream numbers its
    // frames by it, so a decoder is never told to go back to the start of
    // a song it has already played.
    let mut played = 0u64;
    loop {
        if shared.radio.is_live(&slug) || shared.radio.dj_holds(&slug) {
            clock = None; // whoever comes back starts a fresh stream
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        // A track a DJ talked over moves on now they have gone, if the way
        // they left has not already seen to it.
        shared.radio.take_air_back(&slug, |t| is_held(&shared, t));
        let Some(track) = shared.radio.current_track(&slug) else {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        let requested = shared.radio.current_was_requested(&slug);
        // What a moderator is holding back is not played to anybody, the
        // same as it is not handed out anywhere else. It is not even read.
        let held = is_held(&shared, &track);
        let blobs = shared.blobs.clone();
        let id = rabbithole_blobs::BlobId(track.source.0);
        // Reading the file and working out what it is happen off the
        // runtime: it is a disk read and a look at the headers, and this
        // thread is also answering everybody else.
        let read = if held {
            None
        } else {
            tokio::task::spawn_blocking(move || {
                let bytes = blobs.get(&id).ok()?;
                let kind = Sound::of(&bytes);
                Some((bytes, kind))
            })
            .await
            .ok()
            .flatten()
        };
        let (had_bytes, bytes, kind) = match read {
            Some((bytes, kind)) => (true, Some(bytes), kind),
            None => (false, None, None),
        };
        // What this track is, and whether this mount can send it: a
        // listener's decoder is never handed a different kind of sound
        // part-way through, so a station sends one kind and says which
        // tracks it had to leave out.
        let playable = match (kind, sending) {
            (Some(kind), None) => Some(kind),
            (Some(kind), Some(air)) if air.same_as(kind) => Some(kind),
            _ => None,
        };
        // Cutting a track into sends is the expensive part — for FLAC a
        // checksum over the whole file, and another to write it again — so
        // it happens for a track this station is going to send, and not
        // for one it is about to leave out. The mount is silent while this
        // runs, and a station with a shelf of the wrong thing would
        // otherwise be silent for as long as it took to read all of them.
        let from = played;
        let cuts = match (bytes, playable) {
            (Some(bytes), Some(kind)) => tokio::task::spawn_blocking(move || kind.cut(bytes, from))
                .await
                .ok(),
            _ => None,
        };
        // A file can say what it is in its headers and hold no audio at
        // all — a FLAC cut off after STREAMINFO, an upload that stopped.
        // Sending the headers alone would take no time, so the rotation
        // would spin through the whole station at once, saying nothing was
        // wrong. A track with no time in it is a track that cannot play.
        let cuts = cuts.filter(|cut: &Cut| cut.sends.iter().any(|b| b.micros > 0));
        let (Some(kind), Some(cuts)) = (playable, cuts) else {
            // Nothing this station can send as it is: an unreadable file, a
            // format this burrow does not stream, or the wrong kind for a
            // mount already on the air. Said out loud, once per track —
            // silence with no reason is the worst way to find out.
            let reason = match (had_bytes, kind, sending) {
                _ if held => "held back by a moderator".to_string(),
                (false, _, _) => "could not be read".to_string(),
                (true, None, _) => "not audio this burrow can stream".to_string(),
                (true, Some(kind), Some(air)) if !air.same_as(kind) => {
                    format!("not what this station is sending ({})", air.say())
                }
                _ => "no audio could be read from it".to_string(),
            };
            let said = reason.clone();
            // Not offered to listeners from now on. A held track is looked
            // up afresh each time instead: a hold is lifted, a bad file is not.
            if !held {
                shared.radio.cannot_play(&slug, track.id);
            }
            if shared
                .radio
                .left_out(&slug, &track.title, reason, unix_ms())
            {
                tracing::warn!(mount = %slug, track = %track.title, reason = %said, "radio: track left out");
            }
            shared.radio.advance(&slug, unix_ms());
            if !requested {
                passed += 1;
                passed_held |= held;
            }
            // Reading a track and finding it cannot be sent takes time and
            // sends nothing, and the clock keeps running. Once the station
            // has fallen further behind its own clock than a listener's
            // buffer is deep, the next thing it can play would go out in
            // one burst to catch up: the clock starts again instead, from
            // whatever anybody actually hears next.
            if clock.is_some_and(|(started, sent)| started.elapsed() > sent + PUMP_LEAD) {
                clock = None;
            }
            if passed >= shared.radio.track_count(&slug).max(1) {
                passed = 0;
                let run_held = std::mem::take(&mut passed_held);
                if unproven && !run_held {
                    // A whole rotation, and nothing in it is what the
                    // library said this station sends. Take the next thing
                    // that can play instead of being silent all night over
                    // a shelf of music.
                    tracing::warn!(
                        mount = %slug,
                        said = %sending.map(|s| s.say()).unwrap_or_default(),
                        "radio: nothing in the rotation is what this station was said to send; \
                         the next track that plays settles it"
                    );
                    sending = None;
                    unproven = false;
                    continue;
                }
                // Half a minute of not even trying, while the clock it
                // paces by keeps running. Whatever plays next starts a
                // fresh one, or it would go out all at once to catch up
                // and a listener would get half a minute of audio in one
                // breath after half a minute of silence.
                clock = None;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            continue;
        };
        passed = 0;
        passed_held = false;
        // It plays: offered again if it had been left out before, and no
        // longer said to be.
        shared.radio.can_play(&slug, track.id, &track.title);
        // What a listener needs before any of this makes sense. A FLAC
        // mount says what its stream is once, at the head of it; the other
        // kinds say it in every frame and need nothing here.
        let lead: Arc<[u8]> = match kind {
            Sound::Flac(form) => {
                rabbithole_radio::flac::stream_headers(form.rate, form.channels, form.bits).into()
            }
            _ => Arc::from(&[][..]),
        };
        // The first playable track settles what the mount is sending. The
        // mount is only remade when it is up as something else: remaking it
        // cuts whoever is listening, and a station of the kind the mount
        // already went up as must not do that at every start. A mount that
        // has already told its listeners what its stream is counts as
        // something else when what it said no longer holds — better one
        // clean cut than a decoder told something new mid-song.
        if shared
            .radio
            .program_content_type(&slug)
            .is_some_and(|sending| sending != kind.content_type())
            || shared.radio.lead_differs(&slug, &lead)
        {
            shared.radio.retire_program_mount(&slug);
        }
        sending = Some(kind);
        unproven = false;
        let Some((tx, title_slot)) = shared.radio.program_mount(&slug, kind) else {
            continue; // a DJ got there first; the top of the loop waits
        };
        // Everyone who arrives from here on is handed it as they connect;
        // whoever was already here, waiting through the first track's read,
        // is sent it now.
        if shared.radio.set_lead(&slug, lead.clone()) && !lead.is_empty() {
            let _ = tx.send(lead);
        }
        played = next_number(played + cuts.samples);
        *title_slot.lock() = shared.radio.now_playing(&slug);
        publish_now_playing(&shared, &slug, false);

        let (started, mut sent) = clock.unwrap_or((std::time::Instant::now(), Duration::ZERO));
        let mut interrupted = false;
        let Cut { audio, sends, .. } = cuts;
        for batch in sends {
            if !shared.radio.owns_air(&slug, &tx) {
                interrupted = true;
                break;
            }
            if tx.receiver_count() > 0 {
                let _ = tx.send(Arc::from(&audio[batch.bytes.clone()]));
            }
            sent += Duration::from_micros(batch.micros);
            let due = sent.saturating_sub(PUMP_LEAD);
            let elapsed = started.elapsed();
            if due > elapsed {
                tokio::time::sleep(due - elapsed).await;
            }
        }
        clock = (!interrupted).then_some((started, sent));
        // Finished or interrupted, the rotation moves on: a station that was
        // talked over does not replay the song from the top. Talked over, it
        // moves on when the DJ leaves.
        if interrupted {
            shared.radio.owe_advance(&slug);
        } else {
            shared.radio.advance(&slug, unix_ms());
        }
    }
}

/// Whether a moderator is holding `track`'s file back, or its content is
/// refused outright.
pub fn is_held(shared: &Shared, track: &Track) -> bool {
    shared.moderation.file_quarantined(Some(&track.source.0))
        || shared.moderation.is_denied(&track.source.0)
}

/// Bind + serve the DJ **source ingest** surface (SOURCE/PUT). Distinct from
/// [`spawn_radio`], which is the listener *delivery* surface. Returns the bound
/// address and the accept-loop handle. Mirrors the other legacy spawn helpers.
pub async fn spawn_radio_source(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, peer)) = listener.accept().await else {
                break;
            };
            // Over the per-IP connection budget: drop it on the floor.
            if !shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_ingest(sock, shared, Some(peer.ip())).await {
                    tracing::debug!("radio source session error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// One DJ source connection: read the head, ingest, then shut the write half
/// down gracefully (`.shutdown()` before drop) so the final response is not
/// truncated by an RST on macOS/Windows.
///
/// Besides SOURCE/PUT streams, this surface also answers the short-lived
/// `GET /admin/metadata` / `GET /admin.cgi` **updinfo** request encoders use
/// to announce track changes mid-stream.
async fn serve_ingest(
    mut sock: tokio::net::TcpStream,
    shared: Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> Result<()> {
    let (mut rd, mut wr) = sock.split();
    let (head, body) = read_head(&mut rd).await?;
    let result = if head.is_empty() {
        Ok(()) // client hung up before sending anything
    } else if let Ok(update) = parse_metadata_update(&head) {
        handle_metadata_update(update, &mut wr, &shared, peer_ip).await
    } else {
        ingest_source(&head, body, &mut rd, &mut wr, &shared, peer_ip).await
    };
    let _ = wr.shutdown().await;
    result
}

/// Apply a mid-stream metadata (updinfo) request: verify the credentials
/// against the configured source user/password, update the mount's
/// now-playing (splitting `"Artist - Title"`), republish presence, and answer
/// with the codec's Icecast-convention XML reply (or `401`).
async fn handle_metadata_update<W>(
    update: MetadataUpdate,
    wr: &mut W,
    shared: &Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    // Failed updinfo credentials drain the per-IP auth budget; an empty
    // bucket refuses before checking.
    if let Some(ip) = peer_ip {
        if !shared.rate_probe(Scope::Ip(ip), rl::AUTH) {
            wr.write_all(metadata_update_unauthorized().as_bytes())
                .await?;
            return Ok(());
        }
    }
    // Same discipline as `ingest_source`: an empty configured password
    // refuses every update (fail safe); the username is matched only when the
    // encoder supplied one (SHOUTcast v1 sends only a password).
    let (want_user, want_pass) = {
        let cfg = shared.config.read();
        (
            cfg.radio_source_user.clone(),
            cfg.radio_source_password.clone(),
        )
    };
    let user_ok = update.user.as_deref().is_none_or(|u| u == want_user);
    let pass_ok = update.pass.as_deref() == Some(want_pass.as_str());
    if want_pass.is_empty() || !pass_ok || !user_ok {
        if let Some(ip) = peer_ip {
            let _ = shared.rate_allow(Scope::Ip(ip), rl::AUTH);
        }
        wr.write_all(metadata_update_unauthorized().as_bytes())
            .await?;
        return Ok(());
    }

    // Resolve the mount: explicit (`mount=`), else the sole live mount (the
    // SHOUTcast admin.cgi form, where the port implies the station).
    let slug = match update.mount.as_deref() {
        Some(m) => Some(slug_of(m).to_string()),
        None => shared.radio.sole_mount(),
    };
    let Some(slug) = slug.filter(|s| !s.is_empty()) else {
        wr.write_all(metadata_update_failed("no mount").as_bytes())
            .await?;
        return Ok(());
    };

    let (artist, title) = split_song(&update.song);
    let Some(np) = shared.radio.update_live_metadata(&slug, &title, &artist) else {
        // Policy failure (no live source on that mount): Icecast keeps the
        // 200 status and reports it in the XML body.
        wr.write_all(metadata_update_failed("source not connected").as_bytes())
            .await?;
        return Ok(());
    };

    // Republish presence directly from the mount's now-playing so pure-DJ
    // mounts (no library program) update too.
    let listeners = shared.radio.registry.listener_count(&slug).unwrap_or(0);
    publish_status(
        shared,
        RadioStatus {
            station: slug.clone(),
            title: np.title,
            artist: np.artist,
            dj: np.dj,
            listeners,
            live: true,
        },
    );
    tracing::info!(mount = %slug, song = %update.song, "radio metadata updated");
    wr.write_all(metadata_update_ok().as_bytes()).await?;
    Ok(())
}

/// Authenticate a DJ source against the configured credentials, take over the
/// matching station (pausing playlist rotation), and feed its body to the mount
/// fan-out until it disconnects (then resume rotation).
async fn ingest_source<R, W>(
    head: &[u8],
    body: Vec<u8>,
    rd: &mut R,
    wr: &mut W,
    shared: &Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let req = match parse_source_request(head) {
        Ok(r) => r,
        Err(_) => {
            wr.write_all(source_forbidden().as_bytes()).await?;
            return Ok(());
        }
    };
    let slug = slug_of(&req.mount).to_string();
    if slug.is_empty() {
        wr.write_all(source_forbidden().as_bytes()).await?;
        return Ok(());
    }

    // Failed source logins drain the per-IP auth budget; an empty bucket
    // refuses the attempt before it is tried.
    if let Some(ip) = peer_ip {
        if !shared.rate_probe(Scope::Ip(ip), rl::AUTH) {
            wr.write_all(source_unauthorized().as_bytes()).await?;
            return Ok(());
        }
    }
    // Authenticate against the admin-configured source credentials. An empty
    // configured password refuses every source (fail safe); the username is
    // matched only when the DJ supplied one (SHOUTcast v1 has none).
    let (want_user, want_pass) = {
        let cfg = shared.config.read();
        (
            cfg.radio_source_user.clone(),
            cfg.radio_source_password.clone(),
        )
    };
    let user_ok = req.user.is_empty() || req.user == want_user;
    if want_pass.is_empty() || req.pass != want_pass || !user_ok {
        if let Some(ip) = peer_ip {
            let _ = shared.rate_allow(Scope::Ip(ip), rl::AUTH);
        }
        wr.write_all(source_unauthorized().as_bytes()).await?;
        return Ok(());
    }

    // Claim the mount byte fan-out (reject if a source already holds it), so
    // listeners on the delivery surface hear this DJ. The lock is dropped
    // before any `.await`.
    let dj_name = if want_user.is_empty() {
        AUTOMATION_DJ.to_string()
    } else {
        want_user.clone()
    };
    let np = now_playing_from_ice(&req.metadata, &dj_name);
    let claim = {
        let mut mounts = shared.radio.mounts.lock();
        // A live source holding the mount is refused; a rotation's mount gives
        // way. Replacing the entry closes the rotation's listeners, who
        // reconnect to the DJ (whose stream may well be another codec).
        if mounts.get(&slug).is_some_and(|m| !m.program_owned) {
            None
        } else {
            let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
            mounts.insert(
                slug.clone(),
                MountEntry {
                    tx: tx.clone(),
                    meta: req.metadata.clone(),
                    content_type: req.content_type.clone(),
                    now_playing: Arc::new(Mutex::new(Some(np.clone()))),
                    program_owned: false,
                    lead: Arc::from(&[][..]),
                },
            );
            Some(tx)
        }
    };
    let Some(tx) = claim else {
        wr.write_all(source_forbidden().as_bytes()).await?;
        return Ok(());
    };

    // Ensure the station exists in the directory (a pure-DJ mount has no
    // library program), take it live, and surface the now-playing.
    let _ = shared.radio.registry.create(StationConfig {
        slug: slug.clone(),
        display_name: req.metadata.name.clone(),
        description: req.metadata.genre.clone(),
        enabled: true,
    });
    let _ = shared.radio.registry.set_enabled(&slug, true);
    shared.radio.go_live(&slug, np);
    publish_now_playing(shared, &slug, true);
    shared.stats.incr("radio", "sources_connected");

    wr.write_all(source_ok(req.method).as_bytes()).await?;
    tracing::info!(mount = %slug, dj = %dj_name, "DJ live source connected");

    // Fan the body out verbatim and count bytes until the DJ disconnects.
    if !body.is_empty() {
        shared.radio.add_source_bytes(&slug, body.len() as u64);
        let _ = tx.send(Arc::from(body.into_boxed_slice()));
    }
    let mut chunk = vec![0u8; SOURCE_CHUNK];
    loop {
        let n = match rd.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        shared.radio.add_source_bytes(&slug, n as u64);
        let _ = tx.send(Arc::from(&chunk[..n]));
    }

    // DJ gone: drop the mount and resume the playlist (or take the station off
    // the air if it was a pure-DJ mount with no library rotation to fall back
    // to).
    shared.radio.mounts.lock().remove(&slug);
    shared.radio.end_live(&slug, |t| is_held(shared, t));
    shared.radio.take_air_back(&slug, |t| is_held(shared, t));
    // A library format may have arrived while this source was connected.
    if shared.radio.now_playing(&slug).is_some() {
        publish_now_playing(shared, &slug, false);
    } else {
        shared.radio.note_off_air(&slug);
        shared.presence.clear_radio_now_playing(&slug);
        // Off the air: a typed RADIO `RadioOff` push (projected in
        // `session::push_for_event`), like the now-playing `RadioNowPlaying`.
        shared.bus.publish(ServerEvent::RadioOff {
            station: slug.clone(),
        });
        let _ = shared.radio.registry.set_enabled(&slug, false);
    }
    tracing::info!(mount = %slug, "DJ live source ended");
    Ok(())
}

/// Spawn the playlist rotation driver: on a 1 s cadence it advances any
/// non-live program whose current track has finished and republishes the new
/// now-playing. Stops on [`ServerEvent::Shutdown`].
pub fn spawn_playlist_driver(shared: Arc<Shared>) -> JoinHandle<()> {
    tokio::spawn(playlist_driver(shared))
}

async fn playlist_driver(shared: Arc<Shared>) {
    let mut rx = shared.bus.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // The same clock the pump advances by and a rotation starts
                // on: a station handed from its pump to this timer when the
                // stream listener stops must still come to the end of a song.
                let held = |t: &Track| is_held(&shared, t);
                for slug in shared.radio.advance_finished(unix_ms(), held) {
                    publish_now_playing(&shared, &slug, false);
                }
            }
            ev = rx.recv() => {
                if matches!(
                    ev,
                    Ok(ServerEvent::Shutdown) | Err(broadcast::error::RecvError::Closed)
                ) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_node(id: i64, name: &str, mime: &str, blob: Option<[u8; 32]>) -> FileNodeRow {
        FileNodeRow {
            id,
            area_id: 1,
            area: "music".into(),
            parent_id: None,
            kind: KIND_FILE,
            name: name.into(),
            path: name.into(),
            is_dropbox: false,
            blob_id: blob,
            size: 0,
            mime: mime.into(),
            icon: String::new(),
            comment: "The Lagomorphs".into(),
            uploader: "dj".into(),
            uploader_id: Some(1),
            downloads: 0,
            target_id: None,
            created_at: 0,
            rating_avg: 0.0,
            rating_count: 0,
        }
    }

    /// `count` structurally valid MP3 frames (MPEG-1 Layer III, 128 kbit/s,
    /// 44.1 kHz: 417 bytes and 26.122 ms each) with silent bodies.
    fn mp3_frames(count: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(count * 417);
        for _ in 0..count {
            out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            out.extend(std::iter::repeat_n(0u8, 413));
        }
        out
    }

    /// One Ogg page carrying `payload`, built the way the codec's own
    /// tests do (the checksum is not read by anything here).
    fn ogg_page(granule: Option<u64>, payload: &[u8], beginning: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(u8::from(beginning) << 1);
        out.extend_from_slice(&granule.unwrap_or(u64::MAX).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut table = Vec::new();
        let mut left = payload.len();
        while left >= 255 {
            table.push(255u8);
            left -= 255;
        }
        table.push(left as u8);
        out.push(table.len() as u8);
        out.extend_from_slice(&table);
        out.extend_from_slice(payload);
        out
    }

    /// `seconds` of Opus: the two header pages, then a page every tenth.
    fn opus_track(seconds: u64) -> Vec<u8> {
        let mut out = ogg_page(
            Some(0),
            b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00",
            true,
        );
        out.extend(ogg_page(Some(0), b"OpusTags\x00\x00\x00\x00", false));
        for tenth in 1..=seconds * 10 {
            out.extend(ogg_page(Some(tenth * 4_800), &[7u8; 120], false));
        }
        out
    }

    /// The very file `rabbithole-radio`'s own test measures itself
    /// against: written by the reference encoder, not by us.
    const REFERENCE_FLAC: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");
    /// What that file is: 8 kHz, one channel, 16 bits a sample.
    const REFERENCE_FORM: Form = Form {
        rate: 8_000,
        channels: 1,
        bits: 16,
    };

    #[test]
    fn a_flac_track_is_written_again_as_the_next_of_a_stream() {
        assert_eq!(Sound::of(REFERENCE_FLAC), Some(Sound::Flac(REFERENCE_FORM)));
        let cut = Sound::Flac(REFERENCE_FORM).cut(REFERENCE_FLAC.to_vec(), 0);
        // What goes out is the frames, written again — not the file. The
        // file's own headers stay behind: the mount said what it is once.
        assert_eq!(
            cut.sends.iter().map(|b| b.bytes.len()).sum::<usize>(),
            cut.audio.len(),
            "every byte of it goes out"
        );
        assert_eq!(cut.sends[0].bytes.start, 0);
        assert_ne!(&cut.audio[..4], b"fLaC", "no second set of headers");
        assert_eq!(cut.samples, 960, "what it adds to what the mount has sent");
        assert_eq!(
            cut.sends.iter().map(|b| b.micros).sum::<u64>(),
            120_000,
            "0.12 s of tone, paced by the samples each frame holds"
        );
        // Whole frames only: no send ends inside one. The frames are the
        // ones the walker finds when the mount's own head is put in front
        // of what goes out — which is what a listener is given.
        let mut whole = rabbithole_radio::flac::stream_headers(8_000, 1, 16);
        let head = whole.len();
        whole.extend_from_slice(&cut.audio);
        let starts: Vec<usize> = rabbithole_radio::flac::frames(&whole)
            .iter()
            .map(|f| f.offset - head)
            .collect();
        assert_eq!(starts.len(), 4, "the file's four frames, written again");
        for batch in &cut.sends {
            assert!(
                batch.bytes.end == cut.audio.len() || starts.contains(&batch.bytes.end),
                "a send ends where a frame does: {:?}",
                batch.bytes
            );
        }
    }

    #[test]
    fn a_file_with_headers_and_no_audio_is_not_a_track_to_play() {
        // A FLAC cut off after its metadata still says it is a FLAC, and
        // its headers are real. Sending them takes no time, so a station
        // that counted that as playing would race through its whole
        // rotation in an instant and never say a word about why.
        let info = rabbithole_radio::flac::streaminfo(REFERENCE_FLAC).unwrap();
        let headers = &REFERENCE_FLAC[..info.audio_at];
        assert_eq!(
            Sound::of(headers),
            None,
            "headers with no frame behind them are not a track"
        );
        // And the guard behind that one: a file whose first frame is there
        // but does not finish. It still says FLAC, and there is nothing to
        // send, so the station says so rather than playing it in no time.
        let torn = &REFERENCE_FLAC[..info.audio_at + 10];
        assert_eq!(Sound::of(torn), Some(Sound::Flac(REFERENCE_FORM)));
        assert!(
            Sound::Flac(REFERENCE_FORM)
                .cut(torn.to_vec(), 0)
                .sends
                .is_empty(),
            "nothing to send and no time to send it in"
        );
    }

    #[test]
    fn a_library_of_both_kinds_becomes_a_mount_of_each() {
        let nodes = vec![
            file_node(1, "one.mp3", "audio/mpeg", Some([1u8; 32])),
            file_node(2, "two.opus", "audio/ogg", Some([2u8; 32])),
            file_node(3, "three.ogg", "application/octet-stream", Some([3u8; 32])),
            file_node(4, "four.flac", "audio/flac", Some([4u8; 32])),
            file_node(6, "five.aac", "audio/aac", Some([6u8; 32])),
            file_node(5, "notes.txt", "text/plain", Some([5u8; 32])),
        ];
        let split = split_by_sound(&nodes);
        assert_eq!(split.mpeg.len(), 1, "the MP3");
        assert_eq!(
            split.ogg.len(),
            2,
            "both Ogg files, by name as well as type"
        );
        assert_eq!(split.flac.len(), 1, "the FLAC");
        assert_eq!(
            split.other.len(),
            1,
            "the AAC, which it cannot send as it is"
        );
        // Not audio at all never becomes a track in the first place.
        assert_eq!(split.mpeg.len() + split.ogg.len() + split.flac.len(), 4);

        // What the name says, without reading the file.
        assert_eq!(sound_of_name("a.mp3", ""), Some(Sound::Mpeg));
        assert_eq!(sound_of_name("a", "audio/mpeg"), Some(Sound::Mpeg));
        assert_eq!(sound_of_name("a.opus", ""), Some(Sound::Ogg(0)));
        assert_eq!(sound_of_name("a.OGG", ""), Some(Sound::Ogg(0)));
        assert_eq!(
            sound_of_name("a.flac", ""),
            Some(Sound::Flac(Form::default()))
        );
        assert_eq!(
            sound_of_name("a", "audio/flac"),
            Some(Sound::Flac(Form::default()))
        );
        assert_eq!(
            sound_of_name("a.aac", "audio/aac"),
            None,
            "not one it can send"
        );
    }

    #[test]
    fn an_ogg_track_is_paced_by_what_its_pages_say_and_headers_cost_no_time() {
        let track = opus_track(1);
        assert_eq!(
            Sound::of(&track),
            Some(Sound::Ogg(48_000)),
            "an Opus stream is Ogg at 48 kHz"
        );
        let cut = Sound::Ogg(48_000).cut(track.clone(), 0).sends;
        assert_eq!(
            cut.iter().map(|b| b.micros).sum::<u64>(),
            1_000_000,
            "a second of pages is a second of sound"
        );
        assert_eq!(
            cut.iter().map(|b| b.bytes.len()).sum::<usize>(),
            track.len(),
            "every byte goes out, whole pages only"
        );
        assert!(
            (4..=5).contains(&cut.len()),
            "about a quarter second each: {}",
            cut.len()
        );
        assert_eq!(cut[0].bytes.start, 0, "the headers lead");

        // A station sends one kind of sound: MPEG and Ogg are not
        // interchangeable mid-stream.
        assert!(Sound::Mpeg.same_as(Sound::Mpeg));
        assert!(Sound::Ogg(48_000).same_as(Sound::Ogg(44_100)));
        assert!(!Sound::Mpeg.same_as(Sound::Ogg(48_000)));
        assert_eq!(Sound::Mpeg.content_type(), "audio/mpeg");
        assert_eq!(Sound::Ogg(48_000).content_type(), "audio/ogg");
        assert_eq!(Sound::Flac(REFERENCE_FORM).content_type(), "audio/flac");
        assert!(Sound::Flac(REFERENCE_FORM).same_as(Sound::Flac(REFERENCE_FORM)));
        assert!(!Sound::Flac(REFERENCE_FORM).same_as(Sound::Mpeg));
        assert!(!Sound::Flac(REFERENCE_FORM).same_as(Sound::Ogg(0)));
        // A mount sends one form of FLAC. A track of another is not the
        // same sound, whatever its name says, and a form nobody has looked
        // up yet — all a file name can say — is any of them.
        let other = Form {
            rate: 44_100,
            channels: 2,
            bits: 24,
        };
        assert!(!Sound::Flac(REFERENCE_FORM).same_as(Sound::Flac(other)));
        assert!(Sound::Flac(Form::default()).same_as(Sound::Flac(other)));
        assert_eq!(
            Sound::Flac(other).say(),
            "audio/flac, 44100 Hz, stereo, 24 bit"
        );
        assert_eq!(
            Sound::Flac(REFERENCE_FORM).say(),
            "audio/flac, 8000 Hz, mono, 16 bit"
        );
        assert_eq!(Sound::Mpeg.say(), "audio/mpeg");
        // And a file that is neither is not sent at all.
        assert_eq!(Sound::of(b"not audio at all"), None);
        assert_eq!(Sound::of(&mp3_frames(3)), Some(Sound::Mpeg));
    }

    #[test]
    fn a_track_is_cut_into_whole_contiguous_quarter_second_sends() {
        // 43 frames is a little over a second: four full sends and a tail.
        let track = mp3_frames(43);
        let cut = batches(&track);
        assert_eq!(cut.len(), 4 + 1);
        // Ten frames reach 250 ms (10 x 26.122 = 261 ms); nothing is split.
        assert_eq!(cut[0].bytes, 0..4170);
        assert_eq!(cut[0].micros, 261_220);
        assert!(
            cut.iter().all(|b| b.bytes.len() % 417 == 0),
            "whole frames only"
        );
        assert_eq!(
            cut.iter().map(|b| b.bytes.len()).sum::<usize>(),
            track.len()
        );
        assert_eq!(
            cut.last().unwrap().bytes.len(),
            3 * 417,
            "the tail is what is left"
        );
        assert_eq!(cut.last().unwrap().bytes.end, track.len());
        // Junk between frames is not audio: the run ends before it and a new
        // one starts after it, and the junk itself is never sent.
        let mut dirty = mp3_frames(3);
        dirty.extend_from_slice(b"\xFF\xFFgarbage, twenty-four bytes");
        dirty.extend(mp3_frames(3));
        let cut = batches(&dirty);
        assert_eq!(cut.len(), 2);
        assert_eq!(cut[0].bytes, 0..1251);
        assert_eq!(cut[1].bytes.len(), 1251);
        assert!(cut[1].bytes.start > 1251);
        assert!(batches(b"not audio at all").is_empty());
    }

    #[test]
    fn a_rotations_mount_gives_way_to_a_dj_and_not_to_a_second_one() {
        let radio = Stations::new();
        let track = Track::new(TrackId(1), "One.mp3", "", DEFAULT_TRACK_MS, BlobId([1; 32]));
        radio.install_program("ambient", "Ambient", "slow", vec![track], Some(Sound::Mpeg));
        assert_eq!(radio.track_count("ambient"), 1);
        assert!(!radio.is_streaming("ambient"), "no pump, no audio");
        let (tx, _) = radio
            .program_mount("ambient", Sound::Mpeg)
            .expect("the air is free");
        assert!(radio.is_streaming("ambient"));
        assert!(radio.owns_air("ambient", &tx));
        assert!(!radio.dj_holds("ambient"));
        // Asking again is the same mount, not a second one.
        let (again, _) = radio.program_mount("ambient", Sound::Mpeg).unwrap();
        assert!(again.same_channel(&tx));

        // A DJ replaces it (what both source surfaces do under the lock).
        let (dj_tx, _) = broadcast::channel(8);
        radio.mounts.lock().insert(
            "ambient".into(),
            MountEntry {
                tx: dj_tx,
                meta: StationMeta::default(),
                content_type: "audio/ogg".into(),
                now_playing: Arc::new(Mutex::new(None)),
                program_owned: false,
                lead: Arc::from(&[][..]),
            },
        );
        assert!(radio.dj_holds("ambient"));
        assert!(!radio.owns_air("ambient", &tx), "the pump must notice");
        assert!(
            radio.program_mount("ambient", Sound::Mpeg).is_none(),
            "and must not barge back in"
        );

        // The DJ leaves; the rotation can have its air back.
        radio.mounts.lock().remove("ambient");
        let (fresh, _) = radio
            .program_mount("ambient", Sound::Mpeg)
            .expect("free again");
        assert!(
            !fresh.same_channel(&tx),
            "a fresh stream for fresh listeners"
        );

        // A pumped rotation is not also advanced by the timer.
        radio.set_pumped("ambient", true);
        assert!(radio.advance_finished(u64::MAX, |_| false).is_empty());
        radio.set_pumped("ambient", false);
        assert_eq!(radio.advance_finished(u64::MAX, |_| false), ["ambient"]);
    }

    #[test]
    fn a_track_left_out_is_news_once_per_reason() {
        let radio = Stations::new();
        assert!(radio.left_out("jukebox", "memo.m4a", "not audio".into(), 1));
        assert!(
            !radio.left_out("jukebox", "memo.m4a", "not audio".into(), 2),
            "passing it again is not news"
        );
        assert!(radio.left_out("jukebox", "memo.m4a", "could not be read".into(), 3));
        assert_eq!(radio.left_out_for("jukebox").len(), 1, "one line a track");
    }

    #[test]
    fn a_rotation_the_timer_drives_comes_to_the_end_of_a_song() {
        // The pump and the timer advance a station by the same clock, so a
        // station handed from one to the other still moves on.
        let radio = Stations::new();
        let track = |n: u64| {
            Track::new(
                TrackId(n),
                format!("t{n}.mp3"),
                "",
                1_000,
                BlobId([n as u8; 32]),
            )
        };
        radio.install_program(
            "ambient",
            "Ambient",
            "",
            vec![track(1), track(2)],
            Some(Sound::Mpeg),
        );
        radio.set_pumped("ambient", true);
        radio.advance("ambient", unix_ms());
        radio.set_pumped("ambient", false);
        assert!(
            radio.advance_finished(unix_ms(), |_| false).is_empty(),
            "not yet"
        );
        assert_eq!(
            radio.advance_finished(unix_ms() + 2_000, |_| false),
            ["ambient"]
        );
    }

    #[test]
    fn a_talked_over_track_is_moved_past_as_the_dj_leaves() {
        let radio = Stations::new();
        let track = |n: u64| {
            Track::new(
                TrackId(n),
                format!("t{n}.mp3"),
                "",
                1_000,
                BlobId([n as u8; 32]),
            )
        };
        radio.install_program(
            "ambient",
            "Ambient",
            "",
            vec![track(1), track(2), track(3)],
            Some(Sound::Mpeg),
        );
        radio.request("ambient", 3, "alice", |_| false).unwrap();
        // A DJ takes the air in the middle of t1.
        radio.go_live(
            "ambient",
            NowPlaying {
                title: "Live Set".into(),
                artist: String::new(),
                dj: "source".into(),
            },
        );
        radio.owe_advance("ambient");
        let during = radio.requests("ambient", "alice", |_| false).unwrap();
        assert!(during.dj_live);
        assert_eq!(
            during.queue.len(),
            1,
            "nothing is spent while the DJ has it"
        );
        assert!(
            radio.request("ambient", 1, "bob", |_| false).is_ok(),
            "talked over, t1 is not playing now"
        );
        // They leave: what is announced is what plays, the most wanted first.
        radio.end_live("ambient", |_| false);
        assert_eq!(radio.now_playing("ambient").unwrap().title, "t3.mp3");
        assert!(
            !radio.take_air_back("ambient", |_| false),
            "moved on once, not twice"
        );
    }

    #[test]
    fn a_station_follows_its_folder_without_cutting_the_song() {
        let radio = Stations::new();
        let track = |n: u64| {
            Track::new(
                TrackId(n),
                format!("t{n}.mp3"),
                "",
                1_000,
                BlobId([n as u8; 32]),
            )
        };
        radio.install_program(
            "ambient",
            "Ambient",
            "",
            vec![track(1), track(2), track(3)],
            Some(Sound::Mpeg),
        );
        radio.request("ambient", 3, "alice", |_| false).unwrap();
        assert_eq!(
            radio.refresh_program(
                "ambient",
                "Ambient",
                "",
                vec![track(1), track(2), track(3)],
                Some(Sound::Mpeg),
                []
            ),
            Refreshed::Unchanged,
            "the same folder is no change"
        );
        // t3 is taken out of the folder and t4 is added.
        assert_eq!(
            radio.refresh_program(
                "ambient",
                "Ambient",
                "",
                vec![track(1), track(2), track(4)],
                Some(Sound::Mpeg),
                []
            ),
            Refreshed::Changed {
                dropped: 1,
                started: false
            },
            "the request for the file that went is dropped"
        );
        assert_eq!(radio.track_count("ambient"), 3);
        assert_eq!(
            radio.now_playing("ambient").unwrap().title,
            "t1.mp3",
            "t1 plays on"
        );
        let waiting = radio.requests("ambient", "alice", |_| false).unwrap();
        assert!(waiting.queue.is_empty());
        radio.request("ambient", 4, "alice", |_| false).unwrap();

        // A station that had nothing to play starts when it has.
        radio.install_program("later", "Later", "", Vec::new(), None);
        assert!(radio.now_playing("later").is_none());
        assert_eq!(
            radio.refresh_program("later", "Later", "", vec![track(9)], Some(Sound::Mpeg), []),
            Refreshed::Changed {
                dropped: 0,
                started: true
            }
        );
        assert_eq!(radio.now_playing("later").unwrap().title, "t9.mp3");
        // And a mount the folder now asks for, which was not there, is put up.
        assert_eq!(
            radio.refresh_program("later.ogg", "Later (OGG)", "", vec![track(8)], None, []),
            Refreshed::Installed
        );
        assert_eq!(radio.track_count("later.ogg"), 1);
    }

    #[test]
    fn a_live_dj_keeps_new_or_previously_empty_automation_until_departure() {
        for existing in [false, true] {
            let radio = Stations::new();
            if existing {
                radio.install_program("later", "Later", "music", Vec::new(), None);
            }
            let live = NowPlaying {
                title: "Live set".into(),
                artist: "DJ's choice".into(),
                dj: "alice".into(),
            };
            let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
            radio.mounts.lock().insert(
                "later".into(),
                MountEntry {
                    tx: tx.clone(),
                    meta: StationMeta::default(),
                    content_type: "audio/ogg".into(),
                    now_playing: Arc::new(Mutex::new(Some(live.clone()))),
                    program_owned: false,
                    lead: Arc::from(&[][..]),
                },
            );
            radio.refresh_program(
                "later",
                "Later",
                "music",
                vec![Track::new(TrackId(1), "one.mp3", "", 1_000, BlobId::ZERO)],
                Some(Sound::Mpeg),
                [],
            );
            assert!(radio.is_live("later"));
            assert_eq!(radio.now_playing("later"), Some(live));
            let mount = radio.mounts.lock();
            assert!(mount["later"].tx.same_channel(&tx));
            assert_eq!(mount["later"].content_type, "audio/ogg");
            drop(mount);
            radio.mounts.lock().remove("later");
            radio.end_live("later", |_| false);
            assert!(!radio.is_live("later"));
            assert_eq!(radio.now_playing("later").unwrap().title, "one.mp3");
        }
    }

    #[test]
    fn a_dj_leaving_does_not_announce_what_is_held() {
        let radio = Stations::new();
        let track = |n: u64| {
            Track::new(
                TrackId(n),
                format!("t{n}.mp3"),
                "",
                1_000,
                BlobId([n as u8; 32]),
            )
        };
        radio.install_program(
            "ambient",
            "Ambient",
            "",
            vec![track(1), track(2), track(3), track(4)],
            Some(Sound::Mpeg),
        );
        // t3 is the most wanted, then its file is held back; t4 is wanted too.
        radio.request("ambient", 3, "alice", |_| false).unwrap();
        radio.vote_request("ambient", 3, "bob", |_| false).unwrap();
        radio.request("ambient", 4, "carol", |_| false).unwrap();
        radio.go_live(
            "ambient",
            NowPlaying {
                title: "Live Set".into(),
                artist: String::new(),
                dj: "source".into(),
            },
        );
        radio.owe_advance("ambient");
        radio.end_live("ambient", |t| t.id == TrackId(3));
        assert_eq!(radio.now_playing("ambient").unwrap().title, "t4.mp3");
    }

    #[test]
    fn a_track_that_plays_is_offered_whatever_was_guessed_about_it() {
        let radio = Stations::new();
        let track =
            |n: u64, name: &str| Track::new(TrackId(n), name, "", 1_000, BlobId([n as u8; 32]));
        radio.install_program(
            "ambient",
            "Ambient",
            "",
            vec![
                track(1, "a.mp3"),
                track(2, "odd-name"),
                track(3, "Intro.mp3"),
                track(4, "Intro.mp3"),
            ],
            Some(Sound::Mpeg),
        );
        let offered = |radio: &Stations| -> Vec<u64> {
            radio
                .offer("ambient", "", |_| false)
                .unwrap()
                .tracks
                .iter()
                .map(|t| t.id)
                .collect()
        };
        // Guessed at install not to be sendable; it turns out to play.
        radio.set_unsendable("ambient", [TrackId(2)]);
        assert!(!offered(&radio).contains(&2));
        radio.can_play("ambient", TrackId(2), "odd-name");
        assert!(offered(&radio).contains(&2));

        // Two files of one name, one of them bad: the good one playing
        // does not take the bad one off the operator's list.
        radio.cannot_play("ambient", TrackId(3));
        radio.left_out("ambient", "Intro.mp3", "could not be read".into(), 1);
        radio.can_play("ambient", TrackId(4), "Intro.mp3");
        assert_eq!(radio.left_out_for("ambient").len(), 1, "still out");
        assert!(!offered(&radio).contains(&3));
        radio.can_play("ambient", TrackId(3), "Intro.mp3");
        assert!(radio.left_out_for("ambient").is_empty(), "and now fixed");
    }

    #[test]
    fn a_timer_driven_station_passes_over_what_is_held() {
        let radio = Stations::new();
        let track = |n: u64| {
            Track::new(
                TrackId(n),
                format!("t{n}.mp3"),
                "",
                1_000,
                BlobId([n as u8; 32]),
            )
        };
        radio.install_program(
            "ambient",
            "Ambient",
            "",
            vec![track(1), track(2), track(3)],
            Some(Sound::Mpeg),
        );
        let held = |t: &Track| t.id == TrackId(2);
        assert_eq!(radio.advance_finished(unix_ms() + 2_000, held), ["ambient"]);
        assert_eq!(radio.now_playing("ambient").unwrap().title, "t3.mp3");
        // A station held back entirely still lands somewhere.
        assert_eq!(
            radio.advance_finished(unix_ms() + 4_000, |_| true),
            ["ambient"]
        );
    }

    #[test]
    fn a_mount_with_no_rotation_takes_no_requests_rather_than_refusing() {
        let radio = Stations::new();
        let _ = radio.registry.create(StationConfig {
            slug: "live".into(),
            display_name: "Live".into(),
            description: String::new(),
            enabled: true,
        });
        let view = radio
            .requests("live", "alice", |_| false)
            .expect("a station");
        assert!(!view.requestable && view.queue.is_empty());
        assert!(radio.requests("nowhere", "alice", |_| false).is_none());
        assert!(radio.offer("live", "", |_| false).is_none());
    }

    #[test]
    fn history_moves_on_a_change_of_track_and_never_on_a_repeat() {
        let radio = Stations::new();
        radio.note_now_playing("live", "One", "A", 1_000);
        // Now-playing is republished whenever the listener count moves. Those
        // repeats are not plays.
        radio.note_now_playing("live", "One", "A", 1_500);
        radio.note_now_playing("live", "One", "A", 1_900);
        assert!(
            radio.recent("live").is_empty(),
            "the current track is not history yet"
        );
        radio.note_now_playing("live", "Two", "B", 2_000);
        assert_eq!(
            radio.recent("live"),
            [Played {
                title: "One".into(),
                artist: "A".into(),
                started_unix_ms: 1_000
            }]
        );
        // Newest first, and only the last ten.
        for n in 3..=14u64 {
            radio.note_now_playing("live", &format!("T{n}"), "", n * 1_000);
        }
        let recent = radio.recent("live");
        assert_eq!(recent.len(), RECENT_TRACKS);
        assert_eq!(recent[0].title, "T13", "T14 is still playing");
        assert_eq!(recent[RECENT_TRACKS - 1].title, "T4");
        // Going off the air retires the current track and keeps the list, so
        // coming back on does not start from nothing.
        radio.note_off_air("live");
        assert_eq!(radio.recent("live")[0].title, "T14");
        // Stations do not share a history.
        assert!(radio.recent("ambient").is_empty());
    }

    #[test]
    fn a_track_finds_its_cover_the_way_music_folders_do() {
        let in_album = |id, name: &str, mime: &str, blob: u8| {
            let mut n = file_node(id, name, mime, Some([blob; 32]));
            n.parent_id = Some(10);
            n
        };
        let nodes = vec![
            in_album(1, "Down the Hole.mp3", "audio/mpeg", 1),
            in_album(2, "down the hole.JPG", "image/jpeg", 2),
            in_album(3, "Carrot Cake.mp3", "audio/mpeg", 3),
            in_album(4, "Cover.png", "image/png", 4),
            // A loose single in the area root: no image beside it.
            file_node(5, "Loose.mp3", "audio/mpeg", Some([5; 32])),
            // An image in another folder is nobody's cover here.
            file_node(6, "folder.jpg", "image/jpeg", Some([6; 32])),
            file_node(7, "Warren.ogg", "audio/ogg", Some([7; 32])),
        ];
        let covers = covers_from_nodes(&nodes);
        assert_eq!(
            covers.get("Down the Hole.mp3"),
            Some(&[2; 32]),
            "its own image wins"
        );
        assert_eq!(
            covers.get("Carrot Cake.mp3"),
            Some(&[4; 32]),
            "else the folder's cover"
        );
        assert_eq!(
            covers.get("Loose.mp3"),
            Some(&[6; 32]),
            "the root's folder.jpg"
        );
        assert_eq!(covers.get("Warren.ogg"), Some(&[6; 32]));
        assert_eq!(
            covers.len(),
            4,
            "images and folders get no entry of their own"
        );

        let radio = Stations::new();
        radio.set_covers("ambient", covers);
        assert_eq!(radio.cover_for("ambient", "Carrot Cake.mp3"), Some([4; 32]));
        assert_eq!(radio.cover_for("ambient", "Nothing"), None);
        assert_eq!(radio.cover_for("live", "Carrot Cake.mp3"), None);
    }

    #[test]
    fn is_audio_by_mime_and_extension() {
        assert!(is_audio("track", "audio/mpeg"));
        assert!(is_audio("Song.MP3", "application/octet-stream"));
        assert!(is_audio("clip.flac", ""));
        assert!(!is_audio("readme.txt", "text/plain"));
        assert!(!is_audio("cover.png", "image/png"));
    }

    #[test]
    fn tracks_map_preserves_order_and_drops_non_audio() {
        let nodes = vec![
            file_node(10, "a.mp3", "audio/mpeg", Some([1u8; 32])),
            file_node(11, "notes.txt", "text/plain", Some([2u8; 32])),
            file_node(12, "b.ogg", "application/octet-stream", Some([3u8; 32])),
            // audio by name but no blob → not playable, dropped.
            file_node(13, "c.wav", "audio/wav", None),
        ];
        let tracks = tracks_from_nodes(&nodes);
        let ids: Vec<u64> = tracks.iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec![10, 12]);
        assert_eq!(tracks[0].title, "a.mp3");
        assert_eq!(tracks[0].artist, "The Lagomorphs");
        assert_eq!(tracks[0].source, BlobId([1u8; 32]));
    }

    #[test]
    fn a_mounts_count_starts_again_between_songs_and_never_mid_song() {
        let limit = rabbithole_radio::flac::NUMBER_LIMIT;
        assert_eq!(next_number(0), 0);
        assert_eq!(next_number(44_100), 44_100, "a minute in, it keeps going");
        assert_eq!(next_number(limit / 2 - 1), limit / 2 - 1);
        // Far enough that the next song might run past what a number can
        // hold: it starts again here, between songs, rather than inside one.
        assert_eq!(next_number(limit / 2), 0);
        assert_eq!(next_number(limit), 0);
        // And what is left is a number the coded shape can hold in six
        // bytes, with room for a song of any length anybody has.
        assert!(limit / 2 + 44_100 * 60 * 60 * 6 < limit);
        // A track longer than that wraps inside itself, and where it ends
        // is where the next one starts: never a number it just wrote.
        assert_eq!(next_number(limit + 5), 5);
        assert_eq!(next_number(limit), 0);
        assert_eq!(next_number(limit + limit / 2), 0);
    }

    #[test]
    fn a_rotation_of_names_says_which_kind_of_sound_it_is() {
        let t = |name: &str| Track::new(TrackId(1), name, "", DEFAULT_TRACK_MS, BlobId([1; 32]));
        assert_eq!(sound_of_tracks(&[]), None);
        // Nothing playable says nothing, rather than saying MP3 and putting
        // a mount up as something it will never send.
        assert_eq!(sound_of_tracks(&[t("notes.txt"), t("rip.wav")]), None);
        // Three FLACs and three files this burrow cannot send is a FLAC
        // station: only what can be played gets a vote.
        assert_eq!(
            sound_of_tracks(&[
                t("a.flac"),
                t("b.flac"),
                t("c.flac"),
                t("a.wav"),
                t("b.wav"),
                t("c.wav"),
            ]),
            Some(Sound::Flac(Form::default()))
        );
        // Two Oggs against one MP3 is an Ogg station, majority or not.
        assert_eq!(
            sound_of_tracks(&[t("a.opus"), t("b.oga"), t("c.mp3")]),
            Some(Sound::Ogg(0))
        );
        // A tie goes the way mounts are handed out: MP3, then Ogg.
        assert_eq!(
            sound_of_tracks(&[t("a.mp3"), t("b.flac")]),
            Some(Sound::Mpeg)
        );
        assert_eq!(
            sound_of_tracks(&[t("a.ogg"), t("b.flac")]),
            Some(Sound::Ogg(0))
        );
    }

    #[test]
    fn program_goes_live_and_resumes() {
        let stations = Stations::new();
        stations.install_program(
            "live",
            "Live FM",
            "test",
            vec![Track::new(
                TrackId(1),
                "auto track",
                "artist",
                DEFAULT_TRACK_MS,
                BlobId([1u8; 32]),
            )],
            None,
        );
        // Playlist automation is now-playing to start.
        assert!(!stations.is_live("live"));
        assert_eq!(stations.now_playing("live").unwrap().title, "auto track");

        // A DJ takes over: now-playing switches, rotation is paused.
        stations.go_live(
            "live",
            NowPlaying {
                title: "Live Set".into(),
                artist: "DJ Hop".into(),
                dj: "source".into(),
            },
        );
        assert!(stations.is_live("live"));
        assert_eq!(stations.now_playing("live").unwrap().title, "Live Set");
        // A live program never advances, even long past the track duration.
        assert!(stations
            .advance_finished(unix_ms() + DEFAULT_TRACK_MS * 10, |_| false)
            .is_empty());
        stations.add_source_bytes("live", 4096);
        assert_eq!(stations.source_bytes("live"), 4096);

        // DJ disconnects: rotation resumes, playlist now-playing returns.
        stations.end_live("live", |_| false);
        assert!(!stations.is_live("live"));
        assert_eq!(stations.now_playing("live").unwrap().title, "auto track");
    }
}
