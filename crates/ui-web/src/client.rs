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

use crate::state::{derive_server_name, Board, DmMessage, DmThread, Member, Post, Thread};

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

    /// Snapshot of the board tree. Boards have no [`Event`] variant yet, so —
    /// like [`who`](Self::who) — they are a direct query until the board
    /// protocol family and its events land.
    fn boards(&self) -> Vec<Board>;

    /// Threads belonging to the board identified by `slug`.
    fn threads(&self, slug: &str) -> Vec<Thread>;

    /// Posts belonging to the thread identified by `thread_id`.
    fn posts(&self, thread_id: u64) -> Vec<Post>;

    /// Snapshot of the member directory.
    fn members(&self) -> Vec<Member>;

    /// Snapshot of the direct-message conversations.
    fn dm_threads(&self) -> Vec<DmThread>;

    /// Append `text` to the DM conversation identified by `thread_id`, sent as
    /// the current user, and return the stored message. Returns `None` if the
    /// conversation is unknown. The real transport will echo a server event
    /// instead of appending locally.
    fn send_dm(&mut self, thread_id: &str, text: &str) -> Option<DmMessage>;
}

/// In-memory [`UiClient`] used alongside the real WebSocket transport.
///
/// Also implements the async [`EventClient`](crate::wire::EventClient) seam so
/// it is interchangeable with [`WsClient`](crate::ws::WsClient): a registered
/// sink receives the same events `send` returns, pushed synchronously.
#[derive(Clone)]
pub struct MockClient {
    connected: bool,
    signed_in: bool,
    server_name: String,
    current_user: Option<String>,
    who: Vec<String>,
    boards: Vec<Board>,
    threads: Vec<Thread>,
    posts: Vec<Post>,
    members: Vec<Member>,
    dm_threads: Vec<DmThread>,
    /// Sink registered through the async [`EventClient`] seam, if any. Skipped
    /// by [`Debug`] (closures are not `Debug`).
    sink: Option<crate::wire::EventSink>,
}

