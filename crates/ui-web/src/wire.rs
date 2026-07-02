//! Transport-agnostic RHP wire mapping and the async client seam.
//!
//! This module is **host-testable** and DOM-free: it holds no `web_sys` or
//! `wasm_bindgen` types so the mapping logic can be unit-tested with
//! `cargo test` on the host target. The wasm-only WebSocket glue lives in
//! [`crate::ws`] and calls straight into the pure functions defined here.
//!
//! # The async seam
//!
//! [`UiClient`](crate::client::UiClient) is the synchronous, request/response
//! seam used by the current component tree. A real socket is asynchronous, so
//! this module adds a second, callback-driven seam — [`EventClient`] — that
//! both the in-memory [`MockClient`](crate::client::MockClient) (host + wasm)
//! and the browser [`WsClient`](crate::ws::WsClient) (wasm only) implement,
//! making them interchangeable:
//!
//! ```text
//! Component ──dispatch(Command)──▶ EventClient ─(async)─▶ EventSink(Event) ──▶ UiState
//! ```
//!
//! # Coverage
//!
//! Command → request `Frame` and inbound `Frame` → [`Event`] mapping is
//! implemented for the families the SPA drives today:
//!
//! - **session / auth (family 0):** the [`Hello`] handshake ([`hello_request`]),
//!   [`AuthPassword`] sign-in, keepalive [`Ping`], and the [`HelloAck`] reply
//!   (→ [`Event::Connected`]). Error replies map to [`Event::CommandFailed`].
//! - **presence / who (family 1):** the [`Who`] request ([`who_request`]) and
//!   the [`WhoList`] reply are framed and decoded. The core [`Event`] enum has
//!   no who-list variant yet, so a decoded [`WhoList`] folds to no events —
//!   the roster is wired at the frame layer and surfaces once the api grows a
//!   presence event.
//! - **chat (family 2):** [`ChatSend`] outbound and the [`ChatMessage`] push
//!   inbound (→ [`Event::ChatMessage`]).
//!
//! # Deferred
//!
//! - Reconnect / backoff and session resume ([`AuthResume`] is unused here).
//! - Binary attachments and the blob/transfer families.
//! - The board (family 4) and DM (family 3) families: their proto messages
//!   exist, but the api [`Command`]/[`Event`] enums carry no board/DM variants,
//!   so there is nothing to map yet. When those land, add arms to
//!   [`command_to_frame`] and [`frame_to_events`] and the transport is done.
//! - [`AuthOk`]/[`Welcome`] carry no api [`Event`] counterpart, so a successful
//!   sign-in emits nothing until the api grows an auth-success event; the
//!   history back-fill a client would issue after auth is likewise deferred.
//!
//! [`AuthResume`]: rabbithole_proto::session::AuthResume
//! [`AuthOk`]: rabbithole_proto::session::AuthOk
//! [`Welcome`]: rabbithole_proto::session::Welcome

use std::rc::Rc;

use rabbithole_core::api::{Command, Event};
use rabbithole_proto::chat::{ChatMessage, ChatSend};
use rabbithole_proto::hello::{CapabilitySet, Hello, HelloAck};
use rabbithole_proto::presence::Who;
use rabbithole_proto::session::{AuthPassword, Ping};
use rabbithole_proto::{Frame, ProtoError, RequestId};

/// Client software name announced in the [`Hello`] handshake.
pub const CLIENT_NAME: &str = "rabbit-web";
/// Client software version announced in the [`Hello`] handshake.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A sink the transport pushes decoded [`Event`]s into. `Rc<dyn Fn>` (not a
/// `Box`) because the browser transport shares one sink across several
/// independent WebSocket event handlers. It is deliberately `!Send`: the web
/// SPA is single-threaded.
pub type EventSink = Rc<dyn Fn(Event)>;

/// The async, callback-driven client seam.
///
/// Unlike [`UiClient`](crate::client::UiClient), commands are fire-and-forget
/// and events arrive later through the sink registered with [`on_event`].
/// Implemented by both [`MockClient`](crate::client::MockClient) and
/// [`WsClient`](crate::ws::WsClient) so components can hold either behind a
/// `dyn EventClient` / generic bound.
///
/// [`on_event`]: EventClient::on_event
pub trait EventClient {
    /// Register the sink every subsequent [`Event`] is delivered to. The most
    /// recent registration wins.
    fn on_event(&mut self, sink: EventSink);

    /// Drive one [`Command`]. Any resulting [`Event`]s arrive asynchronously
    /// through the registered sink.
    fn dispatch(&mut self, command: Command);
}

/// Build the [`Hello`] request frame that opens every RHP session.
pub fn hello_request(id: RequestId) -> Result<Frame, ProtoError> {
    let hello = Hello::new(CLIENT_NAME, CLIENT_VERSION, CapabilitySet::default());
    Frame::request(id, &hello)
}

