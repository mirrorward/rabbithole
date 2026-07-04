//! Pure, DOM-free UI state and its event reducer.
//!
//! This module deliberately holds **no** Leptos or `web_sys` types so the
//! reducer can be unit-tested on the host (see the `#[cfg(test)]` block) with
//! `cargo test`. View code in [`crate::components`] owns a reactive
//! `RwSignal<UiState>` and folds [`Event`]s into it via [`UiState::apply`].

use rabbithole_core::api::Event;

use crate::conn::ConnState;

/// One rendered line of chat scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    /// Handle of the sender.
    pub from: String,
    /// The message body.
    pub text: String,
}

/// A board in the board tree.
///
/// Boards, threads and posts have **no** [`Event`]/[`Command`] variants in
/// [`rabbithole_core::api`] yet, so they are modelled here as view-local
/// state seeded by [`crate::client::MockClient`]. When the board protocol
/// family lands, the transport slice will map real events onto the same
/// [`UiState::set_boards`] / [`UiState::select_board`] / [`UiState::open_thread`]
/// mutators — the reducer surface is intentionally kept transport-shaped.
///
/// [`Command`]: rabbithole_core::api::Command
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    /// URL-safe identifier used in the `/boards/:slug` route.
    pub slug: String,
    /// Human-readable board name.
    pub name: String,
    /// One-line description shown in the tree.
    pub description: String,
}

/// A discussion thread within a [`Board`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// Stable thread identifier: a synthetic `t<n>` in the mock, the root
    /// post's hex blake3 id over a live transport.
    pub id: String,
    /// Slug of the [`Board`] this thread belongs to.
    pub board: String,
    /// Thread subject line.
    pub title: String,
    /// Handle of the thread starter.
    pub author: String,
}

/// A single post inside a [`Thread`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// Stable post identifier (hex blake3 id over a live transport).
    pub id: String,
    /// Identifier of the owning [`Thread`] (its root id).
    pub thread: String,
    /// Handle of the poster.
    pub author: String,
    /// Post body text.
    pub body: String,
}

/// One message in a direct-message conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmMessage {
    /// Handle of the sender.
    pub from: String,
    /// Message body.
    pub text: String,
}

/// A direct-message conversation with a single peer.
///
/// Like boards, DMs are view-local until the DM protocol family lands; the
/// transport slice will replace [`UiState::append_dm`]'s local append with an
/// echoed server event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmThread {
    /// Stable conversation identifier.
    pub id: String,
    /// Handle of the other party.
    pub peer: String,
    /// Messages, oldest first.
    pub messages: Vec<DmMessage>,
}

/// A member's full profile card (from a live `ProfileGet`), richer than the
/// directory-list [`Member`] row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberProfile {
    pub screen_name: String,
    pub location: Option<String>,
    pub interests: Option<String>,
    pub quote: Option<String>,
    pub plan: Option<String>,
    pub pronouns: Option<String>,
    /// Whether the persona is online right now (from `online_transport`).
    pub online: bool,
    /// The avatar's blake3 blob id (hex), if the persona has one — fetched
    /// separately via `BlobGet`.
    pub avatar_hex: Option<String>,
    /// A `data:` URL for the avatar once its blob has been fetched.
    pub avatar_src: Option<String>,
}

/// A user present in a room's live roster, richer than a bare handle: carries
/// their presence state (for the status dot) and the door they came in through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    /// Screen name (`name@origin`).
    pub screen_name: String,
    /// Online / Away / Idle / Invisible.
    pub state: rabbithole_proto::presence::PresenceState,
    /// Transport: "websocket", "quic", "telnet", "hotline", …
    pub transport: String,
}

/// A member listed in the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Login handle.
    pub handle: String,
    /// Friendly display name.
    pub display_name: String,
    /// Short profile blurb shown on the profile card.
    pub bio: String,
    /// Whether the member is currently online.
    pub online: bool,
}

