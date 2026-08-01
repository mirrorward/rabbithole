//! Server-to-server (S2S) federation transport (Wave 9): an authenticated,
//! admin-approved peering session between two burrows over QUIC.
//!
//! # What this slice does
//!
//! Two burrows establish a **mutually-authenticated peering session**:
//!
//! 1. The dialer opens a QUIC connection to the peer's `federation_addr`,
//!    pinning the peer's self-signed TLS certificate fingerprint.
//! 2. Both sides exchange the [`PeerHello`]/[`PeerHelloAck`] announcements from
//!    the [`rabbithole_federation`] crate (identity key, immutable federation
//!    origin, display name, protocol version, capabilities/software) plus a
//!    nonce each.
//! 3. Each side signs a session transcript
//!    (`context ‖ dialer_key ‖ listener_key ‖ dialer_origin ‖
//!    listener_origin ‖ dialer_nonce ‖ listener_nonce`)
//!    with its Ed25519 server key and the other verifies it — proving live
//!    possession of the announced identity and binding the proof to *this*
//!    connection (nonces defeat replay).
//! 4. A new peer origin-key tuple is **not** trusted automatically: an inbound
//!    handshake from an unknown tuple is recorded [`PeerState::Pending`]
//!    (see [`rabbithole_server_core::PeerRegistry`]) and refused. An admin
//!    approves it (audited, via the `ctl` peer commands); approved tuples
//!    persist to `<data_dir>/federation/approved_peers.json` and reload on boot.
//!    Only then does a subsequent handshake transition to
//!    [`PeerState::Connected`].
//!
//! # Endpoint / ALPN decision
//!
//! S2S runs on a **dedicated QUIC endpoint** bound to `federation_addr`,
//! separate from the client QUIC/WebSocket listeners, reusing the burrow's
//! existing TLS identity ([`QuicListener`]/[`QuicTransport`] from
//! [`rabbithole_net`], ALPN `rhp/1`). Isolation is by port + by carrying the
//! handshake in the dedicated [`Family::FEDERATION`] frame family, so peer
//! traffic never mixes with client sessions. This reuses the existing QUIC
//! transport rather than hand-rolling a socket.
//!
//! # Catalog sync over the session
//!
//! Once a session is live (both sides authenticated, dialer approved), signed
//! file-catalogs ride the same [`Family::FEDERATION`] frames:
//!
//! - the dialer announces its local catalog id/generation
//!   ([`MT_CATALOG_ANNOUNCE`]) and the listener answers with its own;
//! - if the listener's announced generation is fresher than what the dialer
//!   holds, the dialer requests a full fetch ([`MT_CATALOG_GET`]) and the
//!   listener replies with its `SignedCatalog` bytes ([`MT_CATALOG`]);
//! - the dialer verifies the catalog against the peer's **pinned key** (the
//!   Ed25519 key the handshake proved) and generation staleness before
//!   storing it (see [`crate::fed_catalog::ingest_peer_catalog`]).
//!
//! Sync is **dialer-pull**: the listener serves announces/fetches but pulls
//! the dialer's catalog only when it dials back itself (the background dialer
//! does this for configured peers). Building/serving the local catalog and
//! the per-peer verified store live in [`crate::fed_catalog`]; `fed-search`
//! in `ctl` runs the cross-server search over them. Client-facing RHP search
//! over federated catalogs is a follow-up.
//!
//! # Board-event flood-fill over the session
//!
//! Alongside catalogs, signed **board posts** gossip across the mesh on the
//! same [`Family::FEDERATION`] frames (`MT_SUBSCRIBE`..`MT_EVENTS` = 8..11).
//! It is subscription-driven: each side announces the board slugs it wants
//! ([`crate::Shared`]'s `federation_board_subscribe`, opt-in), and thereafter
//! whoever holds a matching event offers its ids ([`IHave`]), the subscriber
//! pulls what it lacks ([`PullRequest`]), and the holder delivers the raw
//! signed events ([`PushEvents`]/[`FedEvent`]) plus each event's origin server
//! key. A carried key can select an existing trusted origin binding but can
//! never establish one. On ingest the origin signature is verified (mirroring
//! [`crate::fed_catalog::ingest_peer_catalog`]), the id is deduped through the
//! shared [`rabbithole_server_core::DedupStore`], and the post is projected via
//! `BoardService` **unchanged** — never re-signed as local. A fresh ingest
//! re-fires `BoardPost`, which floods it to the next hop, so a post reaches
//! every subscribed burrow through the mesh. A per-edge Bloom seen-set makes an
//! event flood each edge once and never bounce back to its source (loop-safe).
//!
//! **Board follow-ups** (Edit/Tombstone) flood the same way: they are signed
//! events too, served from the `board_followups` table (see
//! [`crate::Shared`]'s `boards`), advertised/pulled by the same
//! `IHave`/`PullRequest`, and re-fired as `BoardEvent` (distinct from
//! `BoardPost` so they don't bump unread). Beyond the origin-signature check,
//! a follow-up runs an **authorization gate** in
//! [`rabbithole_server_core::boards::BoardService::ingest_event`] — apply only
//! if the same author edits their own post, or the post's home server
//! moderates its own content — so a peer can't forge an edit/retraction of
//! someone else's post. Out-of-order follow-ups (arriving before their target
//! post) park pending and reconcile when the post lands.
//! Post-welcome, subscriptions/offers are exchanged with **approved peers
//! only**, and every list on the wire is length-bounded.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use rabbithole_federation::{
    is_valid_server_name, BloomFilter, FedEvent, IHave, PeerHello, PeerHelloAck, PullRequest,
    PushEvents, Subscription,
};
use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use rabbithole_net::quic::{QuicListener, QuicTransport};
use rabbithole_net::tls::{CertFingerprint, ServerAuth, TlsIdentity};
use rabbithole_net::{Connection, Listener, Transport};
use rabbithole_proto::{Family, Frame, FrameKind, Payload, RequestId, PROTOCOL_VERSION};
use rabbithole_server_core::boards::IngestOutcome;
use rabbithole_server_core::events::{EventBody, SignedEvent};
use rabbithole_server_core::{BoardError, SeenKey, ServerEvent};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::Shared;

/// Domain separator for the S2S handshake proof signatures.
const AUTH_CONTEXT: &[u8] = b"rhp-fed-s2s-auth-v2";

/// Federation protocol version this build speaks.
const FED_PROTOCOL: u32 = 2;

/// Software id announced in the handshake.
const SOFTWARE: &str = concat!("rabbithole/", env!("CARGO_PKG_VERSION"));

/// Largest handshake frame payload we will decode (belt-and-suspenders).
const MAX_MSG: usize = 64 * 1024;

/// Largest catalog frame payload we will decode (a full listing is bigger
/// than any handshake message, but still bounded).
const MAX_CATALOG: usize = 4 * 1024 * 1024;
/// Hard cap on durable peer approvals loaded from disk.
const MAX_APPROVED_PEERS: usize = 4096;

