//! `rabbit` — the RabbitHole command-line client.
//!
//! `rabbit login` establishes a session and caches it (endpoint, pinned
//! fingerprint, resume token) in the user's data dir; every other command
//! dials with the cached session, does its work, and exits. `--json` turns
//! output machine-readable for scripting.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rabbithole_core::Client;
use rabbithole_proto::chat::ChatMessage;
use rabbithole_proto::presence::{UserJoined, UserLeft};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "rabbit", version, about = "RabbitHole client", long_about = None)]
struct Cli {
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Dial a server and perform only the hello handshake (diagnostics).
    Hello {
        endpoint: String,
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        server_name: Option<String>,
    },
    /// Sign in and cache the session for the other commands.
    Login {
        /// host:port (QUIC) or ws:// URL (WebSocket).
        endpoint: String,
        /// Server cert fingerprint (hex) — required for QUIC.
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        server_name: Option<String>,
        /// Account login (omit for --guest).
        #[arg(long, conflicts_with = "guest")]
        user: Option<String>,
        /// Password (or set RABBIT_PASSWORD).
        #[arg(long)]
        password: Option<String>,
        /// Sign in as a guest.
        #[arg(long)]
        guest: bool,
        /// Guest display name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget the cached session.
    Logout,
    /// Show the cached session.
    Status,
    /// List who's online.
    Who,
    /// Say something in the lobby.
    Say { text: Vec<String> },
    /// Print recent lobby scrollback.
    History {
        #[arg(default_value_t = 25)]
        limit: u32,
    },
    /// Stream lobby chat and presence until interrupted.
    Tail,
}

/// The cached session (written by `login`).
#[derive(Debug, Serialize, Deserialize)]
struct Session {
    endpoint: String,
    server_name: Option<String>,
    fingerprint: Option<String>,
    /// Resume token; None = guest (re-login each invocation).
    token: Option<String>,
    guest_name: Option<String>,
    screen_name: String,
    replay_cursor: u64,
}

fn session_path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("no data dir on this platform")?
        .join("rabbithole");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("session.json"))
}

