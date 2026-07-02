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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use rabbithole_audio::NowPlaying;
use rabbithole_legacy_icecast::{
    build_listener_response, parse_listener_request, parse_source_request, source_forbidden,
    source_ok, source_unauthorized, IcyMetaInterleaver, StationMeta, DEFAULT_METAINT,
};
use rabbithole_radio::{StationConfig, StationRegistry};
use rabbithole_server_core::Caps;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::Shared;

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
    /// Monotonic listener-id source for registry accounting.
    next_listener: AtomicU64,
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
            next_listener: AtomicU64::new(1),
        }
    }

    fn next_listener_id(&self) -> u64 {
        self.next_listener.fetch_add(1, Ordering::Relaxed)
    }

    /// Subscribe a listener to a mount, if a source is currently live there.
    fn subscribe(&self, slug: &str) -> Option<MountHandle> {
        self.mounts.lock().get(slug).map(MountEntry::subscribe)
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
            let Ok((sock, _peer)) = listener.accept().await else {
                break;
            };
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = serve(sock, shared).await {
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
async fn serve(mut sock: tokio::net::TcpStream, shared: Arc<Shared>) -> Result<()> {
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
        serve_source(&head, body, &mut rd, &mut wr, &shared).await
    }
}

/// Serve a DJ source: authenticate, claim the mount, and fan the body out.
async fn serve_source<R, W>(
    head: &[u8],
    body: Vec<u8>,
    rd: &mut R,
    wr: &mut W,
    shared: &Arc<Shared>,
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

    // Authenticate the Basic credentials, then require the broadcast capability.
    let authed = match shared.auth.login_password(&req.user, &req.pass, None).await {
        Ok(u) => u,
        Err(_) => {
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
    let _ = shared.radio.registry.set_enabled(&slug, false);
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
