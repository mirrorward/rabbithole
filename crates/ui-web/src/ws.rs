//! Browser WebSocket transport for RHP (`wasm32-unknown-unknown` only).
//!
//! [`WsClient`] speaks the RabbitHole Protocol over a real browser
//! [`WebSocket`], one binary message per [`Frame`] (the message-transport
//! framing described in [`rabbithole_proto::codec`] — no length prefix). It
//! implements the async [`EventClient`](crate::wire::EventClient) seam, so it
//! is a drop-in alternative to [`MockClient`](crate::client::MockClient).
//!
//! All Command ↔ Frame ↔ Event mapping lives in [`crate::wire`] (host-tested)
//! and the reconnect schedule in [`crate::conn`] (also host-tested); this module
//! is only the wasm glue — socket lifecycle, binary I/O, timers, and wiring the
//! browser's event callbacks into the registered sinks. It is validated by
//! `cargo check --target wasm32-unknown-unknown`.
//!
//! # Lifecycle
//!
//! 1. [`Command::Connect`] opens the socket (binary type = `ArrayBuffer`) and
//!    latches "connection wanted" so a dropped socket auto-reconnects.
//! 2. On `open`, a [`Hello`](rabbithole_proto::Hello) request is (re)sent and
//!    the connection state becomes [`ConnState::Online`].
//! 3. Each inbound binary message is decoded once to a [`Frame`] and fanned out:
//!    [`wire::frame_to_events`](crate::wire::frame_to_events) → the api-event
//!    sink, [`wire::frame_to_file_events`](crate::wire::frame_to_file_events)
//!    → the FILE-family sink,
//!    [`wire::frame_to_admin_events`](crate::wire::frame_to_admin_events) →
//!    the ADMIN-family sink, and
//!    [`wire::frame_to_notice_route`](crate::wire::frame_to_notice_route) →
//!    the notice sink (radio bridge updates vs. operator notices, pre-split).
//! 4. [`Command::Disconnect`] clears "connection wanted" and closes the socket
//!    (emitting [`Event::Disconnected`]); an *unexpected* close instead
//!    schedules a jittered, capped exponential-backoff reconnect
//!    ([`crate::conn::backoff_delay`]) and reports [`ConnState::Reconnecting`].
//!
//! A 30-second keepalive [`Ping`](rabbithole_proto::session::Ping) loop runs for
//! each connected socket's lifetime.
//!
//! # Deferred
//!
//! Session resume and binary attachments — see the [`crate::wire`] module docs
//! for the full deferred list.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use js_sys::{ArrayBuffer, Function, Math, Promise, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{BinaryType, CloseEvent, Event as WebEvent, MessageEvent, WebSocket};

use rabbithole_core::api::{Command, Event};
use rabbithole_proto::{decode_frame, encode_frame, Frame, FrameKind, RequestId};

use crate::conn::{backoff_delay, ConnState};
use crate::wire::{
    self, AdminCommand, AdminEvent, EventClient, EventSink, FileCommand, FileEvent, NoticeRoute,
    PresenceDelta,
};

/// Keepalive interval, milliseconds.
const KEEPALIVE_MS: u32 = 30_000;
/// `WebSocket.readyState` value for an open socket.
const WS_OPEN: u16 = 1;

/// A sink the transport pushes connection-state changes into.
pub type ConnSink = Rc<dyn Fn(ConnState)>;
/// A sink the transport pushes decoded [`FileEvent`]s into.
pub type FileSink = Rc<dyn Fn(FileEvent)>;
/// A sink the transport pushes decoded [`AdminEvent`]s into. The optional
/// key is the in-flight `GetConfig`/`SetConfig` key (FIFO-paired), so a
/// `Failed` reply still knows which read it belongs to.
pub type AdminSink = Rc<dyn Fn((Option<String>, Vec<AdminEvent>))>;
/// A sink the transport pushes routed `ServerNotice` pushes into (radio
/// bridge updates vs. operator notices, already split by
/// [`wire::frame_to_notice_route`]).
pub type NoticeSink = Rc<dyn Fn(NoticeRoute)>;
/// A sink the transport pushes the present-user roster into (from a decoded
/// [`WhoList`](rabbithole_proto::presence::WhoList) reply). The core [`Event`]
/// enum has no roster variant, so this rides its own sink like FILE/notices.
pub type WhoSink = Rc<dyn Fn(Vec<crate::state::Presence>)>;

