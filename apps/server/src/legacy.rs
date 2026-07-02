//! Legacy-protocol listeners (Wave 6): the telnet BBS and finger surfaces,
//! adapted onto the same accounts/personas/presence the native server uses.
//!
//! The `rabbithole-legacy-telnet` / `rabbithole-legacy-finger` crates are
//! transport-only and depend on nothing server-side; here we bridge their
//! pluggable seams (`TelnetAuth`, `FingerDirectory`) to [`Shared`] and spawn
//! their accept loops. Both are opt-in via config (`telnet_enabled` /
//! `finger_enabled`) and off by default.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use rabbithole_legacy_finger::{FingerDirectory, FingerServer, Profile, WhoEntry};
use rabbithole_legacy_telnet::{run_shell, Encoding, ShellOptions, TelnetAuth};
use rabbithole_store_server::repo2::PersonasRepo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::Shared;

/// Bridges the telnet login prompt to the account authenticator. TOTP-gated
/// accounts can't complete the minimal telnet prompt yet (no second-factor
/// step), so they're rejected here — a later slice adds the prompt.
struct TelnetAuthAdapter {
    shared: Arc<Shared>,
}

#[async_trait::async_trait]
impl TelnetAuth for TelnetAuthAdapter {
    async fn login(&self, username: &str, password: &str) -> Option<String> {
        match self
            .shared
            .auth
            .login_password(username, password, None)
            .await
        {
            Ok(user) => Some(user.persona.screen_name),
            Err(_) => None,
        }
    }
}

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
    let name = shared.config.read().name;
    let auth: Arc<dyn TelnetAuth> = Arc::new(TelnetAuthAdapter {
        shared: shared.clone(),
    });
    let opts = ShellOptions {
        banner: format!("\n*** {name} ***\nDown the rabbit hole we go.\n\n"),
        encoding: Encoding::Utf8,
        max_attempts: 3,
    };
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, _peer)) = listener.accept().await else {
                break;
            };
            let auth = auth.clone();
            let opts = opts.clone();
            tokio::spawn(async move {
                let _ = run_shell(sock, auth.as_ref(), &opts).await;
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
