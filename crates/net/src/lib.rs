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

/// The write half of a dedicated bulk-transfer stream.
pub type BulkSend = Box<dyn tokio::io::AsyncWrite + Send + Unpin>;
/// The read half of a dedicated bulk-transfer stream.
pub type BulkRecv = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

/// Opens additional streams on a live connection for bulk transfers
/// (Wave 4.2), independent of the control channel. Only multiplexing
/// transports (QUIC) offer this; a `None` from [`Connection::bulk`] means the
/// caller must fall back to windowed control-frame chunks (the WebSocket
/// path). The handle is cheap to clone/hold ('static) so transfers run
/// concurrently with the control loop.
#[async_trait]
pub trait BulkStreams: Send + Sync {
    /// Client side: open a fresh bidirectional stream to the peer.
    async fn open(&self) -> Result<(BulkSend, BulkRecv), NetError>;

    /// Server side: accept the next peer-opened bidirectional stream.
    async fn accept(&self) -> Result<(BulkSend, BulkRecv), NetError>;
}

/// A live control channel to a peer: ordered, reliable frames both ways.
#[async_trait]
pub trait Connection: Send {
    /// Send one frame. Frames are delivered in order.
    async fn send(&mut self, frame: Frame) -> Result<(), NetError>;

    /// Receive the next frame. `Ok(None)` = clean close by the peer.
    async fn recv(&mut self) -> Result<Option<Frame>, NetError>;

    fn peer(&self) -> &PeerInfo;

    /// A handle for opening dedicated bulk-transfer streams, if this
    /// transport multiplexes (QUIC). Defaults to `None` — WebSocket and any
    /// single-stream transport transfer over control-frame chunks instead.
    fn bulk(&self) -> Option<Box<dyn BulkStreams>> {
        None
    }

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

/// Write a length-prefixed (`u32` big-endian) message to a bulk stream — the
/// framing for the [`crate::BulkStreams`] preamble and any framed bulk
/// payloads (Wave 4.2).
pub async fn write_framed<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    bytes: &[u8],
) -> Result<(), NetError> {
    use tokio::io::AsyncWriteExt;
    w.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    w.write_all(bytes).await?;
    Ok(())
}

/// Read a length-prefixed message written by [`write_framed`]. `max` bounds
/// the accepted length (a hostile peer can't force a huge allocation).
pub async fn read_framed<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    max: usize,
) -> Result<Vec<u8>, NetError> {
    use tokio::io::AsyncReadExt;
    let mut len_be = [0u8; 4];
    r.read_exact(&mut len_be).await?;
    let len = u32::from_be_bytes(len_be) as usize;
    if len > max {
        return Err(NetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "framed message exceeds maximum length",
        )));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}
