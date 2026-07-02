//! The minimal telnet session shell: banner → login → MAIN MENU → quit.
//!
//! [`run_shell`] drives one connection start to finish: it opens option
//! negotiation, shows the banner and a login prompt (verified through the
//! pluggable [`TelnetAuth`] trait), then a deliberately tiny main menu with
//! a quit command. The full BBS surface — welcome art, who, boards, chat —
//! replaces the menu body in later Wave 6 slices; the frame stays.

use std::io;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::auth::TelnetAuth;
use crate::encoding::Encoding;
use crate::stream::{Echo, TelnetStream};

/// Tunables for one shell session.
#[derive(Debug, Clone)]
pub struct ShellOptions {
    /// Text shown on connect, before the login prompt.
    pub banner: String,
    /// Session character encoding.
    pub encoding: Encoding,
    /// Failed logins allowed before the connection is closed.
    pub max_attempts: u32,
}

impl Default for ShellOptions {
    fn default() -> ShellOptions {
        ShellOptions {
            banner: "\n*** RabbitHole BBS ***\nDown the rabbit hole we go.\n\n".into(),
            encoding: Encoding::Utf8,
            max_attempts: 3,
        }
    }
}

/// Run one telnet session over `io` to completion (quit, login failure
/// lockout, or disconnect). Burrow calls this once per accepted socket.
pub async fn run_shell<S>(io: S, auth: &dyn TelnetAuth, opts: &ShellOptions) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut t = TelnetStream::new(io);
    t.set_encoding(opts.encoding);
    t.start().await?;
    t.write_str(&opts.banner).await?;

    let Some(screen_name) = login(&mut t, auth, opts.max_attempts).await? else {
        return Ok(()); // disconnected or out of attempts
    };

    greet(&mut t, &screen_name).await?;
    menu_loop(&mut t, &screen_name).await
}

/// Prompt for credentials until success, disconnect, or `max_attempts`.
async fn login<S>(
    t: &mut TelnetStream<S>,
    auth: &dyn TelnetAuth,
    max_attempts: u32,
) -> io::Result<Option<String>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    for _ in 0..max_attempts {
        t.write_str("login: ").await?;
        let Some(user) = t.read_line(Echo::On).await? else {
            return Ok(None);
        };
        let user = user.trim().to_string();
        t.write_str("password: ").await?;
        let Some(pass) = t.read_line(Echo::Hidden).await? else {
            return Ok(None);
        };
        if user.is_empty() {
            t.write_str("Login incorrect.\n\n").await?;
            continue;
        }
        match auth.login(&user, &pass).await {
            Some(screen_name) => return Ok(Some(screen_name)),
            None => t.write_str("Login incorrect.\n\n").await?,
        }
    }
    t.write_str("Too many failures. Goodbye.\n").await?;
    Ok(None)
}

/// Post-login greeting, including what negotiation learned about the peer.
async fn greet<S>(t: &mut TelnetStream<S>, screen_name: &str) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut line = format!("\nWelcome, {screen_name}!");
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

/// The (deliberately tiny) main menu. Full BBS menus come later.
async fn menu_loop<S>(t: &mut TelnetStream<S>, screen_name: &str) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        t.write_str("\n=== MAIN MENU ===\n [Q] Quit\n\nCommand: ")
            .await?;
        let Some(cmd) = t.read_line(Echo::On).await? else {
            return Ok(()); // peer went away
        };
        match cmd.trim().to_ascii_lowercase().as_str() {
            "q" | "quit" | "g" | "goodbye" => {
                t.write_str(&format!("\nGoodbye, {screen_name}!\n")).await?;
                return Ok(());
            }
            "" => {}
            other => {
                t.write_str(&format!("\nUnknown command: {other}\n"))
                    .await?;
            }
        }
    }
}
