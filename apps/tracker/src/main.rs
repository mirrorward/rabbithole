//! `looking-glass` — the RabbitHole tracker/directory daemon.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use looking_glass::{service, Registry};
use tokio::net::{TcpListener, UdpSocket};

#[derive(Parser)]
#[command(name = "looking-glass", version, about = "RabbitHole tracker/directory", long_about = None)]
struct Cli {
    /// HTRK listing listener (classic Hotline tracker clients, TCP).
    #[arg(long, default_value = "0.0.0.0:5498")]
    htrk_tcp: SocketAddr,
    /// HTRK registration listener (server heartbeats, UDP).
    #[arg(long, default_value = "0.0.0.0:5499")]
    htrk_udp: SocketAddr,
    /// Plain-text status listener (native placeholder, TCP).
    #[arg(long, default_value = "0.0.0.0:4655")]
    status: SocketAddr,
    /// Seconds a registration stays listed without a fresh heartbeat.
    #[arg(long, default_value_t = 360)]
    ttl: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let registry = Arc::new(Registry::new(Duration::from_secs(cli.ttl)));

    let udp = UdpSocket::bind(cli.htrk_udp)
        .await
        .with_context(|| format!("binding HTRK registration UDP {}", cli.htrk_udp))?;
    let listing = TcpListener::bind(cli.htrk_tcp)
        .await
        .with_context(|| format!("binding HTRK listing TCP {}", cli.htrk_tcp))?;
    let status = TcpListener::bind(cli.status)
        .await
        .with_context(|| format!("binding status TCP {}", cli.status))?;

    tracing::info!(addr = %cli.htrk_udp, "HTRK registration (UDP) listening");
    tracing::info!(addr = %cli.htrk_tcp, "HTRK listing (TCP) listening");
    tracing::info!(addr = %cli.status, "status (TCP) listening");
    tracing::info!(ttl_secs = cli.ttl, "registration TTL");

    let mut registration = tokio::spawn(service::run_registration_udp(udp, registry.clone()));
    let mut listing = tokio::spawn(service::run_listing_tcp(listing, registry.clone()));
    let mut status = tokio::spawn(service::run_status_tcp(status, registry));

    tracing::info!("press Ctrl-C to shut down");
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for Ctrl-C")?;
            tracing::info!("shutting down");
        }
        result = &mut registration => {
            result.context("registration listener panicked")??;
        }
        result = &mut listing => {
            result.context("listing listener panicked")??;
        }
        result = &mut status => {
            result.context("status listener panicked")??;
        }
    }

    registration.abort();
    listing.abort();
    status.abort();
    Ok(())
}
