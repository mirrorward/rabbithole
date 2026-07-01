//! RabbitHole transports.
//!
//! One trait pair — [`Transport`]/[`Listener`] yielding [`Connection`]s —
//! with two implementations:
//!
//! - **QUIC** ([`quic`]): the primary transport. TLS 1.3 baked in, stream
//!   multiplexing for later bulk-transfer waves, connection migration for
//!   mobile. ALPN `rhp/1`.
//! - **WebSocket** ([`ws`]): the mandatory fallback. Browsers/wasm can't
//!   speak raw QUIC, and some networks block UDP. One binary message = one
//!   frame.
//!
//! Certificates: servers generate a self-signed cert ([`tls`]) whose
//! blake3 fingerprint is pinned by clients (fingerprints travel in rabbit
//! links, Looking Glass listings, and `.well-known`). ACME/Let's Encrypt
//! wiring is Wave 1 (`rustls-acme`).

#![forbid(unsafe_code)]

pub mod quic;
pub mod tls;
pub mod ws;

use async_trait::async_trait;
use rabbithole_proto::Frame;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("quic: {0}")]
    Quic(String),
    #[error("websocket: {0}")]
    Ws(String),
    #[error("protocol: {0}")]
    Proto(#[from] rabbithole_proto::ProtoError),
    #[error("connection closed")]
    Closed,
}

/// Metadata about the remote end of a connection.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub remote_addr: std::net::SocketAddr,
    /// Which transport carried this connection (for presence display and
    /// per-surface policy).
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Quic,
    WebSocket,
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportKind::Quic => f.write_str("quic"),
            TransportKind::WebSocket => f.write_str("websocket"),
        }
    }
}

/// A live control channel to a peer: ordered, reliable frames both ways.
#[async_trait]
pub trait Connection: Send {
    /// Send one frame. Frames are delivered in order.
    async fn send(&mut self, frame: Frame) -> Result<(), NetError>;

    /// Receive the next frame. `Ok(None)` = clean close by the peer.
    async fn recv(&mut self) -> Result<Option<Frame>, NetError>;

    fn peer(&self) -> &PeerInfo;

    /// Close gracefully.
    async fn close(&mut self);
}

/// Client side: dial a server.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn connect(&self, endpoint: &str) -> Result<Box<dyn Connection>, NetError>;
}

/// Server side: accept connections.
#[async_trait]
pub trait Listener: Send {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, NetError>;

    fn local_addr(&self) -> Result<std::net::SocketAddr, NetError>;
}