impl std::fmt::Debug for MockClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockClient")
            .field("connected", &self.connected)
            .field("signed_in", &self.signed_in)
            .field("server_name", &self.server_name)
            .field("current_user", &self.current_user)
            .field("who", &self.who)
            .field("boards", &self.boards)
            .field("threads", &self.threads)
            .field("posts", &self.posts)
            .field("members", &self.members)
            .field("dm_threads", &self.dm_threads)
            .field("sink", &self.sink.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl Default for MockClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockClient {
    /// A fresh, disconnected mock with a seeded member list, board tree, DM
    /// conversations and directory.
    pub fn new() -> Self {
        Self {
            connected: false,
            signed_in: false,
            server_name: String::new(),
            current_user: None,
            who: vec!["rabbit".to_string(), "alice".to_string(), "bob".to_string()],
            boards: Self::seeded_boards(),
            threads: Self::seeded_threads(),
            posts: Self::seeded_posts(),
            members: Self::seeded_members(),
            dm_threads: Self::seeded_dms(),
            sink: None,
        }
    }

    /// Register the async event sink (used by the
    /// [`EventClient`](crate::wire::EventClient) impl).
    pub(crate) fn set_sink(&mut self, sink: crate::wire::EventSink) {
        self.sink = Some(sink);
    }

    /// Push events into the registered sink, if one is set.
    pub(crate) fn emit_events(&self, events: &[Event]) {
        if let Some(sink) = &self.sink {
            for event in events {
                sink(event.clone());
            }
        }
    }

    fn seeded_boards() -> Vec<Board> {
        vec![
            Board {
                slug: "general".to_string(),
                name: "General".to_string(),
                description: "Warren-wide chatter and announcements.".to_string(),
            },
            Board {
                slug: "tech".to_string(),
                name: "Tech Talk".to_string(),
                description: "Protocols, clients and self-hosting.".to_string(),
            },
        ]
    }

    fn seeded_threads() -> Vec<Thread> {
        vec![
            Thread {
                id: 1,
                board: "general".to_string(),
                title: "Warren rules & etiquette".to_string(),
                author: "rabbit".to_string(),
            },
            Thread {
                id: 2,
                board: "general".to_string(),
                title: "Introduce yourself".to_string(),
                author: "alice".to_string(),
            },
            Thread {
                id: 3,
                board: "tech".to_string(),
                title: "Running your own burrow".to_string(),
                author: "bob".to_string(),
            },
        ]
    }

    fn seeded_posts() -> Vec<Post> {
        vec![
            Post {
                id: 11,
                thread: 1,
                author: "rabbit".to_string(),
                body: "Be excellent to each other. No spam.".to_string(),
            },
            Post {
                id: 12,
                thread: 1,
                author: "alice".to_string(),
                body: "Sounds good to me!".to_string(),
            },
            Post {
                id: 21,
                thread: 2,
                author: "alice".to_string(),
                body: "Hi, I'm Alice. Long-time lurker.".to_string(),
            },
            Post {
                id: 31,
                thread: 3,
                author: "bob".to_string(),
                body: "Here's how I set up my burrow behind NAT.".to_string(),
            },
        ]
    }

    fn seeded_members() -> Vec<Member> {
        vec![
            Member {
                handle: "rabbit".to_string(),
                display_name: "The Rabbit".to_string(),
                bio: "Warren keeper and host.".to_string(),
                online: true,
            },
            Member {
                handle: "alice".to_string(),
                display_name: "Alice Down".to_string(),
                bio: "Curious about everything.".to_string(),
                online: true,
            },
            Member {
                handle: "bob".to_string(),
                display_name: "Bob Hutch".to_string(),
                bio: "Self-hosting enthusiast.".to_string(),
                online: false,
            },
        ]
    }

    fn seeded_dms() -> Vec<DmThread> {
        vec![
            DmThread {
                id: "alice".to_string(),
                peer: "alice".to_string(),
                messages: vec![
                    DmMessage {
                        from: "alice".to_string(),
                        text: "hey, did you see the new board?".to_string(),
                    },
                    DmMessage {
                        from: "rabbit".to_string(),
                        text: "yep, looks great".to_string(),
                    },
                ],
            },
            DmThread {
                id: "bob".to_string(),
                peer: "bob".to_string(),
                messages: vec![DmMessage {
                    from: "bob".to_string(),
                    text: "ping me when you're around".to_string(),
                }],
            },
        ]
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

    fn boards(&self) -> Vec<Board> {
        self.boards.clone()
    }

    fn threads(&self, slug: &str) -> Vec<Thread> {
        self.threads
            .iter()
            .filter(|t| t.board == slug)
            .cloned()
            .collect()
    }

    fn posts(&self, thread_id: u64) -> Vec<Post> {
        self.posts
            .iter()
            .filter(|p| p.thread == thread_id)
            .cloned()
            .collect()
    }

    fn members(&self) -> Vec<Member> {
        self.members.clone()
    }

    fn dm_threads(&self) -> Vec<DmThread> {
        self.dm_threads.clone()
    }

    fn send_dm(&mut self, thread_id: &str, text: &str) -> Option<DmMessage> {
        let from = self
            .current_user
            .clone()
            .unwrap_or_else(|| "me".to_string());
        let thread = self.dm_threads.iter_mut().find(|t| t.id == thread_id)?;
        let msg = DmMessage {
            from,
            text: text.to_string(),
        };
        thread.messages.push(msg.clone());
        Some(msg)
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
    fn boards_are_seeded() {
        let c = MockClient::new();
        let boards = c.boards();
        assert_eq!(boards.len(), 2);
        assert!(boards.iter().any(|b| b.slug == "general"));
        assert!(boards.iter().any(|b| b.slug == "tech"));
    }

    #[test]
    fn threads_filter_by_board_slug() {
        let c = MockClient::new();
        let general = c.threads("general");
        assert_eq!(general.len(), 2);
        assert!(general.iter().all(|t| t.board == "general"));
        let tech = c.threads("tech");
        assert_eq!(tech.len(), 1);
        assert!(c.threads("nope").is_empty());
    }

    #[test]
    fn posts_filter_by_thread_id() {
        let c = MockClient::new();
        let posts = c.posts(1);
        assert_eq!(posts.len(), 2);
        assert!(posts.iter().all(|p| p.thread == 1));
        assert!(c.posts(999).is_empty());
    }

    #[test]
    fn members_are_seeded() {
        let c = MockClient::new();
        assert_eq!(c.members().len(), 3);
    }

    #[test]
    fn dm_threads_are_seeded() {
        let c = MockClient::new();
        let dms = c.dm_threads();
        assert_eq!(dms.len(), 2);
        assert_eq!(dms[0].messages.len(), 2);
    }

    #[test]
    fn send_dm_appends_as_current_user() {
        let mut c = connect_and_sign_in("kevin");
        let msg = c.send_dm("alice", "hello there").unwrap();
        assert_eq!(msg.from, "kevin");
        assert_eq!(msg.text, "hello there");
        // The append persists on the mock.
        let alice = c
            .dm_threads()
            .into_iter()
            .find(|t| t.id == "alice")
            .unwrap();
        assert_eq!(alice.messages.len(), 3);
        assert_eq!(alice.messages.last().unwrap().text, "hello there");
    }

    #[test]
    fn send_dm_to_unknown_thread_returns_none() {
        let mut c = connect_and_sign_in("kevin");
        assert!(c.send_dm("nobody", "hi").is_none());
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
