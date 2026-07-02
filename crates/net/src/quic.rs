//! QUIC transport (quinn): the primary RabbitHole transport.
//!
//! Session model (PLAN §5.1): one QUIC connection per session; the first
//! client-opened bidirectional stream is the **control stream** carrying
//! RHP frames. Later waves add server push uni-streams and per-transfer
//! bulk streams on the same connection.

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
            let conn = connecting
                .await
                .map_err(|e| NetError::Quic(e.to_string()))?;
            // The client opens the control stream; accept it here so the
            // returned Connection is immediately usable.
            let (send, recv) = conn
                .accept_bi()
                .await
                .map_err(|e| NetError::Quic(e.to_string()))?;
            let peer = PeerInfo {
                remote_addr: conn.remote_address(),
                transport: TransportKind::Quic,
            };
            return Ok(Box::new(QuicConnection {
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

        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
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
        Ok(Box::new(QuicConnection {
            conn,
            send,
            recv,
            codec: FrameCodec::new(),
            peer,
        }))
    }
}

struct QuicConnection {
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

    async fn close(&mut self) {
        // quinn's close() discards data still in flight, so first FIN the
        // control stream and wait for the peer to acknowledge it — this is
        // what keeps a final reply from being destroyed by our own close.
        let _ = self.send.finish();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.send.stopped()).await;
        self.conn.close(0u32.into(), b"");
    }
}
