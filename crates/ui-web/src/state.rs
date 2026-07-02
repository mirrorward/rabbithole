//! Pure, DOM-free UI state and its event reducer.
//!
//! This module deliberately holds **no** Leptos or `web_sys` types so the
//! reducer can be unit-tested on the host (see the `#[cfg(test)]` block) with
//! `cargo test`. View code in [`crate::components`] owns a reactive
//! `RwSignal<UiState>` and folds [`Event`]s into it via [`UiState::apply`].

use rabbithole_core::api::Event;

/// One rendered line of chat scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    /// Handle of the sender.
    pub from: String,
    /// The message body.
    pub text: String,
}

/// The full, flat UI model. `Default` is the pre-connection state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiState {
    /// Whether the (mock) transport reports an active session.
    pub connected: bool,
    /// Human-readable server name, once known.
    pub server_name: String,
    /// One-line status shown in the header bar.
    pub status: String,
    /// Chat scrollback for the lobby, oldest first.
    pub messages: Vec<ChatLine>,
    /// Handles currently present in the room.
    pub who: Vec<String>,
}

impl UiState {
    /// Fold a single [`Event`] into the state. Unknown (`#[non_exhaustive]`)
    /// events are ignored, matching the core's "tolerate unknown events"
    /// contract.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Connected {
                server_name,
                server_version,
            } => {
                self.connected = true;
                self.server_name = server_name.clone();
                self.status = format!("Connected to {server_name} ({server_version})");
            }
            Event::Disconnected { reason } => {
                self.connected = false;
                self.status = format!("Disconnected: {reason}");
            }
            Event::CommandFailed { detail } => {
                self.status = format!("Error: {detail}");
            }
            Event::ChatMessage { from, text, .. } => {
                self.messages.push(ChatLine {
                    from: from.clone(),
                    text: text.clone(),
                });
            }
            _ => {}
        }
    }
}

/// Derive a friendly server name from a connection endpoint such as
/// `ws://lobby.example:9000` or `host:port`. Pure and testable.
pub fn derive_server_name(endpoint: &str) -> String {
    let no_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    let host = no_scheme
        .split(['/', ':'])
        .next()
        .unwrap_or(no_scheme)
        .trim();
    if host.is_empty() {
        "server".to_string()
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_event_sets_name_and_flag() {
        let mut s = UiState::default();
        s.apply(&Event::Connected {
            server_name: "Rabbit Lobby".into(),
            server_version: "0.5.0".into(),
        });
        assert!(s.connected);
        assert_eq!(s.server_name, "Rabbit Lobby");
        assert!(s.status.contains("Rabbit Lobby"));
    }

    #[test]
    fn chat_messages_accumulate_in_order() {
        let mut s = UiState::default();
        s.apply(&Event::ChatMessage {
            room: "lobby".into(),
            from: "alice".into(),
            text: "hi".into(),
        });
        s.apply(&Event::ChatMessage {
            room: "lobby".into(),
            from: "bob".into(),
            text: "yo".into(),
        });
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].from, "alice");
        assert_eq!(s.messages[1].text, "yo");
    }

    #[test]
    fn disconnect_clears_connected_flag() {
        let mut s = UiState::default();
        s.apply(&Event::Connected {
            server_name: "x".into(),
            server_version: "1".into(),
        });
        s.apply(&Event::Disconnected {
            reason: "bye".into(),
        });
        assert!(!s.connected);
        assert!(s.status.contains("bye"));
    }

    #[test]
    fn command_failed_surfaces_detail() {
        let mut s = UiState::default();
        s.apply(&Event::CommandFailed {
            detail: "nope".into(),
        });
        assert!(s.status.contains("nope"));
    }

    #[test]
    fn server_name_derivation() {
        assert_eq!(
            derive_server_name("ws://lobby.example:9000"),
            "lobby.example"
        );
        assert_eq!(derive_server_name("host:1234"), "host");
        assert_eq!(derive_server_name("plainhost"), "plainhost");
        assert_eq!(derive_server_name(""), "server");
        assert_eq!(derive_server_name("wss://a.b.c/path"), "a.b.c");
    }
}
