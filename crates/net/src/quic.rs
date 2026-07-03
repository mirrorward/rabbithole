//! QUIC transport (quinn): the primary RabbitHole transport.
//!
//! Session model (PLAN §5.1): one QUIC connection per session; the first
//! client-opened bidirectional stream is the **control stream** carrying
//! RHP frames. Later waves add server push uni-streams and per-transfer
//! bulk streams on the same connection.
//!
//! # Connection migration (mobile resilience)
//!
//! The client-side `QuicConnection` retains its [`quinn::Endpoint`] (rather
//! than letting it drop after the dial) so that
//! [`Connection::migrate`] can call
//! [`Endpoint::rebind`](quinn::Endpoint::rebind) with a fresh wildcard UDP
//! socket. quinn moves every connection on the endpoint onto the new socket
//! without a new handshake — QUIC connection IDs keep the session identified
//! across the local-address change — so a phone roaming WiFi↔cellular keeps
//! its live streams and in-flight frames. Contrast the WebSocket path, which
//! can't migrate and instead reconnects + `auth_resume`s (see the crate-level
//! docs). Server-accepted connections don't retain a per-connection endpoint
//! and report [`NetError::Unsupported`] from `migrate`.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rabbithole_proto::{version::ALPN, Frame, FrameCodec};

use crate::tls::{ensure_crypto_provider, PinnedCertVerifier, ServerAuth, TlsIdentity};
use crate::{
    BulkRecv, BulkSend, BulkStreams, Connection, Listener, NetError, PeerInfo, Transport,
    TransportKind,
};

/// A listening QUIC endpoint.
pub struct QuicListener {
    endpoint: quinn::Endpoint,
}

impl QuicListener {
    /// Bind a QUIC server on `addr` with the given TLS identity.
    pub fn bind(addr: SocketAddr, identity: &TlsIdentity) -> Result<Self, NetError> {
        ensure_crypto_provider();
        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![identity.cert_der.clone()], identity.clone_key().into())
            .map_err(|e| NetError::Tls(e.to_string()))?;
        server_crypto.alpn_protocols = vec![ALPN.to_vec()];

        let server_config = quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(server_crypto).map_err(|e| NetError::Tls(e.to_string()))?,
        ));
        let endpoint = quinn::Endpoint::server(server_config, addr)?;
        Ok(Self { endpoint })
    }
}

#[async_trait]
impl Listener for QuicListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, NetError> {
        loop {
            let incoming = self.endpoint.accept().await.ok_or(NetError::Closed)?;
            let connecting = match incoming.accept() {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("rejected incoming quic connection: {e}");
                    continue;
                }
            };
            // A failed handshake (e.g. a client pinning the wrong
            // fingerprint) dooms only this connection — skip it rather than
            // tearing down the whole accept loop for every other peer.
            let conn = match connecting.await {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("quic handshake failed: {e}");
                    continue;
                }
            };
            // The client opens the control stream; accept it here so the
            // returned Connection is immediately usable.
            let (send, recv) = match conn.accept_bi().await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::debug!("quic control stream not opened: {e}");
                    continue;
                }
            };
            let peer = PeerInfo {
                remote_addr: conn.remote_address(),
                transport: TransportKind::Quic,
            };
            return Ok(Box::new(QuicConnection {
                // Server side: the listening endpoint is shared across every
                // accepted connection, so it is not held here — a server
                // connection can't migrate its socket (and wouldn't want to;
                // it's the roaming client that moves). `migrate` reports
                // Unsupported for these.
                endpoint: None,
                conn,
                send,
                recv,
                codec: FrameCodec::new(),
                peer,
            }));
        }
    }

    fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.endpoint.local_addr()?)
    }
}

/// Client-side QUIC transport configuration.
pub struct QuicTransport {
    auth: ServerAuth,
    /// SNI/certificate name expected from the server.
    server_name: String,
}

impl QuicTransport {
    pub fn new(server_name: impl Into<String>, auth: ServerAuth) -> Self {
        ensure_crypto_provider();
        Self {
            auth,
            server_name: server_name.into(),
        }
    }

    fn client_config(&self) -> Result<quinn::ClientConfig, NetError> {
        let client_crypto = match &self.auth {
            ServerAuth::Pinned(fp) => rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PinnedCertVerifier::new(*fp)))
                .with_no_client_auth(),
            ServerAuth::WebPki => {
                // Wave 1: platform/Mozilla roots via rustls-native-certs or
                // webpki-roots alongside ACME server certs.
                return Err(NetError::Tls(
                    "WebPKI validation lands in Wave 1; use pinning".into(),
                ));
            }
        };
        let mut client_crypto = client_crypto;
        client_crypto.alpn_protocols = vec![ALPN.to_vec()];
        Ok(quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(client_crypto).map_err(|e| NetError::Tls(e.to_string()))?,
        )))
    }
}

