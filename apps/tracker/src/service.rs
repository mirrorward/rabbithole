//! The async listeners that glue sockets to the [`Registry`].
//!
//! Four tiny services, each an infinite loop over a pre-bound socket
//! (binding is the caller's job so tests can use ephemeral `127.0.0.1:0`
//! ports and read back the real address):
//!
//! - [`run_registration_udp`] — HTRK heartbeats in, registry updates out.
//! - [`run_listing_tcp`] — HTRK hello in, the current server list out.
//! - [`run_status_tcp`] — native placeholder: `LIST`, `CATEGORIES`, the
//!   `INDEX` directory index, and per-server `HEALTH` in; tab-separated
//!   lines out (until the RHP tracker family lands).
//! - [`run_gossip_udp`] — signed announces from servers plus digest/want/
//!   batch exchanges with peer trackers (see [`crate::gossip`]).
//!
//! Malformed input never takes a listener down: bad datagrams are logged and
//! dropped, bad TCP sessions are logged and closed.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::gossip::{self, GossipMessage, MAX_GOSSIP_DATAGRAM};
use crate::health;
use crate::htrk;
use crate::registry::{IndexRow, Registry, ServerEntry};

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
/// - `INDEX` (and `INDEX cat=<name>`) — the directory index, one line per
///   live server, sorted signed-first, then observed uptime descending,
///   then name:
///   `name<TAB>ip:port<TAB>users<TAB>categories<TAB>uptime_24h<TAB>last_seen_secs<TAB>signed<TAB>key<TAB>gen`
///   where `uptime_24h` is a percent (this tracker's **local observation**,
///   never a signed claim — see [`crate::health`]), `signed` is `yes`/`no`,
///   `key` is the first 8 bytes of the verified server key in hex (`-` when
///   unsigned), and `gen` is the signed descriptor's generation/attestation
///   timestamp (unix ms; the descriptor's `timestamp` doubles as both — see
///   [`crate::descriptor`]). `key` + `gen` let a client fetch the full
///   signed descriptor (e.g. a gossip `Want` to this tracker) and verify it
///   offline instead of trusting the line.
/// - `HEALTH <ip:port>` — one slot's detail plus a bucket sparkline:
///   `ip:port<TAB>live=…<TAB>uptime_24h=…<TAB>first_seen_secs=…<TAB>last_seen_secs=…<TAB>flaps=…`
///   then a line of `#`/`+`/`.` per 15-minute bucket, oldest first (see
///   [`crate::health`]). Works for recently-expired servers too.
///
/// Unknown commands, unparseable addresses, and unknown servers each get a
/// one-line `ERR …` reply — never a dropped connection, never a panic.
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
    let out = status_response(registry, line.trim());
    write.write_all(out.as_bytes()).await?;
    write.shutdown().await?;
    Ok(())
}

/// Computes the reply to one status-port command line. Total: every input —
/// including garbage — yields a reply, so this is directly unit-testable
/// without sockets.
fn status_response(registry: &Registry, command: &str) -> String {
    if command == "LIST" {
        list_lines(&registry.snapshot())
    } else if let Some(category) = command.strip_prefix("LIST cat=") {
        list_lines(&registry.snapshot_category(category.trim()))
    } else if command == "CATEGORIES" {
        let mut out = String::new();
        for (category, count) in registry.category_counts() {
            let _ = writeln!(out, "{}\t{}", sanitize(&category), count);
        }
        out
    } else if command == "INDEX" {
        index_lines(&registry.index())
    } else if let Some(category) = command.strip_prefix("INDEX cat=") {
        index_lines(&registry.index_category(category.trim()))
    } else if let Some(addr) = command.strip_prefix("HEALTH ") {
        health_lines(registry, addr.trim())
    } else {
        "ERR unknown command\n".to_owned()
    }
}

/// Formats `LIST` output: one tab-separated line per entry.
fn list_lines(entries: &[ServerEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            sanitize(&entry.name),
            entry.addr,
            entry.users_online,
            sanitize(&entry.description),
            sanitize(&categories_str(entry)),
        );
    }
    out
}

/// Formats `INDEX` output: one tab-separated line per row (see the
/// [`run_status_tcp`] protocol docs for the columns).
fn index_lines(rows: &[IndexRow]) -> String {
    let mut out = String::new();
    for row in rows {
        let entry = &row.entry;
        let (signed, key, generation) = match &entry.signed {
            Some(sd) => (
                "yes",
                key_prefix(&sd.descriptor.server_key),
                sd.descriptor.timestamp.to_string(),
            ),
            None => ("no", "-".to_owned(), "-".to_owned()),
        };
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            sanitize(&entry.name),
            entry.addr,
            entry.users_online,
            sanitize(&categories_str(entry)),
            health::format_permille(row.uptime_permille),
            row.last_seen_secs,
            signed,
            key,
            generation,
        );
    }
    out
}

