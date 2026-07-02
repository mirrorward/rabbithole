//! Burrow's telnet BBS shell: banner → login → MAIN MENU (doors, quit).
//!
//! The `rabbithole-legacy-telnet` crate owns the protocol layer
//! ([`TelnetStream`]: negotiation, line IO, encodings) and keeps a minimal
//! reference shell of its own; this module is the burrow-side shell — it
//! authenticates against the real [`AuthService`](rabbithole_server_core::AuthService)
//! (yielding a full [`AuthedUser`] with a permission [`Subject`], which the
//! trait-shaped `TelnetAuth` seam cannot carry) and hosts the door-game
//! commands (`doors` to list, `door <id>` to play — see [`crate::doors`]).

use std::io;
use std::net::IpAddr;
use std::sync::Arc;

use rabbithole_legacy_telnet::{Echo, Encoding, TelnetStream};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::AuthedUser;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{doors, Shared};

/// Failed logins allowed before the connection is closed.
const MAX_ATTEMPTS: u32 = 3;

/// Run one telnet session over `io` to completion (quit, login lockout, or
/// disconnect). Burrow calls this once per accepted socket; the caller keeps
/// the socket and performs the graceful FIN + drain close afterwards.
/// `peer_ip` keys the per-IP auth/legacy rate buckets (`None` — e.g. a test
/// harness driving an in-memory stream — is unlimited).
pub async fn run_shell<S>(io: S, shared: &Arc<Shared>, peer_ip: Option<IpAddr>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut t = TelnetStream::new(io);
    t.set_encoding(Encoding::Utf8);
    t.start().await?;
    let name = shared.config.read().name;
    t.write_str(&format!(
        "\n*** {name} ***\nDown the rabbit hole we go.\n\n"
    ))
    .await?;

    let Some(authed) = login(&mut t, shared, peer_ip).await? else {
        return Ok(()); // disconnected or out of attempts
    };
    greet(&mut t, &authed).await?;
    menu_loop(&mut t, shared, &authed, peer_ip).await
}

/// Prompt for credentials until success, disconnect, or [`MAX_ATTEMPTS`].
/// TOTP-gated accounts can't complete the minimal prompt yet (no
/// second-factor step), so they fail here like a bad password. Failed
/// attempts also drain the per-IP `auth` rate bucket; an empty bucket ends
/// the session before (or right after) an attempt.
async fn login<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> io::Result<Option<AuthedUser>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    for _ in 0..MAX_ATTEMPTS {
        if let Some(ip) = peer_ip {
            if !shared.rate_probe(Scope::Ip(ip), rl::AUTH) {
                t.write_str("Too many failed logins. Try again later.\n")
                    .await?;
                return Ok(None);
            }
        }
        t.write_str("login: ").await?;
        let Some(user) = t.read_line(Echo::On).await? else {
            return Ok(None);
        };
        let user = user.trim().to_string();
        t.write_str("password: ").await?;
        let Some(pass) = t.read_line(Echo::Hidden).await? else {
            return Ok(None);
        };
        if !user.is_empty() {
            if let Ok(authed) = shared.auth.login_password(&user, &pass, None).await {
                return Ok(Some(authed));
            }
        }
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::AUTH) {
                t.write_str("Too many failed logins. Try again later.\n")
                    .await?;
                return Ok(None);
            }
        }
        t.write_str("Login incorrect.\n\n").await?;
    }
    t.write_str("Too many failures. Goodbye.\n").await?;
    Ok(None)
}

/// Post-login greeting, including what negotiation learned about the peer.
async fn greet<S>(t: &mut TelnetStream<S>, authed: &AuthedUser) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut line = format!("\nWelcome, {}!", authed.persona.screen_name);
    let mut details = Vec::new();
    if let Some(term) = t.terminal() {
        details.push(term.to_string());
    }
    if let Some((cols, rows)) = t.window() {
        details.push(format!("{cols}x{rows}"));
    }
    if !details.is_empty() {
        line.push_str(&format!(" [{}]", details.join(", ")));
    }
    line.push('\n');
    t.write_str(&line).await
}

/// The main menu. `[D]` appears only when door hosting is switched on, but
/// the commands always answer (with a polite refusal when disabled).
async fn menu_loop<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let mut menu = String::from("\n=== MAIN MENU ===\n");
        if shared.doors.enabled() {
            menu.push_str(" [D] Doors\n");
        }
        menu.push_str(" [Q] Quit\n\nCommand: ");
        t.write_str(&menu).await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(()); // peer went away
        };
        // Coarse per-IP legacy command budget: refuse the command, keep the
        // session.
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        // Lowercase only the verb — door ids are matched exactly.
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or("").to_ascii_lowercase();
        let arg = words.next();
        match (verb.as_str(), arg) {
            ("q" | "quit" | "g" | "goodbye", _) => {
                t.write_str(&format!("\nGoodbye, {}!\n", authed.persona.screen_name))
                    .await?;
                return Ok(());
            }
            ("", _) => {}
            ("d" | "doors", None) => list_doors(t, shared).await?,
            ("d" | "door" | "doors" | "open", Some(id)) => {
                doors::run_door(t, shared, authed, id).await?;
            }
            (other, _) => {
                t.write_str(&format!("\nUnknown command: {other}\n"))
                    .await?;
            }
        }
    }
}

/// Print the door menu (insertion order = the sysop's `[[doors]]` order).
async fn list_doors<S>(t: &mut TelnetStream<S>, shared: &Arc<Shared>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !shared.doors.enabled() {
        return t
            .write_str("\nDoors are not enabled on this system.\n")
            .await;
    }
    let list = shared.doors.list();
    if list.is_empty() {
        return t.write_str("\nNo doors are installed.\n").await;
    }
    let mut out = String::from("\n--- Door Games ---\n");
    for d in list {
        out.push_str(&format!("  {:<12} {}\n", d.id, d.title));
    }
    out.push_str("\nType `door <id>` to play.\n");
    t.write_str(&out).await
}
