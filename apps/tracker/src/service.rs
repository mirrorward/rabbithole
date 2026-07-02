//! The async listeners that glue sockets to the [`Registry`].
//!
//! Three tiny services, each an infinite accept loop over a pre-bound socket
//! (binding is the caller's job so tests can use ephemeral `127.0.0.1:0`
//! ports and read back the real address):
//!
//! - [`run_registration_udp`] — HTRK heartbeats in, registry updates out.
//! - [`run_listing_tcp`] — HTRK hello in, the current server list out.
//! - [`run_status_tcp`] — native placeholder: `LIST\n` in, one tab-separated
//!   line per live server out (until the RHP tracker family lands).
//!
//! Malformed input never takes a listener down: bad datagrams are logged and
//! dropped, bad TCP sessions are logged and closed.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::htrk;
use crate::registry::{Registry, ServerEntry};

/// Largest registration datagram we accept: prefix + two maximal pascal
/// strings, with headroom for trailing junk from sloppy senders.
const MAX_REGISTRATION_DATAGRAM: usize = 2048;

/// Runs the HTRK registration listener: one UDP datagram per heartbeat.
///
/// The registry entry is keyed by the **observed source IP** plus the port
/// declared inside the packet (the source port is ephemeral and meaningless).
pub async fn run_registration_udp(socket: UdpSocket, registry: Arc<Registry>) -> Result<()> {
    let mut buf = vec![0u8; MAX_REGISTRATION_DATAGRAM];
    loop {
        let (len, from) = socket.recv_from(&mut buf).await?;
        match htrk::Registration::decode(&buf[..len]) {
            Ok(reg) => {
                let entry = ServerEntry {
                    name: reg.name,
                    description: reg.description,
                    addr: (from.ip(), reg.port).into(),
                    users_online: reg.users_online,
                    last_heartbeat: Instant::now(),
                };
                tracing::debug!(%from, addr = %entry.addr, name = %entry.name, "heartbeat");
                registry.register(entry);
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
/// Protocol: the client sends `LIST\n`; the tracker answers one line per
/// live server — `name<TAB>ip:port<TAB>users<TAB>description` — and closes.
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
    if line.trim() != "LIST" {
        write.write_all(b"ERR unknown command\n").await?;
        return Ok(());
    }
    let mut out = String::new();
    for entry in registry.snapshot() {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            sanitize(&entry.name),
            entry.addr,
            entry.users_online,
            sanitize(&entry.description),
        ));
    }
    write.write_all(out.as_bytes()).await?;
    write.shutdown().await?;
    Ok(())
}

/// Keeps registrant-supplied text from breaking the line/tab framing.
fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}