fn load_session() -> Result<Session> {
    let path = session_path()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no session — run `rabbit login` first ({})", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_session(s: &Session) -> Result<()> {
    std::fs::write(session_path()?, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

const CLIENT_NAME: &str = "rabbit";

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Hello {
            endpoint,
            fingerprint,
            server_name,
        } => {
            let mut c = Client::connect(
                &endpoint,
                server_name.as_deref(),
                fingerprint.as_deref(),
                CLIENT_NAME,
                env!("CARGO_PKG_VERSION"),
            )
            .await?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "server_name": c.server.server_name,
                        "server_version": c.server.server_version,
                        "server_key": hex::encode(c.server.server_key),
                    })
                );
            } else {
                println!("connected to \"{}\"", c.server.server_name);
                println!("server software: {}", c.server.server_version);
                println!("server identity key: {}", hex::encode(c.server.server_key));
            }
            c.close().await;
            Ok(())
        }
        Cmd::Login {
            endpoint,
            fingerprint,
            server_name,
            user,
            password,
            guest,
            name,
        } => {
            let mut c = Client::connect(
                &endpoint,
                server_name.as_deref(),
                fingerprint.as_deref(),
                CLIENT_NAME,
                env!("CARGO_PKG_VERSION"),
            )
            .await?;
            let ok = if guest {
                c.auth_guest(name.clone()).await?
            } else {
                let user = user.context("--user LOGIN (or --guest)")?;
                let password = password
                    .or_else(|| std::env::var("RABBIT_PASSWORD").ok())
                    .context("--password or RABBIT_PASSWORD")?;
                c.auth_password(&user, &password).await?
            };
            let welcome = c.expect_welcome().await?;

            let session = Session {
                endpoint,
                server_name,
                fingerprint,
                token: (!ok.token.is_empty()).then(|| ok.token.clone()),
                guest_name: guest
                    .then(|| name.unwrap_or_default())
                    .filter(|s| !s.is_empty()),
                screen_name: ok.screen_name.clone(),
                replay_cursor: c.replay_cursor,
            };
            save_session(&session)?;

            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "screen_name": ok.screen_name,
                        "server": c.server.server_name,
                        "resumable": session.token.is_some(),
                        "motd": welcome.motd,
                        "agreement_pending": welcome.agreement.is_some(),
                    })
                );
            } else {
                println!(
                    "signed in to \"{}\" as {}",
                    c.server.server_name, ok.screen_name
                );
                if !welcome.motd.is_empty() {
                    println!("\n{}\n", welcome.motd);
                }
                if welcome.agreement.is_some() {
                    println!("(this server has an agreement; commands will auto-accept — read it with `rabbit status`)");
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Logout => {
            let path = session_path()?;
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            if !cli.json {
                println!("logged out");
            }
            Ok(())
        }
        Cmd::Status => {
            let s = load_session()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!("endpoint:    {}", s.endpoint);
                println!("screen name: {}", s.screen_name);
                println!("resumable:   {}", s.token.is_some());
            }
            Ok(())
        }
        Cmd::Who => {
            let (mut c, _) = reconnect().await?;
            let users = c.who().await?;
            if cli.json {
                let rows: Vec<_> = users
                    .iter()
                    .map(|u| {
                        serde_json::json!({
                            "screen_name": u.screen_name,
                            "role": u.role,
                            "transport": u.transport,
                            "connected_secs": u.connected_secs,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else {
                println!("{} online:", users.len());
                for u in users {
                    println!(
                        "  {:24} {:10} {:>6}s",
                        u.screen_name, u.transport, u.connected_secs
                    );
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Say { text } => {
            let line = text.join(" ");
            if line.trim().is_empty() {
                bail!("nothing to say");
            }
            let (mut c, _) = reconnect().await?;
            c.chat_send("lobby", &line).await?;
            c.close().await;
            persist_cursor(&c);
            Ok(())
        }
        Cmd::History { limit } => {
            let (mut c, _) = reconnect().await?;
            let messages = c.chat_history("lobby", limit).await?;
            if cli.json {
                let rows: Vec<_> = messages
                    .iter()
                    .map(
                        |m| serde_json::json!({"from": m.from, "text": m.text, "at": m.at_unix_ms}),
                    )
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else {
                for m in messages {
                    println!("<{}> {}", m.from, m.text);
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Tail => {
            let (mut c, _) = reconnect().await?;
            eprintln!("tailing lobby — Ctrl-C to stop");
            loop {
                tokio::select! {
                    push = c.next_push() => {
                        let Some(frame) = push? else { break };
                        if let Some(Ok(m)) = frame.decode::<ChatMessage>() {
                            if cli.json {
                                println!("{}", serde_json::json!({"type": "chat", "from": m.from, "text": m.text, "at": m.at_unix_ms}));
                            } else {
                                println!("<{}> {}", m.from, m.text);
                            }
                        } else if let Some(Ok(j)) = frame.decode::<UserJoined>() {
                            if !cli.json {
                                println!("* {} joined", j.user.screen_name);
                            }
                        } else if let Some(Ok(l)) = frame.decode::<UserLeft>() {
                            if !cli.json && !l.screen_name.is_empty() {
                                println!("* {} left", l.screen_name);
                            }
                        }
                    }
                    _ = tokio::signal::ctrl_c() => break,
                }
            }
            persist_cursor(&c);
            c.close().await;
            Ok(())
        }
    }
}

/// Re-establish a session from the cache: token resume for accounts,
/// fresh guest sign-in for guests. Auto-accepts a pending agreement
/// (the login command surfaced it to the human).
async fn reconnect() -> Result<(Client, Session)> {
    let mut s = load_session()?;
    let mut c = Client::connect(
        &s.endpoint,
        s.server_name.as_deref(),
        s.fingerprint.as_deref(),
        CLIENT_NAME,
        env!("CARGO_PKG_VERSION"),
    )
    .await?;
    let ok = match &s.token {
        Some(token) => c.auth_resume(token, s.replay_cursor).await?,
        None => c.auth_guest(s.guest_name.clone()).await?,
    };
    s.screen_name = ok.screen_name.clone();
    let welcome = c.expect_welcome().await?;
    if welcome.agreement.is_some() {
        c.agreement_accept().await?;
    }
    Ok((c, s))
}

/// Best-effort: remember the replay cursor for the next resume.
fn persist_cursor(c: &Client) {
    if let Ok(mut s) = load_session() {
        if s.replay_cursor < c.replay_cursor {
            s.replay_cursor = c.replay_cursor;
            let _ = save_session(&s);
        }
    }
}
