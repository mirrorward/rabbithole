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

use anyhow::Result;
use rabbithole_legacy_finger::{FingerDirectory, FingerServer, Profile, WhoEntry};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_store_server::repo2::PersonasRepo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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

/// Bind + serve the finger surface. Returns the bound address and task handle.
pub async fn spawn_finger(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let dir: Arc<dyn FingerDirectory> = Arc::new(FingerDirAdapter { shared });
    let handle = tokio::spawn(async move {
        let _ = FingerServer::new(dir).serve(listener).await;
    });
    Ok((local, handle))
}
