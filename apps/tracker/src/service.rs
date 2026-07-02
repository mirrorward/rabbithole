//! The async listeners that glue sockets to the [`Registry`].
//!
//! Four tiny services, each an infinite loop over a pre-bound socket
//! (binding is the caller's job so tests can use ephemeral `127.0.0.1:0`
//! ports and read back the real address):
//!
//! - [`run_registration_udp`] — HTRK heartbeats in, registry updates out.
//! - [`run_listing_tcp`] — HTRK hello in, the current server list out.
//! - [`run_status_tcp`] — native placeholder: `LIST\n` (optionally
//!   `LIST cat=<name>\n`) or `CATEGORIES\n` in, tab-separated lines out
//!   (until the RHP tracker family lands).
//! - [`run_gossip_udp`] — signed announces from servers plus digest/want/
//!   batch exchanges with peer trackers (see [`crate::gossip`]).
//!
//! Malformed input never takes a listener down: bad datagrams are logged and
//! dropped, bad TCP sessions are logged and closed.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::gossip::{self, GossipMessage, MAX_GOSSIP_DATAGRAM};
use crate::htrk;
use crate::registry::{Registry, ServerEntry};

/// Largest registration datagram we accept: prefix + two maximal pascal
/// strings, with headroom for trailing junk from sloppy senders.
const MAX_REGISTRATION_DATAGRAM: usize = 2048;

/// Largest gossip datagram we read (a little above what we ever build, so a
/// slightly chatty peer is decoded rather than silently truncated).
const MAX_GOSSIP_READ: usize = 4096;

/// Runs the HTRK registration listener: one UDP datagram per heartbeat.
///
/// The registry entry is keyed by the **observed source IP** plus the port
/// declared inside the packet (the source port is ephemeral and meaningless).
/// Heartbeats go through [`Registry::register_unsigned`], so they refresh but
/// never overwrite a slot held by a verified signed descriptor.
pub async fn run_registration_udp(socket: UdpSocket, registry: Arc<Registry>) -> Result<()> {
    let mut buf = vec![0u8; MAX_REGISTRATION_DATAGRAM];
    loop {
        let (len, from) = socket.recv_from(&mut buf).await?;
        match htrk::Registration::decode(&buf[..len]) {
            Ok(reg) => {
                let entry = ServerEntry::unsigned(
                    reg.name,
                    reg.description,
                    (from.ip(), reg.port).into(),
                    reg.users_online,
                );
                tracing::debug!(%from, addr = %entry.addr, name = %entry.name, "heartbeat");
                registry.register_unsigned(entry);
            }
            Err(err) => {
                tracing::debug!(%from, len, %err, "ignoring malformed registration");
            }
        }
    }
}

/// Runs the HTRK listing listener: hello in, server list out, close.
pub async fn run_listing_tcp(listener: TcpListener, registry: Arc<Registry>) -> Result<()> {
    loop {
        let (stream, from) = listener.accept().await?;
        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            if let Err(err) = serve_listing(stream, &registry).await {
                tracing::debug!(%from, %err, "listing session ended early");
            }
        });
    }
}

async fn serve_listing(mut stream: TcpStream, registry: &Registry) -> Result<()> {
    let mut hello = [0u8; htrk::HELLO_LEN];
    stream.read_exact(&mut hello).await?;
    htrk::decode_hello(&hello)?;

    let servers: Vec<htrk::ListedServer> = registry
        .snapshot()
        .into_iter()
        .filter_map(|entry| {
            // HTRK listings carry raw IPv4 addresses only; unwrap v4-mapped
            // v6 sources and skip everything else.
            let ip = match entry.addr.ip() {
                IpAddr::V4(v4) => v4,
                IpAddr::V6(v6) => v6.to_ipv4_mapped()?,
            };
            Some(htrk::ListedServer {
                ip,
                port: entry.addr.port(),
                users_online: entry.users_online,
                name: entry.name,
                description: entry.description,
            })
        })
        .collect();

    stream.write_all(&htrk::encode_hello()).await?;
    stream.write_all(&htrk::encode_listing(&servers)).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Runs the native placeholder status listener.
///
/// Protocol (one command line in, text out, close):
///
/// - `LIST` — one line per live server:
///   `name<TAB>ip:port<TAB>users<TAB>description<TAB>categories`, where
///   `categories` is comma-joined (`-` when the entry has none).
/// - `LIST cat=<name>` — same lines, only servers carrying that category
///   (ASCII-case-insensitive).
/// - `CATEGORIES` — the summary, one `name<TAB>live-count` line per
///   category, sorted by name.
pub async fn run_status_tcp(listener: TcpListener, registry: Arc<Registry>) -> Result<()> {
    loop {
        let (stream, from) = listener.accept().await?;
        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            if let Err(err) = serve_status(stream, &registry).await {
                tracing::debug!(%from, %err, "status session ended early");
            }
        });
    }
}

