//! The client seam and its in-memory mock.
//!
//! # The transport seam
//!
//! Components never talk to a socket directly; they drive a [`UiClient`]. Its
//! contract is deliberately tiny and **async-free**: hand it a
//! [`Command`], get back the [`Event`]s it produced, plus a synchronous
//! who-list query. This lets the whole UI compile and be unit-tested today —
//! before the real browser WebSocket transport lands.
//!
//! [`MockClient`] is the stand-in implementation: it keeps a lobby room, a
//! seeded scrollback and member list entirely in memory. When the real wasm
//! WebSocket transport arrives (a later Wave 8 slice) it becomes a second
//! `UiClient` impl that pushes [`Event`]s asynchronously; the component layer
//! is expected to grow a callback/stream sink at that point, but the
//! command-in / event-out shape stays the same.

use rabbithole_core::api::{Command, Event};

use crate::state::derive_server_name;

/// The single room the mock exposes.
pub const LOBBY: &str = "lobby";

/// The seam every component drives instead of a raw transport.
pub trait UiClient {
    /// Drive one [`Command`] and return the [`Event`]s it produced. The real
    /// transport will deliver events asynchronously; the mock produces them
    /// synchronously so the flow is testable without an executor.
    fn send(&mut self, command: Command) -> Vec<Event>;

    /// Snapshot of the handles currently present in `room`. Not modelled as
    /// an [`Event`] yet (the core's `Event` enum has no who-list variant), so
    /// it is exposed as a direct query on the seam.
    fn who(&self, room: &str) -> Vec<String>;
}

/// In-memory [`UiClient`] used until the real WebSocket transport lands.
#[derive(Debug, Clone)]
pub struct MockClient {
    connected: bool,
    signed_in: bool,
    server_name: String,
    current_user: Option<String>,
    who: Vec<String>,
}

impl Default for MockClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockClient {
    /// A fresh, disconnected mock with a seeded member list.
    pub fn new() -> Self {
        Self {
            connected: false,
            signed_in: false,
            server_name: String::new(),
            current_user: None,
            who: vec!["rabbit".to_string(), "alice".to_string(), "bob".to_string()],
        }
    }

    /// The lobby scrollback every fresh session is seeded with.
    fn seeded_messages() -> Vec<Event> {
        [
            (
                "rabbit",
                "Welcome to the warren. Be excellent to each other.",
            ),
            ("alice", "morning all \u{2600}"),
            ("bob", "anyone up for a game later?"),
        ]
        .into_iter()
        .map(|(from, text)| Event::ChatMessage {
            room: LOBBY.to_string(),
            from: from.to_string(),
            text: text.to_string(),
        })
        .collect()
    }
}

impl UiClient for MockClient {
    fn send(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::Connect { endpoint, .. } => {
                self.connected = true;
                self.server_name = derive_server_name(&endpoint);
                vec![Event::Connected {
                    server_name: self.server_name.clone(),
                    server_version: "0.5.0-mock".to_string(),
                }]
            }
            Command::Disconnect => {
                self.connected = false;
                self.signed_in = false;
                self.current_user = None;
                vec![Event::Disconnected {
                    reason: "client requested".to_string(),
                }]
            }
            Command::SignIn { login, .. } => {
                if !self.connected {
                    return vec![Event::CommandFailed {
                        detail: "not connected".to_string(),
                    }];
                }
                self.signed_in = true;
                if !self.who.iter().any(|h| h == &login) {
                    self.who.push(login.clone());
                }
                self.current_user = Some(login);
                Self::seeded_messages()
            }
            Command::SendChat { room, text } => {
                if !self.signed_in {
                    return vec![Event::CommandFailed {
                        detail: "sign in first".to_string(),
                    }];
                }
                let from = self
                    .current_user
                    .clone()
                    .unwrap_or_else(|| "me".to_string());
                vec![Event::ChatMessage { room, from, text }]
            }
            _ => vec![Event::CommandFailed {
                detail: "unsupported command".to_string(),
            }],
        }
    }

    fn who(&self, _room: &str) -> Vec<String> {
        self.who.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_and_sign_in(handle: &str) -> MockClient {
        let mut c = MockClient::new();
        c.send(Command::Connect {
            endpoint: "ws://localhost:9000".into(),
            pinned_fingerprint: None,
        });
        c.send(Command::SignIn {
            login: handle.into(),
            password: String::new(),
        });
        c
    }

    #[test]
    fn connect_emits_connected_with_derived_name() {
        let mut c = MockClient::new();
        let ev = c.send(Command::Connect {
            endpoint: "ws://warren.example:9000".into(),
            pinned_fingerprint: None,
        });
        assert_eq!(
            ev,
            vec![Event::Connected {
                server_name: "warren.example".into(),
                server_version: "0.5.0-mock".into(),
            }]
        );
    }

    #[test]
    fn sign_in_requires_connection() {
        let mut c = MockClient::new();
        let ev = c.send(Command::SignIn {
            login: "kevin".into(),
            password: String::new(),
        });
        assert!(matches!(ev.as_slice(), [Event::CommandFailed { .. }]));
    }

    #[test]
    fn sign_in_accepts_any_user_and_seeds_chat() {
        let mut c = MockClient::new();
        c.send(Command::Connect {
            endpoint: "host:1".into(),
            pinned_fingerprint: None,
        });
        let ev = c.send(Command::SignIn {
            login: "kevin".into(),
            password: "whatever".into(),
        });
        assert_eq!(ev.len(), 3);
        assert!(ev.iter().all(|e| matches!(e, Event::ChatMessage { .. })));
    }

    #[test]
    fn sign_in_adds_user_to_who_list_once() {
        let c = connect_and_sign_in("kevin");
        let who = c.who(LOBBY);
        assert!(who.contains(&"kevin".to_string()));
        assert_eq!(who.iter().filter(|h| *h == "kevin").count(), 1);
    }

    #[test]
    fn send_chat_echoes_from_current_user() {
        let mut c = connect_and_sign_in("kevin");
        let ev = c.send(Command::SendChat {
            room: LOBBY.into(),
            text: "hello warren".into(),
        });
        assert_eq!(
            ev,
            vec![Event::ChatMessage {
                room: LOBBY.into(),
                from: "kevin".into(),
                text: "hello warren".into(),
            }]
        );
    }

    #[test]
    fn send_chat_before_sign_in_fails() {
        let mut c = MockClient::new();
        let ev = c.send(Command::SendChat {
            room: LOBBY.into(),
            text: "hi".into(),
        });
        assert!(matches!(ev.as_slice(), [Event::CommandFailed { .. }]));
    }

    #[test]
    fn disconnect_resets_session() {
        let mut c = connect_and_sign_in("kevin");
        let ev = c.send(Command::Disconnect);
        assert!(matches!(ev.as_slice(), [Event::Disconnected { .. }]));
        // After disconnect, sending chat should fail again.
        let ev = c.send(Command::SendChat {
            room: LOBBY.into(),
            text: "hi".into(),
        });
        assert!(matches!(ev.as_slice(), [Event::CommandFailed { .. }]));
    }
}
