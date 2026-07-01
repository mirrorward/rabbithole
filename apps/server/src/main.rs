//! `burrow` — the RabbitHole server daemon.
//!
//! Wave 0: binds the QUIC (4653) and WebSocket (4654) listeners with a
//! fresh self-signed TLS identity, answers RHP hello handshakes, and logs
//! sessions to the event bus. Auth, presence, chat, and persistence arrive
//! in Wave 1; until then every hello is greeted and the connection closed
//! when the client goes away.

use std::net::SocketAddr;

use anyhow::Result;
use clap::Parser;
use rabbithole_net::quic::QuicListener;
use rabbithole_net::tls::TlsIdentity;
use rabbithole_net::ws::WsListener;
use rabbithole_net::{Connection, Listener};
use rabbithole_proto::version::MIN_SUPPORTED_VERSION;
use rabbithole_proto::{
    CapabilitySet, ErrorCode, Frame, FrameKind, Hello, HelloAck, ProtocolVersion, PROTOCOL_VERSION,
};
use rabbithole_server_core::{EventBus, ServerEvent};

#[derive(Parser)]
#[command(name = "burrow", version, about = "RabbitHole server", long_about = None)]
struct Args {
    /// QUIC listener address.
    #[arg(long, default_value = "0.0.0.0:4653")]
    quic: SocketAddr,
    /// WebSocket listener address.
    #[arg(long, default_value = "0.0.0.0:4654")]
    ws: SocketAddr,
    /// Server display name.
    #[arg(long, default_value = "An Unnamed Burrow")]
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let identity = TlsIdentity::self_signed(&["localhost".into()])?;
    tracing::info!(
        fingerprint = identity.fingerprint().to_hex(),
        "generated self-signed TLS identity (persistent identity lands in Wave 1)"
    );

    let bus = EventBus::default();

    let quic = QuicListener::bind(args.quic, &identity)?;
    tracing::info!(addr = %quic.local_addr()?, "quic listener up");
    let ws = WsListener::bind(args.ws).await?;
    tracing::info!(addr = %ws.local_addr()?, "websocket listener up");

    tokio::try_join!(
        serve(Box::new(quic), args.name.clone(), bus.clone()),
        serve(Box::new(ws), args.name.clone(), bus.clone()),
    )?;
    Ok(())
}

async fn serve(mut listener: Box<dyn Listener>, server_name: String, bus: EventBus) -> Result<()> {
    let mut next_session: u64 = 1;
    loop {
        match listener.accept().await {
            Ok(conn) => {
                let session_id = next_session;
                next_session += 1;
                let name = server_name.clone();
                let bus = bus.clone();
                tokio::spawn(async move {
                    if let Err(e) = session(conn, session_id, &name, &bus).await {
                        tracing::debug!(session_id, "session ended with error: {e}");
                    }
                    bus.publish(ServerEvent::SessionClosed { session_id });
                });
            }
            Err(e) => {
                tracing::warn!("accept failed: {e}");
            }
        }
    }
}

async fn session(
    mut conn: Box<dyn Connection>,
    session_id: u64,
    server_name: &str,
    bus: &EventBus,
) -> Result<()> {
    let peer = conn.peer().clone();
    tracing::info!(session_id, remote = %peer.remote_addr, transport = %peer.transport, "connection");

    while let Some(frame) = conn.recv().await? {
        if frame.kind != FrameKind::Request {
            continue;
        }
        if let Some(hello) = frame.decode::<Hello>() {
            let hello = match hello {
                Ok(h) => h,
                Err(_) => {
                    conn.send(Frame::error_reply(&frame, ErrorCode::BadRequest))
                        .await?;
                    continue;
                }
            };
            let Some(version) = ProtocolVersion::negotiate(PROTOCOL_VERSION, hello.version) else {
                tracing::info!(session_id, theirs = %hello.version, min = %MIN_SUPPORTED_VERSION, "version mismatch");
                conn.send(Frame::error_reply(&frame, ErrorCode::VersionMismatch))
                    .await?;
                continue;
            };
            // Wave 1: the zero key becomes the persistent Ed25519 server identity key.
            let ack = HelloAck::new(
                version,
                CapabilitySet::default(),
                server_name,
                env!("CARGO_PKG_VERSION"),
                [0u8; 32],
            );
            conn.send(Frame::reply_to(&frame, &ack)?).await?;
            tracing::info!(session_id, client = %hello.client_name, version = %version, "hello");
            bus.publish(ServerEvent::SessionOpened {
                session_id,
                screen_name: format!("{}@{}", hello.client_name, peer.remote_addr),
            });
        } else {
            // Unknown message type: tolerated, answered, never fatal.
            conn.send(Frame::error_reply(&frame, ErrorCode::Unsupported))
                .await?;
        }
    }
    Ok(())
}
