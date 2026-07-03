//! Legacy-protocol listeners (Wave 6): the telnet BBS and finger surfaces,
//! adapted onto the same accounts/personas/presence the native server uses.
//!
//! The `rabbithole-legacy-telnet` / `rabbithole-legacy-finger` crates are
//! transport-only and depend on nothing server-side; the burrow-side telnet
//! shell (login, main menu, doors) lives in [`crate::telnet`], and finger's
//! pluggable `FingerDirectory` seam is bridged to [`Shared`] here. Both
//! listeners are opt-in via config (`telnet_enabled` / `finger_enabled`)
//! and off by default.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rabbithole_legacy_finger::{
    handle_query, to_wire, FingerDirectory, Profile, WhoEntry, MAX_QUERY_BYTES,
};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::Role;
use rabbithole_store_server::repo2::PersonasRepo;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::Shared;

/// Answers finger queries from live presence + persona profiles. Invisible
/// (Cheshire-mode) sessions are omitted from the who-list, mirroring the
/// native who query.
struct FingerDirAdapter {
    shared: Arc<Shared>,
}

#[async_trait::async_trait]
impl FingerDirectory for FingerDirAdapter {
    async fn who(&self) -> Vec<WhoEntry> {
        self.shared
            .presence
            .snapshot()
            .into_iter()
            .filter(|e| e.state != 3) // hide invisible
            .map(|e| WhoEntry {
                screen_name: e.screen_name,
                idle_secs: e.connected_at.elapsed().as_secs(),
                location: None,
            })
            .collect()
    }

    async fn lookup(&self, user: &str) -> Option<Profile> {
        let row = PersonasRepo(&self.shared.pool)
            .by_screen_name(user)
            .await
            .ok()
            .flatten()?;
        Some(Profile {
            screen_name: row.screen_name,
            real_name: None,
            location: row.location,
            interests: row.interests,
            quote: row.quote,
            pronouns: row.pronouns,
            plan: row.plan,
        })
    }
}

/// Bind + serve the telnet BBS. Returns the bound address (useful when the
/// config asked for port 0) and the accept-loop task handle.
pub async fn spawn_telnet(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, peer)) = listener.accept().await else {
                break;
            };
            // Over the per-IP connection budget: drop it on the floor.
            if !shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let shared = shared.clone();
            tokio::spawn(async move {
                let _ = crate::telnet::run_shell(&mut sock, &shared, Some(peer.ip())).await;
                // Windows-safe close discipline (see `hotline::serve_htxf`):
                // send FIN explicitly, then drain to the peer's FIN so
                // buffered farewell bytes are delivered rather than
                // discarded by an RST from a bare drop.
                let _ = sock.shutdown().await;
                let mut sink = [0u8; 1024];
                let drain = async {
                    while let Ok(n) = sock.read(&mut sink).await {
                        if n == 0 {
                            break;
                        }
                    }
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), drain).await;
            });
        }
    });
    Ok((local, handle))
}

/// Bind + serve the finger surface. Returns the bound address and task
/// handle.
///
/// The accept loop is burrow's own (rather than `FingerServer::serve`) so
/// each connection can consult the live config: finger has no login (RFC
/// 1288 is anonymous), so any `finger_min_role` above "guest" refuses every
/// query with a polite notice — checked per connection, applied live.
pub async fn spawn_finger(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let dir: Arc<dyn FingerDirectory> = Arc::new(FingerDirAdapter {
        shared: shared.clone(),
    });
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, peer)) = listener.accept().await else {
                break;
            };
            // Over the per-IP connection budget: drop it on the floor.
            if !shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let dir = dir.clone();
            let shared = shared.clone();
            tokio::spawn(async move {
                let restricted = Role::parse_min_role(&shared.config.read().finger_min_role)
                    .unwrap_or(Role::Guest)
                    > Role::Guest;
                if let Err(err) = serve_finger_conn(stream, dir.as_ref(), restricted).await {
                    tracing::debug!(%peer, %err, "finger connection error");
                }
            });
        }
    });
    Ok((local, handle))
}

/// One finger connection: read one capped query line, answer it (or refuse
/// the whole surface when restricted), close gracefully. Mirrors the
/// legacy-finger crate's own connection discipline (query cap, deadline,
/// [`to_wire`] sanitization).
async fn serve_finger_conn(
    mut stream: TcpStream,
    directory: &dyn FingerDirectory,
    restricted: bool,
) -> std::io::Result<()> {
    if restricted {
        // Anonymous protocol + a minimum above guest = the surface is
        // members-only, and finger has no way to prove membership.
        //
        // Read (and discard) the query line BEFORE refusing: closing a socket
        // with the client's query still unread in our receive buffer turns
        // the close into an RST on macOS/Windows, and the client sees
        // ConnectionReset instead of the refusal text. Bounded so a silent
        // client can't hold the connection open.
        let _ = tokio::time::timeout(Duration::from_secs(10), read_query_line(&mut stream)).await;
        stream
            .write_all(to_wire("This finger service is restricted. Ask your sysop.\n").as_bytes())
            .await?;
        stream.shutdown().await?;
        // Drain until the peer's FIN so the refusal is delivered before the
        // socket drops (the serve_htxf close discipline).
        let mut sink = [0u8; 256];
        let drain = async {
            loop {
                match stream.read(&mut sink).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), drain).await;
        return Ok(());
    }
    let query = tokio::time::timeout(Duration::from_secs(30), read_query_line(&mut stream)).await;
    let text = match query {
        Ok(Ok(Some(line))) => handle_query(directory, &line).await,
        Ok(Ok(None)) => "finger: query too long.\n".to_string(),
        Ok(Err(err)) => return Err(err),
        // Deadline passed without a full query line: just hang up.
        Err(_elapsed) => return Ok(()),
    };
    stream.write_all(to_wire(&text).as_bytes()).await?;
    stream.shutdown().await
}

/// Read one query line, capped at [`MAX_QUERY_BYTES`] (plus line ending);
/// `Ok(None)` when the client exceeded the cap without a newline.
async fn read_query_line(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut limited = BufReader::new(stream.take((MAX_QUERY_BYTES + 2) as u64));
    let mut buf = Vec::with_capacity(128);
    limited.read_until(b'\n', &mut buf).await?;
    if !buf.ends_with(b"\n") && buf.len() > MAX_QUERY_BYTES {
        return Ok(None);
    }
    while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}
