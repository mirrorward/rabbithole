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
//!    the [`rabbithole_federation`] crate (identity key, name, protocol version,
//!    capabilities/software) plus a nonce each.
//! 3. Each side signs a session transcript
//!    (`context ‖ dialer_key ‖ listener_key ‖ dialer_nonce ‖ listener_nonce`)
//!    with its Ed25519 server key and the other verifies it — proving live
//!    possession of the announced identity and binding the proof to *this*
//!    connection (nonces defeat replay).
//! 4. A new peer key is **not** trusted automatically: an inbound handshake
//!    from an unknown key is recorded [`PeerState::Pending`]
//!    (see [`rabbithole_server_core::PeerRegistry`]) and refused. An admin
//!    approves it (audited, via the `ctl` peer commands); approved keys persist
//!    to `<data_dir>/federation/approved_peers.json` and are reloaded on boot.
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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use rabbithole_federation::{PeerHello, PeerHelloAck};
use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use rabbithole_net::quic::{QuicListener, QuicTransport};
use rabbithole_net::tls::{CertFingerprint, ServerAuth, TlsIdentity};
use rabbithole_net::{Connection, Listener, Transport};
use rabbithole_proto::{Family, Frame, FrameKind, Payload, RequestId, PROTOCOL_VERSION};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::Shared;

/// Domain separator for the S2S handshake proof signatures.
const AUTH_CONTEXT: &[u8] = b"rhp-fed-s2s-auth-v1";

/// Federation protocol version this build speaks.
const FED_PROTOCOL: u32 = 1;

/// Software id announced in the handshake.
const SOFTWARE: &str = concat!("rabbithole/", env!("CARGO_PKG_VERSION"));

/// Largest handshake frame payload we will decode (belt-and-suspenders).
const MAX_MSG: usize = 64 * 1024;

/// Largest catalog frame payload we will decode (a full listing is bigger
/// than any handshake message, but still bounded).
const MAX_CATALOG: usize = 4 * 1024 * 1024;

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

/// The exact bytes both sides sign: context ‖ dialer_key ‖ listener_key ‖
/// dialer_nonce ‖ listener_nonce. Fixed role order so both compute identically.
fn auth_transcript(
    dialer_key: &[u8; 32],
    listener_key: &[u8; 32],
    dialer_nonce: &[u8; 32],
    listener_nonce: &[u8; 32],
) -> Vec<u8> {
    let mut m = Vec::with_capacity(AUTH_CONTEXT.len() + 128);
    m.extend_from_slice(AUTH_CONTEXT);
    m.extend_from_slice(dialer_key);
    m.extend_from_slice(listener_key);
    m.extend_from_slice(dialer_nonce);
    m.extend_from_slice(listener_nonce);
    m
}

fn my_hello(shared: &Shared) -> PeerHello {
    PeerHello {
        server_key: shared.server_key,
        server_name: shared.config.read().name,
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

    // 2. Reply with our announcement + proof. `accepted` reflects the current
    //    approval of the claimed key (advisory; the session only goes live
    //    after we verify the dialer's proof below).
    let listener_nonce = random_nonce();
    let transcript = auth_transcript(&dialer_key, &my_key, &hello.nonce, &listener_nonce);
    let approved = shared.peers.is_approved(&dialer_key);
    let ack = PeerHelloAck {
        server_key: my_key,
        server_name: shared.config.read().name,
        protocol_version: FED_PROTOCOL,
        software: SOFTWARE.to_string(),
        accepted: approved,
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
    let connected = if approved {
        shared
            .peers
            .set_connected(dialer_key, dialer_name.clone(), Some(remote.clone()));
        tracing::info!(peer = %PublicKey(dialer_key).fingerprint(), %remote, "federation peer connected");
        true
    } else {
        shared
            .peers
            .note_pending(dialer_key, dialer_name.clone(), Some(remote.clone()));
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

    // Serve catalog traffic until the peer drops the session. Unknown
    // federation message types are ignored (forward compatibility).
    while let Ok(Some(frame)) = conn.recv().await {
        if frame.family != Family::FEDERATION {
            continue;
        }
        let served = match frame.message_type {
            MT_CATALOG_ANNOUNCE => serve_catalog_announce(conn.as_mut(), &shared, &frame).await,
            MT_CATALOG_GET => serve_catalog_get(conn.as_mut(), &shared, &dialer_key).await,
            _ => Ok(()),
        };
        if let Err(e) = served {
            tracing::debug!(
                peer = %PublicKey(dialer_key).fingerprint(),
                "federation catalog exchange ended: {e}"
            );
            break;
        }
    }
    shared.peers.set_disconnected(&dialer_key);
    tracing::info!(peer = %PublicKey(dialer_key).fingerprint(), "federation peer disconnected");
    conn.close().await;
    Ok(())
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
) -> Result<()> {
    if !shared.peers.is_approved(dialer_key) {
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
    if let Some(expected) = target.expected_key {
        if expected != listener_key {
            bail!("peer presented an unexpected server key");
        }
    }
    let transcript = auth_transcript(&my_key, &listener_key, &dialer_nonce, &ack.nonce);
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
    shared
        .peers
        .seed_approved(listener_key, listener_name.clone());
    let remote = conn.peer().remote_addr.to_string();

    if welcome.connected {
        shared
            .peers
            .set_connected(listener_key, listener_name.clone(), Some(remote));
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
            hold_dialer(conn, listener_key, hold).await;
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

/// Keep a dialer-side session alive until the peer drops it.
async fn hold_dialer(mut conn: Box<dyn Connection>, peer_key: [u8; 32], shared: Arc<Shared>) {
    // Drain until the peer drops the session (the dialer's catalog sync ran
    // before the hold; inbound frames here are ignored).
    while let Ok(Some(_)) = conn.recv().await {}
    shared.peers.set_disconnected(&peer_key);
    conn.close().await;
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
            tracing::warn!(addr = %peer.addr, "federation: skipping peer with bad key/fingerprint");
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

/// Build a [`DialTarget`] from a configured peer, or `None` if its key/
/// fingerprint hex is malformed.
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
    Some(DialTarget {
        addr: peer.addr.clone(),
        server_name,
        fingerprint,
        expected_key,
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

/// Load the admin-approved peer keys from disk (empty when absent/corrupt).
pub fn load_approved(data_dir: &Path) -> Vec<[u8; 32]> {
    let path = approved_path(data_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(hexes) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        tracing::warn!(path = %path.display(), "federation: approved_peers.json unreadable");
        return Vec::new();
    };
    hexes.iter().filter_map(|h| hex_key(h)).collect()
}

/// Persist the registry's current approved keys to disk.
pub fn persist_approved(shared: &Shared) -> Result<()> {
    let data_dir = shared.config.read().data_dir;
    let path = approved_path(&data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let hexes: Vec<String> = shared
        .peers
        .approved_keys()
        .iter()
        .map(hex::encode)
        .collect();
    std::fs::write(&path, serde_json::to_vec_pretty(&hexes)?)?;
    Ok(())
}
