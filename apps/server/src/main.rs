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
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (default when no subcommand is given).
    Run,
    /// Talk to a running burrow through its local ctl socket.
    Ctl {
        /// e.g.: status | config-get | config-set | account-create | who
        cmd: String,
        /// Positional args: config-get KEY, config-set KEY VALUE,
        /// account-create LOGIN PASSWORD [ROLE]
        args: Vec<String>,
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

    match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Run => run(config).await,
        Cmd::Ctl { cmd, args } => ctl_client(config, &cmd, &args).await,
    }
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
        _ => anyhow::bail!(
            "usage: burrow ctl <status|who|config-get KEY|config-set KEY VALUE|account-create LOGIN PASSWORD [ROLE]>"
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