async fn serve_status(stream: TcpStream, registry: &Registry) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    let command = line.trim();

    let entries = if command == "LIST" {
        Some(registry.snapshot())
    } else {
        command
            .strip_prefix("LIST cat=")
            .map(|category| registry.snapshot_category(category.trim()))
    };

    let mut out = String::new();
    match (entries, command) {
        (Some(entries), _) => {
            for entry in entries {
                let categories = if entry.categories.is_empty() {
                    "-".to_owned()
                } else {
                    entry.categories.join(",")
                };
                out.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    sanitize(&entry.name),
                    entry.addr,
                    entry.users_online,
                    sanitize(&entry.description),
                    sanitize(&categories),
                ));
            }
        }
        (None, "CATEGORIES") => {
            for (category, count) in registry.category_counts() {
                out.push_str(&format!("{}\t{}\n", sanitize(&category), count));
            }
        }
        (None, _) => out.push_str("ERR unknown command\n"),
    }
    write.write_all(out.as_bytes()).await?;
    write.shutdown().await?;
    Ok(())
}

/// Keeps registrant-supplied text from breaking the line/tab framing.
fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Runs the gossip/announce listener: signed descriptors in and out.
///
/// On every `interval` tick the tracker sends its [`gossip::digest_of`] to
/// each static peer; inbound datagrams follow the push–pull exchange
/// documented in [`crate::gossip`]. Everything is best-effort UDP — a lost
/// datagram just waits for the next tick. Malformed datagrams and rejected
/// descriptors are logged and dropped, never fatal.
pub async fn run_gossip_udp(
    socket: UdpSocket,
    registry: Arc<Registry>,
    peers: Vec<SocketAddr>,
    interval: Duration,
) -> Result<()> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut buf = vec![0u8; MAX_GOSSIP_READ];
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if peers.is_empty() {
                    continue;
                }
                let digest = gossip::digest_of(&registry.snapshot());
                let wire = GossipMessage::Digest(digest).encode();
                for peer in &peers {
                    // Best-effort: an unreachable peer must not kill gossip.
                    if let Err(err) = socket.send_to(&wire, peer).await {
                        tracing::debug!(%peer, %err, "gossip digest send failed");
                    }
                }
            }
            received = socket.recv_from(&mut buf) => {
                let (len, from) = received?;
                handle_gossip(&socket, &registry, &buf[..len], from).await;
            }
        }
    }
}

/// Handles one inbound gossip datagram (never errors, never panics).
async fn handle_gossip(socket: &UdpSocket, registry: &Registry, buf: &[u8], from: SocketAddr) {
    let message = match GossipMessage::decode(buf) {
        Ok(message) => message,
        Err(err) => {
            tracing::debug!(%from, len = buf.len(), %err, "ignoring malformed gossip");
            return;
        }
    };
    match message {
        GossipMessage::Digest(theirs) => {
            let snapshot = registry.snapshot();
            let ours = gossip::digest_of(&snapshot);
            // Pull: ask for what they hold newer than us.
            let want = gossip::diff(&ours, &theirs);
            if !want.is_empty() {
                send_best_effort(socket, &GossipMessage::Want(want), from).await;
            }
            // Push: send what the digest shows they are missing. A digest
            // never triggers a digest, so peers cannot storm each other.
            let they_want = gossip::diff(&theirs, &ours);
            if !they_want.is_empty() {
                let batch = gossip::batch_for(&snapshot, &they_want, from, MAX_GOSSIP_DATAGRAM);
                if !batch.is_empty() {
                    send_best_effort(socket, &GossipMessage::Batch(batch), from).await;
                }
            }
        }
        GossipMessage::Want(want) => {
            let batch = gossip::batch_for(&registry.snapshot(), &want, from, MAX_GOSSIP_DATAGRAM);
            if !batch.is_empty() {
                send_best_effort(socket, &GossipMessage::Batch(batch), from).await;
            }
        }
        GossipMessage::Batch(batch) => {
            for signed in batch.descriptors {
                let addr = signed.descriptor.addr;
                // Learned via `from`: TTL'd like any entry, never sent back.
                match registry.register_descriptor(signed, Some(from)) {
                    Ok(()) => tracing::debug!(%from, %addr, "gossip entry registered"),
                    Err(err) => tracing::debug!(%from, %addr, %err, "gossip entry rejected"),
                }
            }
        }
        GossipMessage::Announce(signed) => {
            let addr = signed.descriptor.addr;
            match registry.register_descriptor(*signed, None) {
                Ok(()) => tracing::debug!(%from, %addr, "signed announce registered"),
                Err(err) => tracing::debug!(%from, %addr, %err, "signed announce rejected"),
            }
        }
    }
}

/// Sends one gossip message, logging (not propagating) failures.
async fn send_best_effort(socket: &UdpSocket, message: &GossipMessage, to: SocketAddr) {
    if let Err(err) = socket.send_to(&message.encode(), to).await {
        tracing::debug!(%to, %err, "gossip send failed");
    }
}
