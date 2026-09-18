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

use std::collections::{HashMap, VecDeque};
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
}

/// A listener's view of a mount: an event receiver plus the head it needs to
/// render the ICY response, none of which keeps the source's sender alive.
struct MountHandle {
    rx: broadcast::Receiver<Arc<[u8]>>,
    meta: StationMeta,
    content_type: String,
    now_playing: Arc<Mutex<Option<NowPlaying>>>,
}

impl MountEntry {
    fn subscribe(&self) -> MountHandle {
        MountHandle {
            rx: self.tx.subscribe(),
            meta: self.meta.clone(),
            content_type: self.content_type.clone(),
            now_playing: self.now_playing.clone(),
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
struct Program {
    controller: StationController,
    /// `Some` while a DJ is live: their now-playing overrides the playlist's.
    live: Option<NowPlaying>,
    /// Bytes ingested from the current live DJ (observability + tests).
    source_bytes: u64,
}

impl Program {
    fn is_live(&self) -> bool {
        self.live.is_some()
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
    /// `tracks`. Registers it in the directory and starts playout at the first
    /// track (now-playing is populated immediately, deterministically).
    pub fn install_program(
        &self,
        slug: &str,
        display_name: &str,
        description: &str,
        tracks: Vec<Track>,
    ) {
        let station = Station::new(slug, STATION_CAPACITY);
        let playlist = Playlist::new(tracks, RotationMode::Sequential);
        let mut controller = StationController::new(station, playlist, description, AUTOMATION_DJ);
        // Start playout at the opening track so now-playing is live at once.
        controller.on_track_finished(0);
        let _ = self.registry.create(StationConfig {
            slug: slug.to_string(),
            display_name: display_name.to_string(),
            description: description.to_string(),
            enabled: true,
        });
        let _ = self.registry.set_enabled(slug, true);
        self.programs.lock().insert(
            slug.to_string(),
            Program {
                controller,
                live: None,
                source_bytes: 0,
            },
        );
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

    /// The DJ disconnected from `slug`: resume playlist rotation.
    pub fn end_live(&self, slug: &str) {
        if let Some(p) = self.programs.lock().get_mut(slug) {
            p.live = None;
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
    pub fn advance_finished(&self, now_ms: u64) -> Vec<String> {
        let mut advanced = Vec::new();
        let mut programs = self.programs.lock();
        for (slug, p) in programs.iter_mut() {
            if p.is_live() {
                continue;
            }
            if p.controller.is_finished(now_ms) {
                p.controller.on_track_finished(now_ms);
                advanced.push(slug.clone());
            }
        }
        advanced
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
        if mounts.contains_key(&slug) {
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
    // the station is off it.
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
fn publish_now_playing(shared: &Arc<Shared>, slug: &str, live: bool) {
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
        if mounts.contains_key(&slug) {
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
    let had_program = shared.radio.go_live(&slug, np);
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
    shared.radio.end_live(&slug);
    if had_program && shared.radio.now_playing(&slug).is_some() {
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
    let start = std::time::Instant::now();
    let mut rx = shared.bus.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now_ms = start.elapsed().as_millis() as u64;
                for slug in shared.radio.advance_finished(now_ms) {
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
        assert!(stations.advance_finished(DEFAULT_TRACK_MS * 10).is_empty());
        stations.add_source_bytes("live", 4096);
        assert_eq!(stations.source_bytes("live"), 4096);

        // DJ disconnects: rotation resumes, playlist now-playing returns.
        stations.end_live("live");
        assert!(!stations.is_live("live"));
        assert_eq!(stations.now_playing("live").unwrap().title, "auto track");
    }
}
