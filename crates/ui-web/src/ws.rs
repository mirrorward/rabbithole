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
//!    → the FILE-family sink, and
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
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use js_sys::{ArrayBuffer, Math, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{BinaryType, CloseEvent, Event as WebEvent, MessageEvent, WebSocket};

use rabbithole_core::api::{Command, Event};
use rabbithole_proto::{decode_frame, encode_frame, RequestId};

use crate::conn::{backoff_delay, ConnState};
use crate::wire::{
    self, EventClient, EventSink, FileCommand, FileEvent, NoticeRoute, PresenceDelta,
};

/// Keepalive interval, milliseconds.
const KEEPALIVE_MS: u32 = 30_000;
/// `WebSocket.readyState` value for an open socket.
const WS_OPEN: u16 = 1;

/// A sink the transport pushes connection-state changes into.
pub type ConnSink = Rc<dyn Fn(ConnState)>;
/// A sink the transport pushes decoded [`FileEvent`]s into.
pub type FileSink = Rc<dyn Fn(FileEvent)>;
/// A sink the transport pushes routed `ServerNotice` pushes into (radio
/// bridge updates vs. operator notices, already split by
/// [`wire::frame_to_notice_route`]).
pub type NoticeSink = Rc<dyn Fn(NoticeRoute)>;
/// A sink the transport pushes the present-user roster into (from a decoded
/// [`WhoList`](rabbithole_proto::presence::WhoList) reply). The core [`Event`]
/// enum has no roster variant, so this rides its own sink like FILE/notices.
pub type WhoSink = Rc<dyn Fn(Vec<String>)>;
/// A sink the transport pushes live roster deltas into (join/leave), keeping
/// the presence list fresh between full [`WhoSink`] snapshots.
pub type PresenceSink = Rc<dyn Fn(PresenceDelta)>;
/// A sink the transport pushes the board list into (from a decoded
/// [`BoardList`](rabbithole_proto::board::BoardList) reply).
pub type BoardSink = Rc<dyn Fn(Vec<crate::state::Board>)>;
/// A sink the transport pushes a board's thread list into.
pub type ThreadSink = Rc<dyn Fn(Vec<crate::state::Thread>)>;
/// A sink the transport pushes a thread's posts into.
pub type PostSink = Rc<dyn Fn(Vec<crate::state::Post>)>;
/// A sink the transport pushes the DM conversation list into.
pub type DmThreadSink = Rc<dyn Fn(Vec<crate::state::DmThread>)>;
/// A sink the transport pushes one conversation's message history into.
pub type DmHistorySink = Rc<dyn Fn(Vec<crate::state::DmMessage>)>;
/// A sink the transport pushes a live `(peer, message)` DM into.
pub type DmReceivedSink = Rc<dyn Fn((String, crate::state::DmMessage))>;

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
    notice_sink: Option<NoticeSink>,
    who_sink: Option<WhoSink>,
    presence_sink: Option<PresenceSink>,
    board_sink: Option<BoardSink>,
    thread_sink: Option<ThreadSink>,
    post_sink: Option<PostSink>,
    dm_thread_sink: Option<DmThreadSink>,
    dm_history_sink: Option<DmHistorySink>,
    dm_received_sink: Option<DmReceivedSink>,
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

    fn emit_notice(&self, route: NoticeRoute) {
        if let Some(sink) = &self.notice_sink {
            sink(route);
        }
    }

    fn emit_who(&self, roster: Vec<String>) {
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

    fn emit_dm_threads(&self, threads: Vec<crate::state::DmThread>) {
        if let Some(sink) = &self.dm_thread_sink {
            sink(threads);
        }
    }

    fn emit_dm_history(&self, msgs: Vec<crate::state::DmMessage>) {
        if let Some(sink) = &self.dm_history_sink {
            sink(msgs);
        }
    }

    fn emit_dm_received(&self, msg: (String, crate::state::DmMessage)) {
        if let Some(sink) = &self.dm_received_sink {
            sink(msg);
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
                notice_sink: None,
                who_sink: None,
                presence_sink: None,
                board_sink: None,
                thread_sink: None,
                post_sink: None,
                dm_thread_sink: None,
                dm_history_sink: None,
                dm_received_sink: None,
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
        let (url, attempt) = {
            let b = inner.borrow();
            (wire::normalize_ws_url(&b.endpoint), b.reconnect_attempt)
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
                match wire::hello_request(id).and_then(|f| encode_frame(&f)) {
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
                        for event in wire::frame_to_events(&frame) {
                            b.emit(event);
                        }
                        for event in wire::frame_to_file_events(&frame) {
                            b.emit_file(event);
                        }
                        if let Some(route) = wire::frame_to_notice_route(&frame) {
                            b.emit_notice(route);
                        }
                        if let Some(roster) = wire::frame_to_who(&frame) {
                            b.emit_who(roster);
                        }
                        if let Some(delta) = wire::frame_to_presence(&frame) {
                            b.emit_presence(delta);
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
                        if let Some(msgs) = wire::frame_to_dm_history(&frame) {
                            b.emit_dm_history(msgs);
                        }
                        if let Some(dm) = wire::frame_to_dm_received(&frame) {
                            b.emit_dm_received(dm);
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