/// Receives the burrow's front page (welcome-screen widgets) once per session.
pub type FrontPageSink = Rc<dyn Fn(Vec<rabbithole_proto::welcome::WelcomeWidget>)>;
/// A sink the transport pushes live roster deltas into (join/leave), keeping
/// the presence list fresh between full [`WhoSink`] snapshots.
pub type PresenceSink = Rc<dyn Fn(PresenceDelta)>;
/// A sink the transport pushes the board list into (from a decoded
/// [`BoardList`](rabbithole_proto::board::BoardList) reply).
pub type BoardSink = Rc<dyn Fn(Vec<crate::state::Board>)>;
/// Receives every session on the burrow (the roster before it is collapsed
/// to people).
pub type SessionsSink = Rc<dyn Fn(Vec<crate::state::SessionRow>)>;
/// The bytes of a file this session asked for. What happens to them — kept,
/// or looked at — is the app's to decide; without a sink they are saved,
/// which is what a download is.
pub type FileBytesSink = Rc<dyn Fn(crate::wire::DownloadedFile)>;
/// The Wishing Well's list, and single wishes as they change.
pub type WishesSink = Rc<dyn Fn(Vec<rabbithole_proto::wish::WishView>)>;
pub type WishSink = Rc<dyn Fn(rabbithole_proto::wish::WishView)>;
/// The burrow's rooms, and single rooms as they change.
pub type RoomsSink = Rc<dyn Fn(Vec<rabbithole_proto::chat::RoomInfo>)>;
pub type RoomSink = Rc<dyn Fn(rabbithole_proto::chat::RoomInfo)>;
/// A station's answer to a listener: its queue, what can be asked for, or
/// which ask it refused.
pub type RadioRequestsSink = Rc<dyn Fn(crate::wire::RadioAnswer)>;
/// Receives the whole board tree (categories included), for the console.
pub type BoardTreeSink = Rc<dyn Fn(Vec<crate::state::BoardNode>)>;
/// A sink the transport pushes a board's thread list into.
pub type ThreadSink = Rc<dyn Fn(Vec<crate::state::Thread>)>;
/// A sink the transport pushes a thread's posts into.
pub type PostSink = Rc<dyn Fn(Vec<crate::state::Post>)>;
/// A sink the transport pushes the DM conversation list into.
pub type DmThreadSink = Rc<dyn Fn(Vec<crate::state::DmThread>)>;
/// Receives a burrow's radio listing: where the audio is, and what is on.
pub type RadioListingSink = Rc<dyn Fn(wire::RadioListing)>;
/// A sink the transport pushes one conversation's message history into.
pub type DmHistorySink = Rc<dyn Fn((String, Vec<crate::state::DmMessage>))>;
/// A sink the transport pushes a live `(peer, message)` DM into.
pub type DmReceivedSink = Rc<dyn Fn((String, crate::state::DmMessage))>;
/// A sink the transport pushes the directory member list into.
pub type MembersSink = Rc<dyn Fn(Vec<crate::state::Member>)>;
/// A sink the transport pushes one member's profile card into.
pub type ProfileSink = Rc<dyn Fn(crate::state::MemberProfile)>;
/// A sink the transport pushes a fetched avatar `(hex_id, data_url)` into. The
/// hex lets the app confirm the blob still belongs to the selected profile
/// (a `BlobData` reply is otherwise id-less).
pub type AvatarSink = Rc<dyn Fn((String, String))>;

/// A browser WebSocket [`EventClient`] speaking RHP.
///
/// Cheap to clone: all state lives behind a shared `Rc<RefCell<..>>` so the
/// socket's event callbacks, the keepalive task, and the reconnect timer can
/// reach it.
#[derive(Clone)]
pub struct WsClient {
    inner: Rc<RefCell<Inner>>,
}

/// Shared, mutable transport state.
struct Inner {
    ws: Option<WebSocket>,
    sink: Option<EventSink>,
    conn_sink: Option<ConnSink>,
    file_sink: Option<FileSink>,
    admin_sink: Option<AdminSink>,
    /// What each in-flight management request was about
    /// ([`AdminCommand::tag`]), by request id: a reply carries its request's
    /// id and nothing else about it. By id and not by arrival order, because
    /// management requests are not all in one family (a board is created in
    /// BOARD, an area in FILE) and order across families is nobody's promise.
    pending_admin: RefCell<std::collections::HashMap<RequestId, Option<String>>>,
    /// Replies an async flow is awaiting ([`WsClient::call`]), by request id:
    /// the resolver of the promise it awaits. A reply resolves it with the
    /// frame's bytes and goes nowhere else; the socket closing resolves every
    /// one with `null`, so nothing waits on a dead connection.
    pending_calls: RefCell<std::collections::HashMap<RequestId, Function>>,
    /// The burrow's server identity key, from its handshake: what another
    /// burrow is told to send files to.
    server_key: std::cell::Cell<Option<[u8; 32]>>,
    notice_sink: Option<NoticeSink>,
    who_sink: Option<WhoSink>,
    sessions_sink: Option<SessionsSink>,
    file_bytes_sink: Option<FileBytesSink>,
    wishes_sink: Option<WishesSink>,
    wish_sink: Option<WishSink>,
    rooms_sink: Option<RoomsSink>,
    room_sink: Option<RoomSink>,
    radio_requests_sink: Option<RadioRequestsSink>,
    front_page_sink: Option<FrontPageSink>,
    presence_sink: Option<PresenceSink>,
    board_sink: Option<BoardSink>,
    board_tree_sink: Option<BoardTreeSink>,
    thread_sink: Option<ThreadSink>,
    post_sink: Option<PostSink>,
    dm_thread_sink: Option<DmThreadSink>,
    radio_listing_sink: Option<RadioListingSink>,
    dm_history_sink: Option<DmHistorySink>,
    /// Peers of in-flight DM-history requests, FIFO (a `DmHistory` reply is
    /// id-less; the ordered socket answers in request order).
    pending_dm_history: RefCell<VecDeque<String>>,
    dm_received_sink: Option<DmReceivedSink>,
    members_sink: Option<MembersSink>,
    profile_sink: Option<ProfileSink>,
    avatar_sink: Option<AvatarSink>,
    /// Hex ids of in-flight avatar `BlobGet`s, FIFO. A `BlobData` reply carries
    /// no id, but the single ordered socket answers in request order, so the
    /// front hex identifies the next reply's content. `RefCell` so the onmessage
    /// handler (which holds a shared `Inner` borrow) can pop without escalating.
    pending_avatars: RefCell<VecDeque<String>>,
    next_id: u64,
    /// While `true`, the keepalive loop keeps pinging; cleared on close.
    alive: bool,
    /// The user wants a live connection: an unexpected close reconnects; a
    /// [`Command::Disconnect`] clears this so the close is final.
    want_connected: bool,
    /// Endpoint to (re)dial.
    endpoint: String,
    /// 0-based count of consecutive reconnect attempts; reset on a clean open.
    reconnect_attempt: u32,
    /// Bumped on every `connect()`. The keepalive loop captures its socket's
    /// generation and exits once a newer socket supersedes it, so reconnects
    /// don't accumulate zombie ping loops.
    generation: u64,
    // The browser holds these callbacks by reference; we own them so they live
    // exactly as long as the socket.
    _on_open: Option<Closure<dyn FnMut(WebEvent)>>,
    _on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
    _on_close: Option<Closure<dyn FnMut(CloseEvent)>>,
    _on_error: Option<Closure<dyn FnMut(WebEvent)>>,
}