/// Build a keepalive [`Ping`] request frame.
pub fn ping_request(id: RequestId) -> Result<Frame, ProtoError> {
    Frame::request(id, &Ping)
}

/// Build a presence [`Who`] request frame.
pub fn who_request(id: RequestId) -> Result<Frame, ProtoError> {
    Frame::request(id, &Who)
}

/// Map an outbound [`Command`] to the RHP request [`Frame`] that carries it.
///
/// Returns `Ok(None)` for commands the transport handles itself rather than by
/// sending a frame — [`Command::Connect`] (open the socket + [`hello_request`])
/// and [`Command::Disconnect`] (close the socket).
pub fn command_to_frame(command: &Command, id: RequestId) -> Result<Option<Frame>, ProtoError> {
    let frame = match command {
        Command::Connect { .. } | Command::Disconnect => return Ok(None),
        Command::SignIn { login, password } => {
            Frame::request(id, &AuthPassword::new(login.clone(), password.clone()))?
        }
        Command::SendChat { room, text } => {
            Frame::request(id, &ChatSend::new(room.clone(), text.clone()))?
        }
        // `Command` is `#[non_exhaustive]`: unknown commands have no framing.
        _ => return Ok(None),
    };
    Ok(Some(frame))
}

/// Normalise a connection endpoint into a WebSocket URL.
///
/// An endpoint that already carries a `ws://`/`wss://` scheme is used verbatim;
/// a bare `host:port` (the rabbit-link form) defaults to the plaintext `ws://`
/// scheme. TLS selection (`wss://`) is the caller's decision.
pub fn normalize_ws_url(endpoint: &str) -> String {
    let e = endpoint.trim();
    if e.starts_with("ws://") || e.starts_with("wss://") {
        e.to_string()
    } else {
        format!("ws://{e}")
    }
}

/// Map an inbound RHP [`Frame`] to the api [`Event`]s it produces.
///
/// Frames that carry no api-visible event (e.g. presence rosters, auth acks,
/// keepalive pongs, or families the SPA doesn't consume yet) map to an empty
/// vector — matching the core's "tolerate unknown messages" contract.
pub fn frame_to_events(frame: &Frame) -> Vec<Event> {
    // An error reply supersedes any payload decode.
    if let Some(code) = frame.error {
        return vec![Event::CommandFailed {
            detail: format!("server error: {code:?}"),
        }];
    }

    // Session: the handshake reply completes the connection.
    if let Some(Ok(ack)) = frame.decode::<HelloAck>() {
        return vec![Event::Connected {
            server_name: ack.server_name,
            server_version: ack.server_version,
        }];
    }

    // Chat: a line arrived (a push, or an echo of our own send).
    if let Some(Ok(msg)) = frame.decode::<ChatMessage>() {
        return vec![Event::ChatMessage {
            room: msg.room,
            from: msg.from,
            text: msg.text,
        }];
    }

    // Presence rosters, auth acks, welcomes, pongs and not-yet-mapped families
    // decode fine but have no api::Event counterpart — see module docs.
    Vec::new()
}

impl EventClient for crate::client::MockClient {
    fn on_event(&mut self, sink: EventSink) {
        self.set_sink(sink);
    }

