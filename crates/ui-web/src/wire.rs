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
use rabbithole_proto::filelib::{
    AreaList, AreaListRequest, FileAdded, FileAreaView, FileContent, FileDownloadRequest,
    FileNodeView, FileUpload, FolderListRequest, NodeGet, NodeList, NodeReply,
};
use rabbithole_proto::hello::{CapabilitySet, Hello, HelloAck};
use rabbithole_proto::presence::Who;
use rabbithole_proto::session::{AuthPassword, Ping};
use rabbithole_proto::transfer::{
    FileChunk, FileChunkRequest, TransferAbort, TransferOpen, TransferTicket,
};
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

// ---------------------------------------------------------------------------
// FILE family (family 5): libraries, folders, metadata, and transfers.
//
// The core [`Command`]/[`Event`] enums carry no file variants yet (and this
// crate must not modify the core), so the SPA drives file libraries through a
// *local*, file-specific vocabulary. The mapping mirrors [`command_to_frame`]
// / [`frame_to_events`] exactly — pure, DOM-free, and host-tested — so it is
// ready to plug into either transport unchanged. When the core api grows file
// variants, these fold into the shared enums with no shape change.
// ---------------------------------------------------------------------------

/// A file-library action the SPA issues, mapped to a FILE-family request
/// [`Frame`] by [`file_command_to_frame`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileCommand {
    /// List every file area. → [`AreaList`].
    ListAreas,
    /// List a folder's children (`path` `None`/empty = area root).
    /// → [`NodeList`].
    ListFolder {
        /// Area slug to browse.
        area: String,
        /// Folder path within the area, or `None` for the root.
        path: Option<String>,
    },
    /// Fetch a single node's metadata. → [`NodeReply`].
    GetNode {
        /// Node id.
        id: i64,
    },
    /// Download a small file inline. → [`FileContent`].
    Download {
        /// Node id to download.
        id: i64,
    },
    /// Upload a small file inline. → [`NodeReply`] (+ a [`FileAdded`] push).
    Upload {
        /// Destination area slug.
        area: String,
        /// Destination folder path, or `None` for the area root.
        parent: Option<String>,
        /// File name.
        name: String,
        /// MIME type.
        mime: String,
        /// Uploader comment.
        comment: String,
        /// File bytes.
        bytes: Vec<u8>,
    },
    /// Open a ticketed (resumable) download. → [`TransferTicket`].
    OpenDownload {
        /// Node id to transfer.
        node_id: i64,
    },
    /// Request one byte range of an open transfer. → [`FileChunk`].
    RequestChunk {
        /// Transfer id from the ticket.
        transfer_id: u64,
        /// Byte offset.
        offset: u64,
        /// Range length.
        len: u32,
    },
    /// Abandon an open transfer. → ack.
    AbortTransfer {
        /// Transfer id to abort.
        transfer_id: u64,
    },
}

/// A file-library event decoded from an inbound FILE-family [`Frame`] by
/// [`frame_to_file_events`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum FileEvent {
    /// The area list arrived.
    AreasListed(Vec<FileAreaView>),
    /// A folder listing arrived.
    FolderListed {
        /// The folder's child nodes.
        nodes: Vec<FileNodeView>,
    },
    /// A single node's metadata arrived (from a get, upload, edit, or rate).
    NodeUpdated(FileNodeView),
    /// An inline download completed.
    FileDownloaded {
        /// The downloaded node.
        node: FileNodeView,
        /// Number of bytes received.
        size: usize,
    },
    /// A file landed in an area (a push): clients refresh listings.
    FileAdded {
        /// Area slug.
        area: String,
        /// New node id.
        id: i64,
    },
    /// A ticketed transfer was authorised.
    TransferOpened {
        /// Transfer id.
        transfer_id: u64,
        /// Total size in bytes.
        size: u64,
        /// Bytes the server already holds (resume point).
        server_have: u64,
    },
    /// A transfer byte range arrived.
    ChunkReceived {
        /// Transfer id.
        transfer_id: u64,
        /// Range offset.
        offset: u64,
        /// Whether this was the final chunk.
        last: bool,
        /// Range length in bytes.
        len: usize,
    },
    /// A FILE request failed.
    Failed(String),
}