/// The full, flat UI model. `Default` is the pre-connection state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiState {
    /// Whether the (mock) transport reports an active session.
    pub connected: bool,
    /// The transport's connection lifecycle, surfaced in the header.
    pub conn: ConnState,
    /// Human-readable server name, once known.
    pub server_name: String,
    /// One-line status shown in the header bar.
    pub status: String,
    /// Chat scrollback for the lobby, oldest first.
    pub messages: Vec<ChatLine>,
    /// Users currently present in the room, with their presence state.
    pub who: Vec<Presence>,
    /// The board tree.
    pub boards: Vec<Board>,
    /// Threads of the currently selected board.
    pub threads: Vec<Thread>,
    /// Posts of the currently opened thread.
    pub posts: Vec<Post>,
    /// Slug of the selected board, if any.
    pub selected_board: Option<String>,
    /// Id of the opened thread, if any.
    pub selected_thread: Option<String>,
    /// Direct-message conversations.
    pub dm_threads: Vec<DmThread>,
    /// Id of the selected DM conversation, if any.
    pub selected_dm: Option<String>,
    /// Member directory.
    pub members: Vec<Member>,
    /// Current directory search query.
    pub directory_query: String,
    /// The selected member's full profile card (live `ProfileGet` reply).
    pub selected_profile: Option<MemberProfile>,
    /// Handle of the member whose profile card is shown, if any.
    pub selected_member: Option<String>,
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
                self.conn = ConnState::Online;
                self.server_name = server_name.clone();
                self.status = format!("Connected to {server_name} ({server_version})");
            }
            Event::Disconnected { reason } => {
                self.connected = false;
                self.conn = ConnState::Offline;
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

    /// Append an operator notice to the chat scrollback as a marked system
    /// line. Radio now-playing never reaches here — RADIO-family frames are
    /// split off by [`frame_to_notice_route`](crate::wire::frame_to_notice_route)
    /// before the chat log.
    pub fn push_notice(&mut self, from: &str, text: &str) {
        self.messages.push(ChatLine {
            from: format!("! {from}"),
            text: text.to_string(),
        });
    }

    /// Set the transport connection state (driven by the transport's
    /// connection-lifecycle callback: `Connecting`/`Reconnecting` between the
    /// `Connected`/`Disconnected` events the reducer already folds).
    pub fn set_conn(&mut self, conn: ConnState) {
        self.conn = conn;
        if conn.is_pending() {
            self.status = conn.label().to_string();
        }
    }

    /// Replace the board tree (from a client snapshot).
    pub fn set_boards(&mut self, boards: Vec<Board>) {
        self.boards = boards;
    }

    /// Select a board: record the slug, load its threads and reset any open
    /// thread. Mirrors the shape a future `Event::BoardSelected` would take.
    pub fn select_board(&mut self, slug: &str, threads: Vec<Thread>) {
        self.selected_board = Some(slug.to_string());
        self.threads = threads;
        self.selected_thread = None;
        self.posts.clear();
    }

    /// Open a thread within the selected board and load its posts.
    pub fn open_thread(&mut self, id: String, posts: Vec<Post>) {
        self.selected_thread = Some(id);
        self.posts = posts;
    }

    /// Replace the DM conversation list (from a client snapshot).
    pub fn set_dm_threads(&mut self, threads: Vec<DmThread>) {
        self.dm_threads = threads;
    }

    /// Select a DM conversation by id, creating an empty one if the peer isn't
    /// in the list yet (so a fresh conversation can be started).
    pub fn select_dm(&mut self, id: &str) {
        if !self.dm_threads.iter().any(|t| t.id == id) {
            self.dm_threads.push(DmThread {
                id: id.to_string(),
                peer: id.to_string(),
                messages: Vec::new(),
            });
        }
        self.selected_dm = Some(id.to_string());
    }

    /// Append a message to the identified DM conversation. No-op if the id is
    /// unknown.
    pub fn append_dm(&mut self, id: &str, msg: DmMessage) {
        if let Some(t) = self.dm_threads.iter_mut().find(|t| t.id == id) {
            t.messages.push(msg);
        }
    }

    /// Replace the identified conversation's messages (from a live history
    /// reply). No-op if the id is unknown.
    pub fn set_dm_messages(&mut self, id: &str, messages: Vec<DmMessage>) {
        if let Some(t) = self.dm_threads.iter_mut().find(|t| t.id == id) {
            t.messages = messages;
        }
    }

    /// Fold a live-received DM from `peer`: append to the existing conversation,
    /// or start one (so a first-contact message surfaces immediately).
    pub fn receive_dm(&mut self, peer: &str, msg: DmMessage) {
        match self.dm_threads.iter_mut().find(|t| t.peer == peer) {
            Some(t) => t.messages.push(msg),
            None => self.dm_threads.push(DmThread {
                id: peer.to_string(),
                peer: peer.to_string(),
                messages: vec![msg],
            }),
        }
    }

    /// The currently selected DM conversation, if any.
    pub fn active_dm(&self) -> Option<&DmThread> {
        let id = self.selected_dm.as_deref()?;
        self.dm_threads.iter().find(|t| t.id == id)
    }

    /// Replace the member directory (from a client snapshot).
    pub fn set_members(&mut self, members: Vec<Member>) {
        self.members = members;
    }

    /// Update the directory search query.
    pub fn set_directory_query(&mut self, query: String) {
        self.directory_query = query;
    }

    /// Members matching the current, case-insensitive directory query on
    /// either handle or display name. An empty query matches everyone. The
    /// `online` badge is recomputed from the **live roster** ([`Self::who`]) at
    /// call time, so presence join/leave deltas keep it fresh (the directory
    /// list itself carries no presence flag, and may load before the roster).
    pub fn matching_members(&self) -> Vec<Member> {
        let q = self.directory_query.trim().to_lowercase();
        self.members
            .iter()
            .filter(|m| {
                q.is_empty()
                    || m.handle.to_lowercase().contains(&q)
                    || m.display_name.to_lowercase().contains(&q)
            })
            .map(|m| Member {
                online: self.who.iter().any(|p| p.screen_name == m.handle),
                ..m.clone()
            })
            .collect()
    }

    /// Show a member's profile card by handle.
    pub fn select_member(&mut self, handle: &str) {
        self.selected_member = Some(handle.to_string());
        // A live profile card loads asynchronously; clear the previous one so a
        // stale card never shows under a new selection.
        self.selected_profile = None;
    }

    /// Store a fetched live profile card.
    pub fn set_profile(&mut self, profile: MemberProfile) {
        self.selected_profile = Some(profile);
    }

    /// Attach a fetched avatar `data:` URL to the selected profile card, but
    /// only if the blob's `hex` id still matches the selected profile's avatar
    /// (a late reply from a previously-selected member is dropped — otherwise it
    /// would paint the wrong face, persistently if the new member has none).
    pub fn set_avatar_src(&mut self, hex: &str, src: String) {
        if let Some(p) = &mut self.selected_profile {
            if p.avatar_hex.as_deref() == Some(hex) {
                p.avatar_src = Some(src);
            }
        }
    }

    /// The member whose profile card is shown, if any.
    pub fn active_member(&self) -> Option<&Member> {
        let handle = self.selected_member.as_deref()?;
        self.members.iter().find(|m| m.handle == handle)
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
    fn operator_notice_lands_in_the_scrollback_marked() {
        let mut s = UiState::default();
        s.push_notice("rabbit", "server restarts at midnight");
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].from, "! rabbit");
        assert_eq!(s.messages[0].text, "server restarts at midnight");
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
    fn conn_state_tracks_events_and_pending_overrides() {
        let mut s = UiState::default();
        assert_eq!(s.conn, ConnState::Offline);
        s.apply(&Event::Connected {
            server_name: "x".into(),
            server_version: "1".into(),
        });
        assert_eq!(s.conn, ConnState::Online);
        // A transport-driven pending state surfaces on the status line.
        s.set_conn(ConnState::Reconnecting);
        assert_eq!(s.conn, ConnState::Reconnecting);
        assert!(s.status.contains("Reconnecting"));
        s.apply(&Event::Disconnected {
            reason: "bye".into(),
        });
        assert_eq!(s.conn, ConnState::Offline);
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
    fn select_board_loads_threads_and_resets_open_thread() {
        let mut s = UiState::default();
        s.open_thread("7".into(), vec![]);
        assert_eq!(s.selected_thread.as_deref(), Some("7"));
        let threads = vec![Thread {
            id: "1".into(),
            board: "general".into(),
            title: "hello".into(),
            author: "rabbit".into(),
        }];
        s.select_board("general", threads.clone());
        assert_eq!(s.selected_board.as_deref(), Some("general"));
        assert_eq!(s.threads, threads);
        assert_eq!(s.selected_thread, None);
        assert!(s.posts.is_empty());
    }

    #[test]
    fn open_thread_loads_posts() {
        let mut s = UiState::default();
        let posts = vec![Post {
            id: "10".into(),
            thread: "1".into(),
            author: "alice".into(),
            body: "first".into(),
        }];
        s.open_thread("1".into(), posts.clone());
        assert_eq!(s.selected_thread.as_deref(), Some("1"));
        assert_eq!(s.posts, posts);
    }

    #[test]
    fn set_avatar_src_only_attaches_to_the_matching_profile() {
        let mut s = UiState::default();
        // A profile whose avatar hex is "aa".
        s.set_profile(MemberProfile {
            screen_name: "alice".into(),
            avatar_hex: Some("aa".into()),
            ..Default::default()
        });
        // A late blob reply for a *different* hex is dropped.
        s.set_avatar_src("bb", "data:img-b".into());
        assert_eq!(s.selected_profile.as_ref().unwrap().avatar_src, None);
        // The matching one attaches.
        s.set_avatar_src("aa", "data:img-a".into());
        assert_eq!(
            s.selected_profile.as_ref().unwrap().avatar_src.as_deref(),
            Some("data:img-a")
        );
        // A profile with no avatar never gets one painted on.
        s.set_profile(MemberProfile {
            screen_name: "bob".into(),
            avatar_hex: None,
            ..Default::default()
        });
        s.set_avatar_src("aa", "data:img-a".into());
        assert_eq!(s.selected_profile.as_ref().unwrap().avatar_src, None);
    }

    #[test]
    fn dm_append_targets_selected_thread() {
        let mut s = UiState::default();
        s.set_dm_threads(vec![
            DmThread {
                id: "a".into(),
                peer: "alice".into(),
                messages: vec![],
            },
            DmThread {
                id: "b".into(),
                peer: "bob".into(),
                messages: vec![],
            },
        ]);
        s.select_dm("b");
        s.append_dm(
            "b",
            DmMessage {
                from: "kevin".into(),
                text: "yo".into(),
            },
        );
        assert_eq!(s.active_dm().unwrap().peer, "bob");
        assert_eq!(s.active_dm().unwrap().messages.len(), 1);
        // The other conversation is untouched.
        assert_eq!(s.dm_threads[0].messages.len(), 0);
    }

    #[test]
    fn dm_append_to_unknown_id_is_noop() {
        let mut s = UiState::default();
        s.set_dm_threads(vec![DmThread {
            id: "a".into(),
            peer: "alice".into(),
            messages: vec![],
        }]);
        s.append_dm(
            "missing",
            DmMessage {
                from: "x".into(),
                text: "y".into(),
            },
        );
        assert_eq!(s.dm_threads[0].messages.len(), 0);
    }

    #[test]
    fn directory_search_filters_by_handle_and_name() {
        let mut s = UiState::default();
        s.set_members(vec![
            Member {
                handle: "alice".into(),
                display_name: "Alice Down".into(),
                bio: "".into(),
                online: true,
            },
            Member {
                handle: "bob".into(),
                display_name: "Bob Hutch".into(),
                bio: "".into(),
                online: false,
            },
        ]);
        // Empty query matches everyone.
        assert_eq!(s.matching_members().len(), 2);
        // Handle match, case-insensitive.
        s.set_directory_query("ALI".into());
        let m = s.matching_members();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].handle, "alice");
        // Display-name match.
        s.set_directory_query("hutch".into());
        let m = s.matching_members();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].handle, "bob");
        // No match.
        s.set_directory_query("zzz".into());
        assert!(s.matching_members().is_empty());
    }

    #[test]
    fn select_member_exposes_profile() {
        let mut s = UiState::default();
        s.set_members(vec![Member {
            handle: "alice".into(),
            display_name: "Alice".into(),
            bio: "hi".into(),
            online: true,
        }]);
        assert!(s.active_member().is_none());
        s.select_member("alice");
        assert_eq!(s.active_member().unwrap().bio, "hi");
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