/// Formats `HEALTH <ip:port>` output: a detail line plus the sparkline, or
/// a one-line `ERR …` for bad addresses / unknown servers (total, like
/// every parser here).
fn health_lines(registry: &Registry, arg: &str) -> String {
    let Ok(addr) = arg.parse::<SocketAddr>() else {
        return "ERR bad address\n".to_owned();
    };
    match registry.health_report(addr) {
        Some(report) => format!(
            "{}\tlive={}\tuptime_24h={}\tfirst_seen_secs={}\tlast_seen_secs={}\tflaps={}\n{}\n",
            report.addr,
            if report.live { "yes" } else { "no" },
            health::format_permille(report.uptime_permille),
            report.first_seen_secs,
            report.last_seen_secs,
            report.flap_count,
            report.sparkline,
        ),
        None => "ERR unknown server\n".to_owned(),
    }
}

/// Comma-joined category tags, `-` when the entry has none.
fn categories_str(entry: &ServerEntry) -> String {
    if entry.categories.is_empty() {
        "-".to_owned()
    } else {
        entry.categories.join(",")
    }
}

/// The first 8 bytes of a server key as lowercase hex — enough for a client
/// to match the index line against the descriptor it fetches.
fn key_prefix(key: &[u8; 32]) -> String {
    let mut out = String::with_capacity(16);
    for byte in &key[..8] {
        let _ = write!(out, "{byte:02x}");
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Descriptor;
    use crate::registry::DEFAULT_TTL;
    use rabbithole_identity::IdentityKey;
    use std::time::Instant;

    /// A registry with one signed and one unsigned entry, both observed at
    /// `now` (injected time — no sleeps anywhere).
    fn seeded_registry(now: Instant) -> (Registry, [u8; 32]) {
        let registry = Registry::new(DEFAULT_TTL);
        let key = IdentityKey::from_seed(&[7u8; 32]);
        let signed = Descriptor::new("Wonderland", ([10, 0, 0, 1], 5500).into())
            .with_description("Down the rabbit hole")
            .with_category("chat")
            .with_users(12)
            .with_timestamp(1_700_000_000_000)
            .sign(&key)
            .unwrap();
        registry.register_descriptor_at(signed, None, now).unwrap();
        registry.register_unsigned_at(
            ServerEntry::unsigned("Plain", "no key", ([10, 0, 0, 2], 5510).into(), 3),
            now,
        );
        (registry, key.public().0)
    }

    #[test]
    fn index_lines_carry_uptime_signature_and_generation() {
        let now = Instant::now();
        let (registry, key) = seeded_registry(now);
        let out = index_lines(&registry.index_at(now));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        // Signed sorts first; every line has exactly nine columns.
        let expected_prefix = key_prefix(&key);
        assert_eq!(
            lines[0],
            format!(
                "Wonderland\t10.0.0.1:5500\t12\tchat\t100.0\t0\tyes\t{expected_prefix}\t1700000000000"
            )
        );
        assert_eq!(lines[1], "Plain\t10.0.0.2:5510\t3\t-\t100.0\t0\tno\t-\t-");
        for line in lines {
            assert_eq!(line.split('\t').count(), 9);
        }
    }

    #[test]
    fn status_response_answers_index_health_and_garbage_totally() {
        let now = Instant::now();
        let (registry, _) = seeded_registry(now);

        // INDEX cat= filters (unsigned entries carry no tags).
        let chat = status_response(&registry, "INDEX cat=CHAT");
        assert_eq!(chat.lines().count(), 1);
        assert!(chat.starts_with("Wonderland\t"));
        assert_eq!(status_response(&registry, "INDEX cat=nope"), "");

        // HEALTH detail: one header line + one sparkline line.
        let health = status_response(&registry, "HEALTH 10.0.0.1:5500");
        let lines: Vec<&str> = health.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("10.0.0.1:5500\tlive=yes\tuptime_24h=100.0\t"));
        assert!(lines[0].ends_with("flaps=0"));
        assert_eq!(lines[1], "#");

        // Malformed input is answered, never dropped or panicked over.
        assert_eq!(
            status_response(&registry, "HEALTH not-an-address"),
            "ERR bad address\n"
        );
        assert_eq!(status_response(&registry, "HEALTH "), "ERR bad address\n");
        assert_eq!(
            status_response(&registry, "HEALTH 10.0.0.9:1"),
            "ERR unknown server\n"
        );
        assert_eq!(
            status_response(&registry, "HEALTH"),
            "ERR unknown command\n"
        );
        assert_eq!(status_response(&registry, ""), "ERR unknown command\n");
        assert_eq!(
            status_response(&registry, "FROLIC"),
            "ERR unknown command\n"
        );
    }

    #[test]
    fn key_prefix_is_eight_bytes_of_lowercase_hex() {
        let mut key = [0u8; 32];
        key[0] = 0xAB;
        key[7] = 0x01;
        key[8] = 0xFF; // beyond the prefix — never rendered
        assert_eq!(key_prefix(&key), "ab00000000000001");
    }
}