/// Map a [`FileCommand`] to the FILE-family request [`Frame`] that carries it.
pub fn file_command_to_frame(
    command: &FileCommand,
    id: RequestId,
) -> Result<Option<Frame>, ProtoError> {
    let frame =
        match command {
            FileCommand::ListAreas => Frame::request(id, &AreaListRequest)?,
            FileCommand::ListFolder { area, path } => {
                Frame::request(id, &FolderListRequest::new(area.clone(), path.clone()))?
            }
            FileCommand::GetNode { id: node_id } => Frame::request(id, &NodeGet::new(*node_id))?,
            FileCommand::Download { id: node_id } => {
                Frame::request(id, &FileDownloadRequest::new(*node_id))?
            }
            FileCommand::Upload {
                area,
                parent,
                name,
                mime,
                comment,
                bytes,
            } => Frame::request(
                id,
                &FileUpload::new(area.clone(), parent.clone(), name.clone(), bytes.clone())
                    .with_meta(mime.clone(), String::new(), comment.clone()),
            )?,
            FileCommand::OpenDownload { node_id } => {
                Frame::request(id, &TransferOpen::download(*node_id))?
            }
            FileCommand::RequestChunk {
                transfer_id,
                offset,
                len,
            } => Frame::request(id, &FileChunkRequest::new(*transfer_id, *offset, *len))?,
            FileCommand::AbortTransfer { transfer_id } => {
                Frame::request(id, &TransferAbort::new(*transfer_id))?
            }
        };
    Ok(Some(frame))
}