    fn dispatch(&mut self, command: Command) {
        let events = crate::client::UiClient::send(self, command);
        self.emit_events(&events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::frame::{Family, FrameKind};
    use rabbithole_proto::presence::{UserSummary, WhoList};
    use rabbithole_proto::{encode_frame, ErrorCode};

    #[test]
    fn connect_and_disconnect_have_no_frame() {
        assert!(command_to_frame(
            &Command::Connect {
                endpoint: "ws://x".into(),
                pinned_fingerprint: None,
            },
            RequestId(1),
        )
        .unwrap()
        .is_none());
        assert!(command_to_frame(&Command::Disconnect, RequestId(1))
            .unwrap()
            .is_none());
    }

    #[test]
    fn sign_in_maps_to_auth_password_frame() {
        let frame = command_to_frame(
            &Command::SignIn {
                login: "alice".into(),
                password: "hunter2".into(),
            },
            RequestId(7),
        )
        .unwrap()
        .expect("sign-in produces a frame");
        assert_eq!(frame.kind, FrameKind::Request);
        assert_eq!(frame.family, Family::SESSION);
        assert_eq!(frame.id, RequestId(7));
        let decoded = frame.decode::<AuthPassword>().unwrap().unwrap();
        assert_eq!(decoded.login, "alice");
        assert_eq!(decoded.password, "hunter2");
    }

    #[test]
    fn send_chat_maps_to_chat_send_frame() {
        let frame = command_to_frame(
            &Command::SendChat {
                room: "lobby".into(),
                text: "hi warren".into(),
            },
            RequestId(3),
        )
        .unwrap()
        .expect("chat produces a frame");
        assert_eq!(frame.family, Family::CHAT);
        let decoded = frame.decode::<ChatSend>().unwrap().unwrap();
        assert_eq!(decoded.room, "lobby");
        assert_eq!(decoded.text, "hi warren");
    }

    #[test]
    fn hello_request_carries_client_identity() {
        let frame = hello_request(RequestId(1)).unwrap();
        assert_eq!(frame.family, Family::SESSION);
        let hello = frame.decode::<Hello>().unwrap().unwrap();
        assert_eq!(hello.client_name, CLIENT_NAME);
        assert_eq!(hello.client_version, CLIENT_VERSION);
    }

    #[test]
    fn who_and_ping_requests_target_their_families() {
        assert_eq!(who_request(RequestId(1)).unwrap().family, Family::PRESENCE);
        assert_eq!(ping_request(RequestId(1)).unwrap().family, Family::SESSION);
    }

    #[test]
    fn hello_ack_maps_to_connected() {
        // A server-shaped HelloAck reply frame.
        let ack = HelloAck::new(
            rabbithole_proto::PROTOCOL_VERSION,
            CapabilitySet::default(),
            "Rabbit Lobby",
            "0.9.0",
            [0u8; 32],
        );
        let req = hello_request(RequestId(1)).unwrap();
        let reply = Frame::reply_to(&req, &ack).unwrap();
        let events = frame_to_events(&reply);
        assert_eq!(
            events,
            vec![Event::Connected {
                server_name: "Rabbit Lobby".into(),
                server_version: "0.9.0".into(),
            }]
        );
    }

    #[test]
    fn chat_push_maps_to_chat_message() {
        let push = Frame::push(&ChatMessage::new("lobby", "bob", "yo", 1_700_000_000_000)).unwrap();
        let events = frame_to_events(&push);
        assert_eq!(
            events,
            vec![Event::ChatMessage {
                room: "lobby".into(),
                from: "bob".into(),
                text: "yo".into(),
            }]
        );
    }

    #[test]
    fn error_reply_maps_to_command_failed() {
        let req = hello_request(RequestId(1)).unwrap();
        let reply = Frame::error_reply(&req, ErrorCode::Unauthenticated);
        let events = frame_to_events(&reply);
        assert!(matches!(
            events.as_slice(),
            [Event::CommandFailed { detail }] if detail.contains("Unauthenticated")
        ));
    }

    #[test]
    fn who_list_reply_decodes_but_yields_no_event() {
        let who = WhoList::new(vec![UserSummary::new(1, "alice", 1, "websocket", 10)]);
        let req = who_request(RequestId(1)).unwrap();
        let reply = Frame::reply_to(&req, &who).unwrap();
        // Framed + decodable, but no api::Event variant exists for it yet.
        assert!(reply.decode::<WhoList>().unwrap().is_ok());
        assert!(frame_to_events(&reply).is_empty());
    }

    #[test]
    fn frame_survives_wire_roundtrip_before_mapping() {
        let push = Frame::push(&ChatMessage::new("lobby", "bob", "hi", 0)).unwrap();
        let bytes = encode_frame(&push).unwrap();
        let decoded = rabbithole_proto::decode_frame(&bytes).unwrap();
        assert_eq!(
            frame_to_events(&decoded),
            vec![Event::ChatMessage {
                room: "lobby".into(),
                from: "bob".into(),
                text: "hi".into(),
            }]
        );
    }

    #[test]
    fn normalize_url_adds_scheme_when_missing() {
        assert_eq!(normalize_ws_url("host:9000"), "ws://host:9000");
        assert_eq!(normalize_ws_url("ws://host:9000"), "ws://host:9000");
        assert_eq!(normalize_ws_url("wss://host:9000"), "wss://host:9000");
        assert_eq!(normalize_ws_url("  host:1  "), "ws://host:1");
    }

    #[test]
    fn mock_client_is_an_event_client() {
        use crate::client::MockClient;
        use std::cell::RefCell;

        let seen: Rc<RefCell<Vec<Event>>> = Rc::new(RefCell::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut client = MockClient::new();
        EventClient::on_event(
            &mut client,
            Rc::new(move |e| sink_seen.borrow_mut().push(e)),
        );

        EventClient::dispatch(
            &mut client,
            Command::Connect {
                endpoint: "ws://warren.example:9000".into(),
                pinned_fingerprint: None,
            },
        );
        let events = seen.borrow();
        assert!(matches!(events.as_slice(), [Event::Connected { .. }]));
    }
}