impl Inner {
    fn emit(&self, event: Event) {
        if let Some(sink) = &self.sink {
            sink(event);
        }
    }

    fn emit_conn(&self, state: ConnState) {
        if let Some(sink) = &self.conn_sink {
            sink(state);
        }
    }

    fn emit_file(&self, event: FileEvent) {
        if let Some(sink) = &self.file_sink {
            sink(event);
        }
    }

    fn emit_admin(&self, batch: (Option<String>, Vec<AdminEvent>)) {
        if let Some(sink) = &self.admin_sink {
            sink(batch);
        }
    }

    fn emit_notice(&self, route: NoticeRoute) {
        if let Some(sink) = &self.notice_sink {
            sink(route);
        }
    }

    fn emit_front_page(&self, widgets: Vec<rabbithole_proto::welcome::WelcomeWidget>) {
        if let Some(sink) = &self.front_page_sink {
            sink(widgets);
        }
    }

    fn emit_who(&self, roster: Vec<crate::state::Presence>) {
        if let Some(sink) = &self.who_sink {
            sink(roster);
        }
    }

    fn emit_presence(&self, delta: PresenceDelta) {
        if let Some(sink) = &self.presence_sink {
            sink(delta);
        }
    }

    fn emit_boards(&self, boards: Vec<crate::state::Board>) {
        if let Some(sink) = &self.board_sink {
            sink(boards);
        }
    }

    fn emit_threads(&self, threads: Vec<crate::state::Thread>) {
        if let Some(sink) = &self.thread_sink {
            sink(threads);
        }
    }

    fn emit_posts(&self, posts: Vec<crate::state::Post>) {
        if let Some(sink) = &self.post_sink {
            sink(posts);
        }
    }

    fn emit_radio_listing(&self, listing: wire::RadioListing) {
        if let Some(sink) = &self.radio_listing_sink {
            sink(listing);
        }
    }

    fn emit_dm_threads(&self, threads: Vec<crate::state::DmThread>) {
        if let Some(sink) = &self.dm_thread_sink {
            sink(threads);
        }
    }

    fn emit_dm_history(&self, msgs: (String, Vec<crate::state::DmMessage>)) {
        if let Some(sink) = &self.dm_history_sink {
            sink(msgs);
        }
    }

    fn emit_dm_received(&self, msg: (String, crate::state::DmMessage)) {
        if let Some(sink) = &self.dm_received_sink {
            sink(msg);
        }
    }

    fn emit_members(&self, members: Vec<crate::state::Member>) {
        if let Some(sink) = &self.members_sink {
            sink(members);
        }
    }

    fn emit_profile(&self, profile: crate::state::MemberProfile) {
        if let Some(sink) = &self.profile_sink {
            sink(profile);
        }
    }

    fn emit_avatar(&self, avatar: (String, String)) {
        if let Some(sink) = &self.avatar_sink {
            sink(avatar);
        }
    }

    fn next_request_id(&mut self) -> RequestId {
        self.next_id += 1;
        RequestId(self.next_id)
    }
}