/// Map an inbound FILE-family [`Frame`] to the [`FileEvent`]s it produces.
///
/// Frames from other families, or FILE frames carrying a not-yet-mapped
/// message type, produce an empty vector — matching the "tolerate unknown
/// messages" contract.
pub fn frame_to_file_events(frame: &Frame) -> Vec<FileEvent> {
    if let Some(code) = frame.error {
        return vec![FileEvent::Failed(format!("server error: {code:?}"))];
    }
    if let Some(Ok(m)) = frame.decode::<AreaList>() {
        return vec![FileEvent::AreasListed(m.areas)];
    }
    if let Some(Ok(m)) = frame.decode::<NodeList>() {
        return vec![FileEvent::FolderListed { nodes: m.nodes }];
    }
    if let Some(Ok(m)) = frame.decode::<FileContent>() {
        let size = m.bytes.len();
        return vec![FileEvent::FileDownloaded { node: m.node, size }];
    }
    // NodeReply backs get/upload/edit/rate/alias: the reducer upserts it.
    if let Some(Ok(m)) = frame.decode::<NodeReply>() {
        return vec![FileEvent::NodeUpdated(m.node)];
    }
    if let Some(Ok(m)) = frame.decode::<FileAdded>() {
        return vec![FileEvent::FileAdded {
            area: m.area,
            id: m.id,
        }];
    }
    if let Some(Ok(m)) = frame.decode::<TransferTicket>() {
        return vec![FileEvent::TransferOpened {
            transfer_id: m.transfer_id,
            size: m.size,
            server_have: m.server_have,
        }];
    }
    if let Some(Ok(m)) = frame.decode::<FileChunk>() {
        let len = m.bytes.len();
        return vec![FileEvent::ChunkReceived {
            transfer_id: m.transfer_id,
            offset: m.offset,
            last: m.last,
            len,
        }];
    }
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
    fn file_list_areas_maps_to_area_list_request() {
        let frame = file_command_to_frame(&FileCommand::ListAreas, RequestId(1))
            .unwrap()
            .expect("list-areas produces a frame");
        assert_eq!(frame.family, Family::FILE);
        assert!(frame.decode::<AreaListRequest>().unwrap().is_ok());
    }

    #[test]
    fn file_list_folder_carries_area_and_path() {
        let frame = file_command_to_frame(
            &FileCommand::ListFolder {
                area: "warez".into(),
                path: Some("utils".into()),
            },
            RequestId(2),
        )
        .unwrap()
        .expect("folder listing produces a frame");
        let decoded = frame.decode::<FolderListRequest>().unwrap().unwrap();
        assert_eq!(decoded.area, "warez");
        assert_eq!(decoded.path.as_deref(), Some("utils"));
    }

    #[test]
    fn file_upload_carries_bytes_and_meta() {
        let frame = file_command_to_frame(
            &FileCommand::Upload {
                area: "warez".into(),
                parent: None,
                name: "a.txt".into(),
                mime: "text/plain".into(),
                comment: "hi".into(),
                bytes: vec![1, 2, 3],
            },
            RequestId(3),
        )
        .unwrap()
        .expect("upload produces a frame");
        let decoded = frame.decode::<FileUpload>().unwrap().unwrap();
        assert_eq!(decoded.name, "a.txt");
        assert_eq!(decoded.mime, "text/plain");
        assert_eq!(decoded.comment, "hi");
        assert_eq!(decoded.bytes, vec![1, 2, 3]);
    }

    #[test]
    fn file_transfer_commands_target_file_family() {
        for cmd in [
            FileCommand::GetNode { id: 1 },
            FileCommand::Download { id: 1 },
            FileCommand::OpenDownload { node_id: 1 },
            FileCommand::RequestChunk {
                transfer_id: 1,
                offset: 0,
                len: 64,
            },
            FileCommand::AbortTransfer { transfer_id: 1 },
        ] {
            let frame = file_command_to_frame(&cmd, RequestId(9))
                .unwrap()
                .expect("command produces a frame");
            assert_eq!(frame.family, Family::FILE, "{cmd:?}");
        }
    }

    #[test]
    fn area_list_reply_maps_to_areas_listed() {
        let reply = AreaList::new(vec![FileAreaView::new("warez", "Warez", "the goods")]);
        let frame = Frame::push(&reply).unwrap();
        assert_eq!(
            frame_to_file_events(&frame),
            vec![FileEvent::AreasListed(vec![FileAreaView::new(
                "warez",
                "Warez",
                "the goods"
            )])]
        );
    }

    #[test]
    fn node_list_reply_maps_to_folder_listed() {
        let node = FileNodeView::new(7, "warez", 1, "a.lha", "a.lha");
        let frame = Frame::push(&NodeList::new(vec![node.clone()])).unwrap();
        assert_eq!(
            frame_to_file_events(&frame),
            vec![FileEvent::FolderListed { nodes: vec![node] }]
        );
    }

    #[test]
    fn file_content_maps_to_downloaded_with_size() {
        let node = FileNodeView::new(7, "warez", 1, "a.lha", "a.lha");
        let frame = Frame::push(&FileContent::new(node.clone(), vec![0u8; 42])).unwrap();
        assert_eq!(
            frame_to_file_events(&frame),
            vec![FileEvent::FileDownloaded { node, size: 42 }]
        );
    }

    #[test]
    fn node_reply_maps_to_node_updated() {
        let node = FileNodeView::new(7, "warez", 1, "a.lha", "a.lha");
        let frame = Frame::push(&NodeReply::new(node.clone())).unwrap();
        assert_eq!(
            frame_to_file_events(&frame),
            vec![FileEvent::NodeUpdated(node)]
        );
    }

    #[test]
    fn ticket_and_chunk_map_to_transfer_events() {
        let ticket = TransferTicket::new(5, [0; 32], 1024, [0; 16]).with_server_have(256);
        let frame = Frame::push(&ticket).unwrap();
        assert_eq!(
            frame_to_file_events(&frame),
            vec![FileEvent::TransferOpened {
                transfer_id: 5,
                size: 1024,
                server_have: 256,
            }]
        );

        let chunk = FileChunk::new(5, 256, true, vec![0u8; 64]);
        let frame = Frame::push(&chunk).unwrap();
        assert_eq!(
            frame_to_file_events(&frame),
            vec![FileEvent::ChunkReceived {
                transfer_id: 5,
                offset: 256,
                last: true,
                len: 64,
            }]
        );
    }

    #[test]
    fn file_error_reply_maps_to_failed() {
        let req = file_command_to_frame(&FileCommand::ListAreas, RequestId(1))
            .unwrap()
            .unwrap();
        let reply = Frame::error_reply(&req, ErrorCode::Forbidden);
        assert!(matches!(
            frame_to_file_events(&reply).as_slice(),
            [FileEvent::Failed(detail)] if detail.contains("Forbidden")
        ));
    }

    #[test]
    fn non_file_frame_yields_no_file_events() {
        let push = Frame::push(&ChatMessage::new("lobby", "bob", "hi", 0)).unwrap();
        assert!(frame_to_file_events(&push).is_empty());
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
