//! The Command/Event vocabulary between frontends and the core.
//!
//! Both enums are `#[non_exhaustive]`: frontends must tolerate unknown
//! events (render nothing) and the core answers unknown commands with
//! `Event::CommandFailed`. Waves extend these in lockstep with the
//! protocol families they implement.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WsEndpointError {
    #[error("WebSocket endpoint is empty or malformed")]
    Invalid,
    #[error("unsupported WebSocket URL scheme")]
    UnsupportedScheme,
    #[error("remote plaintext WebSocket is unsafe; use wss://")]
    InsecureRemote,
}

/// Normalize a browser/native WebSocket endpoint without ever turning a
/// remote target into plaintext. Bare loopback targets use `ws://` for local
/// development; bare remote targets use `wss://`.
pub fn normalize_secure_ws_endpoint(endpoint: &str) -> Result<String, WsEndpointError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(WsEndpointError::Invalid);
    }

    if endpoint.starts_with("wss://") || endpoint.starts_with("ws://") {
        let url = parse_websocket_url(endpoint)?;
        if url.scheme() == "wss" {
            return Ok(endpoint.to_string());
        }
        return is_loopback_url(&url)
            .then(|| endpoint.to_string())
            .ok_or(WsEndpointError::InsecureRemote);
    }
    if endpoint.contains("://") {
        return Err(WsEndpointError::UnsupportedScheme);
    }

    let candidate = parse_websocket_url(&format!("ws://{endpoint}"))?;
    let scheme = if is_loopback_url(&candidate) {
        "ws"
    } else {
        "wss"
    };
    Ok(format!("{scheme}://{endpoint}"))
}

fn parse_websocket_url(endpoint: &str) -> Result<url::Url, WsEndpointError> {
    let url = url::Url::parse(endpoint).map_err(|_| WsEndpointError::Invalid)?;
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(WsEndpointError::Invalid);
    }
    Ok(url)
}

fn is_loopback_url(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Something a frontend asks the core to do.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Connect to a server. `endpoint` is a host:port or ws:// URL;
    /// `pinned_fingerprint` is the hex cert fingerprint from a rabbit
    /// link / Looking Glass entry (None once WebPKI lands).
    Connect {
        endpoint: String,
        pinned_fingerprint: Option<String>,
    },
    /// Cleanly disconnect the active session.
    Disconnect,
    /// Wave 1: authenticate the connected session.
    SignIn { login: String, password: String },
    /// Resume a prior session with a bearer token (from a previous `AuthOk`),
    /// so a reload reconnects without re-entering the password.
    Resume { token: String },
    /// Send a line to the currently focused chat room.
    SendChat { room: String, text: String },
}

/// Something the core tells frontends happened.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Transport connected; hello/version negotiation succeeded.
    Connected {
        server_name: String,
        server_version: String,
    },
    /// Session ended (cleanly or not).
    Disconnected { reason: String },
    /// A command could not be carried out.
    CommandFailed { detail: String },
    /// A chat line arrived (Wave 1: lobby only).
    ChatMessage {
        room: String,
        from: String,
        text: String,
        /// Server timestamp, unix milliseconds (0 when the transport has no
        /// clock — e.g. seeded mock scrollback on the host).
        at_unix_ms: i64,
    },
    /// The post-auth welcome: message of the day + an optional agreement the
    /// user must accept. Surfaced as a non-modal sheet on connect.
    Welcome {
        motd: String,
        agreement: Option<String>,
    },
    /// Authentication succeeded. `token` is the resume bearer token (empty for
    /// guests, which aren't resumable); the client persists it per-endpoint to
    /// auto-reconnect on next load.
    Authenticated { token: String, screen_name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_endpoint_policy_accepts_loopback_plaintext() {
        for endpoint in [
            "ws://localhost:4654",
            "ws://127.0.0.1:4654",
            "ws://127.42.0.7:4654/rhp",
            "ws://[::1]:4654",
        ] {
            assert_eq!(normalize_secure_ws_endpoint(endpoint).unwrap(), endpoint);
        }
    }

    #[test]
    fn websocket_endpoint_policy_rejects_remote_plaintext() {
        for endpoint in [
            "ws://example.com:4654",
            "ws://localhost.evil:4654",
            "ws://192.168.1.10:4654",
            "ws://0.0.0.0:4654",
        ] {
            assert_eq!(
                normalize_secure_ws_endpoint(endpoint),
                Err(WsEndpointError::InsecureRemote),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn websocket_endpoint_policy_accepts_remote_tls_and_secures_bare_hosts() {
        assert_eq!(
            normalize_secure_ws_endpoint("wss://burrow.example/rhp").unwrap(),
            "wss://burrow.example/rhp"
        );
        assert_eq!(
            normalize_secure_ws_endpoint("burrow.example:443/rhp").unwrap(),
            "wss://burrow.example:443/rhp"
        );
        assert_eq!(
            normalize_secure_ws_endpoint("127.0.0.1:4654").unwrap(),
            "ws://127.0.0.1:4654"
        );
        assert_eq!(
            normalize_secure_ws_endpoint("wss://burrow.example:not-a-port"),
            Err(WsEndpointError::Invalid)
        );
        assert_eq!(
            normalize_secure_ws_endpoint("wss://user:secret@burrow.example"),
            Err(WsEndpointError::Invalid)
        );
    }
}
