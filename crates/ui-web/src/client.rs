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
    origin_trust, peer_state, report_action, report_state, subject_kind, AccountEntry, AccountList,
    AuditEntry, AuditList, BackupEntry, BackupList, BackupMade, BackupVerified, ClassEntry,
    ClassList, ConfigApplied, ConfigValue, DenyHashEntry, DenyHashList, FeedStat, GatewayStat,
    GatewayStatsReply, InviteCode, OriginEntry, OriginList, PeerEntry, PeerList, ReportEntry,
    ReportList, ThemeBundleInfo,
};
use rabbithole_proto::filelib::{
    AreaList, FileAdded, FileAreaView, FileContent, FileNodeView, NodeList, NodeReply,
};
use rabbithole_proto::radio::RadioNowPlaying;
use rabbithole_proto::transfer::{FileChunk, TransferTicket};
use rabbithole_proto::welcome::ThemeBundle;
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

    /// The welcome screen this client would have received on connect. Only the
    /// demo seam has one (a live session gets its widgets over the wire), so
    /// the default is empty.
    fn demo_welcome_widgets(&self) -> Vec<rabbithole_proto::welcome::WelcomeWidget> {
        Vec::new()
    }

    /// Threads belonging to the board identified by `slug`.
    fn threads(&self, slug: &str) -> Vec<Thread>;

    /// Posts belonging to the thread identified by `thread_id`.
    fn posts(&self, thread_id: &str) -> Vec<Post>;

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
    /// The whole tree, categories included, for the admin console.
    board_tree: Vec<crate::state::BoardNode>,
    threads: Vec<Thread>,
    posts: Vec<Post>,
    members: Vec<Member>,
    dm_threads: Vec<DmThread>,
    file_areas: Vec<FileAreaView>,
    file_nodes: Vec<FileNodeView>,
    admin_classes: Vec<ClassEntry>,
    admin_accounts: Vec<AccountEntry>,
    admin_invites: Vec<rabbithole_proto::admin::InviteEntry>,
    admin_reports: Vec<ReportEntry>,
    admin_deny: Vec<DenyHashEntry>,
    admin_audit: Vec<AuditEntry>,
    admin_peers: Vec<PeerEntry>,
    admin_origins: Vec<OriginEntry>,
    admin_backups: Vec<BackupEntry>,
    admin_config: Vec<(String, String)>,
    /// Seeded RADIO now-playing frames, served through
    /// [`MockClient::radio_routes`] so the Radio view renders in dev without a
    /// live server.
    radio_frames: Vec<Frame>,
    invite_seq: u32,
    /// Which seeded burrow this session is playing.
    demo: DemoBurrow,
    /// Sink registered through the async [`EventClient`] seam, if any. Skipped
    /// by [`Debug`] (closures are not `Debug`).
    sink: Option<crate::wire::EventSink>,
}

/// The seeded file whose download always fails — so the failure path (reason,
/// sources tried, Retry) can be exercised in the demo. Named for what it does.
pub const FAILING_DEMO_FILE: &str = "broken-mirror.lha";

/// A seeded demo burrow: enough personality that switching between two of them
/// is visibly switching *places*, not re-rendering the same fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoBurrow {
    /// Display name — what the header and rail tile show.
    pub name: &'static str,
    /// The pseudo-endpoint its session is keyed by (`demo://…`).
    pub endpoint: &'static str,
    /// Message of the day.
    pub motd: &'static str,
    /// The operator's featured item: title, then body.
    pub featured: (&'static str, &'static str),
    /// One-line ticker.
    pub ticker: &'static str,
    /// Who's on.
    pub who: &'static [&'static str],
}

/// The demo warren. Two burrows, because the whole point of this app is being
/// in more than one place at once.
pub const DEMO_BURROWS: &[DemoBurrow] = &[
    DemoBurrow {
        name: "The Warren",
        endpoint: "demo://the-warren",
        motd: "Welcome to the Warren. Be kind, share freely, and mind the carrots.",
        featured: (
            "Tonight: Demoscene Night",
            "Fresh uploads in /demos \u{2014} 40 packs from the Amiga era, all seeded by the swarm.",
        ),
        ticker: "New boards open \u{00b7} Uploads are drag-and-drop \u{00b7} Say hello in the lobby",
        who: &["rabbit", "alice", "bob"],
    },
    DemoBurrow {
        name: "Night Pool",
        endpoint: "demo://night-pool",
        motd: "The Night Pool: slow chat, long files, no hurry.",
        featured: (
            "Archive drive underway",
            "Help us seed the 1993 shareware shelf \u{2014} 12 GB and climbing.",
        ),
        ticker: "Quiet hours 02:00\u{2013}06:00 \u{00b7} Be excellent",
        who: &["maria", "kim", "rabbit"],
    },
];

impl DemoBurrow {
    /// The welcome screen this burrow would send on connect — the same widget
    /// shapes a real burrow's `WelcomeScreen` carries, so the news panel is
    /// exercised by the demo exactly as it is live.
    pub fn welcome_widgets(&self) -> Vec<rabbithole_proto::welcome::WelcomeWidget> {
        use rabbithole_proto::welcome::WelcomeWidget;
        vec![
            WelcomeWidget::Motd(self.motd.to_string()),
            WelcomeWidget::OnlineNow {
                count: self.who.len() as u32,
                sample: self.who.iter().map(|w| w.to_string()).collect(),
            },
            WelcomeWidget::Featured {
                title: self.featured.0.to_string(),
                body: self.featured.1.to_string(),
            },
            WelcomeWidget::Ticker(self.ticker.to_string()),
        ]
    }
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
            .field("radio_frames", &self.radio_frames)
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
    /// The whole board tree, categories included.
    pub fn board_tree(&self) -> Vec<crate::state::BoardNode> {
        self.board_tree.clone()
    }

    fn threads_by_board(&self, slug: &str) -> bool {
        self.threads.iter().any(|t| t.board == slug)
    }

    /// A fresh, disconnected mock with a seeded member list, board tree, DM
    /// conversations and directory.
    pub fn new() -> Self {
        Self::named(&DEMO_BURROWS[0])
    }