#[async_trait]
impl Transport for QuicTransport {
    /// `endpoint` is `host:port` (must resolve to a socket address).
    async fn connect(&self, endpoint: &str) -> Result<Box<dyn Connection>, NetError> {
        let addr: SocketAddr = tokio::net::lookup_host(endpoint)
            .await?
            .next()
            .ok_or_else(|| NetError::Quic(format!("cannot resolve {endpoint}")))?;

        let bind: SocketAddr = wildcard_bind(addr.is_ipv4());
        let mut client = quinn::Endpoint::client(bind)?;
        client.set_default_client_config(self.client_config()?);

        let conn = client
            .connect(addr, &self.server_name)
            .map_err(|e| NetError::Quic(e.to_string()))?
            .await
            .map_err(|e| NetError::Quic(e.to_string()))?;

        // Open the control stream.
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| NetError::Quic(e.to_string()))?;
        let peer = PeerInfo {
            remote_addr: conn.remote_address(),
            transport: TransportKind::Quic,
        };
        // Retain the client endpoint so the connection can migrate its local
        // UDP socket later (mobile WiFi↔cellular). Without this the endpoint
        // would drop here — the live connection keeps it alive internally, but
        // it'd be unreachable, and `Endpoint::rebind` needs a handle to it.
        Ok(Box::new(QuicConnection {
            endpoint: Some(client),
            conn,
            send,
            recv,
            codec: FrameCodec::new(),
            peer,
        }))
    }
}

/// The wildcard bind address for a fresh client UDP socket: any local port on
/// any interface, matching the peer's address family. Used both for the
/// initial dial and for [`QuicConnection::migrate`]'s replacement socket.
fn wildcard_bind(ipv4: bool) -> SocketAddr {
    if ipv4 {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    }
}

struct QuicConnection {
    /// The client-side endpoint, retained so the connection can [`rebind`] its
    /// local UDP socket for migration. `None` on server-accepted connections
    /// (the listener owns the shared endpoint), which therefore can't migrate.
    ///
    /// [`rebind`]: quinn::Endpoint::rebind
    endpoint: Option<quinn::Endpoint>,
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    codec: FrameCodec,
    peer: PeerInfo,
}

/// Bulk-stream opener over a shared QUIC connection (Wave 4.2).
struct QuicBulk(quinn::Connection);

#[async_trait]
impl BulkStreams for QuicBulk {
    async fn open(&self) -> Result<(BulkSend, BulkRecv), NetError> {
        let (send, recv) = self
            .0
            .open_bi()
            .await
            .map_err(|e| NetError::Quic(e.to_string()))?;
        Ok((Box::new(send), Box::new(recv)))
    }

    async fn accept(&self) -> Result<(BulkSend, BulkRecv), NetError> {
        let (send, recv) = self
            .0
            .accept_bi()
            .await
            .map_err(|e| NetError::Quic(e.to_string()))?;
        Ok((Box::new(send), Box::new(recv)))
    }
}

#[async_trait]
impl Connection for QuicConnection {
    async fn send(&mut self, frame: Frame) -> Result<(), NetError> {
        let bytes = FrameCodec::encode(&frame)?;
        self.send
            .write_all(&bytes)
            .await
            .map_err(|e| NetError::Quic(e.to_string()))?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Frame>, NetError> {
        loop {
            if let Some(frame) = self.codec.next_frame()? {
                return Ok(Some(frame));
            }
            let mut buf = [0u8; 8192];
            match self.recv.read(&mut buf).await {
                Ok(Some(n)) => self.codec.feed(&buf[..n]),
                // Stream FIN: the peer finished cleanly.
                Ok(None) => return Ok(None),
                // Graceful connection close (code 0) is EOF, not an error.
                Err(quinn::ReadError::ConnectionLost(
                    quinn::ConnectionError::ApplicationClosed(ref close),
                )) if close.error_code == quinn::VarInt::from_u32(0) => return Ok(None),
                Err(e) => return Err(NetError::Quic(e.to_string())),
            }
        }
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }

    fn bulk(&self) -> Option<Box<dyn BulkStreams>> {
        // `quinn::Connection` is a cheap Arc handle; the opener runs
        // concurrently with the control loop (which keeps the original
        // control bi-stream). Extra bi-streams ride the same connection.
        Some(Box::new(QuicBulk(self.conn.clone())))
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        // The endpoint's bound address; after `migrate` this is the new
        // socket. `None` for server connections (no retained endpoint).
        self.endpoint.as_ref().and_then(|e| e.local_addr().ok())
    }

    fn migrate(&self) -> Result<(), NetError> {
        // Client only: server connections share the listener's endpoint and
        // report Unsupported (the trait default), so callers fall back.
        let endpoint = self
            .endpoint
            .as_ref()
            .ok_or(NetError::Unsupported("connection migration"))?;
        // Bind a brand-new wildcard UDP socket (fresh local port) in the same
        // address family, then hand it to the endpoint. `rebind` swaps every
        // connection on the endpoint onto the new socket in place: QUIC
        // connection IDs keep the session identified across the local-address
        // change, so no handshake and no re-auth occur — exactly what a phone
        // roaming WiFi↔cellular needs. Path validation happens transparently
        // on the next packet sent over the connection.
        let ipv4 = endpoint.local_addr()?.is_ipv4();
        let socket = std::net::UdpSocket::bind(wildcard_bind(ipv4))?;
        endpoint.rebind(socket)?;
        Ok(())
    }

    async fn close(&mut self) {
        // quinn's close() discards data still in flight, so first FIN the
        // control stream and wait for the peer to acknowledge it — this is
        // what keeps a final reply from being destroyed by our own close.
        let _ = self.send.finish();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.send.stopped()).await;
        self.conn.close(0u32.into(), b"");
    }
}
