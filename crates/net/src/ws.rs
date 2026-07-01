//! WebSocket transport: the mandatory fallback.
//!
//! One binary WebSocket message = one RHP frame (no length prefix — the
//! message boundary is the frame boundary). Wave 0 carries `ws://` for
//! loopback and development; in Wave 1 this rides the server's HTTPS
//! endpoint (`wss://…/rhp`) behind axum, sharing the web port.

use std::net::SocketAddr;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rabbithole_proto::{decode_frame, encode_frame, Frame};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::{Connection, Listener, NetError, PeerInfo, Transport, TransportKind};

/// A listening WebSocket endpoint (plain TCP; TLS termination is the web
/// layer's job in Wave 1).
pub struct WsListener {
    listener: TcpListener,
}

impl WsListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self, NetError> {
        Ok(Self {
            listener: TcpListener::bind(addr).await?,
        })
    }
}

#[async_trait]
impl Listener for WsListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, NetError> {
        let (stream, remote_addr) = self.listener.accept().await?;
        let ws = tokio_tungstenite::accept_async(stream)
            .await
            .map_err(|e| NetError::Ws(e.to_string()))?;
        Ok(Box::new(WsConnection {
            ws,
            peer: PeerInfo {
                remote_addr,
                transport: TransportKind::WebSocket,
            },
        }))
    }

    fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.listener.local_addr()?)
    }
}

/// Client-side WebSocket transport.
#[derive(Default)]
pub struct WsTransport;

#[async_trait]
impl Transport for WsTransport {
    /// `endpoint` is a ws/wss URL, e.g. `ws://host:4654/rhp`.
    async fn connect(&self, endpoint: &str) -> Result<Box<dyn Connection>, NetError> {
        let (ws, _resp) = tokio_tungstenite::connect_async(endpoint)
            .await
            .map_err(|e| NetError::Ws(e.to_string()))?;
        let remote_addr = match ws.get_ref() {
            tokio_tungstenite::MaybeTlsStream::Plain(s) => s.peer_addr()?,
            _ => "0.0.0.0:0".parse().unwrap(),
        };
        Ok(Box::new(WsConnection {
            ws,
            peer: PeerInfo {
                remote_addr,
                transport: TransportKind::WebSocket,
            },
        }))
    }
}

struct WsConnection<S> {
    ws: WebSocketStream<S>,
    peer: PeerInfo,
}

#[async_trait]
impl<S> Connection for WsConnection<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, frame: Frame) -> Result<(), NetError> {
        let bytes = encode_frame(&frame)?;
        self.ws
            .send(Message::Binary(bytes.into()))
            .await
            .map_err(|e| NetError::Ws(e.to_string()))?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Frame>, NetError> {
        while let Some(msg) = self.ws.next().await {
            match msg.map_err(|e| NetError::Ws(e.to_string()))? {
                Message::Binary(bytes) => return Ok(Some(decode_frame(&bytes)?)),
                Message::Close(_) => return Ok(None),
                // tungstenite answers pings automatically on flush; ignore
                // pongs and (protocol-violating) text frames.
                Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => {}
            }
        }
        Ok(None)
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }

    async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }
}