/// Federation frame message types (within [`Family::FEDERATION`]).
const MT_HELLO: u16 = 1;
const MT_HELLO_ACK: u16 = 2;
const MT_PROOF: u16 = 3;
const MT_WELCOME: u16 = 4;
/// Catalog id/generation announcement (both directions, post-welcome).
const MT_CATALOG_ANNOUNCE: u16 = 5;
/// Full-catalog fetch request (dialer → listener, post-welcome).
const MT_CATALOG_GET: u16 = 6;
/// Full-catalog reply: `SignedCatalog` wire bytes.
const MT_CATALOG: u16 = 7;
// ---- board-event flood-fill (Wave 9), post-welcome, both directions --------
/// Subscription announce: the sender's board interest (`Vec<Subscription>`,
/// or a single `board_slug == "*"` wildcard). Recorded by the receiver, which
/// then offers matching events it holds.
const MT_SUBSCRIBE: u16 = 8;
/// Offer: `IHave` — event ids the sender holds for a board, gated to the
/// receiver's declared interest.
const MT_IHAVE: u16 = 9;
/// Request: `PullRequest` — specific event ids the requester is missing.
const MT_PULL: u16 = 10;
/// Delivery: `EventsMsg` (wraps the model's `PushEvents`) — the requested
/// signed board events plus a per-event origin server key so a receiver many
/// hops from the origin can still verify the origin signature.
const MT_EVENTS: u16 = 11;

// ---- flood-fill bounds (a peer must never be able to blow memory) ----------
/// Max board slugs accepted in one `MT_SUBSCRIBE`.
const MAX_SUBSCRIBE_BOARDS: usize = 256;
/// Max event ids accepted in one `MT_IHAVE` / offered in one catch-up.
const MAX_IHAVE_IDS: usize = 1024;
/// Max event ids accepted in one `MT_PULL`.
const MAX_PULL_IDS: usize = 1024;
/// Max events packed into one `MT_EVENTS` reply.
const MAX_EVENTS_PER_MSG: usize = 256;
/// Largest `MT_EVENTS` payload we will decode (many signed posts, still
/// bounded — larger than a catalog is unnecessary here).
const MAX_EVENTS_PAYLOAD: usize = 8 * 1024 * 1024;
/// Expected distinct event ids per edge for Bloom sizing (the seen-set is a
/// fixed-footprint filter, so this only tunes the false-positive rate; a false
/// positive at worst suppresses one redundant offer, never a needed ingest).
const EDGE_SEEN_CAPACITY: usize = 200_000;
/// Target false-positive rate for the per-edge seen-set.
const EDGE_SEEN_FP: f64 = 1e-6;

/// How often the background dialer re-checks configured peers.
const DIAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

// ---- wire messages -------------------------------------------------------

/// Dialer → listener: announcement + the dialer's session nonce.
#[derive(Debug, Serialize, Deserialize)]
struct HelloMsg {
    hello: PeerHello,
    nonce: [u8; 32],
}

/// Listener → dialer: its announcement (with the admin-approval verdict), its
/// session nonce, and its signed proof over the transcript.
#[derive(Debug, Serialize, Deserialize)]
struct HelloAckMsg {
    ack: PeerHelloAck,
    nonce: [u8; 32],
    proof: Signature,
}

/// Dialer → listener: the dialer's signed proof over the transcript.
#[derive(Debug, Serialize, Deserialize)]
struct ProofMsg {
    proof: Signature,
}

/// Listener → dialer: sent *after* the registry has been updated, so the
/// dialer has a deterministic readiness signal (`connected` = the peer
/// approved us and marked the session live).
#[derive(Debug, Serialize, Deserialize)]
struct WelcomeMsg {
    connected: bool,
}

/// Both directions, post-welcome: "my current catalog is `catalog_id` at
/// `generation`" — enough for the other side to detect staleness cheaply.
#[derive(Debug, Serialize, Deserialize)]
struct CatalogAnnounceMsg {
    catalog_id: [u8; 32],
    generation: u64,
}

/// Dialer → listener: request the full signed catalog.
#[derive(Debug, Serialize, Deserialize)]
struct CatalogGetMsg {}

/// Listener → dialer: the full `SignedCatalog` in its wire form. The receiver
/// verifies signature + staleness before trusting a byte of it.
#[derive(Debug, Serialize, Deserialize)]
struct CatalogMsg {
    bytes: Vec<u8>,
}

/// `MT_EVENTS` payload: the model's [`PushEvents`] plus a parallel list of
/// origin server keys, one per event (same order + length). The origin key is
/// what verifies each event's origin signature; carrying it lets a burrow that
/// never peered with the origin still verify a relayed post, provided that
/// origin-key tuple was independently trusted first. A carried key is only a
/// selector: `MT_EVENTS` can never create origin authority.
#[derive(Debug, Serialize, Deserialize)]
struct EventsMsg {
    push: PushEvents,
    origin_keys: Vec<[u8; 32]>,
}

/// A peer's declared board interest, learned from its `MT_SUBSCRIBE`.
#[derive(Debug, Default)]
enum Interest {
    /// No interest declared yet (the default) — we offer nothing.
    #[default]
    None,
    /// Wildcard: the peer wants every board.
    All,
    /// The peer wants exactly this set of board slugs.
    Boards(std::collections::HashSet<String>),
}

impl Interest {
    fn covers(&self, board: &str) -> bool {
        match self {
            Interest::None => false,
            Interest::All => true,
            Interest::Boards(set) => set.contains(board),
        }
    }
}

/// Per-edge flood-fill state, owned by one peer session task.
struct FloodEdge {
    /// The peer's proven Ed25519 server key.
    peer_key: [u8; 32],
    /// Immutable origin approved for `peer_key`.
    peer_origin: String,
    /// What the peer wants offered (from its `MT_SUBSCRIBE`).
    interest: Interest,
    /// Whether we've already announced our own interest to the peer, so a
    /// received `MT_SUBSCRIBE` triggers exactly one reply (no ping-pong).
    sent_subscription: bool,
    /// Event ids already offered to / exchanged with this peer, so an event
    /// floods each edge exactly once and never bounces back to its source.
    /// A Bloom filter: fixed footprint, no false negatives (the loop-safety
    /// guarantee), salted per edge so ids collide differently on each hop.
    seen: BloomFilter,
}

impl FloodEdge {
    fn new(peer_key: [u8; 32], peer_origin: String) -> Self {
        // Salt from the peer key so independent edges disagree on collisions.
        let salt = u64::from_le_bytes(peer_key[0..8].try_into().expect("8 bytes"));
        Self {
            peer_key,
            peer_origin,
            interest: Interest::None,
            sent_subscription: false,
            seen: BloomFilter::with_capacity_salted(EDGE_SEEN_CAPACITY, EDGE_SEEN_FP, salt),
        }
    }
}

/// The outcome of a dial attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialOutcome {
    /// The peer approved us; a live session is up (both registries Connected).
    Connected([u8; 32]),
    /// We authenticated but the peer has not approved our key yet; we are
    /// pending on their side.
    Pending([u8; 32]),
}

/// A resolved peer to dial.
#[derive(Debug, Clone)]
pub struct DialTarget {
    /// `host:port` of the peer's `federation_addr`.
    pub addr: String,
    /// TLS SNI / certificate name to expect.
    pub server_name: String,
    /// Pinned TLS certificate fingerprint.
    pub fingerprint: CertFingerprint,
    /// Expected Ed25519 server key, if known (rejects a mismatch).
    pub expected_key: Option<[u8; 32]>,
    /// Immutable federation origin expected from the peer.
    pub expected_origin: String,
}

// ---- frame helpers -------------------------------------------------------