impl WsClient {
    /// A fresh, disconnected client. Register a sink with
    /// [`on_event`](EventClient::on_event), then
    /// [`dispatch`](EventClient::dispatch) a [`Command::Connect`].
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                ws: None,
                sink: None,
                conn_sink: None,
                file_sink: None,
                admin_sink: None,
                pending_admin: RefCell::new(std::collections::HashMap::new()),
                pending_calls: RefCell::new(std::collections::HashMap::new()),
                server_key: std::cell::Cell::new(None),
                notice_sink: None,
                who_sink: None,
                sessions_sink: None,
                file_bytes_sink: None,
                wishes_sink: None,
                wish_sink: None,
                rooms_sink: None,
                room_sink: None,
                radio_requests_sink: None,
                front_page_sink: None,
                presence_sink: None,
                board_sink: None,
                board_tree_sink: None,
                thread_sink: None,
                post_sink: None,
                dm_thread_sink: None,
                radio_listing_sink: None,
                dm_history_sink: None,
                pending_dm_history: RefCell::new(VecDeque::new()),
                dm_received_sink: None,
                members_sink: None,
                profile_sink: None,
                avatar_sink: None,
                pending_avatars: RefCell::new(VecDeque::new()),
                next_id: 0,
                alive: false,
                want_connected: false,
                endpoint: String::new(),
                reconnect_attempt: 0,
                generation: 0,
                _on_open: None,
                _on_message: None,
                _on_close: None,
                _on_error: None,
            })),
        }
    }

    /// Register the connection-state sink (Connecting/Online/Reconnecting/
    /// Offline). The most recent registration wins.
    pub fn on_conn(&mut self, sink: ConnSink) {
        self.inner.borrow_mut().conn_sink = Some(sink);
    }

    /// Manually redial the last endpoint now (a "Reconnect"/"Retry now" button):
    /// reset the backoff and open immediately, reusing the stored endpoint + the
    /// registered sinks (so the `open` callback re-sends Hello + re-auths). A
    /// no-op if a socket is already open (the [`connect`](Self::connect) guard).
    pub fn redial(&self) {
        {
            let mut b = self.inner.borrow_mut();
            b.want_connected = true;
            b.reconnect_attempt = 0;
        }
        Self::connect(&self.inner);
    }

    /// Register the FILE-family event sink. The most recent registration wins.
    pub fn on_file_event(&mut self, sink: FileSink) {
        self.inner.borrow_mut().file_sink = Some(sink);
    }

    /// Register the notice sink (routed `ServerNotice` pushes: radio bridge
    /// updates and operator notices). The most recent registration wins.
    pub fn on_notice(&mut self, sink: NoticeSink) {
        self.inner.borrow_mut().notice_sink = Some(sink);
    }

    /// Register the roster sink (present-user screen names from a `WhoList`
    /// reply). The most recent registration wins.
    /// Register the front-page sink (the burrow's welcome screen).
    pub fn on_front_page(&mut self, sink: FrontPageSink) {
        self.inner.borrow_mut().front_page_sink = Some(sink);
    }

    /// Ask the burrow for its front page — sent once the session is authenticated.
    pub fn request_front_page(&self) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::welcome_screen_request(id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    pub fn on_who(&mut self, sink: WhoSink) {
        self.inner.borrow_mut().who_sink = Some(sink);
    }

    /// Register the presence-delta sink (live join/leave). The most recent
    /// registration wins.
    pub fn on_presence(&mut self, sink: PresenceSink) {
        self.inner.borrow_mut().presence_sink = Some(sink);
    }

    /// Ask the server for the current room roster; the reply arrives through
    /// the [`on_who`](Self::on_who) sink.
    pub fn request_who(&self) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::who_request(id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    /// Broadcast the user's presence status to this server.
    pub fn set_presence(
        &self,
        state: rabbithole_proto::presence::PresenceState,
        status: Option<String>,
    ) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) =
            wire::presence_set_request(state, status, id).and_then(|f| encode_frame(&f))
        {
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the sink for the whole board tree (the same reply as the
    /// board list, unfiltered).
    pub fn on_board_tree(&mut self, sink: BoardTreeSink) {
        self.inner.borrow_mut().board_tree_sink = Some(sink);
    }

    /// Register the sink for every session (what a moderator kicks).
    pub fn on_sessions(&mut self, sink: SessionsSink) {
        self.inner.borrow_mut().sessions_sink = Some(sink);
    }

    /// Where a downloaded file's bytes go. Without one they are saved.
    pub fn on_file_bytes(&mut self, sink: FileBytesSink) {
        self.inner.borrow_mut().file_bytes_sink = Some(sink);
    }

    /// The Wishing Well's listing.
    pub fn on_wishes(&mut self, sink: WishesSink) {
        self.inner.borrow_mut().wishes_sink = Some(sink);
    }

    /// One wish, as it changes.
    pub fn on_wish(&mut self, sink: WishSink) {
        self.inner.borrow_mut().wish_sink = Some(sink);
    }

    /// The burrow's room list.
    pub fn on_rooms(&mut self, sink: RoomsSink) {
        self.inner.borrow_mut().rooms_sink = Some(sink);
    }

    /// One room, as it changes.
    pub fn on_room(&mut self, sink: RoomSink) {
        self.inner.borrow_mut().room_sink = Some(sink);
    }

    /// A station's answers to this listener.
    pub fn on_radio_requests(&mut self, sink: RadioRequestsSink) {
        self.inner.borrow_mut().radio_requests_sink = Some(sink);
    }

    /// Ask a station something as a listener.
    pub fn dispatch_radio_ask(&self, ask: &crate::wire::RadioAsk) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(frame) = wire::radio_ask_to_frame(ask, id) {
            if let Ok(bytes) = encode_frame(&frame) {
                Self::write(&mut b, &bytes);
            }
        }
    }

    /// Ask about rooms: list them, make one, go in, come out.
    pub fn dispatch_room(&self, command: &crate::wire::RoomCommand) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(frame) = wire::room_command_to_frame(command, id) {
            if let Ok(bytes) = encode_frame(&frame) {
                Self::write(&mut b, &bytes);
            }
        }
    }

    /// Ask the Wishing Well for something.
    pub fn dispatch_wish(&self, command: &crate::wire::WishCommand) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(frame) = wire::wish_command_to_frame(command, id) {
            if let Ok(bytes) = encode_frame(&frame) {
                Self::write(&mut b, &bytes);
            }
        }
    }

    /// Register the board-list sink. The most recent registration wins.
    pub fn on_boards(&mut self, sink: BoardSink) {
        self.inner.borrow_mut().board_sink = Some(sink);
    }

    /// Ask the server for the board list; the reply arrives through the
    /// [`on_boards`](Self::on_boards) sink.
    pub fn request_boards(&self) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::board_list_request(id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the thread-list sink. The most recent registration wins.
    pub fn on_threads(&mut self, sink: ThreadSink) {
        self.inner.borrow_mut().thread_sink = Some(sink);
    }

    /// Register the posts sink. The most recent registration wins.
    pub fn on_posts(&mut self, sink: PostSink) {
        self.inner.borrow_mut().post_sink = Some(sink);
    }

    /// Ask for a board's threads; the reply arrives through the
    /// [`on_threads`](Self::on_threads) sink.
    pub fn request_threads(&self, board: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::thread_list_request(board, 200, id).and_then(|f| encode_frame(&f))
        {
            Self::write(&mut b, &bytes);
        }
    }

    /// Ask for a thread's posts by root id; the reply arrives through the
    /// [`on_posts`](Self::on_posts) sink.
    pub fn request_posts(&self, root: [u8; 32]) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::thread_request(root, 500, id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    /// Post a new thread to `board`. The connection is ordered, so a following
    /// [`request_threads`](Self::request_threads) sees the committed post.
    pub fn send_post(&self, board: &str, subject: &str, body: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) =
            wire::post_create(board, subject, body, id).and_then(|f| encode_frame(&f))
        {
            Self::write(&mut b, &bytes);
        }
    }

    /// Reply to the thread rooted at `parent`. A following
    /// [`request_posts`](Self::request_posts) sees the committed reply.
    pub fn send_reply(&self, board: &str, parent: [u8; 32], body: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::post_reply(board, parent, body, id).and_then(|f| encode_frame(&f))
        {
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the radio-listing sink. Most recent registration wins.
    pub fn on_radio_listing(&mut self, sink: RadioListingSink) {
        self.inner.borrow_mut().radio_listing_sink = Some(sink);
    }

    /// Ask what is on the air and where ([`on_radio_listing`](Self::on_radio_listing)).
    pub fn request_radio_stations(&self) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::radio_stations_request(id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the DM conversation-list sink. Most recent registration wins.
    pub fn on_dm_threads(&mut self, sink: DmThreadSink) {
        self.inner.borrow_mut().dm_thread_sink = Some(sink);
    }

    /// Register the DM history sink. Most recent registration wins.
    pub fn on_dm_history(&mut self, sink: DmHistorySink) {
        self.inner.borrow_mut().dm_history_sink = Some(sink);
    }

    /// Register the live DM-received sink. Most recent registration wins.
    pub fn on_dm_received(&mut self, sink: DmReceivedSink) {
        self.inner.borrow_mut().dm_received_sink = Some(sink);
    }

    /// Ask for the DM conversation list ([`on_dm_threads`](Self::on_dm_threads)).
    pub fn request_dm_threads(&self) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::dm_threads_request(id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    /// Ask for the message history with `peer` ([`on_dm_history`](Self::on_dm_history)).
    pub fn request_dm_history(&self, peer: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::dm_history_request(peer, id).and_then(|f| encode_frame(&f)) {
            b.pending_dm_history
                .borrow_mut()
                .push_back(peer.to_string());
            Self::write(&mut b, &bytes);
        }
    }

    /// Send a plaintext DM to `to`. A following
    /// [`request_dm_history`](Self::request_dm_history) sees the sent message.
    pub fn send_dm(&self, to: &str, text: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::dm_send(to, text, id).and_then(|f| encode_frame(&f)) {
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the directory member-list sink. Most recent registration wins.
    pub fn on_members(&mut self, sink: MembersSink) {
        self.inner.borrow_mut().members_sink = Some(sink);
    }

    /// Register the profile-card sink. Most recent registration wins.
    pub fn on_profile(&mut self, sink: ProfileSink) {
        self.inner.borrow_mut().profile_sink = Some(sink);
    }

    /// Ask for the directory member list matching `query` (empty = all).
    pub fn request_directory(&self, query: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) =
            wire::directory_search_request(query, 100, id).and_then(|f| encode_frame(&f))
        {
            Self::write(&mut b, &bytes);
        }
    }

    /// Ask for one member's profile card ([`on_profile`](Self::on_profile)).
    pub fn request_profile(&self, screen_name: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        if let Ok(bytes) = wire::profile_get_request(screen_name, id).and_then(|f| encode_frame(&f))
        {
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the avatar sink (a fetched `data:` URL). Most recent wins.
    pub fn on_avatar(&mut self, sink: AvatarSink) {
        self.inner.borrow_mut().avatar_sink = Some(sink);
    }

    /// Fetch an avatar blob by hex id; the `data:` URL arrives through the
    /// [`on_avatar`](Self::on_avatar) sink.
    pub fn request_blob(&self, hex: &str) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        let frame = wire::blob_get_request(hex, id).map(|r| r.and_then(|f| encode_frame(&f)));
        if let Some(Ok(bytes)) = frame {
            // Remember which blob this reply will carry (FIFO on the ordered
            // socket), so the sink can confirm it still matches the selection.
            b.pending_avatars.borrow_mut().push_back(hex.to_string());
            Self::write(&mut b, &bytes);
        }
    }

    /// Register the ADMIN-family sink. Most recent wins.
    pub fn on_admin(&mut self, sink: AdminSink) {
        self.inner.borrow_mut().admin_sink = Some(sink);
    }

    /// Drive one [`AdminCommand`]: encode it via the host-tested
    /// [`wire::admin_command_to_frame`] and write it to the open socket.
    /// Replies arrive asynchronously through the admin sink.
    pub fn dispatch_admin(&self, command: &AdminCommand) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        let pending = command.tag();
        match wire::admin_command_to_frame(command, id) {
            Ok(Some(frame)) => match encode_frame(&frame) {
                Ok(bytes) => {
                    b.pending_admin.borrow_mut().insert(id, pending);
                    Self::write(&mut b, &bytes);
                }
                Err(err) => {
                    b.emit_admin((pending, vec![AdminEvent::Failed(format!("encode: {err}"))]))
                }
            },
            Ok(None) => {}
            Err(err) => b.emit_admin((pending, vec![AdminEvent::Failed(format!("map: {err}"))])),
        }
    }

    /// Drive one [`FileCommand`]: encode it via the host-tested
    /// [`wire::file_command_to_frame`] and write it to the open socket. Replies
    /// arrive asynchronously through the FILE-family sink.
    pub fn dispatch_file(&self, command: &FileCommand) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        match wire::file_command_to_frame(command, id) {
            Ok(Some(frame)) => match encode_frame(&frame) {
                Ok(bytes) => Self::write(&mut b, &bytes),
                Err(err) => b.emit_file(FileEvent::Failed(format!("encode: {err}"))),
            },
            Ok(None) => {}
            Err(err) => b.emit_file(FileEvent::Failed(format!("map: {err}"))),
        }
    }

    /// Send one request and await its reply, for flows that take several
    /// steps (a ticketed upload: open, chunks, finish). The request is on the
    /// wire before this returns; the future resolves with the reply frame
    /// (errors included, on `frame.error`), or `None` when the socket is not
    /// open or closes first. A claimed reply is not also fanned out to the
    /// sinks, so a transfer ticket meant for an upload never shows up as a
    /// download.
    pub fn call<M: rabbithole_proto::Message>(
        &self,
        msg: &M,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Frame>>>> {
        let promise = {
            let mut b = self.inner.borrow_mut();
            let id = b.next_request_id();
            let bytes = Frame::request(id, msg)
                .ok()
                .and_then(|frame| encode_frame(&frame).ok());
            let mut resolver = None;
            let promise = Promise::new(&mut |resolve, _reject| resolver = Some(resolve));
            let open = matches!(&b.ws, Some(ws) if ws.ready_state() == WS_OPEN);
            match (bytes, resolver, open) {
                (Some(bytes), Some(resolve), true) => {
                    b.pending_calls.borrow_mut().insert(id, resolve);
                    Self::write(&mut b, &bytes);
                }
                (_, Some(resolve), _) => {
                    let _ = resolve.call1(&JsValue::NULL, &JsValue::NULL);
                }
                _ => {}
            }
            promise
        };
        Box::pin(async move {
            let value = JsFuture::from(promise).await.ok()?;
            let bytes = value.dyn_into::<Uint8Array>().ok()?.to_vec();
            decode_frame(&bytes).ok()
        })
    }

    /// The burrow's server key, once its handshake has arrived.
    pub fn server_key(&self) -> Option<[u8; 32]> {
        self.inner.borrow().server_key.get()
    }

    /// Resolve every awaited reply with `null`: the socket is gone.
    fn release_calls(b: &Inner) {
        let waiting: Vec<Function> = b
            .pending_calls
            .borrow_mut()
            .drain()
            .map(|(_, f)| f)
            .collect();
        for resolve in waiting {
            let _ = resolve.call1(&JsValue::NULL, &JsValue::NULL);
        }
    }

    /// Write `bytes` to the socket, surfacing failures on the api-event sink.
    fn write(b: &mut Inner, bytes: &[u8]) {
        match &b.ws {
            Some(ws) if ws.ready_state() == WS_OPEN => {
                if let Err(err) = ws.send_with_u8_array(bytes) {
                    b.emit(Event::CommandFailed {
                        detail: format!("send failed: {err:?}"),
                    });
                }
            }
            // Socket present but still CONNECTING (or closing): drop silently.
            // Sending on a non-OPEN socket throws a spurious error; a read
            // request re-fires on the next navigation, and auth/who are (re)sent
            // from the `open` callback once the socket is ready.
            Some(_) => {}
            None => b.emit(Event::CommandFailed {
                detail: "not connected".to_string(),
            }),
        }
    }

    /// Open the socket to the latched endpoint and wire up its callbacks.
    fn connect(inner: &Rc<RefCell<Inner>>) {
        // A socket already exists: a manual redial raced the pending backoff
        // timer; whichever opened first wins, the other bails (no double dial).
        if inner.borrow().ws.is_some() {
            return;
        }
        let (url, attempt) = {
            let b = inner.borrow();
            (wire::normalize_ws_url(&b.endpoint), b.reconnect_attempt)
        };
        let url = match url {
            Ok(url) => url,
            Err(error) => {
                let mut b = inner.borrow_mut();
                b.want_connected = false;
                b.emit_conn(ConnState::Offline);
                b.emit(Event::CommandFailed {
                    detail: format!("unsafe WebSocket endpoint: {error}"),
                });
                return;
            }
        };
        // A first dial is "Connecting"; a redial after a drop is "Reconnecting".
        inner.borrow().emit_conn(if attempt == 0 {
            ConnState::Connecting
        } else {
            ConnState::Reconnecting
        });

        let ws = match WebSocket::new(&url) {
            Ok(ws) => ws,
            Err(err) => {
                inner.borrow().emit(Event::CommandFailed {
                    detail: format!("could not open {url}: {err:?}"),
                });
                // Treat a failed open like a drop: back off and retry.
                Self::schedule_reconnect(inner);
                return;
            }
        };
        ws.set_binary_type(BinaryType::Arraybuffer);

        // open → reset backoff, go Online, (re)send Hello.
        let on_open = {
            let inner = inner.clone();
            Closure::<dyn FnMut(WebEvent)>::new(move |_evt: WebEvent| {
                let mut b = inner.borrow_mut();
                b.reconnect_attempt = 0;
                b.alive = true;
                b.emit_conn(ConnState::Online);
                let id = b.next_request_id();
                // Present our portable identity in the handshake so peers can
                // verify who we are across burrows (the verified ✓ in People).
                let pubkey = Some(crate::identity::load_or_create().public());
                match wire::hello_request(id, pubkey).and_then(|f| encode_frame(&f)) {
                    Ok(bytes) => {
                        if let Some(ws) = &b.ws {
                            let _ = ws.send_with_u8_array(&bytes);
                        }
                    }
                    Err(err) => b.emit(Event::CommandFailed {
                        detail: format!("hello: {err}"),
                    }),
                }
            })
        };
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        // message → decode Frame once → fan out to api + FILE sinks.
        let on_message = {
            let inner = inner.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |evt: MessageEvent| {
                let Ok(buf) = evt.data().dyn_into::<ArrayBuffer>() else {
                    // Text frames and Blob payloads are not part of RHP framing.
                    return;
                };
                let bytes = Uint8Array::new(&buf).to_vec();
                let b = inner.borrow();
                match decode_frame(&bytes) {
                    Ok(frame) => {
                        // A reply an async flow is awaiting is its alone. Only
                        // a reply: a push carries a stamped sequence number in
                        // its id, which can equal a request id.
                        let is_reply = frame.kind == FrameKind::Reply;
                        let awaited = if is_reply {
                            b.pending_calls.borrow_mut().remove(&frame.id)
                        } else {
                            None
                        };
                        if let Some(resolve) = awaited {
                            let copy = Uint8Array::from(bytes.as_slice());
                            let _ = resolve.call1(&JsValue::NULL, &copy);
                            return;
                        }
                        // Proof of possession: if the handshake ack challenged our
                        // identity key, sign the nonce and return a KeyProof so the
                        // burrow surfaces the key as *verified*. Fire-and-forget
                        // (the server acks it); only the socket + our key are
                        // needed, so it's safe under the immutable borrow.
                        if let Some(key) = wire::hello_ack_server_key(&frame) {
                            b.server_key.set(Some(key));
                        }
                        if let Some(nonce) = wire::hello_ack_challenge(&frame) {
                            if let Some(ws) = &b.ws {
                                // The browser can't read the TLS cert, so we sign
                                // with a zero channel binder: this proves key
                                // possession (stops passive pubkey-copying) but is
                                // not relay-proof over WS — the UI reflects that.
                                let msg = rabbithole_proto::hello::key_auth_message(
                                    &rabbithole_proto::hello::NO_CHANNEL_BINDING,
                                    &nonce,
                                );
                                let sig = crate::identity::load_or_create().sign(&msg).to_vec();
                                if let Some(bytes) = wire::key_proof_frame(sig)
                                    .ok()
                                    .and_then(|f| encode_frame(&f).ok())
                                {
                                    let _ = ws.send_with_u8_array(&bytes);
                                }
                            }
                        }
                        for event in wire::frame_to_events(&frame) {
                            b.emit(event);
                        }
                        for event in wire::frame_to_file_events(&frame) {
                            b.emit_file(event);
                        }
                        // A reply to a management request, whatever its
                        // family: hand it on with what the request was about.
                        let asked = if is_reply {
                            b.pending_admin.borrow_mut().remove(&frame.id)
                        } else {
                            None
                        };
                        if let Some(tag) = asked {
                            let mut events = wire::frame_to_admin_events(&frame);
                            if events.is_empty() && frame.error.is_none() {
                                events.push(AdminEvent::Ack("Done.".into()));
                            }
                            b.emit_admin((tag, events));
                        }
                        // A FileContent reply arrives for anything this
                        // session asked to download. Where the bytes go is
                        // the app's to say — kept, or looked at where they
                        // are — and a save is what happens without a sink.
                        // Touches no Inner state, so it's borrow-safe here.
                        if let Some(dl) = wire::frame_to_file_content(&frame) {
                            match &b.file_bytes_sink {
                                Some(sink) => sink(dl),
                                None => crate::save::save_bytes(&dl.name, &dl.mime, &dl.bytes),
                            }
                        }
                        if let Some(route) = wire::frame_to_notice_route(&frame) {
                            b.emit_notice(route);
                        }
                        if let Some(widgets) = wire::frame_to_front_page(&frame) {
                            b.emit_front_page(widgets);
                        }
                        if let Some(answer) = wire::frame_to_radio_answer(&frame) {
                            if let Some(sink) = &b.radio_requests_sink {
                                sink(answer);
                            }
                        }
                        if let Some(rooms) = wire::frame_to_rooms(&frame) {
                            if let Some(sink) = &b.rooms_sink {
                                sink(rooms);
                            }
                        }
                        if let Some(room) = wire::frame_to_room(&frame) {
                            if let Some(sink) = &b.room_sink {
                                sink(room);
                            }
                        }
                        if let Some(wishes) = wire::frame_to_wishes(&frame) {
                            if let Some(sink) = &b.wishes_sink {
                                sink(wishes);
                            }
                        }
                        if let Some(wish) = wire::frame_to_wish(&frame) {
                            if let Some(sink) = &b.wish_sink {
                                sink(wish);
                            }
                        }
                        if let Some(sessions) = wire::frame_to_sessions(&frame) {
                            if let Some(sink) = &b.sessions_sink {
                                sink(sessions);
                            }
                        }
                        if let Some(roster) = wire::frame_to_who(&frame) {
                            b.emit_who(roster);
                        }
                        if let Some(delta) = wire::frame_to_presence(&frame) {
                            b.emit_presence(delta);
                        }
                        if let Some(tree) = wire::frame_to_board_tree(&frame) {
                            if let Some(sink) = &b.board_tree_sink {
                                sink(tree);
                            }
                        }
                        if let Some(boards) = wire::frame_to_boards(&frame) {
                            b.emit_boards(boards);
                        }
                        if let Some(threads) = wire::frame_to_threads(&frame) {
                            b.emit_threads(threads);
                        }
                        if let Some(posts) = wire::frame_to_posts(&frame) {
                            b.emit_posts(posts);
                        }
                        if let Some(threads) = wire::frame_to_dm_threads(&frame) {
                            b.emit_dm_threads(threads);
                        }
                        if let Some(listing) = wire::frame_to_radio_listing(&frame) {
                            b.emit_radio_listing(listing);
                        }
                        if let Some(msgs) = wire::frame_to_dm_history(&frame) {
                            // Pair (FIFO) with the peer it was requested for so
                            // the app applies it only to that conversation.
                            let peer = b.pending_dm_history.borrow_mut().pop_front();
                            if let Some(peer) = peer {
                                b.emit_dm_history((peer, msgs));
                            }
                        }
                        if let Some(dm) = wire::frame_to_dm_received(&frame) {
                            b.emit_dm_received(dm);
                        }
                        if let Some(members) = wire::frame_to_members(&frame) {
                            b.emit_members(members);
                        }
                        if let Some(profile) = wire::frame_to_profile(&frame) {
                            b.emit_profile(profile);
                        }
                        // A BlobData reply only follows an avatar BlobGet here.
                        // Pair it (FIFO) with the hex it was requested for so the
                        // app can confirm it still matches the selected profile.
                        if let Some(bytes) = wire::frame_to_blob(&frame) {
                            let hex = b.pending_avatars.borrow_mut().pop_front();
                            if let Some(hex) = hex {
                                b.emit_avatar((hex, wire::blob_to_data_url(&bytes)));
                            }
                        }
                    }
                    Err(err) => b.emit(Event::CommandFailed {
                        detail: format!("decode: {err}"),
                    }),
                }
            })
        };
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // close → either a final Disconnected, or a scheduled reconnect.
        let on_close = {
            let inner = inner.clone();
            Closure::<dyn FnMut(CloseEvent)>::new(move |evt: CloseEvent| {
                let want = {
                    let mut b = inner.borrow_mut();
                    b.alive = false;
                    b.ws = None;
                    Self::release_calls(&b);
                    b.want_connected
                };
                if want {
                    // Unexpected drop: back off and retry, staying "Reconnecting".
                    Self::schedule_reconnect(&inner);
                } else {
                    let b = inner.borrow();
                    let reason = evt.reason();
                    let reason = if reason.is_empty() {
                        format!("closed (code {})", evt.code())
                    } else {
                        reason
                    };
                    b.emit(Event::Disconnected { reason });
                    b.emit_conn(ConnState::Offline);
                }
            })
        };
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        // error → CommandFailed (a close event follows and drives reconnect).
        let on_error = {
            let inner = inner.clone();
            Closure::<dyn FnMut(WebEvent)>::new(move |_evt: WebEvent| {
                inner.borrow().emit(Event::CommandFailed {
                    detail: "websocket error".to_string(),
                });
            })
        };
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        {
            let mut b = inner.borrow_mut();
            b.generation = b.generation.wrapping_add(1);
            b.ws = Some(ws);
            b._on_open = Some(on_open);
            b._on_message = Some(on_message);
            b._on_close = Some(on_close);
            b._on_error = Some(on_error);
        }

        Self::spawn_keepalive(inner.clone());
    }

    /// Arm a jittered exponential-backoff timer, then redial (if still wanted).
    ///
    /// The delay comes from the pure, host-tested
    /// [`backoff_delay`](crate::conn::backoff_delay); the jitter seam is the
    /// browser's `Math.random()`.
    fn schedule_reconnect(inner: &Rc<RefCell<Inner>>) {
        let delay = {
            let mut b = inner.borrow_mut();
            if !b.want_connected {
                return;
            }
            let attempt = b.reconnect_attempt;
            b.reconnect_attempt = attempt.saturating_add(1);
            b.emit_conn(ConnState::Reconnecting);
            backoff_delay(attempt, Math::random())
        };
        let inner = inner.clone();
        spawn_local(async move {
            TimeoutFuture::new(delay.as_millis() as u32).await;
            if inner.borrow().want_connected {
                Self::connect(&inner);
            }
        });
    }

    /// Drive a periodic keepalive ping until the socket closes.
    fn spawn_keepalive(inner: Rc<RefCell<Inner>>) {
        let my_generation = inner.borrow().generation;
        spawn_local(async move {
            loop {
                TimeoutFuture::new(KEEPALIVE_MS).await;
                let mut b = inner.borrow_mut();
                // Exit once a newer socket (reconnect) has superseded this one,
                // otherwise the loop would resurrect itself on the shared
                // `alive` flag and pings would multiply across reconnects.
                if !b.alive || b.generation != my_generation {
                    break;
                }
                let Some(ws) = b.ws.clone() else { break };
                if ws.ready_state() != WS_OPEN {
                    continue;
                }
                let id = b.next_request_id();
                if let Ok(bytes) = wire::ping_request(id).and_then(|f| encode_frame(&f)) {
                    let _ = ws.send_with_u8_array(&bytes);
                }
            }
        });
    }

    /// Encode `command` to a frame and write it to the open socket.
    fn send_command(&self, command: &Command) {
        let mut b = self.inner.borrow_mut();
        let id = b.next_request_id();
        match wire::command_to_frame(command, id) {
            Ok(Some(frame)) => match encode_frame(&frame) {
                Ok(bytes) => Self::write(&mut b, &bytes),
                Err(err) => b.emit(Event::CommandFailed {
                    detail: format!("encode: {err}"),
                }),
            },
            Ok(None) => {}
            Err(err) => b.emit(Event::CommandFailed {
                detail: format!("map: {err}"),
            }),
        }
    }
}

impl Default for WsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl EventClient for WsClient {
    fn on_event(&mut self, sink: EventSink) {
        self.inner.borrow_mut().sink = Some(sink);
    }

    fn dispatch(&mut self, command: Command) {
        match &command {
            Command::Connect { endpoint, .. } => {
                {
                    let mut b = self.inner.borrow_mut();
                    b.endpoint = endpoint.clone();
                    b.want_connected = true;
                    b.reconnect_attempt = 0;
                }
                Self::connect(&self.inner);
            }
            Command::Disconnect => {
                let mut b = self.inner.borrow_mut();
                b.alive = false;
                b.want_connected = false;
                if let Some(ws) = &b.ws {
                    // `Disconnected`/`Offline` are emitted by the close callback.
                    let _ = ws.close();
                }
            }
            _ => self.send_command(&command),
        }
    }
}
