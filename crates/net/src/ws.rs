//! WebSocket transport: the mandatory fallback.
//!
//! One binary WebSocket message = one RHP frame (no length prefix — the
//! message boundary is the frame boundary). Wave 0 carries `ws://` for
//! loopback and development; in Wave 1 this rides the server's HTTPS
//! endpoint (`wss://…/rhp`) behind axum, sharing the web port.
//!
//! # No transport-layer migration
//!
//! Unlike QUIC, WebSocket rides a TCP connection whose 4-tuple is fixed for
//! the socket's life: it cannot follow a client that changes local address
//! (the mobile WiFi↔cellular case). So `WsConnection` leaves
//! [`Connection::migrate`] at its default, which
//! returns [`NetError::Unsupported`] — the caller's signal that this transport
//! can't migrate and it must instead reconnect and resume the server-side
//! session via `auth_resume(token, replay_cursor)` (`rabbithole-core`'s
//! client). See the crate-level docs for how the two paths compose.

use std::net::SocketAddr;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rabbithole_proto::{decode_frame, encode_frame, Frame};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::{Connection, Listener, NetError, PeerInfo, Transport, TransportKind};

/// A listening WebSocket endpoint (plain TCP; TLS termination is the web
/// layer's job in Wave 1).
pub struct WsListener {
    listener: TcpListener,
    allowed_origins: Vec<OriginId>,
}

impl WsListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self, NetError> {
        Self::bind_with_allowed_origins(addr, &[]).await
    }

    /// Bind a plaintext backend listener. Browser handshakes are accepted
    /// from loopback origins or an exact configured HTTPS origin; native
    /// clients omit `Origin` and remain available.
    pub async fn bind_with_allowed_origins(
        addr: SocketAddr,
        allowed_origins: &[String],
    ) -> Result<Self, NetError> {
        let allowed_origins = parse_allowed_origins(allowed_origins)?;
        Ok(Self {
            listener: TcpListener::bind(addr).await?,
            allowed_origins,
        })
    }
}

/// Validate configured browser origins without binding a socket.
pub fn validate_allowed_origins(origins: &[String]) -> Result<(), NetError> {
    parse_allowed_origins(origins).map(|_| ())
}