fn fed_frame<T: Serialize>(kind: FrameKind, message_type: u16, msg: &T) -> Frame {
    Frame {
        version: PROTOCOL_VERSION,
        kind,
        family: Family::FEDERATION,
        message_type,
        id: RequestId::PUSH,
        error: None,
        payload: Payload(postcard::to_allocvec(msg).expect("federation msg serializes")),
    }
}

fn decode_fed_bounded<T: DeserializeOwned>(
    frame: &Frame,
    message_type: u16,
    max: usize,
) -> Result<T> {
    if frame.family != Family::FEDERATION {
        bail!("non-federation frame on S2S channel");
    }
    if frame.message_type != message_type {
        bail!(
            "unexpected federation message {} (wanted {message_type})",
            frame.message_type
        );
    }
    if frame.payload.0.len() > max {
        bail!("federation message too large");
    }
    postcard::from_bytes(&frame.payload.0).map_err(|e| anyhow!("federation decode: {e}"))
}

fn decode_fed<T: DeserializeOwned>(frame: &Frame, message_type: u16) -> Result<T> {
    decode_fed_bounded(frame, message_type, MAX_MSG)
}

async fn recv_fed_bounded<T: DeserializeOwned>(
    conn: &mut dyn Connection,
    message_type: u16,
    max: usize,
) -> Result<T> {
    let frame = conn
        .recv()
        .await?
        .ok_or_else(|| anyhow!("peer closed before message {message_type}"))?;
    decode_fed_bounded(&frame, message_type, max)
}

async fn recv_fed<T: DeserializeOwned>(conn: &mut dyn Connection, message_type: u16) -> Result<T> {
    recv_fed_bounded(conn, message_type, MAX_MSG).await
}

fn random_nonce() -> [u8; 32] {
    use rand::RngCore;
    let mut n = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut n);
    n
}

/// The exact bytes both sides sign: context, keys, immutable origins, and
/// nonces in fixed role order.
fn auth_transcript(
    dialer_key: &[u8; 32],
    listener_key: &[u8; 32],
    dialer_origin: &str,
    listener_origin: &str,
    dialer_nonce: &[u8; 32],
    listener_nonce: &[u8; 32],
) -> Vec<u8> {
    let mut m = Vec::with_capacity(
        AUTH_CONTEXT.len() + 128 + dialer_origin.len() + listener_origin.len() + 4,
    );
    m.extend_from_slice(AUTH_CONTEXT);
    m.extend_from_slice(dialer_key);
    m.extend_from_slice(listener_key);
    m.extend_from_slice(&(dialer_origin.len() as u16).to_be_bytes());
    m.extend_from_slice(dialer_origin.as_bytes());
    m.extend_from_slice(&(listener_origin.len() as u16).to_be_bytes());
    m.extend_from_slice(listener_origin.as_bytes());
    m.extend_from_slice(dialer_nonce);
    m.extend_from_slice(listener_nonce);
    m
}

fn my_hello(shared: &Shared) -> PeerHello {
    PeerHello {
        server_key: shared.server_key,
        server_name: shared.config.read().name,
        origin: shared.origin_name(),
        protocol_version: FED_PROTOCOL,
        software: SOFTWARE.to_string(),
    }
}

// ---- listener ------------------------------------------------------------

