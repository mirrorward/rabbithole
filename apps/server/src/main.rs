//! `burrow` — the RabbitHole server daemon.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rabbithole_server_core::ServerConfig;

#[derive(Parser)]
#[command(name = "burrow", version, about = "RabbitHole server", long_about = None)]
struct Cli {
    /// Path to burrow.toml (defaults next to --data-dir).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Data directory override (db, blobs, identity, ctl socket).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Enable the embedded HTTP surface (web SPA + /files downloads)
    /// without editing burrow.toml. Equivalent to `http_enabled = true`.
    #[arg(long)]
    http: bool,
    /// Bind address for the HTTP surface (implies --http).
    /// Overrides `http_addr` from the config (default 0.0.0.0:8080).
    #[arg(long, value_name = "ADDR")]
    http_addr: Option<std::net::SocketAddr>,
    /// Directory of built SPA assets to serve at `/` (implies --http).
    /// Overrides `http_web_root`; relative paths resolve under --data-dir.
    #[arg(long, value_name = "DIR")]
    web_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (default when no subcommand is given).
    Run,
    /// Talk to a running burrow through its local ctl socket.
    Ctl {
        /// e.g.: status | config-get | config-set | account-create | who |
        /// theme-status | theme-clear
        cmd: String,
        /// Positional args: config-get KEY, config-set KEY VALUE,
        /// account-create LOGIN PASSWORD [ROLE], backup DEST-DIR,
        /// backup-verify SNAPSHOT-DIR
        args: Vec<String>,
    },
    /// OFFLINE restore of a `ctl backup` snapshot into --data-dir.
    ///
    /// The server must be stopped first (a live ctl socket makes this
    /// refuse). Verifies the snapshot's MANIFEST.json hashes, moves the
    /// current data dir aside to `<data_dir>.pre-restore-<ts>`, and copies
    /// the snapshot into place. Flow: stop -> restore -> start.
    Restore {
        /// Snapshot directory created by `burrow ctl backup <dest-dir>`.
        snapshot_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let mut config = ServerConfig::load(cli.config.as_deref())?;
    if let Some(dir) = cli.data_dir {
        config.data_dir = dir;
    }
    // CLI overrides for the embedded HTTP surface, so the web SPA can be
    // turned on without editing burrow.toml: `burrow --http`, optionally
    // with `--http-addr` and `--web-root` (each of which implies --http).
    if cli.http || cli.http_addr.is_some() || cli.web_root.is_some() {
        config.http_enabled = true;
    }
    if let Some(addr) = cli.http_addr {
        config.http_addr = addr;
    }
    if let Some(root) = cli.web_root {
        config.http_web_root = root;
    }

    match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Run => run(config).await,
        Cmd::Ctl { cmd, args } => ctl_client(config, &cmd, &args).await,
        Cmd::Restore { snapshot_dir } => restore(config, &snapshot_dir),
    }
}

fn restore(config: ServerConfig, snapshot_dir: &std::path::Path) -> Result<()> {
    let outcome = burrow::backup::restore_offline(snapshot_dir, &config.data_dir)?;
    if let Some(aside) = &outcome.moved_aside {
        println!("previous data dir moved to {}", aside.display());
    }
    println!(
        "restored {} files ({} bytes) into {}; start the server to come back up",
        outcome.files,
        outcome.total_bytes,
        config.data_dir.display()
    );
    Ok(())
}

async fn run(config: ServerConfig) -> Result<()> {
    let burrow = burrow::Burrow::start(config).await?;
    tracing::info!("press Ctrl-C to shut down");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    burrow.shutdown().await;
    Ok(())
}

#[cfg(unix)]
async fn ctl_client(config: ServerConfig, cmd: &str, args: &[String]) -> Result<()> {
    use anyhow::Context;
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let request = match (cmd, args) {
        ("status", _) => json!({"cmd": "status"}),
        ("who", _) => json!({"cmd": "who"}),
        ("config-get", [key]) => json!({"cmd": "config-get", "key": key}),
        ("config-set", [key, value]) => json!({"cmd": "config-set", "key": key, "value": value}),
        ("account-create", [login, password]) => {
            json!({"cmd": "account-create", "login": login, "password": password})
        }
        ("account-create", [login, password, role]) => {
            json!({"cmd": "account-create", "login": login, "password": password, "role": role})
        }
        ("board-create", [slug, title]) => {
            json!({"cmd": "board-create", "slug": slug, "title": title})
        }
        ("board-create", [slug, title, description]) => {
            json!({"cmd": "board-create", "slug": slug, "title": title, "description": description})
        }
        ("board-post", [board, author, subject, body]) => {
            json!({"cmd": "board-post", "board": board, "author": author, "subject": subject, "body": body})
        }
        ("theme-status", _) => json!({"cmd": "theme-status"}),
        ("theme-clear", _) => json!({"cmd": "theme-clear"}),
        ("gateway-stats", _) => json!({"cmd": "gateway-stats"}),
        ("fed-catalogs", _) => json!({"cmd": "fed-catalogs"}),
        ("fed-search", terms) if !terms.is_empty() => {
            json!({"cmd": "fed-search", "terms": terms.join(" ")})
        }
        ("backup", [dest]) => json!({"cmd": "backup", "dest": dest}),
        ("backup-verify", [dir]) => json!({"cmd": "backup-verify", "path": dir}),
        // Always refused by the server with the offline procedure.
        ("restore", [dir]) => json!({"cmd": "restore", "path": dir}),
        _ => anyhow::bail!(
            "usage: burrow ctl <status|who|config-get KEY|config-set KEY VALUE|account-create LOGIN PASSWORD [ROLE]|theme-status|theme-clear|gateway-stats|fed-catalogs|fed-search TERMS…|backup DEST-DIR|backup-verify SNAPSHOT-DIR>"
        ),
    };

    let path = config.data_dir.join("ctl.sock");
    let stream = tokio::net::UnixStream::connect(&path)
        .await
        .with_context(|| format!("is burrow running? (no socket at {})", path.display()))?;
    let (read, mut write) = stream.into_split();
    write.write_all(format!("{request}\n").as_bytes()).await?;

    let mut lines = BufReader::new(read).lines();
    let line = lines.next_line().await?.context("no response")?;
    let response: Value = serde_json::from_str(&line)?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        println!("{}", serde_json::to_string_pretty(&response["data"])?);
        Ok(())
    } else {
        anyhow::bail!("{}", response["error"].as_str().unwrap_or("unknown error"));
    }
}

#[cfg(not(unix))]
async fn ctl_client(_config: ServerConfig, _cmd: &str, _args: &[String]) -> Result<()> {
    anyhow::bail!("burrow ctl is unix-only for now");
}