    /// A demo burrow with a specific identity. Multiple mock sessions are what
    /// make the warren layer testable — one mock server can't exercise burrow
    /// switching, per-burrow unread, or "known from" across places.
    pub fn named(demo: &DemoBurrow) -> Self {
        Self {
            connected: false,
            signed_in: false,
            server_name: demo.name.to_string(),
            demo: *demo,
            current_user: None,
            who: demo.who.iter().map(|w| w.to_string()).collect(),
            boards: Self::seeded_boards(),
            board_tree: Self::seeded_board_tree(),
            threads: Self::seeded_threads(),
            posts: Self::seeded_posts(),
            members: Self::seeded_members(),
            dm_threads: Self::seeded_dms(),
            file_areas: Self::seeded_file_areas(),
            file_nodes: Self::seeded_file_nodes(),
            admin_classes: Self::seeded_classes(),
            admin_accounts: Self::seeded_accounts(),
            admin_invites: Self::seeded_invites(),
            admin_reports: Self::seeded_reports(),
            admin_deny: Vec::new(),
            admin_audit: Self::seeded_audit(),
            admin_peers: Self::seeded_peers(),
            admin_origins: Self::seeded_origins(),
            admin_backups: Self::seeded_backups(),
            admin_config: Self::seeded_config(),
            radio_frames: Self::seeded_radio_frames(),
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
                unread: 3,
            },
            Board {
                slug: "tech".to_string(),
                name: "Tech Talk".to_string(),
                description: "Protocols, clients and self-hosting.".to_string(),
                unread: 0,
            },
        ]
    }

    fn seeded_board_tree() -> Vec<crate::state::BoardNode> {
        let node = |slug: &str, title: &str, description: &str, kind: u8, parent: Option<&str>| {
            crate::state::BoardNode {
                slug: slug.to_string(),
                title: title.to_string(),
                description: description.to_string(),
                kind,
                parent: parent.map(str::to_string),
            }
        };
        vec![
            node("commons", "The Commons", "Where everyone starts.", 0, None),
            node(
                "general",
                "General",
                "Warren-wide chatter and announcements.",
                2,
                Some("commons"),
            ),
            node(
                "tech",
                "Tech Talk",
                "Protocols, clients and self-hosting.",
                2,
                Some("commons"),
            ),
        ]
    }

    fn seeded_threads() -> Vec<Thread> {
        vec![
            Thread {
                id: "t1".to_string(),
                board: "general".to_string(),
                title: "Warren rules & etiquette".to_string(),
                author: "rabbit".to_string(),
                replies: 12,
                last_activity_unix_ms: crate::clock::now_ms() - 2 * 3_600_000,
            },
            Thread {
                id: "t2".to_string(),
                board: "general".to_string(),
                title: "Introduce yourself".to_string(),
                author: "alice".to_string(),
                replies: 34,
                last_activity_unix_ms: crate::clock::now_ms() - 26 * 3_600_000,
            },
            Thread {
                id: "t3".to_string(),
                board: "tech".to_string(),
                title: "Running your own burrow".to_string(),
                author: "bob".to_string(),
                replies: 3,
                last_activity_unix_ms: crate::clock::now_ms() - 5 * 86_400_000,
            },
        ]
    }

    fn seeded_posts() -> Vec<Post> {
        vec![
            Post {
                id: "p11".to_string(),
                thread: "t1".to_string(),
                author: "rabbit".to_string(),
                body: "Be excellent to each other. No spam.".to_string(),
                at_unix_ms: 0,
                removed: false,
            },
            Post {
                id: "p12".to_string(),
                thread: "t1".to_string(),
                author: "alice".to_string(),
                body: "Sounds good to me!".to_string(),
                at_unix_ms: 0,
                removed: false,
            },
            Post {
                id: "p21".to_string(),
                thread: "t2".to_string(),
                author: "alice".to_string(),
                body: "Hi, I'm Alice. Long-time lurker.".to_string(),
                at_unix_ms: 0,
                removed: false,
            },
            Post {
                id: "p31".to_string(),
                thread: "t3".to_string(),
                author: "bob".to_string(),
                body: "Here's how I set up my burrow behind NAT.".to_string(),
                at_unix_ms: 0,
                removed: false,
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
        // Seeded a plausible few minutes before "now", so timestamps and
        // grouping read naturally in the offline demo (and stay 0-based,
        // deterministic, on the host where the clock stub returns 0).
        let now = crate::clock::now_ms();
        vec![
            DmThread {
                id: "alice".to_string(),
                peer: "alice".to_string(),
                messages: vec![
                    DmMessage {
                        from: "alice".to_string(),
                        text: "hey, did you see the new board?".to_string(),
                        at_unix_ms: now - 9 * 60_000,
                    },
                    DmMessage {
                        from: "rabbit".to_string(),
                        text: "yep, looks great".to_string(),
                        at_unix_ms: now - 8 * 60_000,
                    },
                ],
                unread: 2,
                last_text: String::new(),
                last_at_unix_ms: 0,
            },
            DmThread {
                id: "bob".to_string(),
                peer: "bob".to_string(),
                messages: vec![DmMessage {
                    from: "bob".to_string(),
                    text: "ping me when you're around".to_string(),
                    at_unix_ms: now - 25 * 60_000,
                }],
                unread: 0,
                last_text: String::new(),
                last_at_unix_ms: 0,
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
            // What the listing says a file weighs is what downloading it
            // gives you: the seeded bytes are real (`crate::demo_files`).
            if let Some(bytes) = crate::demo_files::bytes_for(name) {
                n.size = bytes.len() as i64;
            }
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
                7,
                "warez",
                FAILING_DEMO_FILE,
                FAILING_DEMO_FILE,
                4_194_304,
                "application/x-lzh",
                "Demo: this download always fails, so the failure UI is testable.",
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

    /// The bytes a demo download of `id` delivers, for the save step. `None`
    /// for an unknown node, a folder, or the file that fails on purpose. An
    /// uploaded file has no seeded content and saves as its recorded size in
    /// zeros, which is what the mock stored of it.
    pub fn download_bytes(&self, id: i64) -> Option<crate::wire::DownloadedFile> {
        let n = self.file_nodes.iter().find(|n| n.id == id)?;
        if n.name == FAILING_DEMO_FILE || n.kind != KIND_FILE {
            return None;
        }
        Some(crate::wire::DownloadedFile {
            name: n.name.clone(),
            mime: n.mime.clone(),
            bytes: crate::demo_files::bytes_for(&n.name)
                .unwrap_or_else(|| vec![0u8; n.size.max(0) as usize]),
        })
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
                // One seeded file always fails, so the failure path — the
                // reason, the source count, Retry — is exercisable in the demo
                // without staging a broken swarm. Its name says so.
                Some(n) if n.name == FAILING_DEMO_FILE => vec![FileEvent::TransferFailed {
                    transfer_id: n.id as u64,
                    detail: "no peer could serve 3 of 40 units \u{2014}                              the last source dropped mid-transfer"
                        .into(),
                    sources_tried: 2,
                    retryable: true,
                }],
                Some(n) => {
                    let bytes = crate::demo_files::bytes_for(&n.name)
                        .unwrap_or_else(|| vec![0u8; n.size.max(0) as usize]);
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

    fn seeded_invites() -> Vec<rabbithole_proto::admin::InviteEntry> {
        use rabbithole_proto::admin::InviteEntry;
        let now = crate::clock::now_ms() / 1000;
        vec![
            InviteEntry::new("WARREN-TEA-PARTY", "rabbit", now + 5 * 86_400, None),
            InviteEntry::new(
                "WARREN-LOOKING-GLASS",
                "rabbit",
                now + 86_400,
                Some("dormouse".into()),
            ),
            InviteEntry::new("WARREN-OLD-HAT", "alice", now - 3_600, None),
        ]
    }

    fn seeded_reports() -> Vec<ReportEntry> {
        let now = crate::clock::now_ms() / 1000;
        vec![
            ReportEntry::new(
                7,
                3,
                subject_kind::POST,
                vec![0xab; 32],
                "Spam: the same crypto link in three threads.",
                now - 3_600,
                report_state::OPEN,
                "",
                None,
                "",
            ),
            ReportEntry::new(
                6,
                5,
                subject_kind::USER,
                b"dormouse".to_vec(),
                "Keeps waking people up in the lobby at 3am.",
                now - 86_400,
                report_state::REVIEWING,
                "rabbit",
                None,
                "",
            ),
            ReportEntry::new(
                5,
                3,
                subject_kind::FILE,
                42i64.to_le_bytes().to_vec(),
                "Not what the name says it is.",
                now - 3 * 86_400,
                report_state::RESOLVED,
                "rabbit",
                Some(now - 2 * 86_400),
                "Quarantined and the uploader warned.",
            ),
        ]
    }

    fn seeded_audit() -> Vec<AuditEntry> {
        let now = crate::clock::now_ms() / 1000;
        vec![
            AuditEntry::new(
                now - 7_200,
                "rabbit",
                "config-set",
                "motd=Welcome to the warren.",
            ),
            AuditEntry::new(now - 5_400, "rabbit", "invite-create", "WARREN-TEA-PARTY"),
            AuditEntry::new(now - 3_000, "alice", "report-resolve", "#5 resolve"),
            AuditEntry::new(now - 600, "rabbit", "kick", "session 12"),
        ]
    }

    /// Give a demo node a new name and path; what is below it follows.
    fn relocate_demo_node(&mut self, id: i64, name: &str, path: &str) {
        let Some(node) = self.file_nodes.iter().find(|n| n.id == id).cloned() else {
            return;
        };
        let old_prefix = format!("{}/", node.path);
        for n in self.file_nodes.iter_mut().filter(|n| n.area == node.area) {
            if n.id == id {
                n.name = name.to_string();
                n.path = path.to_string();
            } else if let Some(rest) = n.path.strip_prefix(&old_prefix) {
                n.path = format!("{path}/{rest}");
            }
        }
    }

    fn seeded_peers() -> Vec<PeerEntry> {
        vec![
            PeerEntry::new(
                [0x5a; 32],
                "Grove",
                Some("grove.example".into()),
                Some("203.0.113.7:4655".into()),
                peer_state::PENDING,
                false,
                false,
            ),
            PeerEntry::new(
                [0x7e; 32],
                "Marsh",
                Some("marsh.example".into()),
                Some("198.51.100.3:4655".into()),
                peer_state::CONNECTED,
                true,
                true,
            ),
            PeerEntry::new(
                [0x91; 32],
                "",
                Some("hollow.example".into()),
                None,
                peer_state::DISCONNECTED,
                true,
                false,
            ),
        ]
    }

    fn seeded_origins() -> Vec<OriginEntry> {
        vec![
            OriginEntry::new("marsh.example", [0x7e; 32], origin_trust::DIRECT_PEER),
            OriginEntry::new("orchard.example", [0x33; 32], origin_trust::OPERATOR),
        ]
    }

    fn seeded_backups() -> Vec<BackupEntry> {
        vec![
            BackupEntry::new(
                "snapshot-20260911-031500",
                "2026-09-11T03:15:00Z",
                "0.219.0",
                418,
                1_286_400_000,
            ),
            BackupEntry::new(
                "snapshot-20260918-031500",
                "2026-09-18T03:15:00Z",
                "0.221.0",
                431,
                1_309_100_000,
            ),
        ]
    }

    fn seeded_config() -> Vec<(String, String)> {
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        vec![
            // The operator keys, under the server's own names
            // (`admin::OPERATOR_KEYS`), so the demo console and a live one
            // show the same rows.
            pair("name", "Rabbit Lobby"),
            pair("motd", "Welcome to the warren."),
            pair("agreement", ""),
            pair("registration_mode", "invite"),
            pair("guest_enabled", "true"),
            pair("chat_max_len", "2000"),
            pair("upload_quota_bytes", "1073741824"),
            pair("max_concurrent_transfers", "4"),
            pair("transfer_rate_bytes_per_sec", "0"),
            pair("ws_public_url", ""),
            pair("advertise_host", ""),
            pair("announce_enabled", "true"),
            pair("announce_description", "A seeded demo burrow."),
            pair("announce_sysop", "rabbit"),
            // Gateway + syndication knobs for the Syndication & Gateways
            // panel, mirroring the server's key names and serializations.
            pair("nntp_enabled", "true"),
            pair("nntp_addr", "0.0.0.0:1119"),
            pair("nntp_tls_enabled", "false"),
            pair("nntp_tls_addr", "0.0.0.0:563"),
            pair("nntp_feed_enabled", "false"),
            pair("nntp_feed_addr", "0.0.0.0:1120"),
            pair("nntp_feed_tls_enabled", "false"),
            pair("nntp_feed_tls_addr", "0.0.0.0:1563"),
            pair("ftn_enabled", "false"),
            pair("ftn_addr", "0.0.0.0:24554"),
            pair("qwk_enabled", "true"),
            pair("syndication_enabled", "true"),
            pair("syndication_poll_secs", "1800"),
            // The real server does NOT expose `syndication_feeds` via config
            // get (it is TOML-only and answers NotFound); the mock serves the
            // TOML table body so the dev panel can demonstrate the read-only
            // feeds table. The panel treats it as read-only either way.
            pair(
                "syndication_feeds",
                "\"https://blog.example.org/feed.xml\" = \"general\"\n\
                 \"https://warren.example/atom.xml\" = \"tech\"\n",
            ),
        ]
    }

    fn seeded_gateway_stats() -> GatewayStatsReply {
        GatewayStatsReply {
            generated_at_ms: 1_783_780_507_000,
            feeds: vec![
                FeedStat {
                    url: "https://blog.example.org/feed.xml".into(),
                    last_poll_ms: 1_783_780_507_000,
                    last_status: "ok".into(),
                    items_seen: 14,
                    items_posted: 11,
                    dupes_dropped: 3,
                },
                FeedStat {
                    url: "https://warren.example/atom.xml".into(),
                    last_poll_ms: 1_783_780_200_000,
                    last_status: "not_modified".into(),
                    items_seen: 4,
                    items_posted: 4,
                    dupes_dropped: 0,
                },
            ],
            gateways: vec![
                GatewayStat {
                    name: "nntp".into(),
                    enabled: true,
                    counters: vec![("posts".into(), 3), ("sessions".into(), 8)],
                },
                GatewayStat {
                    name: "qwk".into(),
                    enabled: true,
                    counters: vec![("packets_built".into(), 2), ("replies_ingested".into(), 1)],
                },
                GatewayStat {
                    name: "syndication".into(),
                    enabled: true,
                    counters: vec![("polls".into(), 6)],
                },
            ],
        }
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
            AdminCommand::CreateAccount {
                login,
                password,
                role,
            } => {
                if self.admin_accounts.iter().any(|a| a.login == login) {
                    vec![AdminEvent::Failed("server error: AlreadyExists".into())]
                } else if password.0.chars().count() < 8 || login.contains(char::is_whitespace) {
                    vec![AdminEvent::Failed("server error: BadRequest".into())]
                } else {
                    let id = self.admin_accounts.iter().map(|a| a.id).max().unwrap_or(0) + 1;
                    self.admin_accounts
                        .push(AccountEntry::new(id, &login, role, None, false));
                    vec![AdminEvent::Ack(format!("Account {login} created."))]
                }
            }
            AdminCommand::SetAccountPassword { login, password } => {
                if !self.admin_accounts.iter().any(|a| a.login == login) {
                    vec![AdminEvent::Failed("server error: NotFound".into())]
                } else if password.0.chars().count() < 8 {
                    vec![AdminEvent::Failed("server error: BadRequest".into())]
                } else {
                    vec![AdminEvent::Ack(format!("Password for {login} changed."))]
                }
            }
            // Nobody in the demo burrow has two-factor set up.
            AdminCommand::ResetAccountTotp { .. } => {
                vec![AdminEvent::Failed("server error: NotFound".into())]
            }
            AdminCommand::ListInvites => {
                vec![AdminEvent::InvitesListed(self.admin_invites.clone())]
            }
            AdminCommand::RevokeInvite { code } => {
                let before = self.admin_invites.len();
                self.admin_invites
                    .retain(|i| i.code != code || i.used_by.is_some());
                if self.admin_invites.len() < before {
                    vec![AdminEvent::Ack("Invitation withdrawn.".into())]
                } else {
                    vec![AdminEvent::Failed("server error: NotFound".into())]
                }
            }
            AdminCommand::CreateInvite { ttl_secs } => {
                self.invite_seq += 1;
                let code = format!("WARREN-{:04}", self.invite_seq);
                let expires = crate::clock::now_ms() / 1000 + ttl_secs;
                self.admin_invites.insert(
                    0,
                    rabbithole_proto::admin::InviteEntry::new(&code, "rabbit", expires, None),
                );
                admin_events(&InviteCode::new(code, expires))
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
                // Mirror the server's documented semantics for the gateway
                // and syndication keys the admin panel drives; for keys
                // outside that vocabulary keep the original mock rule
                // (listener addresses need a restart; the rest is live).
                // What a real burrow says about the key (the snapshot); a key
                // no burrow has is assumed live.
                let live = crate::demo_config::applies_live(&key).unwrap_or(true);
                admin_events(&ConfigApplied::new(live))
            }
            AdminCommand::DescribeConfig => {
                vec![AdminEvent::ConfigDescribed(crate::demo_config::describe(
                    &self.admin_config,
                ))]
            }
            AdminCommand::CreateBoard {
                slug,
                title,
                description,
                kind,
                parent,
            } => {
                if self.board_tree.iter().any(|b| b.slug == slug) {
                    vec![AdminEvent::Failed("server error: AlreadyExists".into())]
                } else {
                    if kind == 2 {
                        self.boards.push(Board {
                            slug: slug.clone(),
                            name: title.clone(),
                            description: description.clone(),
                            unread: 0,
                        });
                    }
                    self.board_tree.push(crate::state::BoardNode {
                        slug,
                        title,
                        description,
                        kind,
                        parent,
                    });
                    vec![AdminEvent::Ack("Board created.".into())]
                }
            }
            AdminCommand::UpdateBoard {
                slug,
                title,
                description,
            } => match self.board_tree.iter_mut().find(|b| b.slug == slug) {
                Some(node) => {
                    node.title = title.clone();
                    node.description = description.clone();
                    if let Some(b) = self.boards.iter_mut().find(|b| b.slug == slug) {
                        b.name = title;
                        b.description = description;
                    }
                    vec![AdminEvent::Ack("Board saved.".into())]
                }
                None => vec![AdminEvent::Failed("server error: NotFound".into())],
            },
            AdminCommand::DeleteBoard { slug } => {
                let has_threads = self.threads_by_board(&slug);
                let has_children = self
                    .board_tree
                    .iter()
                    .any(|b| b.parent.as_deref() == Some(&slug));
                if has_threads || has_children {
                    vec![AdminEvent::Failed("server error: BadRequest".into())]
                } else {
                    self.board_tree.retain(|b| b.slug != slug);
                    self.boards.retain(|b| b.slug != slug);
                    vec![AdminEvent::Ack("Board removed.".into())]
                }
            }
            AdminCommand::ListReports { state } => {
                let reports: Vec<ReportEntry> = self
                    .admin_reports
                    .iter()
                    .filter(|r| state.is_none_or(|s| r.state == s))
                    .cloned()
                    .collect();
                let total = reports.len() as u64;
                admin_events(&ReportList::new(reports, total))
            }
            AdminCommand::ResolveReport { id, action, note } => {
                match self.admin_reports.iter_mut().find(|r| r.id == id) {
                    Some(r) => {
                        r.state = match action {
                            report_action::CLAIM => report_state::REVIEWING,
                            report_action::RESOLVE => report_state::RESOLVED,
                            _ => report_state::DISMISSED,
                        };
                        r.resolver = "rabbit".into();
                        r.resolution = note;
                        vec![AdminEvent::Ack("Report updated.".into())]
                    }
                    None => vec![AdminEvent::Failed("server error: NotFound".into())],
                }
            }
            AdminCommand::ListDenyHashes => {
                admin_events(&DenyHashList::new(self.admin_deny.clone()))
            }
            AdminCommand::AddDenyHash { hash, reason } => {
                if self.admin_deny.iter().any(|d| d.hash == hash) {
                    vec![AdminEvent::Failed("server error: AlreadyExists".into())]
                } else {
                    self.admin_deny.push(DenyHashEntry::new(
                        hash,
                        reason,
                        "rabbit",
                        crate::clock::now_ms() / 1000,
                    ));
                    vec![AdminEvent::Ack("Hash denied.".into())]
                }
            }
            AdminCommand::RemoveDenyHash { hash } => {
                let before = self.admin_deny.len();
                self.admin_deny.retain(|d| d.hash != hash);
                if self.admin_deny.len() < before {
                    vec![AdminEvent::Ack("Hash allowed again.".into())]
                } else {
                    vec![AdminEvent::Failed("server error: NotFound".into())]
                }
            }
            AdminCommand::ListAudit { limit } => admin_events(&AuditList::new(
                self.admin_audit
                    .iter()
                    .rev()
                    .take(limit as usize)
                    .rev()
                    .cloned()
                    .collect(),
            )),
            AdminCommand::ListPeers => admin_events(&PeerList::new(self.admin_peers.clone())),
            AdminCommand::ApprovePeer { key, origin } => {
                let acceptable = |o: &str| crate::admin_federation::origin_is_acceptable(o);
                match self.admin_peers.iter_mut().find(|p| p.key == key) {
                    Some(peer) => {
                        let bound = origin.clone().or_else(|| peer.origin.clone());
                        match bound {
                            Some(o)
                                if acceptable(&o)
                                    && peer.origin.as_deref().is_none_or(|known| known == o) =>
                            {
                                peer.origin = Some(o);
                                peer.approved = true;
                                if peer.state == peer_state::PENDING {
                                    peer.state = peer_state::DISCONNECTED;
                                }
                                vec![AdminEvent::Ack("Peer approved.".into())]
                            }
                            _ => vec![AdminEvent::Failed("server error: BadRequest".into())],
                        }
                    }
                    None => match origin {
                        Some(o) if acceptable(&o) => {
                            self.admin_peers.push(PeerEntry::new(
                                key,
                                "",
                                Some(o),
                                None,
                                peer_state::DISCONNECTED,
                                true,
                                false,
                            ));
                            vec![AdminEvent::Ack("Peer approved.".into())]
                        }
                        _ => vec![AdminEvent::Failed("server error: BadRequest".into())],
                    },
                }
            }
            AdminCommand::RevokePeer { key } => {
                match self.admin_peers.iter_mut().find(|p| p.key == key) {
                    Some(peer) if peer.configured => {
                        vec![AdminEvent::Failed("server error: BadRequest".into())]
                    }
                    Some(peer) => {
                        peer.approved = false;
                        peer.state = peer_state::PENDING;
                        vec![AdminEvent::Ack("Peer revoked.".into())]
                    }
                    None => vec![AdminEvent::Failed("server error: NotFound".into())],
                }
            }
            AdminCommand::ListOrigins => admin_events(&OriginList::new(self.admin_origins.clone())),
            AdminCommand::PinOrigin { origin, key } => {
                let same = self
                    .admin_origins
                    .iter()
                    .any(|o| o.origin == origin && o.key == key);
                let clash = self
                    .admin_origins
                    .iter()
                    .any(|o| (o.origin == origin) != (o.key == key));
                if same {
                    vec![AdminEvent::Ack("Origin pinned.".into())]
                } else if clash || !crate::admin_federation::origin_is_acceptable(&origin) {
                    vec![AdminEvent::Failed("server error: BadRequest".into())]
                } else {
                    self.admin_origins
                        .push(OriginEntry::new(origin, key, origin_trust::OPERATOR));
                    vec![AdminEvent::Ack("Origin pinned.".into())]
                }
            }
            AdminCommand::ListBackups => admin_events(&BackupList::new(
                "/srv/burrow/backups",
                self.admin_backups.clone(),
            )),
            AdminCommand::MakeBackup => {
                let (y, mo, d, h, mi, s) =
                    crate::admin_federation::civil_utc(crate::clock::now_ms() / 1000);
                let entry = BackupEntry::new(
                    format!("snapshot-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}"),
                    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"),
                    env!("CARGO_PKG_VERSION"),
                    433,
                    1_311_800_000,
                );
                self.admin_backups.push(entry.clone());
                admin_events(&BackupMade::new(entry))
            }
            AdminCommand::VerifyBackup { name } => {
                match self.admin_backups.iter().find(|b| b.name == name) {
                    Some(b) => admin_events(&BackupVerified::new(
                        name,
                        true,
                        "ok",
                        b.files,
                        b.total_bytes,
                    )),
                    None => vec![AdminEvent::Failed("server error: NotFound".into())],
                }
            }
            AdminCommand::DeleteBackup { name } => {
                let before = self.admin_backups.len();
                self.admin_backups.retain(|b| b.name != name);
                if self.admin_backups.len() < before {
                    vec![AdminEvent::Ack("Snapshot removed.".into())]
                } else {
                    vec![AdminEvent::Failed("server error: NotFound".into())]
                }
            }
            AdminCommand::CreateArea {
                slug,
                title,
                description,
            } => {
                if self.file_areas.iter().any(|a| a.slug == slug) {
                    vec![AdminEvent::Failed("server error: AlreadyExists".into())]
                } else {
                    self.file_areas
                        .push(FileAreaView::new(slug, title, description));
                    vec![AdminEvent::Ack("Area created.".into())]
                }
            }
            AdminCommand::UpdateArea {
                slug,
                title,
                description,
            } => match self.file_areas.iter_mut().find(|a| a.slug == slug) {
                Some(area) => {
                    area.title = title;
                    area.description = description;
                    vec![AdminEvent::Ack("Area saved.".into())]
                }
                None => vec![AdminEvent::Failed("server error: NotFound".into())],
            },
            AdminCommand::DeleteArea { slug } => {
                if self.file_nodes.iter().any(|n| n.area == slug) {
                    vec![AdminEvent::Failed("server error: BadRequest".into())]
                } else {
                    self.file_areas.retain(|a| a.slug != slug);
                    vec![AdminEvent::Ack("Area removed.".into())]
                }
            }
            AdminCommand::CreateFolder {
                area,
                parent,
                name,
                is_dropbox,
            } => {
                let path = match parent.as_deref() {
                    Some(p) if !p.is_empty() => format!("{p}/{name}"),
                    _ => name.clone(),
                };
                if self
                    .file_nodes
                    .iter()
                    .any(|n| n.area == area && n.path == path)
                {
                    vec![AdminEvent::Failed("server error: AlreadyExists".into())]
                } else {
                    let mut node =
                        FileNodeView::new(self.next_node_id(), area, KIND_FOLDER, name, path);
                    node.is_dropbox = is_dropbox;
                    self.file_nodes.push(node);
                    vec![AdminEvent::Ack("Folder created.".into())]
                }
            }
            AdminCommand::DeleteNode { id, .. } => {
                let Some(node) = self.file_nodes.iter().find(|n| n.id == id).cloned() else {
                    return vec![AdminEvent::Failed("server error: NotFound".into())];
                };
                // A folder takes what is inside it along.
                let inside = format!("{}/", node.path);
                self.file_nodes.retain(|n| {
                    n.id != id && !(n.area == node.area && n.path.starts_with(&inside))
                });
                vec![AdminEvent::Ack("Removed.".into())]
            }
            AdminCommand::RenameNode { id, new_name, .. } => {
                let Some(node) = self.file_nodes.iter().find(|n| n.id == id).cloned() else {
                    return vec![AdminEvent::Failed("server error: NotFound".into())];
                };
                let name = new_name.trim().to_string();
                if name.is_empty() || name.contains('/') {
                    return vec![AdminEvent::Failed("server error: BadRequest".into())];
                }
                let path = match node.path.rsplit_once('/') {
                    Some((parent, _)) => format!("{parent}/{name}"),
                    None => name.clone(),
                };
                if self
                    .file_nodes
                    .iter()
                    .any(|n| n.area == node.area && n.path == path && n.id != id)
                {
                    return vec![AdminEvent::Failed("server error: AlreadyExists".into())];
                }
                self.relocate_demo_node(id, &name, &path);
                vec![AdminEvent::Ack("Renamed.".into())]
            }
            AdminCommand::MoveNode { id, folder, .. } => {
                let Some(node) = self.file_nodes.iter().find(|n| n.id == id).cloned() else {
                    return vec![AdminEvent::Failed("server error: NotFound".into())];
                };
                let dest = folder.filter(|f| !f.is_empty());
                if let Some(d) = &dest {
                    let inside = format!("{}/", node.path);
                    if node.kind == crate::files::KIND_FOLDER
                        && (*d == node.path || d.starts_with(&inside))
                    {
                        return vec![AdminEvent::Failed("server error: BadRequest".into())];
                    }
                    match self
                        .file_nodes
                        .iter()
                        .find(|n| n.area == node.area && n.path == *d)
                    {
                        Some(f) if f.kind == crate::files::KIND_FOLDER => {}
                        Some(_) => {
                            return vec![AdminEvent::Failed("server error: BadRequest".into())]
                        }
                        None => return vec![AdminEvent::Failed("server error: NotFound".into())],
                    }
                }
                let path = match &dest {
                    Some(d) => format!("{d}/{}", node.name),
                    None => node.name.clone(),
                };
                if path == node.path {
                    return vec![AdminEvent::Ack("Already there.".into())];
                }
                if self
                    .file_nodes
                    .iter()
                    .any(|n| n.area == node.area && n.path == path)
                {
                    return vec![AdminEvent::Failed("server error: AlreadyExists".into())];
                }
                let name = node.name.clone();
                self.relocate_demo_node(id, &name, &path);
                vec![AdminEvent::Ack("Moved.".into())]
            }
            AdminCommand::DescribeNode { id, comment, .. } => {
                match self.file_nodes.iter_mut().find(|n| n.id == id) {
                    Some(node) => {
                        node.comment = comment;
                        vec![AdminEvent::Ack("Saved.".into())]
                    }
                    None => vec![AdminEvent::Failed("server error: NotFound".into())],
                }
            }
            AdminCommand::DeletePost { id } => match self.posts.iter_mut().find(|p| p.id == id) {
                Some(post) => {
                    post.removed = true;
                    post.body.clear();
                    vec![AdminEvent::Ack("Post removed.".into())]
                }
                None => vec![AdminEvent::Failed("server error: NotFound".into())],
            },
            AdminCommand::GetSurfaceStatus => {
                vec![AdminEvent::SurfacesReported(crate::demo_config::surfaces(
                    &self.admin_config,
                ))]
            }
            AdminCommand::GetGatewayStats => admin_events(&Self::seeded_gateway_stats()),
            AdminCommand::SetThemeBundle { bundle } => {
                let name = postcard::from_bytes::<ThemeBundle>(&bundle)
                    .map(|b| b.name)
                    .unwrap_or_else(|_| "theme".into());
                let mut info = ThemeBundleInfo::default();
                info.present = true;
                info.name = name;
                admin_events(&info)
            }
        }
    }

    /// The radio-bridge notices a fresh session is seeded with: one live-DJ
    /// station and one automation station, so the Radio view and status-bar
    /// segment render in dev.
    fn seeded_radio_frames() -> Vec<Frame> {
        [
            RadioNowPlaying::new("live", "Down the Hole", "The Lagomorphs", "Robin", 7, true),
            RadioNowPlaying::new("ambient", "Warren Dawn", "", "rotation", 3, false),
        ]
        .iter()
        .filter_map(|m| Frame::push(m).ok())
        .collect()
    }

    /// The demo burrow's radio listing, decoded through the host-tested
    /// [`frame_to_radio_listing`](crate::wire::frame_to_radio_listing) like a
    /// live one. A seeded burrow has no encoder, so nothing here streams and
    /// the port is 0: the player shows everything except a working Listen.
    pub fn radio_listing(&self) -> Option<crate::wire::RadioListing> {
        use rabbithole_proto::radio::{RadioPlayed, RadioStationInfo, RadioStations};
        const LAGOMORPHS: [&str; 10] = [
            "Carrot Cake",
            "Burrow Deep",
            "Moonlit Meadow",
            "Thump Twice",
            "Clover Season",
            "The Long Tunnel",
            "Warren Rules",
            "Whiskers in the Dark",
            "Second Exit",
            "Bramble Patch",
        ];
        let recent = LAGOMORPHS
            .iter()
            .map(|t| RadioPlayed::new(*t, "The Lagomorphs", 0))
            .collect();
        let listing = RadioStations::new(
            "",
            0,
            vec![
                RadioStationInfo::new("live", "Warren FM")
                    .described("Pirate radio from under the hill")
                    .playing("Down the Hole", "The Lagomorphs", "Robin")
                    .on_air(7, true, false)
                    .with_recent(recent),
                RadioStationInfo::new("ambient", "Ambient Burrow")
                    .described("Slow rotation for the small hours")
                    .playing("Warren Dawn", "", "rotation")
                    .on_air(3, false, false)
                    .with_recent(vec![
                        RadioPlayed::new("Dew on the Clover", "", 0),
                        RadioPlayed::new("First Light", "", 0),
                    ]),
            ],
        );
        let frame = Frame::request(RequestId(0), &listing).ok()?;
        crate::wire::frame_to_radio_listing(&frame)
    }

    /// Route the seeded radio pushes exactly as the transport would: each is a
    /// real RADIO `RadioNowPlaying` frame decoded through the host-tested
    /// [`frame_to_notice_route`] — no parallel decode path.
    pub fn radio_routes(&self) -> Vec<NoticeRoute> {
        self.radio_frames
            .iter()
            .filter_map(frame_to_notice_route)
            .collect()
    }

    /// A sample server-published theme bundle, so the server-theming overlay
    /// and the user opt-out are demonstrable in dev. The real transport
    /// delivers this (already server-validated + signed) in the welcome frame;
    /// here it is a plain in-memory [`ThemeBundle`] whose warm per-mode accent
    /// visibly differs from the default pack's indigo. Only tokens the server
    /// grammar permits are set (per-mode `--rh-accent`).
    pub fn server_theme_bundle(&self) -> Option<ThemeBundle> {
        let mut b = ThemeBundle::new("The Warren");
        b.tokens_light = vec![("--rh-accent".to_string(), "#b45309".to_string())];
        b.tokens_dark = vec![("--rh-accent".to_string(), "#f59e0b".to_string())];
        Some(b)
    }

    /// The lobby scrollback every fresh session is seeded with.
    fn seeded_messages() -> Vec<Event> {
        let now = crate::clock::now_ms();
        [
            (
                "rabbit",
                "Welcome to the warren. Be excellent to each other.",
                now - 42 * 60_000,
            ),
            ("alice", "morning all \u{2600}", now - 7 * 60_000),
            ("bob", "anyone up for a game later?", now - 3 * 60_000),
        ]
        .into_iter()
        .map(|(from, text, at_unix_ms)| Event::ChatMessage {
            room: LOBBY.to_string(),
            from: from.to_string(),
            text: text.to_string(),
            at_unix_ms,
        })
        .collect()
    }
}

impl UiClient for MockClient {
    fn demo_welcome_widgets(&self) -> Vec<rabbithole_proto::welcome::WelcomeWidget> {
        self.demo.welcome_widgets()
    }

    fn send(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::Connect { endpoint, .. } => {
                self.connected = true;
                // A demo burrow has a NAME; only an unrecognised endpoint falls
                // back to its host (which is what used to put "localhost" in
                // the title bar).
                if self.server_name.is_empty() {
                    self.server_name = derive_server_name(&endpoint);
                }
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
                vec![Event::ChatMessage {
                    room,
                    from,
                    text,
                    at_unix_ms: crate::clock::now_ms(),
                }]
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

    fn posts(&self, thread_id: &str) -> Vec<Post> {
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
            at_unix_ms: crate::clock::now_ms(),
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
    fn a_demo_burrow_connects_under_its_own_name() {
        // A seeded burrow HAS a name; the endpoint host is only a fallback.
        // Deriving "localhost" from the address is what used to put that word
        // in the title bar instead of the place's name.
        let mut c = MockClient::new();
        let ev = c.send(Command::Connect {
            endpoint: "ws://warren.example:9000".into(),
            pinned_fingerprint: None,
        });
        assert_eq!(
            ev,
            vec![Event::Connected {
                server_name: DEMO_BURROWS[0].name.into(),
                server_version: "0.5.0-mock".into(),
            }]
        );
    }

    #[test]
    fn each_demo_burrow_is_a_distinct_place() {
        // Two mock sessions must differ in the ways switching between them is
        // supposed to show: name, roster, and news.
        let a = &DEMO_BURROWS[0];
        let b = &DEMO_BURROWS[1];
        assert_ne!(a.name, b.name);
        assert_ne!(a.endpoint, b.endpoint);
        assert_ne!(a.who, b.who);
        assert_ne!(a.motd, b.motd);
        // …and each ships a full welcome screen, so the news panel is
        // exercised by the demo exactly as it is live.
        for d in DEMO_BURROWS {
            let w = d.welcome_widgets();
            assert_eq!(w.len(), 4, "{}", d.name);
            assert!(MockClient::named(d).demo_welcome_widgets().len() == 4);
        }
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
                at_unix_ms: 0,
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
        let posts = c.posts("t1");
        assert_eq!(posts.len(), 2);
        assert!(posts.iter().all(|p| p.thread == "t1"));
        assert!(c.posts("nope").is_empty());
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
        // Root of "warez": everything at the top level, and NOT the nested
        // archive. Asserted by identity rather than a bare count, so seeding
        // another demo file doesn't fail a test about *filtering*.
        let root = c.dispatch_file(FileCommand::ListFolder {
            area: "warez".into(),
            path: None,
        });
        let [FileEvent::FolderListed { nodes }] = root.as_slice() else {
            panic!("expected a folder listing");
        };
        assert!(nodes.iter().any(|n| n.name == "utils"));
        assert!(nodes.iter().any(|n| n.name == "readme.txt"));
        assert!(nodes.iter().any(|n| n.name == FAILING_DEMO_FILE));
        assert!(
            !nodes.iter().any(|n| n.name == "lister.lha"),
            "the nested archive belongs to utils/, not the root"
        );

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
                // What arrives is what the listing advertised, and both are
                // the real seeded archive, not a buffer of zeros.
                let real = crate::demo_files::bytes_for("lister.lha").unwrap();
                assert_eq!(*size, real.len());
                assert_eq!(node.size as usize, real.len());
                assert_eq!(c.download_bytes(3).unwrap().bytes, real);
                assert!(
                    c.download_bytes(7).is_none(),
                    "the failing file gives nothing"
                );
                assert!(c.download_bytes(1).is_none(), "a folder is not a download");
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
        let whole = crate::demo_files::bytes_for("welcome.ans").unwrap().len() as u64;
        assert_eq!(*transfer_id, 4);
        assert_eq!(*size, whole);
        let chunk = c.dispatch_file(FileCommand::RequestChunk {
            transfer_id: 4,
            offset: 0,
            len: whole as u32,
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
    fn every_operator_key_is_seeded_so_demo_and_live_consoles_agree() {
        let mut c = MockClient::new();
        for key in crate::admin::OPERATOR_KEYS {
            let got = c.dispatch_admin(AdminCommand::GetConfig { key: (*key).into() });
            assert!(
                matches!(got.as_slice(), [AdminEvent::ConfigLoaded { .. }]),
                "mock has no seed for operator key {key}"
            );
        }
    }

    #[test]
    fn admin_config_get_set_roundtrip() {
        let mut c = MockClient::new();
        let got = c.dispatch_admin(AdminCommand::GetConfig { key: "name".into() });
        assert!(matches!(
            got.as_slice(),
            [AdminEvent::ConfigLoaded { value, .. }] if value == "Rabbit Lobby"
        ));
        // Setting a non-listener key applies live.
        let set = c.dispatch_admin(AdminCommand::SetConfig {
            key: "name".into(),
            value: "New Warren".into(),
        });
        assert!(matches!(
            set.as_slice(),
            [AdminEvent::ConfigApplied { applied_live: true }]
        ));
        // The address every browser arrives on waits for a restart.
        let listen = c.dispatch_admin(AdminCommand::SetConfig {
            key: "ws_addr".into(),
            value: "127.0.0.1:9000".into(),
        });
        assert!(matches!(
            listen.as_slice(),
            [AdminEvent::ConfigApplied {
                applied_live: false
            }]
        ));
        // The updated value reads back.
        let got = c.dispatch_admin(AdminCommand::GetConfig { key: "name".into() });
        assert!(matches!(
            got.as_slice(),
            [AdminEvent::ConfigLoaded { value, .. }] if value == "New Warren"
        ));
    }

    #[test]
    fn admin_syndication_panel_keys_are_seeded() {
        use crate::syndication_admin::{FeedsStatus, SynAdminState, LOAD_KEYS};

        // Drive the exact load the panel performs and fold the paired
        // replies; every key answers from the seeded mock config.
        let mut c = MockClient::new();
        let mut s = SynAdminState::default();
        for key in LOAD_KEYS {
            let events = c.dispatch_admin(AdminCommand::GetConfig {
                key: (*key).to_string(),
            });
            s.apply_get_reply(key, &events);
        }
        let stats = c.dispatch_admin(AdminCommand::GetGatewayStats);
        s.apply_live(None, &stats);
        assert_eq!(s.enabled(), Some(true));
        assert_eq!(s.poll_secs(), Some(1800));
        assert_eq!(
            s.feed_stat("https://blog.example.org/feed.xml")
                .map(|f| f.items_posted),
            Some(11)
        );
        // The seeded TOML table body parses into read-only feed rows whose
        // destinations are real seeded boards.
        let rows = s.feed_rows();
        assert_eq!(rows.len(), 2);
        assert!(matches!(&s.feeds, FeedsStatus::Listed(_)));
        let boards = c.boards();
        for row in &rows {
            assert!(boards.iter().any(|b| b.slug == row.board), "{row:?}");
        }
    }

    #[test]
    fn the_demo_burrow_answers_a_saved_setting_as_a_real_one_would() {
        let mut c = MockClient::new();
        let mut set = |key: &str, value: &str| {
            c.dispatch_admin(AdminCommand::SetConfig {
                key: key.into(),
                value: value.into(),
            })
        };
        // A gateway starts and stops while the burrow runs: live.
        assert!(matches!(
            set("nntp_enabled", "false").as_slice(),
            [AdminEvent::ConfigApplied { applied_live: true }]
        ));
        // What every client arrives on still waits for a restart.
        assert!(matches!(
            set("quic_addr", "0.0.0.0:4700").as_slice(),
            [AdminEvent::ConfigApplied {
                applied_live: false
            }]
        ));
        // And it describes itself, surfaces included.
        let described = c.dispatch_admin(AdminCommand::DescribeConfig);
        let [AdminEvent::ConfigDescribed(entries)] = described.as_slice() else {
            panic!("no description: {described:?}");
        };
        let nntp = entries.iter().find(|e| e.key == "nntp_enabled").unwrap();
        assert_eq!(nntp.value, "false", "the change just made is held");
        let reported = c.dispatch_admin(AdminCommand::GetSurfaceStatus);
        let [AdminEvent::SurfacesReported(surfaces)] = reported.as_slice() else {
            panic!("no report: {reported:?}");
        };
        let nntp = surfaces.iter().find(|s| s.key == "nntp_enabled").unwrap();
        assert_eq!(nntp.state, rabbithole_proto::admin::surface_state::OFF);
        assert!(
            surfaces.iter().all(|s| s.key != "guest_enabled"),
            "not a surface"
        );
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
    fn admin_publish_theme_names_the_bundle() {
        let mut c = MockClient::new();
        let bundle = postcard::to_allocvec(&ThemeBundle::new("Wonderland")).unwrap();
        let ev = c.dispatch_admin(AdminCommand::SetThemeBundle { bundle });
        match ev.as_slice() {
            [AdminEvent::ThemeBundleApplied(info)] => {
                assert!(info.present);
                assert_eq!(info.name, "Wonderland");
            }
            other => panic!("expected theme applied, got {other:?}"),
        }
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
                NoticeRoute::Radio(update) => {
                    radio.apply_update(update);
                }
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