/// Bind + serve the S2S federation surface. Returns the bound address (useful
/// when the config asked for port 0) and the task handle. Mirrors the
/// `spawn_hotline` / `spawn_ftn` helpers.
pub async fn spawn_federation(
    shared: Arc<Shared>,
    addr: SocketAddr,
    tls: &TlsIdentity,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = QuicListener::bind(addr, tls)?;
    let local = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        // Background dialer: keep configured peers connected.
        tokio::spawn(federation_dialer(shared.clone()));

        let mut listener = listener;
        loop {
            match listener.accept().await {
                Ok(conn) => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        if let Err(e) = serve_peer(conn, shared).await {
                            tracing::debug!("federation peer session error: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("federation accept failed: {e}");
                    // A doomed handshake shouldn't spin the accept loop hot.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok((local, handle))
}

/// Handle one inbound peer connection: run the mutual-auth handshake, apply
/// admin approval, and hold the session for its lifetime.
async fn serve_peer(mut conn: Box<dyn Connection>, shared: Arc<Shared>) -> Result<()> {
    let key = IdentityKey::from_seed(&shared.server_signing_seed);
    let my_key = shared.server_key;
    let remote = conn.peer().remote_addr.to_string();

    // 1. Receive the dialer's Hello.
    let hello: HelloMsg = recv_fed(conn.as_mut(), MT_HELLO).await?;
    let dialer_key = hello.hello.server_key;
    let dialer_name = hello.hello.server_name.clone();
    let dialer_origin = hello.hello.origin.clone();
    if hello.hello.protocol_version != FED_PROTOCOL {
        bail!("peer federation protocol version is not supported");
    }
    if !is_valid_server_name(&dialer_origin) {
        bail!("peer announced an invalid federation origin");
    }
    if dialer_origin == shared.origin_name() && dialer_key != my_key {
        bail!("peer attempted to claim our local federation origin");
    }

    // 2. Reply with our announcement + proof. `accepted` reflects the current
    //    approval of the claimed key (advisory; the session only goes live
    //    after we verify the dialer's proof below).
    let listener_nonce = random_nonce();
    let listener_origin = shared.origin_name();
    let transcript = auth_transcript(
        &dialer_key,
        &my_key,
        &dialer_origin,
        &listener_origin,
        &hello.nonce,
        &listener_nonce,
    );
    let approved_at_ack = shared.peers.is_approved_origin(&dialer_key, &dialer_origin);
    let ack = PeerHelloAck {
        server_key: my_key,
        server_name: shared.config.read().name,
        origin: listener_origin,
        protocol_version: FED_PROTOCOL,
        software: SOFTWARE.to_string(),
        accepted: approved_at_ack,
    };
    conn.send(fed_frame(
        FrameKind::Reply,
        MT_HELLO_ACK,
        &HelloAckMsg {
            ack,
            nonce: listener_nonce,
            proof: key.sign(&transcript),
        },
    ))
    .await?;

    // 3. Verify the dialer proved possession of its announced key.
    let proof: ProofMsg = recv_fed(conn.as_mut(), MT_PROOF).await?;
    if !PublicKey(dialer_key).verify(&transcript, &proof.proof) {
        bail!("dialer failed to authenticate its server key");
    }

    // 4. Apply approval, update the registry, then send Welcome as a
    //    deterministic readiness signal.
    let connected = if authorize_authenticated_peer(
        &shared.peers,
        &shared.fed_flood,
        dialer_key,
        &dialer_origin,
        &dialer_name,
        &remote,
    )? {
        tracing::info!(peer = %PublicKey(dialer_key).fingerprint(), %remote, "federation peer connected");
        true
    } else {
        shared.peers.note_pending(
            dialer_key,
            dialer_name.clone(),
            dialer_origin.clone(),
            Some(remote.clone()),
        );
        tracing::info!(
            peer = %PublicKey(dialer_key).fingerprint(),
            %remote,
            "federation peer pending admin approval"
        );
        false
    };
    conn.send(fed_frame(
        FrameKind::Reply,
        MT_WELCOME,
        &WelcomeMsg { connected },
    ))
    .await?;

    if !connected {
        // Refuse: FIN the write half gracefully so the Welcome isn't truncated.
        conn.close().await;
        return Ok(());
    }

    // Serve catalog traffic + board-event flood-fill until the peer drops the
    // session. This listener side also answers catalog announce/get (the
    // dialer pulls; see `sync_catalogs`).
    run_peer_session(conn, dialer_key, dialer_origin, shared, true).await;
    Ok(())
}

/// Commit the inbound post-proof transition only while the exact tuple is
/// still approved. The peer may have been revoked while the proof crossed the
/// network, so the pre-Ack approval bit is deliberately not an input here.
fn authorize_authenticated_peer(
    peers: &rabbithole_server_core::PeerRegistry,
    trusted_origins: &crate::fed_flood::FloodState,
    key: [u8; 32],
    origin: &str,
    name: &str,
    remote: &str,
) -> Result<bool> {
    let trusted =
        peers.while_approved_origin(&key, origin, || trusted_origins.trust_direct(origin, key));
    let Some(trusted) = trusted else {
        return Ok(false);
    };
    trusted.context("peer origin conflicts with trusted provenance")?;
    Ok(peers.set_connected_if_approved_origin(key, origin, name, Some(remote.to_string())))
}

/// The post-welcome session loop, shared by both the listener (`serve_peer`)
/// and the dialer (`hold_dialer`). It drives board-event flood-fill —
/// announcing our board interest, offering/pulling/delivering signed events —
/// and, on the listener side, still answers catalog announce/get.
///
/// Two branches, one `&mut conn` (like `session.rs`): an inbound federation
/// frame, or a local `ServerEvent::BoardPost` that we may offer to this peer.
/// Because `tokio::select!` drops the losing branch's future before running
/// the winner's body, the bus branch is free to `conn.send` an offer even
/// though the recv branch borrowed `conn`.
async fn run_peer_session(
    mut conn: Box<dyn Connection>,
    peer_key: [u8; 32],
    peer_origin: String,
    shared: Arc<Shared>,
    serve_catalog: bool,
) {
    let mut edge = FloodEdge::new(peer_key, peer_origin);
    // The dialer announces its interest first (after its synchronous catalog
    // pull has completed, so it can't collide with that request/reply). The
    // listener stays quiet until it receives a subscription, then replies with
    // its own — this keeps the dialer's catalog exchange frame-ordered.
    let is_dialer = !serve_catalog;
    if is_dialer {
        if let Err(e) = announce_subscription(conn.as_mut(), &shared, &mut edge).await {
            tracing::debug!(
                peer = %PublicKey(peer_key).fingerprint(),
                "federation: subscription announce failed: {e}"
            );
        }
    }
    let mut bus = shared.bus.subscribe();
    let mut approval_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    approval_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = approval_tick.tick() => {
                if !shared
                    .peers
                    .is_approved_origin(&edge.peer_key, &edge.peer_origin)
                {
                    tracing::info!(
                        peer = %PublicKey(peer_key).fingerprint(),
                        "federation: closing session after approval revocation"
                    );
                    break;
                }
            }
            incoming = conn.recv() => {
                match incoming {
                    Ok(Some(frame)) => {
                        if let Err(e) =
                            handle_peer_frame(conn.as_mut(), &shared, &mut edge, &frame, serve_catalog)
                                .await
                        {
                            tracing::debug!(
                                peer = %PublicKey(peer_key).fingerprint(),
                                "federation session exchange ended: {e}"
                            );
                            break;
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            ev = bus.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                match ev {
                    Ok(ServerEvent::BoardPost { board, id, .. })
                    | Ok(ServerEvent::BoardEvent { board, id }) => {
                        if !shared
                            .peers
                            .is_approved_origin(&edge.peer_key, &edge.peer_origin)
                        {
                            break;
                        }
                        if let Err(e) = maybe_offer(conn.as_mut(), &mut edge, &board, id).await {
                            tracing::debug!(
                                peer = %PublicKey(peer_key).fingerprint(),
                                "federation offer failed: {e}"
                            );
                            break;
                        }
                    }
                    Ok(ServerEvent::Shutdown) | Err(RecvError::Closed) => break,
                    // A lagged flood subscriber simply misses some live offers;
                    // catch-up on the next subscribe (or the next post) recovers.
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                }
            }
        }
    }
    shared.peers.set_disconnected(&peer_key);
    tracing::info!(peer = %PublicKey(peer_key).fingerprint(), "federation peer disconnected");
    conn.close().await;
}

/// Answer a peer's catalog announcement with our own id/generation, so the
/// dialer can decide whether a full fetch is worth it.
async fn serve_catalog_announce(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    frame: &Frame,
) -> Result<()> {
    // Decode (validates shape); the announcement itself is informational —
    // pull is dialer-driven, so we don't fetch back on this connection.
    let theirs: CatalogAnnounceMsg = decode_fed(frame, MT_CATALOG_ANNOUNCE)?;
    tracing::debug!(generation = theirs.generation, "peer announced its catalog");
    let mine = crate::fed_catalog::local_catalog(shared).await?;
    conn.send(fed_frame(
        FrameKind::Reply,
        MT_CATALOG_ANNOUNCE,
        &CatalogAnnounceMsg {
            catalog_id: mine.catalog_id().map_err(|e| anyhow!("catalog id: {e}"))?,
            generation: mine.catalog.generation,
        },
    ))
    .await?;
    Ok(())
}

/// Serve the full signed catalog — but only while the peer is still
/// admin-approved (approval can be revoked mid-session).
async fn serve_catalog_get(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    dialer_key: &[u8; 32],
    dialer_origin: &str,
) -> Result<()> {
    if !shared.peers.is_approved_origin(dialer_key, dialer_origin) {
        bail!("peer approval revoked; refusing catalog fetch");
    }
    let signed = crate::fed_catalog::local_catalog(shared).await?;
    conn.send(fed_frame(
        FrameKind::Reply,
        MT_CATALOG,
        &CatalogMsg {
            bytes: signed.to_bytes(),
        },
    ))
    .await?;
    Ok(())
}

// ---- board-event flood-fill ---------------------------------------------

/// Dispatch one inbound federation frame within a live peer session. Unknown
/// message types are ignored (forward compatibility).
async fn handle_peer_frame(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
    frame: &Frame,
    serve_catalog: bool,
) -> Result<()> {
    if !shared
        .peers
        .is_approved_origin(&edge.peer_key, &edge.peer_origin)
    {
        bail!("peer approval revoked");
    }
    if frame.family != Family::FEDERATION {
        bail!("non-federation frame on the S2S channel");
    }
    match frame.message_type {
        MT_CATALOG_ANNOUNCE if serve_catalog => serve_catalog_announce(conn, shared, frame).await,
        MT_CATALOG_GET if serve_catalog => {
            serve_catalog_get(conn, shared, &edge.peer_key, &edge.peer_origin).await
        }
        MT_SUBSCRIBE => {
            handle_subscribe(shared, edge, frame)?;
            // Reply with our own interest if we haven't announced it yet (the
            // listener side), so the exchange is mutual with no ping-pong.
            if !edge.sent_subscription {
                announce_subscription(conn, shared, edge).await?;
            }
            // Immediately offer what we already hold for the peer's new
            // interest, so a post that predates the subscription still floods.
            catchup_offer(conn, shared, edge).await
        }
        MT_IHAVE => handle_ihave(conn, shared, edge, frame).await,
        MT_PULL => handle_pull(conn, shared, edge, frame).await,
        MT_EVENTS => handle_events(shared, edge, frame).await,
        _ => Ok(()),
    }
}

/// Announce our own board interest (`federation_board_subscribe`) to the peer
/// and mark this edge as having sent it. Nothing is sent (and the flag stays
/// clear) when the operator opted into no boards — flood is opt-in.
async fn announce_subscription(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
) -> Result<()> {
    let wanted = shared.config.read().federation_board_subscribe;
    if wanted.is_empty() {
        return Ok(());
    }
    let me = shared.server_key;
    let wildcard = wanted.iter().any(|s| s == "all" || s == "*");
    let subs: Vec<Subscription> = if wildcard {
        vec![Subscription {
            peer_key: me,
            board_slug: "*".to_string(),
        }]
    } else {
        wanted
            .into_iter()
            .take(MAX_SUBSCRIBE_BOARDS)
            .map(|board_slug| Subscription {
                peer_key: me,
                board_slug,
            })
            .collect()
    };
    conn.send(fed_frame(FrameKind::Request, MT_SUBSCRIBE, &subs))
        .await?;
    edge.sent_subscription = true;
    Ok(())
}

/// Record the peer's declared interest from its `MT_SUBSCRIBE`. Only honoured
/// from an admin-approved peer (subscriptions are exchanged post-welcome with
/// approved peers only).
fn handle_subscribe(shared: &Arc<Shared>, edge: &mut FloodEdge, frame: &Frame) -> Result<()> {
    if !shared
        .peers
        .is_approved_origin(&edge.peer_key, &edge.peer_origin)
    {
        bail!("subscribe from non-approved peer");
    }
    let subs: Vec<Subscription> = decode_fed(frame, MT_SUBSCRIBE)?;
    if subs.len() > MAX_SUBSCRIBE_BOARDS {
        bail!("subscription list too large");
    }
    edge.interest = if subs.iter().any(|s| s.board_slug == "*") {
        Interest::All
    } else {
        Interest::Boards(subs.into_iter().map(|s| s.board_slug).collect())
    };
    tracing::debug!(
        peer = %PublicKey(edge.peer_key).fingerprint(),
        "federation: recorded peer board subscription"
    );
    Ok(())
}

/// Offer the event ids we already hold for every board the peer now wants —
/// the catch-up half of the exchange (the live half is `maybe_offer`).
async fn catchup_offer(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
) -> Result<()> {
    let boards = shared.boards.boards().await.map_err(anyhow::Error::msg)?;
    for b in boards {
        if b.kind != 2 || !edge.interest.covers(&b.slug) {
            continue;
        }
        let posts = crate::nntp::group_articles(shared, &b.slug).await?;
        let mut ids: Vec<[u8; 32]> = Vec::new();
        // Newest first, bounded — an offer never grows without limit.
        for p in posts.iter().rev() {
            if ids.len() >= MAX_IHAVE_IDS {
                break;
            }
            if edge.seen.contains(&p.event_id) {
                continue;
            }
            edge.seen.insert(&p.event_id);
            ids.push(p.event_id);
        }
        if ids.is_empty() {
            continue;
        }
        conn.send(fed_frame(
            FrameKind::Push,
            MT_IHAVE,
            &IHave {
                board: b.slug.clone(),
                event_ids: ids,
            },
        ))
        .await?;
    }
    Ok(())
}

/// The live half of offering: a local `BoardPost` fired (authored here or just
/// ingested from another peer). Offer it to this peer if its subscription
/// covers the board and we haven't already touched the id on this edge —
/// which also skips offering an event straight back to the peer we got it from
/// (that peer's edge recorded the id on ingest).
async fn maybe_offer(
    conn: &mut dyn Connection,
    edge: &mut FloodEdge,
    board: &str,
    id: [u8; 32],
) -> Result<()> {
    if !edge.interest.covers(board) || edge.seen.contains(&id) {
        return Ok(());
    }
    edge.seen.insert(&id);
    conn.send(fed_frame(
        FrameKind::Push,
        MT_IHAVE,
        &IHave {
            board: board.to_string(),
            event_ids: vec![id],
        },
    ))
    .await?;
    Ok(())
}

/// A peer offered ids for a board; pull the ones we lack (not already stored,
/// not already processed). Bounded.
async fn handle_ihave(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
    frame: &Frame,
) -> Result<()> {
    let offer: IHave = decode_fed(frame, MT_IHAVE)?;
    if offer.event_ids.len() > MAX_IHAVE_IDS {
        bail!("ihave list too large");
    }
    let mut want: Vec<[u8; 32]> = Vec::new();
    for id in offer.event_ids {
        if want.len() >= MAX_PULL_IDS {
            break;
        }
        // Already seen in the dedupe window, or already durably stored: skip.
        if shared.dedup.seen(&SeenKey::Event(id)) {
            continue;
        }
        if shared
            .boards
            .post_by_id(&id)
            .await
            .map_err(anyhow::Error::msg)?
            .is_some()
        {
            continue;
        }
        want.push(id);
    }
    // Record the offer on this edge so we don't re-offer these ids back.
    for id in &want {
        edge.seen.insert(id);
    }
    if want.is_empty() {
        return Ok(());
    }
    conn.send(fed_frame(
        FrameKind::Request,
        MT_PULL,
        &PullRequest {
            board: offer.board,
            event_ids: want,
        },
    ))
    .await?;
    Ok(())
}

/// Serve a peer's pull: deliver the signed events it requested (only for the
/// named board, and only ones whose origin key we can vouch for). Only an
/// admin-approved peer is served. Bounded per reply.
async fn handle_pull(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
    frame: &Frame,
) -> Result<()> {
    if !shared
        .peers
        .is_approved_origin(&edge.peer_key, &edge.peer_origin)
    {
        bail!("pull from non-approved peer");
    }
    let req: PullRequest = decode_fed(frame, MT_PULL)?;
    if req.event_ids.len() > MAX_PULL_IDS {
        bail!("pull list too large");
    }
    let mut events: Vec<FedEvent> = Vec::new();
    let mut origin_keys: Vec<[u8; 32]> = Vec::new();
    for id in req.event_ids.iter().take(MAX_EVENTS_PER_MSG) {
        // The requested id is either a post or a board follow-up (both are
        // signed board events served the same way).
        let (blob, ev_board) = if let Some(row) = shared
            .boards
            .post_by_id(id)
            .await
            .map_err(anyhow::Error::msg)?
        {
            (row.event_blob, row.board_slug)
        } else if let Some(f) = shared
            .boards
            .followup_by_id(id)
            .await
            .map_err(anyhow::Error::msg)?
        {
            (f.event_blob, f.board_slug)
        } else {
            continue;
        };
        if ev_board != req.board {
            continue; // don't cross boards
        }
        let Ok(signed) = postcard::from_bytes::<SignedEvent>(&blob) else {
            continue;
        };
        let Some(origin_key) = origin_key_for(shared, &signed) else {
            continue; // we can't vouch for its origin key — don't relay it
        };
        edge.seen.insert(id);
        events.push(FedEvent {
            id: *id,
            bytes: blob,
        });
        origin_keys.push(origin_key);
    }
    if events.is_empty() {
        return Ok(());
    }
    conn.send(fed_frame(
        FrameKind::Reply,
        MT_EVENTS,
        &EventsMsg {
            push: PushEvents {
                board: req.board,
                events,
            },
            origin_keys,
        },
    ))
    .await?;
    Ok(())
}

/// Ingest delivered events: verify each origin signature, dedupe, and project
/// via `BoardService` (preserving the origin author + signed blob — never
/// re-signed as local). A newly-ingested post re-fires `BoardPost`, which is
/// what floods it to the next hop.
async fn handle_events(shared: &Arc<Shared>, edge: &mut FloodEdge, frame: &Frame) -> Result<()> {
    let msg: EventsMsg = decode_fed_bounded(frame, MT_EVENTS, MAX_EVENTS_PAYLOAD)?;
    if msg.push.events.len() > MAX_EVENTS_PER_MSG {
        bail!("too many events in one delivery");
    }
    if msg.origin_keys.len() != msg.push.events.len() {
        bail!("origin-key / event length mismatch");
    }
    let board = msg.push.board.clone();
    let Some(brow) = shared
        .boards
        .board(&board)
        .await
        .map_err(anyhow::Error::msg)?
    else {
        return Ok(()); // the board must exist locally to accept its events
    };
    if brow.kind != 2 {
        return Ok(()); // not a postable board
    }
    for (fe, origin_key) in msg.push.events.iter().zip(msg.origin_keys.iter()) {
        if let Err(e) =
            ingest_fed_event(shared, edge, &board, fe, origin_key, brow.max_threads).await
        {
            tracing::debug!(
                peer = %PublicKey(edge.peer_key).fingerprint(),
                board = %board,
                "federation: rejected delivered event: {e}"
            );
        }
    }
    Ok(())
}

/// Verify + store one delivered event. Mirrors `ingest_peer_catalog`: the
/// origin signature must verify under the trusted origin key before a byte is
/// trusted; forgeries and stale/duplicate replays are refused. The board is
/// verified to exist by the caller.
async fn ingest_fed_event(
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
    board: &str,
    fe: &FedEvent,
    carried_origin_key: &[u8; 32],
    max_threads: i64,
) -> Result<()> {
    let signed: SignedEvent =
        postcard::from_bytes(&fe.bytes).map_err(|e| anyhow!("decode event: {e}"))?;
    // The wire content id must match the signed event's own id.
    if signed.id != fe.id {
        bail!("event id does not match its content");
    }
    // Posts are board-scoped by their own body; follow-ups (Edit/Tombstone)
    // target a post by id and are stored under the delivery's named board.
    let is_post = matches!(signed.body, EventBody::Post { .. });
    if let EventBody::Post {
        board: ev_board, ..
    } = &signed.body
    {
        if ev_board != board {
            bail!("delivered post board mismatch");
        }
    }

    // Resolve the origin server key. Our own echoed-back events verify under
    // our key (never a peer-supplied one). Remote origins must already have a
    // provenance-bearing pin established by a direct authenticated session or
    // an explicit operator action. A relay can never create a first pin.
    let local_origin = shared.origin_name();
    let origin_key = trusted_origin_key(
        &local_origin,
        shared.server_key,
        &shared.fed_flood,
        &signed.origin,
        *carried_origin_key,
    )?;

    // Verify the content id + author signature + origin signature. (The
    // Edit/Tombstone *authorization* gate — author-or-home-server — lives in
    // `BoardService::ingest_event`, applied consistently on the present-now
    // and out-of-order paths.)
    signed
        .verify(&origin_key)
        .map_err(|e| anyhow!("signature rejected: {e:?}"))?;

    // Already stored (our own event, or a prior flood): a no-op replay. Posts
    // and follow-ups live in different tables. Record it on this edge so the
    // source is never re-offered, then return without re-firing (loop safety).
    let already = if is_post {
        shared
            .boards
            .post_by_id(&fe.id)
            .await
            .map_err(anyhow::Error::msg)?
            .is_some()
    } else {
        shared
            .boards
            .followup_by_id(&fe.id)
            .await
            .map_err(anyhow::Error::msg)?
            .is_some()
    };
    if already {
        edge.seen.insert(&fe.id);
        return Ok(());
    }
    // Cross-edge dedupe window: the first sighting acts; a copy arriving over
    // another edge drops here without a second projection or re-fire.
    let now = chrono::Utc::now().timestamp_millis();
    if !shared
        .dedup
        .check_and_record(SeenKey::Event(signed.id), now)
    {
        edge.seen.insert(&fe.id);
        return Ok(());
    }
    edge.seen.insert(&fe.id);

    // Ingest without re-signing (origin author + signed blob kept). An
    // unauthorized follow-up is refused and dropped without re-firing.
    let outcome = match shared
        .boards
        .ingest_event(&signed, board, max_threads)
        .await
    {
        Ok(o) => o,
        Err(BoardError::Forbidden) => {
            tracing::debug!(
                peer = %PublicKey(edge.peer_key).fingerprint(),
                origin = %signed.origin,
                "federation: refused unauthorized board follow-up"
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow!("ingest: {e}")),
    };
    // Re-fire on the bus: floods to the next hop, and for a post also updates
    // local unread/pushes. A follow-up fires BoardEvent (no unread bump) — even
    // a pending one, so peers who hold the target apply it.
    match outcome {
        IngestOutcome::Posted(row) => shared.bus.publish(ServerEvent::BoardPost {
            board: row.board_slug.clone(),
            id: row.event_id,
            root: row.root_id,
        }),
        IngestOutcome::Applied { board: b, .. } | IngestOutcome::Pending { board: b, .. } => {
            shared.bus.publish(ServerEvent::BoardEvent {
                board: b,
                id: fe.id,
            })
        }
    }
    tracing::info!(
        peer = %PublicKey(edge.peer_key).fingerprint(),
        board = %board,
        origin = %signed.origin,
        "federation: ingested peer board event"
    );
    Ok(())
}

fn trusted_origin_key(
    local_origin: &str,
    local_key: [u8; 32],
    trusted: &crate::fed_flood::FloodState,
    claimed_origin: &str,
    carried_key: [u8; 32],
) -> Result<[u8; 32]> {
    if claimed_origin == local_origin {
        if carried_key != local_key {
            bail!("peer supplied the wrong key for our local origin");
        }
        return Ok(local_key);
    }
    let Some(pinned) = trusted.resolve(claimed_origin) else {
        bail!(
            "origin {claimed_origin} has no trusted provenance; relays cannot establish origin keys"
        );
    };
    if pinned != carried_key {
        bail!("origin key conflict for {claimed_origin}");
    }
    Ok(pinned)
}

/// The origin server key that signs an event's origin signature: our own key
/// for locally-authored events, else a provenance-bearing trusted pin.
/// `None` = we can't vouch for it (so we won't relay it).
fn origin_key_for(shared: &Arc<Shared>, signed: &SignedEvent) -> Option<[u8; 32]> {
    if signed.origin == shared.origin_name() {
        Some(shared.server_key)
    } else {
        shared.fed_flood.resolve(&signed.origin)
    }
}

// ---- dialer --------------------------------------------------------------

/// Dial a peer and run the handshake. On success (`Connected`) a background
/// task holds the session open; the peer is implicitly approved on our side
/// (we chose to dial it). Returns once both registries reflect the outcome.
pub async fn dial_peer(shared: Arc<Shared>, target: DialTarget) -> Result<DialOutcome> {
    let transport = QuicTransport::new(
        target.server_name.clone(),
        ServerAuth::Pinned(target.fingerprint),
    );
    let mut conn = transport.connect(&target.addr).await?;

    let key = IdentityKey::from_seed(&shared.server_signing_seed);
    let my_key = shared.server_key;
    let dialer_nonce = random_nonce();

    // 1. Send Hello.
    conn.send(fed_frame(
        FrameKind::Request,
        MT_HELLO,
        &HelloMsg {
            hello: my_hello(&shared),
            nonce: dialer_nonce,
        },
    ))
    .await?;

    // 2. Receive HelloAck; verify the peer proved its announced key.
    let ack: HelloAckMsg = recv_fed(conn.as_mut(), MT_HELLO_ACK).await?;
    let listener_key = ack.ack.server_key;
    let listener_name = ack.ack.server_name.clone();
    let listener_origin = ack.ack.origin.clone();
    if ack.ack.protocol_version != FED_PROTOCOL {
        bail!("peer federation protocol version is not supported");
    }
    if !is_valid_server_name(&listener_origin) || listener_origin != target.expected_origin {
        bail!("peer presented an unexpected federation origin");
    }
    if listener_origin == shared.origin_name() && listener_key != my_key {
        bail!("peer attempted to claim our local federation origin");
    }
    if let Some(expected) = target.expected_key {
        if expected != listener_key {
            bail!("peer presented an unexpected server key");
        }
    }
    let dialer_origin = shared.origin_name();
    let transcript = auth_transcript(
        &my_key,
        &listener_key,
        &dialer_origin,
        &listener_origin,
        &dialer_nonce,
        &ack.nonce,
    );
    if !PublicKey(listener_key).verify(&transcript, &ack.proof) {
        bail!("peer failed to authenticate its server key");
    }

    // 3. Send our proof.
    conn.send(fed_frame(
        FrameKind::Request,
        MT_PROOF,
        &ProofMsg {
            proof: key.sign(&transcript),
        },
    ))
    .await?;

    // 4. Await the readiness signal (registry-updated by the peer).
    let welcome: WelcomeMsg = recv_fed(conn.as_mut(), MT_WELCOME).await?;

    // We chose to dial this peer, so it is approved on our side.
    shared.peers.seed_approved(
        listener_key,
        listener_name.clone(),
        Some(listener_origin.clone()),
    );
    let remote = conn.peer().remote_addr.to_string();

    if welcome.connected {
        shared
            .fed_flood
            .trust_direct(&listener_origin, listener_key)
            .context("peer origin conflicts with trusted provenance")?;
        if !shared.peers.set_connected_if_approved_origin(
            listener_key,
            &listener_origin,
            listener_name.clone(),
            Some(remote),
        ) {
            bail!("configured peer approval changed during authentication");
        }
        tracing::info!(
            peer = %PublicKey(listener_key).fingerprint(),
            "federation dial established a session"
        );
        // Catalog sync (dialer-pull) rides the live session before it goes to
        // the background hold, so callers have a deterministic "sync
        // attempted" point. Failure is non-fatal: the peering session is
        // useful without a catalog, and the next dial retries.
        if let Err(e) = sync_catalogs(conn.as_mut(), &shared, listener_key).await {
            tracing::warn!(
                peer = %PublicKey(listener_key).fingerprint(),
                "federation catalog sync failed: {e}"
            );
        }
        // Hold the session open in the background until it drops.
        let hold = shared.clone();
        tokio::spawn(async move {
            hold_dialer(conn, listener_key, listener_origin, hold).await;
        });
        Ok(DialOutcome::Connected(listener_key))
    } else {
        tracing::info!(
            peer = %PublicKey(listener_key).fingerprint(),
            "federation dial pending peer approval"
        );
        conn.close().await;
        Ok(DialOutcome::Pending(listener_key))
    }
}

/// Announce our catalog, learn the peer's, and pull its full signed catalog
/// when the announced generation is fresher than what we hold. The received
/// catalog is verified against the peer's pinned (handshake-proven) key and
/// generation staleness before being stored.
async fn sync_catalogs(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    peer_key: [u8; 32],
) -> Result<()> {
    let mine = crate::fed_catalog::local_catalog(shared).await?;
    conn.send(fed_frame(
        FrameKind::Request,
        MT_CATALOG_ANNOUNCE,
        &CatalogAnnounceMsg {
            catalog_id: mine.catalog_id().map_err(|e| anyhow!("catalog id: {e}"))?,
            generation: mine.catalog.generation,
        },
    ))
    .await?;
    let theirs: CatalogAnnounceMsg = recv_fed(conn, MT_CATALOG_ANNOUNCE).await?;
    if !shared.catalogs.wants(&peer_key, theirs.generation) {
        return Ok(()); // we already hold this generation (or newer)
    }
    conn.send(fed_frame(
        FrameKind::Request,
        MT_CATALOG_GET,
        &CatalogGetMsg {},
    ))
    .await?;
    let msg: CatalogMsg = recv_fed_bounded(conn, MT_CATALOG, MAX_CATALOG).await?;
    let stored = crate::fed_catalog::ingest_peer_catalog(shared, peer_key, &msg.bytes)?;
    tracing::info!(
        peer = %PublicKey(peer_key).fingerprint(),
        generation = stored.catalog.generation,
        entries = stored.catalog.entries.len(),
        "verified peer catalog stored"
    );
    Ok(())
}

/// Keep a dialer-side session alive until the peer drops it, running
/// board-event flood-fill over it. The dialer's catalog sync already ran
/// before this hold, so catalog serving here is disabled (the listener side
/// answers those).
async fn hold_dialer(
    conn: Box<dyn Connection>,
    peer_key: [u8; 32],
    peer_origin: String,
    shared: Arc<Shared>,
) {
    run_peer_session(conn, peer_key, peer_origin, shared, false).await;
}

/// Background task: periodically dial configured peers that aren't connected.
async fn federation_dialer(shared: Arc<Shared>) {
    let mut bus = shared.bus.subscribe();
    let mut tick = tokio::time::interval(DIAL_INTERVAL);
    loop {
        tokio::select! {
            _ = tick.tick() => dial_configured_once(&shared).await,
            ev = bus.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                match ev {
                    Ok(rabbithole_server_core::ServerEvent::Shutdown)
                    | Err(RecvError::Closed) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn dial_configured_once(shared: &Arc<Shared>) {
    for peer in shared.config.read().federation_peers {
        let Some(target) = resolve_target(&peer) else {
            tracing::warn!(addr = %peer.addr, "federation: skipping peer with invalid origin, key, or fingerprint");
            continue;
        };
        // Skip peers we already hold a live session with.
        if let Some(expected) = target.expected_key {
            if shared.peers.state(&expected) == Some(rabbithole_server_core::PeerState::Connected) {
                continue;
            }
        }
        if let Err(e) = dial_peer(shared.clone(), target).await {
            tracing::debug!(addr = %peer.addr, "federation dial failed: {e}");
        }
    }
}

/// Build a [`DialTarget`] from a configured peer, or `None` if its origin,
/// key, or fingerprint is malformed.
pub fn resolve_target(peer: &rabbithole_server_core::config::FederationPeer) -> Option<DialTarget> {
    let fingerprint = CertFingerprint::from_hex(peer.fingerprint.trim())?;
    let expected_key = if peer.key.trim().is_empty() {
        None
    } else {
        Some(hex_key(&peer.key)?)
    };
    let server_name = if peer.server_name.trim().is_empty() {
        "localhost".to_string()
    } else {
        peer.server_name.clone()
    };
    let expected_origin = peer.origin.trim().to_string();
    if !is_valid_server_name(&expected_origin) {
        return None;
    }
    Some(DialTarget {
        addr: peer.addr.clone(),
        server_name,
        fingerprint,
        expected_key,
        expected_origin,
    })
}

/// Parse a 32-byte Ed25519 key from hex.
pub fn hex_key(s: &str) -> Option<[u8; 32]> {
    hex::decode(s.trim()).ok()?.try_into().ok()
}

// ---- approved-peer persistence ------------------------------------------

fn approved_path(data_dir: &Path) -> PathBuf {
    data_dir.join("federation").join("approved_peers.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedPeer {
    pub key: [u8; 32],
    pub origin: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedFile {
    version: u32,
    peers: Vec<PersistedApprovedPeer>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedApprovedPeer {
    key: String,
    origin: String,
}

/// Load only provenance-bearing approved peer tuples. The former key-only
/// format is ignored fail-closed and requires an explicit re-approval.
pub fn load_approved(data_dir: &Path) -> Vec<ApprovedPeer> {
    let path = approved_path(data_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_slice::<ApprovedFile>(&bytes) else {
        tracing::warn!(
            path = %path.display(),
            "federation: ignoring legacy or unreadable peer approvals; approve origin-key tuples again"
        );
        return Vec::new();
    };
    if file.version != 2 {
        tracing::warn!(
            path = %path.display(),
            version = file.version,
            "federation: ignoring unsupported peer approval version"
        );
        return Vec::new();
    }
    if file.peers.len() > MAX_APPROVED_PEERS {
        tracing::warn!(path = %path.display(), "federation: ignoring over-cap peer approval file");
        return Vec::new();
    }
    let mut keys = std::collections::HashSet::new();
    let mut origins = std::collections::HashSet::new();
    let mut approved = Vec::with_capacity(file.peers.len());
    for peer in file.peers {
        let Some(key) = hex_key(&peer.key) else {
            tracing::warn!(path = %path.display(), "federation: ignoring invalid peer approval file");
            return Vec::new();
        };
        if !is_valid_server_name(&peer.origin)
            || !keys.insert(key)
            || !origins.insert(peer.origin.clone())
        {
            tracing::warn!(path = %path.display(), "federation: ignoring conflicting peer approval file");
            return Vec::new();
        }
        approved.push(ApprovedPeer {
            key,
            origin: peer.origin,
        });
    }
    approved
}

/// Persist the registry's current approved origin-key tuples to disk.
pub fn persist_approved(shared: &Shared) -> Result<()> {
    let data_dir = shared.config.read().data_dir;
    let path = approved_path(&data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let peers: Vec<PersistedApprovedPeer> = shared
        .peers
        .approved_origins()
        .into_iter()
        .map(|(key, origin)| PersistedApprovedPeer {
            key: hex::encode(key),
            origin,
        })
        .collect();
    let bytes = serde_json::to_vec_pretty(&ApprovedFile { version: 2, peers })?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    #[test]
    fn relay_cannot_establish_an_unseen_origin() {
        let trusted = crate::fed_flood::FloodState::new();
        let relay_key = [2u8; 32];
        let result =
            trusted_origin_key("warren-a", [1u8; 32], &trusted, "victim.example", relay_key);
        assert!(result.is_err());
        assert!(
            trusted.resolve("victim.example").is_none(),
            "a rejected relay claim must not create durable trust"
        );
    }

    #[test]
    fn handshake_proof_is_bound_to_both_origins() {
        let key = IdentityKey::from_seed(&[9u8; 32]);
        let transcript = auth_transcript(
            &[1u8; 32],
            &key.public().0,
            "warren-a",
            "warren-b",
            &[3u8; 32],
            &[4u8; 32],
        );
        let proof = key.sign(&transcript);
        let substituted = auth_transcript(
            &[1u8; 32],
            &key.public().0,
            "warren-a",
            "victim.example",
            &[3u8; 32],
            &[4u8; 32],
        );
        assert!(key.public().verify(&transcript, &proof));
        assert!(!key.public().verify(&substituted, &proof));
    }

    #[test]
    fn trusted_indirect_origin_accepts_only_its_pinned_key() {
        let trusted = crate::fed_flood::FloodState::new();
        trusted
            .trust_operator("warren-c", [3u8; 32])
            .expect("operator pin");
        assert_eq!(
            trusted_origin_key("warren-a", [1u8; 32], &trusted, "warren-c", [3u8; 32],).unwrap(),
            [3u8; 32]
        );
        assert!(
            trusted_origin_key("warren-a", [1u8; 32], &trusted, "warren-c", [4u8; 32],).is_err()
        );
    }

    #[test]
    fn legacy_key_only_peer_approvals_are_not_promoted() {
        let dir = tempfile::tempdir().unwrap();
        let path = approved_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![hex::encode([7u8; 32])]).unwrap(),
        )
        .unwrap();
        assert!(
            load_approved(dir.path()).is_empty(),
            "a key-only approval has no authenticated namespace binding"
        );

        let file = ApprovedFile {
            version: 2,
            peers: vec![PersistedApprovedPeer {
                key: hex::encode([7u8; 32]),
                origin: "warren-b".into(),
            }],
        };
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(
            load_approved(dir.path()),
            vec![ApprovedPeer {
                key: [7u8; 32],
                origin: "warren-b".into(),
            }]
        );

        let conflicting = ApprovedFile {
            version: 2,
            peers: vec![
                PersistedApprovedPeer {
                    key: hex::encode([7u8; 32]),
                    origin: "warren-b".into(),
                },
                PersistedApprovedPeer {
                    key: hex::encode([7u8; 32]),
                    origin: "victim.example".into(),
                },
            ],
        };
        std::fs::write(&path, serde_json::to_vec(&conflicting).unwrap()).unwrap();
        assert!(
            load_approved(dir.path()).is_empty(),
            "conflicting approval state must fail closed as a whole"
        );
    }

    #[test]
    fn revoke_between_ack_and_proof_cannot_connect_or_pin() {
        let peers = rabbithole_server_core::PeerRegistry::new();
        let trusted = crate::fed_flood::FloodState::new();
        let key = [0x51u8; 32];
        peers.seed_approved(key, "peer", Some("peer.example".into()));

        // The Ack may already have advertised acceptance when the operator
        // revokes. The post-proof commit must consult current state instead.
        assert!(peers.is_approved_origin(&key, "peer.example"));
        peers.revoke(&key);
        assert!(!authorize_authenticated_peer(
            &peers,
            &trusted,
            key,
            "peer.example",
            "peer",
            "127.0.0.1:4655",
        )
        .unwrap());
        assert_eq!(
            peers.state(&key),
            Some(rabbithole_server_core::PeerState::Pending)
        );
        assert_eq!(trusted.resolve("peer.example"), None);
    }
}
