//! Browser WebSocket transport for RHP (`wasm32-unknown-unknown` only).
//!
//! [`WsClient`] speaks the RabbitHole Protocol over a real browser
//! [`WebSocket`], one binary message per [`Frame`] (the message-transport
//! framing described in [`rabbithole_proto::codec`] — no length prefix). It
//! implements the async [`EventClient`](crate::wire::EventClient) seam, so it
//! is a drop-in alternative to [`MockClient`](crate::client::MockClient).
//!
//! All Command ↔ Frame ↔ Event mapping lives in [`crate::wire`] (host-tested);
//! this module is only the wasm glue — socket lifecycle, binary I/O, and
//! wiring the browser's event callbacks into the registered sink. It is
//! validated by `cargo check --target wasm32-unknown-unknown`.
//!
//! # Lifecycle
//!
//! 1. [`Command::Connect`] opens the socket (binary type = `ArrayBuffer`).
//! 2. On `open`, a [`Hello`](rabbithole_proto::Hello) request is sent.
//! 3. Each inbound binary message is decoded to a [`Frame`] and mapped to
//!    [`Event`]s via [`wire::frame_to_events`](crate::wire::frame_to_events),
//!    which are pushed into the sink. The server's `HelloAck` becomes
//!    [`Event::Connected`].
//! 4. [`Command::Disconnect`] (or a server `close`) closes the socket; `close`
//!    emits [`Event::Disconnected`].
//!
//! A 30-second keepalive [`Ping`](rabbithole_proto::session::Ping) loop runs
//! for the socket's lifetime.
//!
//! # Deferred
//!
//! Reconnect / backoff, session resume, and binary attachments — see the
//! [`crate::wire`] module docs for the full deferred list.

use std::cell::RefCell;
use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{BinaryType, CloseEvent, Event as WebEvent, MessageEvent, WebSocket};

use rabbithole_core::api::{Command, Event};
use rabbithole_proto::{decode_frame, encode_frame, RequestId};

use crate::wire::{self, EventClient, EventSink};

/// Keepalive interval, milliseconds.
const KEEPALIVE_MS: u32 = 30_000;
/// `WebSocket.readyState` value for an open socket.
const WS_OPEN: u16 = 1;

/// A browser WebSocket [`EventClient`] speaking RHP.
///
/// Cheap to clone: all state lives behind a shared `Rc<RefCell<..>>` so the
/// socket's event callbacks and the keepalive task can reach it.
#[derive(Clone)]
pub struct WsClient {
    inner: Rc<RefCell<Inner>>,
}

/// Shared, mutable transport state.
struct Inner {
    ws: Option<WebSocket>,
    sink: Option<EventSink>,
    next_id: u64,
    /// While `true`, the keepalive loop keeps pinging; cleared on close.
    alive: bool,
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
                next_id: 0,
                alive: false,
                _on_open: None,
                _on_message: None,
                _on_close: None,
                _on_error: None,
            })),
        }
    }

    /// Open the socket to `endpoint` and wire up its callbacks.
    fn connect(inner: &Rc<RefCell<Inner>>, endpoint: &str) {
        let url = wire::normalize_ws_url(endpoint);
        let ws = match WebSocket::new(&url) {
            Ok(ws) => ws,
            Err(err) => {
                inner.borrow().emit(Event::CommandFailed {
                    detail: format!("could not open {url}: {err:?}"),
                });
                return;
            }
        };
        ws.set_binary_type(BinaryType::Arraybuffer);

        // open → send Hello.
        let on_open = {
            let inner = inner.clone();
            Closure::<dyn FnMut(WebEvent)>::new(move |_evt: WebEvent| {
                let mut b = inner.borrow_mut();
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

        // message → decode Frame → map → sink.
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
                    }
                    Err(err) => b.emit(Event::CommandFailed {
                        detail: format!("decode: {err}"),
                    }),
                }
            })
        };
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // close → Disconnected.
        let on_close = {
            let inner = inner.clone();
            Closure::<dyn FnMut(CloseEvent)>::new(move |evt: CloseEvent| {
                let mut b = inner.borrow_mut();
                b.alive = false;
                let reason = evt.reason();
                let reason = if reason.is_empty() {
                    format!("closed (code {})", evt.code())
                } else {
                    reason
                };
                b.emit(Event::Disconnected { reason });
            })
        };
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        // error → CommandFailed (a close event follows for the disconnect).
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
            b.ws = Some(ws);
            b.alive = true;
            b._on_open = Some(on_open);
            b._on_message = Some(on_message);
            b._on_close = Some(on_close);
            b._on_error = Some(on_error);
        }

        Self::spawn_keepalive(inner.clone());
    }

    /// Drive a periodic keepalive ping until the socket closes.
    fn spawn_keepalive(inner: Rc<RefCell<Inner>>) {
        spawn_local(async move {
            loop {
                TimeoutFuture::new(KEEPALIVE_MS).await;
                let mut b = inner.borrow_mut();
                if !b.alive {
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
                Ok(bytes) => match &b.ws {
                    Some(ws) => {
                        if let Err(err) = ws.send_with_u8_array(&bytes) {
                            b.emit(Event::CommandFailed {
                                detail: format!("send failed: {err:?}"),
                            });
                        }
                    }
                    None => b.emit(Event::CommandFailed {
                        detail: "not connected".to_string(),
                    }),
                },
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
            Command::Connect { endpoint, .. } => Self::connect(&self.inner, endpoint),
            Command::Disconnect => {
                let mut b = self.inner.borrow_mut();
                b.alive = false;
                if let Some(ws) = &b.ws {
                    // `Disconnected` is emitted by the close callback.
                    let _ = ws.close();
                }
            }
            _ => self.send_command(&command),
        }
    }
}
