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
use rabbithole_proto::admin::{
    AccountEntry, AccountList, ClassEntry, ClassList, ConfigApplied, ConfigValue, InviteCode,
};
use rabbithole_proto::filelib::{
    AreaList, FileAdded, FileAreaView, FileContent, FileNodeView, NodeList, NodeReply,
};
use rabbithole_proto::session::ServerNotice;
use rabbithole_proto::transfer::{FileChunk, TransferTicket};
use rabbithole_proto::{Frame, Message, RequestId};

use crate::files::{KIND_FILE, KIND_FOLDER};
use crate::state::{derive_server_name, Board, DmMessage, DmThread, Member, Post, Thread};
use crate::wire::{
    frame_to_admin_events, frame_to_file_events, frame_to_notice_route, AdminCommand, AdminEvent,
    FileCommand, FileEvent, NoticeRoute,
};

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
    file_areas: Vec<FileAreaView>,
    file_nodes: Vec<FileNodeView>,
    admin_classes: Vec<ClassEntry>,
    admin_accounts: Vec<AccountEntry>,
    admin_config: Vec<(String, String)>,
    /// Seeded radio-bridge `ServerNotice` texts (see [`crate::radio`] for the
    /// format), served through [`MockClient::radio_routes`] so the Radio view
    /// renders in dev without a live server.
    radio_notices: Vec<String>,
    invite_seq: u32,
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
            .field("file_areas", &self.file_areas)
            .field("file_nodes", &self.file_nodes)
            .field("admin_classes", &self.admin_classes)
            .field("admin_accounts", &self.admin_accounts)
            .field("admin_config", &self.admin_config)
            .field("radio_notices", &self.radio_notices)
            .field("invite_seq", &self.invite_seq)
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
            file_areas: Self::seeded_file_areas(),
            file_nodes: Self::seeded_file_nodes(),
            admin_classes: Self::seeded_classes(),
            admin_accounts: Self::seeded_accounts(),
            admin_config: Self::seeded_config(),
            radio_notices: Self::seeded_radio_notices(),
            invite_seq: 0,
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

    fn seeded_file_areas() -> Vec<FileAreaView> {
        vec![
            FileAreaView::new("warez", "Warez", "Utilities and demos for the warren."),
            FileAreaView::new("art", "ANSI Gallery", "CP437 art and loaders."),
        ]
    }

    fn seeded_file_nodes() -> Vec<FileNodeView> {
        let file = |id, area: &str, name: &str, path: &str, size, mime: &str, comment: &str| {
            let mut n = FileNodeView::new(id, area, KIND_FILE, name, path);
            n.size = size;
            n.mime = mime.into();
            n.comment = comment.into();
            n.uploader = "rabbit".into();
            n.blob_id = Some([0u8; 32]);
            n
        };
        vec![
            FileNodeView::new(1, "warez", KIND_FOLDER, "utils", "utils"),
            file(
                2,
                "warez",
                "readme.txt",
                "readme.txt",
                734,
                "text/plain",
                "Start here.",
            ),
            file(
                3,
                "warez",
                "lister.lha",
                "utils/lister.lha",
                40_960,
                "application/x-lzh",
                "Classic file lister.",
            ),
            file(
                4,
                "art",
                "welcome.ans",
                "welcome.ans",
                2_048,
                "text/x-ansi",
                "Warren welcome screen.",
            ),
        ]
    }

    /// The next free node id (max existing + 1).
    fn next_node_id(&self) -> i64 {
        self.file_nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1
    }

    /// Serve a file-library [`FileCommand`] from the in-memory library.
    ///
    /// Replies are built as real FILE-family [`Frame`]s from seeded data and
    /// decoded back through [`frame_to_file_events`], so the mock exercises the
    /// exact host-tested wire mapping the browser transport uses — no parallel
    /// decode path.
    pub fn dispatch_file(&mut self, command: FileCommand) -> Vec<FileEvent> {
        match command {
            FileCommand::ListAreas => file_events(&AreaList::new(self.file_areas.clone())),
            FileCommand::ListFolder { area, path } => {
                let want = path.unwrap_or_default();
                let nodes: Vec<FileNodeView> = self
                    .file_nodes
                    .iter()
                    .filter(|n| n.area == area && parent_path(&n.path) == want)
                    .cloned()
                    .collect();
                file_events(&NodeList::new(nodes))
            }
            FileCommand::GetNode { id } => match self.file_nodes.iter().find(|n| n.id == id) {
                Some(n) => file_events(&NodeReply::new(n.clone())),
                None => vec![FileEvent::Failed(format!("no node #{id}"))],
            },
            FileCommand::Download { id } => match self.file_nodes.iter().find(|n| n.id == id) {
                Some(n) => {
                    let bytes = vec![0u8; n.size.max(0) as usize];
                    file_events(&FileContent::new(n.clone(), bytes))
                }
                None => vec![FileEvent::Failed(format!("no node #{id}"))],
            },
            FileCommand::Upload {
                area,
                parent,
                name,
                mime,
                comment,
                bytes,
            } => {
                let id = self.next_node_id();
                let path = match &parent {
                    Some(p) if !p.is_empty() => format!("{p}/{name}"),
                    _ => name.clone(),
                };
                let mut node = FileNodeView::new(id, area.clone(), KIND_FILE, name, path);
                node.size = bytes.len() as i64;
                node.mime = mime;
                node.comment = comment;
                node.uploader = self
                    .current_user
                    .clone()
                    .unwrap_or_else(|| "me".to_string());
                node.blob_id = Some([0u8; 32]);
                self.file_nodes.push(node.clone());
                let mut events = file_events(&NodeReply::new(node));
                events.extend(file_events(&FileAdded::new(area, id)));
                events
            }
            FileCommand::OpenDownload { node_id } => {
                match self.file_nodes.iter().find(|n| n.id == node_id) {
                    Some(n) => {
                        let size = n.size.max(0) as u64;
                        // Mock: reuse the node id as the transfer id so the
                        // queue can name the transfer from the loaded listing.
                        let ticket = TransferTicket::new(node_id as u64, [0; 32], size, [0; 16])
                            .with_server_have(0);
                        file_events(&ticket)
                    }
                    None => vec![FileEvent::Failed(format!("no node #{node_id}"))],
                }
            }
            FileCommand::RequestChunk {
                transfer_id,
                offset,
                len,
            } => file_events(&FileChunk::new(
                transfer_id,
                offset,
                true,
                vec![0u8; len as usize],
            )),
            FileCommand::AbortTransfer { .. } => Vec::new(),
        }
    }

    fn seeded_classes() -> Vec<ClassEntry> {
        vec![
            ClassEntry::new("admin", 0xFFFF_FFFF_FFFF_FFFF, 1),
            ClassEntry::new("staff", 0x0000_0000_00FF_FFFF, 3),
            ClassEntry::new("member", 0x0000_0000_0000_00FF, 128),
        ]
    }

    fn seeded_accounts() -> Vec<AccountEntry> {
        vec![
            AccountEntry::new(1, "rabbit", 2, Some("admin".into()), false),
            AccountEntry::new(2, "alice", 1, Some("member".into()), false),
            AccountEntry::new(3, "bob", 1, Some("member".into()), false),
            AccountEntry::new(4, "spammer", 0, Some("member".into()), true),
        ]
    }

    fn seeded_config() -> Vec<(String, String)> {
        vec![
            ("server.name".to_string(), "Rabbit Lobby".to_string()),
            (
                "server.motd".to_string(),
                "Welcome to the warren.".to_string(),
            ),
            ("registration.mode".to_string(), "invite".to_string()),
            ("chat.slowmode_secs".to_string(), "0".to_string()),
        ]
    }

    /// Serve an [`AdminCommand`] from the in-memory admin console.
    ///
    /// Replies that carry a payload (`ClassList`, `AccountList`, `ConfigValue`,
    /// `ConfigApplied`, `InviteCode`) are built as real ADMIN-family [`Frame`]s
    /// and decoded back through [`frame_to_admin_events`], so the mock exercises
    /// the exact host-tested wire mapping the browser transport uses. Commands
    /// whose server reply is an empty ack (`SetClass`, `SetAccount`,
    /// `Broadcast`, `Kick`) mutate the seeded state and synthesise an
    /// [`AdminEvent::Ack`] for the console status line.
    pub fn dispatch_admin(&mut self, command: AdminCommand) -> Vec<AdminEvent> {
        match command {
            AdminCommand::ListClasses => admin_events(&ClassList::new(self.admin_classes.clone())),
            AdminCommand::SetClass { name, base_mask } => {
                if let Some(c) = self.admin_classes.iter_mut().find(|c| c.name == name) {
                    c.base_mask = base_mask;
                } else {
                    self.admin_classes
                        .push(ClassEntry::new(&name, base_mask, 0));
                }
                vec![AdminEvent::Ack(format!("Class {name} saved."))]
            }
            AdminCommand::ListAccounts { offset, limit } => {
                let total = self.admin_accounts.len() as u64;
                let page: Vec<AccountEntry> = self
                    .admin_accounts
                    .iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .cloned()
                    .collect();
                admin_events(&AccountList::new(page, total))
            }
            AdminCommand::SetAccount {
                login,
                role,
                class,
                disabled,
            } => match self.admin_accounts.iter_mut().find(|a| a.login == login) {
                Some(a) => {
                    if let Some(r) = role {
                        a.role = r;
                    }
                    if let Some(c) = class {
                        a.class = Some(c);
                    }
                    if let Some(d) = disabled {
                        a.disabled = d;
                    }
                    vec![AdminEvent::Ack(format!("Account {login} updated."))]
                }
                None => vec![AdminEvent::Failed(format!("no account {login}"))],
            },
            AdminCommand::CreateInvite { ttl_secs } => {
                self.invite_seq += 1;
                let code = format!("WARREN-{:04}", self.invite_seq);
                admin_events(&InviteCode::new(code, ttl_secs))
            }
            AdminCommand::Broadcast { text } => {
                vec![AdminEvent::Ack(format!("Broadcast sent: {text}"))]
            }
            AdminCommand::Kick { session_id } => {
                vec![AdminEvent::Ack(format!("Kicked session #{session_id}."))]
            }
            AdminCommand::GetConfig { key } => {
                match self.admin_config.iter().find(|(k, _)| *k == key) {
                    Some((k, v)) => admin_events(&ConfigValue::new(k.clone(), v.clone())),
                    None => vec![AdminEvent::Failed(format!("no config key {key}"))],
                }
            }
            AdminCommand::SetConfig { key, value } => {
                if let Some(entry) = self.admin_config.iter_mut().find(|(k, _)| *k == key) {
                    entry.1 = value;
                } else {
                    self.admin_config.push((key.clone(), value));
                }
                // Listener addresses need a restart; everything else is live.
                let live = !key.starts_with("listen.");
                admin_events(&ConfigApplied::new(live))
            }
        }
    }

    /// The radio-bridge notices a fresh session is seeded with: one live-DJ
    /// station and one automation station, so the Radio view and status-bar
    /// segment render in dev.
    fn seeded_radio_notices() -> Vec<String> {
        vec![
            "[radio] live|live|7|Robin|The Lagomorphs|Down the Hole".to_string(),
            "[radio] ambient|auto|3|rotation||Warren Dawn".to_string(),
        ]
    }

    /// Route the seeded radio notices exactly as the transport would: each is
    /// framed as a real [`ServerNotice`] push and decoded back through the
    /// host-tested [`frame_to_notice_route`] — no parallel decode path.
    pub fn radio_routes(&self) -> Vec<NoticeRoute> {
        self.radio_notices
            .iter()
            .filter_map(|text| Frame::push(&ServerNotice::new(text.clone(), "radio")).ok())
            .filter_map(|frame| frame_to_notice_route(&frame))
            .collect()
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

/// Build [`FileEvent`]s from a seeded FILE-family message by round-tripping it
/// through a [`Frame`] and [`frame_to_file_events`].
fn file_events<M: Message>(msg: &M) -> Vec<FileEvent> {
    match Frame::request(RequestId(0), msg) {
        Ok(frame) => frame_to_file_events(&frame),
        Err(err) => vec![FileEvent::Failed(format!("encode: {err}"))],
    }
}

/// Build [`AdminEvent`]s from a seeded ADMIN-family message by round-tripping
/// it through a [`Frame`] and [`frame_to_admin_events`].
fn admin_events<M: Message>(msg: &M) -> Vec<AdminEvent> {
    match Frame::request(RequestId(0), msg) {
        Ok(frame) => frame_to_admin_events(&frame),
        Err(err) => vec![AdminEvent::Failed(format!("encode: {err}"))],
    }
}

/// The parent folder path of `path` (`""` for a root-level node).
fn parent_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
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
    fn file_list_areas_returns_seeded_areas() {
        let c = MockClient::new();
        let ev = c.clone().dispatch_file(FileCommand::ListAreas);
        match ev.as_slice() {
            [FileEvent::AreasListed(areas)] => {
                assert_eq!(areas.len(), 2);
                assert!(areas.iter().any(|a| a.slug == "warez"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn file_list_folder_filters_by_parent() {
        let mut c = MockClient::new();
        // Root of "warez": the utils folder + readme.txt (not the nested lha).
        let root = c.dispatch_file(FileCommand::ListFolder {
            area: "warez".into(),
            path: None,
        });
        let [FileEvent::FolderListed { nodes }] = root.as_slice() else {
            panic!("expected a folder listing");
        };
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().any(|n| n.name == "utils"));
        assert!(nodes.iter().any(|n| n.name == "readme.txt"));

        // Inside utils: just the nested archive.
        let sub = c.dispatch_file(FileCommand::ListFolder {
            area: "warez".into(),
            path: Some("utils".into()),
        });
        let [FileEvent::FolderListed { nodes }] = sub.as_slice() else {
            panic!("expected a folder listing");
        };
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "lister.lha");
    }

    #[test]
    fn file_download_yields_content_sized_to_node() {
        let mut c = MockClient::new();
        let ev = c.dispatch_file(FileCommand::Download { id: 3 });
        match ev.as_slice() {
            [FileEvent::FileDownloaded { node, size }] => {
                assert_eq!(node.id, 3);
                assert_eq!(*size, 40_960);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn file_upload_adds_node_and_announces_it() {
        let mut c = connect_and_sign_in("kevin");
        let ev = c.dispatch_file(FileCommand::Upload {
            area: "warez".into(),
            parent: None,
            name: "hi.txt".into(),
            mime: "text/plain".into(),
            comment: "mine".into(),
            bytes: vec![1, 2, 3, 4],
        });
        assert!(ev
            .iter()
            .any(|e| matches!(e, FileEvent::NodeUpdated(n) if n.name == "hi.txt" && n.uploader == "kevin")));
        assert!(ev.iter().any(|e| matches!(e, FileEvent::FileAdded { .. })));
        // The new node is now listed at the root.
        let root = c.dispatch_file(FileCommand::ListFolder {
            area: "warez".into(),
            path: None,
        });
        let [FileEvent::FolderListed { nodes }] = root.as_slice() else {
            panic!("expected a folder listing");
        };
        assert!(nodes.iter().any(|n| n.name == "hi.txt"));
    }

    #[test]
    fn file_open_download_and_chunk_drive_a_transfer() {
        let mut c = MockClient::new();
        let opened = c.dispatch_file(FileCommand::OpenDownload { node_id: 4 });
        let [FileEvent::TransferOpened {
            transfer_id, size, ..
        }] = opened.as_slice()
        else {
            panic!("expected a ticket");
        };
        assert_eq!(*transfer_id, 4);
        assert_eq!(*size, 2_048);
        let chunk = c.dispatch_file(FileCommand::RequestChunk {
            transfer_id: 4,
            offset: 0,
            len: 2_048,
        });
        assert!(matches!(
            chunk.as_slice(),
            [FileEvent::ChunkReceived { last: true, .. }]
        ));
    }

    #[test]
    fn file_get_unknown_node_fails() {
        let mut c = MockClient::new();
        let ev = c.dispatch_file(FileCommand::GetNode { id: 999 });
        assert!(matches!(ev.as_slice(), [FileEvent::Failed(_)]));
    }

    #[test]
    fn admin_list_classes_returns_seeded_classes() {
        let mut c = MockClient::new();
        let ev = c.dispatch_admin(AdminCommand::ListClasses);
        match ev.as_slice() {
            [AdminEvent::ClassesListed(classes)] => {
                assert_eq!(classes.len(), 3);
                assert!(classes.iter().any(|c| c.name == "admin"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn admin_list_accounts_paginates_with_total() {
        let mut c = MockClient::new();
        let ev = c.dispatch_admin(AdminCommand::ListAccounts {
            offset: 1,
            limit: 2,
        });
        match ev.as_slice() {
            [AdminEvent::AccountsListed { accounts, total }] => {
                assert_eq!(*total, 4);
                assert_eq!(accounts.len(), 2);
                assert_eq!(accounts[0].login, "alice");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn admin_set_account_mutates_and_acks() {
        let mut c = MockClient::new();
        let ev = c.dispatch_admin(AdminCommand::SetAccount {
            login: "alice".into(),
            role: Some(2),
            class: None,
            disabled: Some(true),
        });
        assert!(matches!(ev.as_slice(), [AdminEvent::Ack(_)]));
        // The mutation persists and shows up in a subsequent listing.
        let listed = c.dispatch_admin(AdminCommand::ListAccounts {
            offset: 0,
            limit: 50,
        });
        let [AdminEvent::AccountsListed { accounts, .. }] = listed.as_slice() else {
            panic!("expected an account listing");
        };
        let alice = accounts.iter().find(|a| a.login == "alice").unwrap();
        assert_eq!(alice.role, 2);
        assert!(alice.disabled);
    }

    #[test]
    fn admin_set_unknown_account_fails() {
        let mut c = MockClient::new();
        let ev = c.dispatch_admin(AdminCommand::SetAccount {
            login: "ghost".into(),
            role: Some(1),
            class: None,
            disabled: None,
        });
        assert!(matches!(ev.as_slice(), [AdminEvent::Failed(_)]));
    }

    #[test]
    fn admin_config_get_set_roundtrip() {
        let mut c = MockClient::new();
        let got = c.dispatch_admin(AdminCommand::GetConfig {
            key: "server.name".into(),
        });
        assert!(matches!(
            got.as_slice(),
            [AdminEvent::ConfigLoaded { value, .. }] if value == "Rabbit Lobby"
        ));
        // Setting a non-listener key applies live.
        let set = c.dispatch_admin(AdminCommand::SetConfig {
            key: "server.name".into(),
            value: "New Warren".into(),
        });
        assert!(matches!(
            set.as_slice(),
            [AdminEvent::ConfigApplied { applied_live: true }]
        ));
        // A listener key needs a restart.
        let listen = c.dispatch_admin(AdminCommand::SetConfig {
            key: "listen.ws".into(),
            value: "0.0.0.0:9000".into(),
        });
        assert!(matches!(
            listen.as_slice(),
            [AdminEvent::ConfigApplied {
                applied_live: false
            }]
        ));
        // The updated value reads back.
        let got = c.dispatch_admin(AdminCommand::GetConfig {
            key: "server.name".into(),
        });
        assert!(matches!(
            got.as_slice(),
            [AdminEvent::ConfigLoaded { value, .. }] if value == "New Warren"
        ));
    }

    #[test]
    fn admin_create_invite_mints_unique_codes() {
        let mut c = MockClient::new();
        let first = c.dispatch_admin(AdminCommand::CreateInvite { ttl_secs: 3600 });
        let second = c.dispatch_admin(AdminCommand::CreateInvite { ttl_secs: 3600 });
        let code = |ev: &[AdminEvent]| match ev {
            [AdminEvent::InviteCreated(code)] => code.code.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        assert_ne!(code(&first), code(&second));
    }

    #[test]
    fn admin_broadcast_and_kick_ack() {
        let mut c = MockClient::new();
        assert!(matches!(
            c.dispatch_admin(AdminCommand::Broadcast { text: "hi".into() })
                .as_slice(),
            [AdminEvent::Ack(_)]
        ));
        assert!(matches!(
            c.dispatch_admin(AdminCommand::Kick { session_id: 9 })
                .as_slice(),
            [AdminEvent::Ack(_)]
        ));
    }

    #[test]
    fn seeded_radio_notices_route_to_the_radio_reducer() {
        use crate::radio::RadioState;

        let c = MockClient::new();
        let routes = c.radio_routes();
        assert_eq!(routes.len(), 2);
        let mut radio = RadioState::default();
        for route in routes {
            match route {
                NoticeRoute::Radio(update) => radio.apply_update(update),
                other => panic!("seeded notice routed to chat: {other:?}"),
            }
        }
        let slugs: Vec<&str> = radio.stations().map(|s| s.station.as_str()).collect();
        assert_eq!(slugs, ["ambient", "live"]);
        // The live-DJ station is featured over automation.
        assert_eq!(radio.on_air().unwrap().station, "live");
        assert!(matches!(
            radio.get("ambient"),
            Some(s) if !s.live && s.listeners == 3
        ));
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