fn parse_allowed_origins(origins: &[String]) -> Result<Vec<OriginId>, NetError> {
    origins
        .iter()
        .map(|origin| {
            parse_origin(origin).and_then(|parsed| {
                if parsed.scheme == "https" || is_loopback_host(&parsed.host) {
                    Ok(parsed)
                } else {
                    Err("non-loopback allowed origins must use https")
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| NetError::Ws(format!("invalid ws_allowed_origins: {error}")))
}

#[async_trait]
impl Listener for WsListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, NetError> {
        let (stream, remote_addr) = self.listener.accept().await?;
        let allowed_origins = self.allowed_origins.clone();
        let ws = tokio_tungstenite::accept_hdr_async(
            stream,
            move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                validate_browser_origin(request, &allowed_origins)
                    .map(|()| response)
                    .map_err(forbidden)
            },
        )
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct OriginId {
    scheme: String,
    host: String,
    port: u16,
}

fn validate_browser_origin(request: &Request, allowed: &[OriginId]) -> Result<(), &'static str> {
    if !matches!(request.uri().path(), "/" | "/rhp") {
        return Err("WebSocket path must be /rhp");
    }
    let mut origins = request.headers().get_all("origin").iter();
    let Some(origin) = origins.next() else {
        // Non-browser clients do not send Origin. Authentication still occurs
        // at the RHP layer, so keep that transport path available.
        return Ok(());
    };
    if origins.next().is_some() {
        return Err("multiple WebSocket Origin headers are not allowed");
    }

    let origin = origin
        .to_str()
        .ok()
        .and_then(|value| parse_origin(value).ok())
        .ok_or("invalid WebSocket Origin header")?;
    if is_loopback_host(&origin.host) {
        let request_host = request
            .headers()
            .get("host")
            .and_then(|host| host.to_str().ok())
            .and_then(authority_host)
            .ok_or("WebSocket request has no valid Host header")?;
        return is_loopback_host(&request_host)
            .then_some(())
            .ok_or("loopback Origin is valid only for a loopback WebSocket");
    }
    if origin.scheme != "https" {
        return Err("public browser WebSocket origins must use https");
    }
    allowed
        .contains(&origin)
        .then_some(())
        .ok_or("WebSocket Origin is not allowlisted")
}

fn parse_origin(value: &str) -> Result<OriginId, &'static str> {
    let origin = value
        .parse::<tokio_tungstenite::tungstenite::http::Uri>()
        .map_err(|_| "invalid origin URI")?;
    if origin
        .path_and_query()
        .is_some_and(|path| path.as_str() != "/")
    {
        return Err("origin must not contain a path or query");
    }
    let scheme = origin
        .scheme_str()
        .filter(|scheme| matches!(*scheme, "http" | "https"))
        .ok_or("origin must use http or https")?
        .to_string();
    let authority = origin.authority().ok_or("origin has no authority")?;
    let host = authority.host().to_ascii_lowercase();
    let port = authority
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    Ok(OriginId { scheme, host, port })
}

fn authority_host(value: &str) -> Option<String> {
    value
        .parse::<tokio_tungstenite::tungstenite::http::uri::Authority>()
        .ok()
        .map(|authority| authority.host().to_ascii_lowercase())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn forbidden(message: &'static str) -> ErrorResponse {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(Some(message.to_string()))
        .expect("static WebSocket rejection response is valid")
}

/// Client-side WebSocket transport.
#[derive(Default)]
pub struct WsTransport;

#[async_trait]
impl Transport for WsTransport {
    /// `endpoint` is a ws/wss URL, e.g. `ws://host:4654/rhp`.
    async fn connect(&self, endpoint: &str) -> Result<Box<dyn Connection>, NetError> {
        validate_client_endpoint(endpoint)?;
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

fn validate_client_endpoint(endpoint: &str) -> Result<(), NetError> {
    let uri = endpoint
        .parse::<tokio_tungstenite::tungstenite::http::Uri>()
        .map_err(|_| NetError::Ws("invalid WebSocket endpoint".into()))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| NetError::Ws("WebSocket endpoint has no scheme".into()))?;
    let host = uri
        .host()
        .ok_or_else(|| NetError::Ws("WebSocket endpoint has no host".into()))?;
    if uri
        .authority()
        .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return Err(NetError::Ws(
            "WebSocket endpoint must not contain credentials".into(),
        ));
    }
    match scheme {
        "wss" => Ok(()),
        "ws" if is_loopback_host(host) => Ok(()),
        "ws" => Err(NetError::Ws(
            "remote plaintext WebSocket is unsafe; use wss://".into(),
        )),
        _ => Err(NetError::Ws("endpoint must use ws:// or wss://".into())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    fn request(host: &str, origin: Option<&str>) -> Request {
        let mut builder = Request::builder().header("host", host);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn native_client_without_origin_is_allowed() {
        assert!(validate_browser_origin(&request("burrow.example", None), &[]).is_ok());
    }

    #[test]
    fn loopback_browser_origin_may_use_http_and_a_different_port() {
        let req = request("127.0.0.1:4654", Some("http://localhost:8080"));
        assert!(validate_browser_origin(&req, &[]).is_ok());
    }

    #[test]
    fn cross_site_browser_origin_is_rejected() {
        let req = request("127.0.0.1:4654", Some("https://evil.example"));
        assert!(validate_browser_origin(&req, &[]).is_err());
    }

    #[test]
    fn configured_public_origin_requires_https() {
        let allowed = vec![parse_origin("https://burrow.example").unwrap()];
        let secure = request("burrow.example:4654", Some("https://burrow.example"));
        assert!(validate_browser_origin(&secure, &allowed).is_ok());

        let plaintext = request("burrow.example:4654", Some("http://burrow.example"));
        assert!(validate_browser_origin(&plaintext, &allowed).is_err());
    }

    #[test]
    fn exact_configured_cross_origin_is_allowed() {
        let req = request("ws.example:4654", Some("https://app.example"));
        let allowed = vec![parse_origin("https://app.example").unwrap()];
        assert!(validate_browser_origin(&req, &allowed).is_ok());

        let req = request("ws.example:4654", Some("https://evil.example"));
        let allowed = vec![parse_origin("https://burrow.example").unwrap()];
        assert!(validate_browser_origin(&req, &allowed).is_err());
    }

    #[test]
    fn public_origin_allowlist_includes_the_effective_port() {
        let allowed = vec![parse_origin("https://burrow.example:8443").unwrap()];
        let wrong_port = request("burrow.example:4654", Some("https://burrow.example"));
        assert!(validate_browser_origin(&wrong_port, &allowed).is_err());

        let exact = request("burrow.example:4654", Some("https://burrow.example:8443"));
        assert!(validate_browser_origin(&exact, &allowed).is_ok());
    }

    #[test]
    fn transport_refuses_remote_plaintext_before_dialing() {
        assert!(validate_client_endpoint("ws://127.0.0.1:4654").is_ok());
        assert!(validate_client_endpoint("ws://[::1]:4654").is_ok());
        assert!(validate_client_endpoint("wss://burrow.example/rhp").is_ok());
        assert!(validate_client_endpoint("ws://burrow.example:4654").is_err());
    }

    #[tokio::test]
    async fn handshake_accepts_loopback_origin_before_rhp_frames() {
        let mut listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await });

        let mut request = format!("ws://{addr}/rhp").into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", HeaderValue::from_static("http://localhost:8080"));
        let connected = tokio_tungstenite::connect_async(request).await;

        assert!(connected.is_ok());
        assert!(accept.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn handshake_rejects_cross_site_origin_before_rhp_frames() {
        let mut listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await });

        let mut request = format!("ws://{addr}/rhp").into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", HeaderValue::from_static("https://evil.example"));
        let connected = tokio_tungstenite::connect_async(request).await;

        assert!(connected.is_err());
        assert!(accept.await.unwrap().is_err());
    }
}
