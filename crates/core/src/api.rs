//! The Command/Event vocabulary between frontends and the core.
//!
//! Both enums are `#[non_exhaustive]`: frontends must tolerate unknown
//! events (render nothing) and the core answers unknown commands with
//! `Event::CommandFailed`. Waves extend these in lockstep with the
//! protocol families they implement.

use serde::{Deserialize, Serialize};

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
    Authenticated {
        token: String,
        screen_name: String,
    },
}
