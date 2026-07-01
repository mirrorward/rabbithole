//! `rabbit` — the RabbitHole command-line client.
//!
//! Wave 0 ships a single command, `hello`, which dials a Burrow over QUIC
//! (or WebSocket) and performs the RHP version/capability handshake — the
//! end-to-end proof of proto + net. The real command surface (login, chat,
//! boards, transfers) grows from Wave 1 on.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rabbithole_net::quic::QuicTransport;
use rabbithole_net::tls::{CertFingerprint, ServerAuth};
use rabbithole_net::ws::WsTransport;
use rabbithole_net::Transport;
use rabbithole_proto::{CapabilitySet, Frame, Hello, HelloAck, RequestId};

#[derive(Parser)]
#[command(name = "rabbit", version, about = "RabbitHole client", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Dial a server and perform the RHP hello handshake.
    Hello {
        /// host:port for QUIC, or a ws:// URL for WebSocket.
        endpoint: String,
        /// Server certificate fingerprint (hex) to pin (QUIC).
        #[arg(long)]
        fingerprint: Option<String>,
        /// TLS server name (QUIC), defaults to the endpoint host.
        #[arg(long)]
        server_name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Hello {
            endpoint,
            fingerprint,
            server_name,
        } => hello(&endpoint, fingerprint.as_deref(), server_name.as_deref()).await,
    }
}

async fn hello(endpoint: &str, fingerprint: Option<&str>, server_name: Option<&str>) -> Result<()> {
    let transport: Box<dyn Transport> = if endpoint.starts_with("ws://")
        || endpoint.starts_with("wss://")
    {
        Box::new(WsTransport)
    } else {
        let Some(fp_hex) = fingerprint else {
            bail!("QUIC needs --fingerprint <hex> (from the server's startup log or rabbit link)");
        };
        let fp = CertFingerprint::from_hex(fp_hex).context("malformed fingerprint hex")?;
        let name = server_name
            .map(str::to_owned)
            .unwrap_or_else(|| endpoint.split(':').next().unwrap_or("localhost").to_owned());
        Box::new(QuicTransport::new(name, ServerAuth::Pinned(fp)))
    };

    let mut conn = transport
        .connect(endpoint)
        .await
        .context("connect failed")?;

    let hello = Hello::new(
        "rabbit",
        env!("CARGO_PKG_VERSION"),
        CapabilitySet::default(),
    );
    conn.send(Frame::request(RequestId(1), &hello)?).await?;

    let Some(reply) = conn.recv().await? else {
        bail!("server closed before replying")
    };
    if let Some(code) = reply.error {
        bail!("server refused hello: {code:?}");
    }
    let ack: HelloAck = reply
        .decode::<HelloAck>()
        .context("reply was not a HelloAck")?
        .context("HelloAck failed to decode")?;

    println!(
        "connected to \"{}\" ({} {})",
        ack.server_name,
        ack.version,
        conn.peer().transport
    );
    println!("server software: {}", ack.server_version);
    println!("server identity key: {}", hex_short(&ack.server_key));
    conn.close().await;
    Ok(())
}

fn hex_short(bytes: &[u8; 32]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}…{}", &hex[..8], &hex[56..])
}
