//! The native client session driver (feature `native`).
//!
//! Frontends (CLI now; TUI/GUI later) drive a [`Client`]: sequential
//! request/reply with pipelined push buffering. Pushes that arrive while a
//! reply is awaited are queued, never dropped, and read back via
//! [`Client::next_push`]. The wasm build gets its own driver in Wave 8 on
//! the same proto types.

use std::collections::VecDeque;

use rabbithole_net::quic::QuicTransport;
use rabbithole_net::tls::{CertFingerprint, ServerAuth};
use rabbithole_net::ws::WsTransport;
use rabbithole_net::{Connection, NetError, Transport};
use rabbithole_proto::{
    chat as pchat, presence as ppres, session as psess, CapabilitySet, ErrorCode, Frame, FrameKind,
    Hello, HelloAck, Message, RequestId,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("network: {0}")]
    Net(#[from] NetError),
    #[error("protocol: {0}")]
    Proto(#[from] rabbithole_proto::ProtoError),
    #[error("server refused: {0:?}")]
    Refused(ErrorCode),
    #[error("connection closed")]
    Closed,
    #[error("unexpected reply (family {family}, type {message_type})")]
    UnexpectedReply { family: u8, message_type: u16 },
    #[error("bad fingerprint: {0}")]
    BadFingerprint(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("integrity check failed: transferred bytes do not match the file's hash")]
    IntegrityError,
}

/// Whether a [`ClientError`] is a transient network drop that a
/// [`Client::reconnect`] can recover from — as opposed to a server refusal,
/// an unexpected reply, or a decode error, none of which retrying would fix.
fn is_transient(e: &ClientError) -> bool {
    matches!(
        e,
        ClientError::Net(_) | ClientError::Closed | ClientError::Io(_)
    )
}

/// A connected, hello-negotiated session.
pub struct Client {
    conn: Box<dyn Connection>,
    next_id: u64,
    pushes: VecDeque<Frame>,
    /// Highest push sequence seen (the resume replay cursor).
    pub replay_cursor: u64,
    pub server: HelloAck,
    /// Client-side transfer bandwidth cap in bytes/sec (`None` = unlimited).
    rate_limit: Option<u64>,
    /// Remembered dial parameters, so [`Client::reconnect`] can re-establish
    /// the same session after a drop (mobile network change, idle timeout).
    endpoint: String,
    server_name: Option<String>,
    fingerprint: Option<String>,
    client_name: String,
    client_version: String,
    /// The resumable session bearer token from the last successful password
    /// auth or resume (`None` for a guest session — the server does not let
    /// guests resume).
    session_token: Option<String>,
}

/// Bandwidth cap: sleep to hold the average transfer rate at or under `rate`
/// bytes/sec. `None`/0 disables it. A per-chunk sleep is a lower bound on the
/// achieved rate, which is the safe direction for a cap.
async fn throttle(rate: Option<u64>, bytes: usize) {
    if let Some(rate) = rate {
        if rate > 0 && bytes > 0 {
            let secs = bytes as f64 / rate as f64;
            tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
        }
    }
}

impl Client {
    /// Dial and perform the hello exchange. `endpoint` is `host:port`
    /// (QUIC, needs `fingerprint`) or a `ws://`/`wss://` URL.
    pub async fn connect(
        endpoint: &str,
        server_name: Option<&str>,
        fingerprint: Option<&str>,
        client_name: &str,
        client_version: &str,
    ) -> Result<Client, ClientError> {
        Self::connect_with_identity(
            endpoint,
            server_name,
            fingerprint,
            client_name,
            client_version,
            None,
        )
        .await
    }

    /// [`connect`](Self::connect), presenting a portable identity and *proving
    /// possession*: the public key rides in the `Hello`, and if the server
    /// answers with a challenge nonce the client signs it and returns a
    /// [`KeyProof`](rabbithole_proto::hello::KeyProof). Only then does the server
    /// surface the key in presence — the verified key peers use to tell
    /// same-handle strangers apart across burrows.
    pub async fn connect_with_identity(
        endpoint: &str,
        server_name: Option<&str>,
        fingerprint: Option<&str>,
        client_name: &str,
        client_version: &str,
        identity: Option<&rabbithole_identity::IdentityKey>,
    ) -> Result<Client, ClientError> {
        let pubkey = identity.map(|k| k.public().0);
        let transport: Box<dyn Transport> =
            if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
                Box::new(WsTransport)
            } else {
                let fp_hex = fingerprint
                    .ok_or_else(|| ClientError::BadFingerprint("required for QUIC".into()))?;
                let fp = CertFingerprint::from_hex(fp_hex)
                    .ok_or_else(|| ClientError::BadFingerprint(fp_hex.into()))?;
                let name = server_name.map(str::to_owned).unwrap_or_else(|| {
                    endpoint.split(':').next().unwrap_or("localhost").to_owned()
                });
                Box::new(QuicTransport::new(name, ServerAuth::Pinned(fp)))
            };

        let conn = transport.connect(endpoint).await?;
        let mut client = Client {
            conn,
            next_id: 1,
            pushes: VecDeque::new(),
            replay_cursor: 0,
            rate_limit: None,
            server: HelloAck::new(
                rabbithole_proto::PROTOCOL_VERSION,
                CapabilitySet::default(),
                "",
                "",
                [0; 32],
            ),
            endpoint: endpoint.to_string(),
            server_name: server_name.map(str::to_owned),
            fingerprint: fingerprint.map(str::to_owned),
            client_name: client_name.to_string(),
            client_version: client_version.to_string(),
            session_token: None,
        };
        let hello =
            Hello::new(client_name, client_version, CapabilitySet::default()).with_pubkey(pubkey);
        let ack: HelloAck = client.request(&hello).await?;
        // Prove possession of the identity key if the server challenged it. Sign
        // the channel-bound message: over QUIC the binder is the cert fingerprint
        // we pinned when dialing (so a malicious burrow can't relay this proof to
        // a server with a different cert); over WS there is no cert, so a zero
        // binder (possession-proven, not relay-proof — see hello::key_auth_message).
        if let (Some(key), Some(nonce)) = (identity, ack.challenge) {
            let binder = fingerprint
                .and_then(CertFingerprint::from_hex)
                .map(|fp| fp.0)
                .unwrap_or(rabbithole_proto::hello::NO_CHANNEL_BINDING);
            let msg = rabbithole_proto::hello::key_auth_message(&binder, &nonce);
            let sig = key.sign(&msg).0.to_vec();
            client
                .request_ack(&rabbithole_proto::hello::KeyProof::new(sig))
                .await?;
        }
        client.server = ack;
        Ok(client)
    }

    /// Send a typed request and await its typed reply. Pushes seen in the
    /// meantime are buffered.
    pub async fn request<M: Message, R: Message>(&mut self, msg: &M) -> Result<R, ClientError> {
        let id = self.send_request(msg).await?;
        let frame = self.wait_reply(id).await?;
        match frame.decode::<R>() {
            Some(Ok(reply)) => Ok(reply),
            Some(Err(e)) => Err(e.into()),
            None => Err(ClientError::UnexpectedReply {
                family: frame.family.0,
                message_type: frame.message_type,
            }),
        }
    }

    /// Send a typed request expecting an empty ack.
    pub async fn request_ack<M: Message>(&mut self, msg: &M) -> Result<(), ClientError> {
        let id = self.send_request(msg).await?;
        self.wait_reply(id).await.map(|_| ())
    }

    async fn send_request<M: Message>(&mut self, msg: &M) -> Result<RequestId, ClientError> {
        let id = RequestId(self.next_id);
        self.next_id += 1;
        self.conn.send(Frame::request(id, msg)?).await?;
        Ok(id)
    }

    async fn wait_reply(&mut self, id: RequestId) -> Result<Frame, ClientError> {
        loop {
            let Some(frame) = self.conn.recv().await? else {
                return Err(ClientError::Closed);
            };
            match frame.kind {
                FrameKind::Reply if frame.id == id => {
                    if let Some(code) = frame.error {
                        return Err(ClientError::Refused(code));
                    }
                    return Ok(frame);
                }
                FrameKind::Push => self.buffer_push(frame),
                // Replies to other (pipelined) requests aren't possible in
                // this sequential driver; ignore defensively.
                _ => {}
            }
        }
    }

    fn buffer_push(&mut self, frame: Frame) {
        self.replay_cursor = self.replay_cursor.max(frame.id.0);
        self.pushes.push_back(frame);
    }

    /// Next push: buffered if any, otherwise await one from the wire.
    pub async fn next_push(&mut self) -> Result<Option<Frame>, ClientError> {
        if let Some(f) = self.pushes.pop_front() {
            return Ok(Some(f));
        }
        loop {
            let Some(frame) = self.conn.recv().await? else {
                return Ok(None);
            };
            if frame.kind == FrameKind::Push {
                self.replay_cursor = self.replay_cursor.max(frame.id.0);
                return Ok(Some(frame));
            }
        }
    }

    // ---- Convenience wrappers -------------------------------------------

    pub async fn auth_password(
        &mut self,
        login: &str,
        password: &str,
    ) -> Result<psess::AuthOk, ClientError> {
        let ok: psess::AuthOk = self
            .request(&psess::AuthPassword::new(login, password))
            .await?;
        self.remember_token(&ok);
        Ok(ok)
    }

    pub async fn auth_guest(
        &mut self,
        desired_name: Option<String>,
    ) -> Result<psess::AuthOk, ClientError> {
        let ok: psess::AuthOk = self.request(&psess::AuthGuest::new(desired_name)).await?;
        self.remember_token(&ok); // guests get an empty token → not resumable
        Ok(ok)
    }

    pub async fn auth_resume(
        &mut self,
        token: &str,
        replay_cursor: u64,
    ) -> Result<psess::AuthOk, ClientError> {
        let ok: psess::AuthOk = self
            .request(&psess::AuthResume::new(token, replay_cursor))
            .await?;
        self.remember_token(&ok);
        Ok(ok)
    }

    /// Remember a non-empty session token so [`Client::reconnect`] can resume.
    /// An empty token (guest) leaves the session non-resumable.
    fn remember_token(&mut self, ok: &psess::AuthOk) {
        if !ok.token.is_empty() {
            self.session_token = Some(ok.token.clone());
        }
    }

    /// Whether this session can be resumed after a drop: a non-guest auth left
    /// a bearer token. Guest sessions and pre-auth connections cannot resume.
    pub fn is_resumable(&self) -> bool {
        self.session_token.is_some()
    }

    /// Re-establish the session after a connection drop: re-dial the same
    /// endpoint (fresh transport + Hello), then `AuthResume` with the stored
    /// token and the replay cursor so the server replays the pushes missed
    /// since. Buffered-but-unread pushes from before the drop are preserved
    /// ahead of any replayed ones, and the replay cursor never rewinds.
    ///
    /// Errors with [`ClientError::Closed`] if the session is not resumable
    /// (guest / never authenticated); network and refusal errors from the
    /// re-dial or resume propagate. On success the live connection is replaced
    /// and the returned [`psess::AuthOk`] has `resumed == true`.
    pub async fn reconnect(&mut self) -> Result<psess::AuthOk, ClientError> {
        let token = self.session_token.clone().ok_or(ClientError::Closed)?;
        let mut fresh = Client::connect(
            &self.endpoint,
            self.server_name.as_deref(),
            self.fingerprint.as_deref(),
            &self.client_name,
            &self.client_version,
        )
        .await?;
        let ok = fresh.auth_resume(&token, self.replay_cursor).await?;
        // Carry forward session-local state the fresh dial doesn't know about:
        // the (never-rewound) replay cursor, the bandwidth cap, and any pushes
        // we buffered but the frontend hadn't read yet — those precede the
        // server's replayed ones.
        fresh.replay_cursor = self.replay_cursor.max(fresh.replay_cursor);
        fresh.rate_limit = self.rate_limit;
        let mut pending = std::mem::take(&mut self.pushes);
        pending.extend(std::mem::take(&mut fresh.pushes));
        fresh.pushes = pending;
        *self = fresh;
        Ok(ok)
    }

    /// Like [`Client::request`], but transparently [`Client::reconnect`]s once
    /// and retries when the send/await fails on a **transient** network error
    /// (a dropped connection). Refusals, decode failures, and non-resumable
    /// (guest) sessions are not retried.
    ///
    /// Retry semantics are **at-least-once**: if the original request reached
    /// the server before the drop, the retry runs it again. Safe for
    /// idempotent reads (who / history / listings); non-idempotent callers
    /// (chat/post/transfer starts) should prefer [`Client::request`] and drive
    /// [`Client::reconnect`] themselves, or tolerate a possible duplicate.
    pub async fn request_resilient<M: Message, R: Message>(
        &mut self,
        msg: &M,
    ) -> Result<R, ClientError> {
        match self.request(msg).await {
            Err(e) if is_transient(&e) && self.is_resumable() => {
                self.reconnect().await?;
                self.request(msg).await
            }
            other => other,
        }
    }

    /// Await the Welcome push (sent right after auth). Other pushes seen
    /// while waiting stay buffered in arrival order.
    pub async fn expect_welcome(&mut self) -> Result<psess::Welcome, ClientError> {
        if let Some(pos) = self
            .pushes
            .iter()
            .position(|f| f.decode::<psess::Welcome>().is_some())
        {
            let frame = self.pushes.remove(pos).expect("position exists");
            return Ok(frame.decode::<psess::Welcome>().expect("checked")?);
        }
        loop {
            let Some(frame) = self.conn.recv().await? else {
                return Err(ClientError::Closed);
            };
            if frame.kind != FrameKind::Push {
                continue;
            }
            if let Some(w) = frame.decode::<psess::Welcome>() {
                self.replay_cursor = self.replay_cursor.max(frame.id.0);
                return Ok(w?);
            }
            self.buffer_push(frame);
        }
    }

    pub async fn who(&mut self) -> Result<Vec<ppres::UserSummary>, ClientError> {
        Ok(self.request::<_, ppres::WhoList>(&ppres::Who).await?.users)
    }

    pub async fn chat_send(&mut self, room: &str, text: &str) -> Result<(), ClientError> {
        self.request_ack(&pchat::ChatSend::new(room, text)).await
    }

    pub async fn chat_history(
        &mut self,
        room: &str,
        limit: u32,
    ) -> Result<Vec<pchat::ChatMessage>, ClientError> {
        Ok(self
            .request::<_, pchat::ChatHistory>(&pchat::ChatHistoryRequest::new(room, limit))
            .await?
            .messages)
    }

    pub async fn agreement_accept(&mut self) -> Result<(), ClientError> {
        self.request_ack(&psess::AgreementAccept).await
    }

    pub async fn ping(&mut self) -> Result<(), ClientError> {
        let _: psess::Pong = self.request(&psess::Ping).await?;
        Ok(())
    }

    // ---- Wave 2: registration, personas, directory, blobs, admin ---------

    pub async fn register(
        &mut self,
        login: &str,
        password: &str,
        invite_code: Option<String>,
    ) -> Result<psess::AuthOk, ClientError> {
        self.request(&rabbithole_proto::persona::Register::new(
            login,
            password,
            invite_code,
        ))
        .await
    }

    pub async fn personas(
        &mut self,
    ) -> Result<rabbithole_proto::persona::PersonaList, ClientError> {
        self.request(&rabbithole_proto::persona::PersonaListRequest)
            .await
    }

    pub async fn persona_create(
        &mut self,
        screen_name: &str,
    ) -> Result<rabbithole_proto::persona::PersonaReply, ClientError> {
        self.request(&rabbithole_proto::persona::PersonaCreate::new(screen_name))
            .await
    }

    pub async fn persona_switch(
        &mut self,
        id: i64,
    ) -> Result<rabbithole_proto::persona::PersonaReply, ClientError> {
        self.request(&rabbithole_proto::persona::PersonaSwitch::new(id))
            .await
    }

    pub async fn persona_update(
        &mut self,
        update: &rabbithole_proto::persona::PersonaUpdate,
    ) -> Result<rabbithole_proto::persona::PersonaReply, ClientError> {
        self.request(update).await
    }

    pub async fn profile_get(
        &mut self,
        screen_name: &str,
    ) -> Result<rabbithole_proto::directory::ProfileCard, ClientError> {
        self.request(&rabbithole_proto::directory::ProfileGet::new(screen_name))
            .await
    }

    pub async fn directory_search(
        &mut self,
        query: &str,
        limit: u32,
    ) -> Result<rabbithole_proto::directory::DirectoryResults, ClientError> {
        self.request(&rabbithole_proto::directory::DirectorySearch::new(
            query, limit,
        ))
        .await
    }

    pub async fn blob_put(
        &mut self,
        purpose: rabbithole_proto::blob::BlobPurpose,
        bytes: Vec<u8>,
    ) -> Result<[u8; 32], ClientError> {
        let r: rabbithole_proto::blob::BlobRef = self
            .request(&rabbithole_proto::blob::BlobPut::new(purpose, bytes))
            .await?;
        Ok(r.id)
    }

    pub async fn blob_get(&mut self, id: [u8; 32]) -> Result<Vec<u8>, ClientError> {
        let r: rabbithole_proto::blob::BlobData = self
            .request(&rabbithole_proto::blob::BlobGet::new(id))
            .await?;
        Ok(r.bytes)
    }

    // ---- Wave 2.2: presence, buddies, DMs ---------------------------------

    pub async fn presence_set(
        &mut self,
        state: rabbithole_proto::presence::PresenceState,
        status: Option<String>,
    ) -> Result<(), ClientError> {
        self.request_ack(&ppres::PresenceSet::new(state, status))
            .await
    }

    pub async fn buddy_list(&mut self) -> Result<ppres::BuddyList, ClientError> {
        self.request(&ppres::BuddyListRequest).await
    }

    pub async fn buddy_add(&mut self, screen_name: &str, group: &str) -> Result<(), ClientError> {
        self.request_ack(&ppres::BuddyAdd::new(screen_name, group))
            .await
    }

    pub async fn buddy_remove(&mut self, screen_name: &str) -> Result<(), ClientError> {
        self.request_ack(&ppres::BuddyRemove::new(screen_name))
            .await
    }

    pub async fn block_add(&mut self, screen_name: &str) -> Result<(), ClientError> {
        self.request_ack(&ppres::BlockAdd::new(screen_name)).await
    }

    pub async fn block_remove(&mut self, screen_name: &str) -> Result<(), ClientError> {
        self.request_ack(&ppres::BlockRemove::new(screen_name))
            .await
    }

    pub async fn dm_send(
        &mut self,
        msg: &rabbithole_proto::dm::DmSend,
    ) -> Result<rabbithole_proto::dm::DmSent, ClientError> {
        self.request(msg).await
    }

    pub async fn dm_history(
        &mut self,
        with: &str,
        before_id: i64,
        limit: u32,
    ) -> Result<Vec<rabbithole_proto::dm::DmMessage>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::dm::DmHistory>(
                &rabbithole_proto::dm::DmHistoryRequest::new(with, before_id, limit),
            )
            .await?
            .messages)
    }

    pub async fn dm_threads(
        &mut self,
    ) -> Result<Vec<rabbithole_proto::dm::DmThreadSummary>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::dm::DmThreads>(&rabbithole_proto::dm::DmThreadsRequest)
            .await?
            .threads)
    }

    pub async fn dm_mark_read(&mut self, with: &str, up_to_id: i64) -> Result<(), ClientError> {
        self.request_ack(&rabbithole_proto::dm::DmMarkRead::new(with, up_to_id))
            .await
    }

    // ---- Wave 13: E2EE prekey bundles + encrypted DM carriage -------------

    /// Publish this account's E2EE prekey bundle (public keys only).
    pub async fn key_bundle_publish(
        &mut self,
        bundle: &rabbithole_proto::keybundle::KeyBundlePublish,
    ) -> Result<(), ClientError> {
        self.request_ack(bundle).await
    }

    /// Fetch `screen_name`'s prekey bundle (consumes one one-time prekey
    /// server-side) to establish an encrypted session.
    pub async fn key_bundle_fetch(
        &mut self,
        screen_name: &str,
    ) -> Result<rabbithole_proto::keybundle::KeyBundle, ClientError> {
        self.request(&rabbithole_proto::keybundle::KeyBundleRequest::new(
            screen_name,
        ))
        .await
    }

    /// Send an end-to-end encrypted DM: the server relays `payload` opaquely and
    /// stores no plaintext.
    pub async fn dm_send_encrypted(
        &mut self,
        to: &str,
        payload: rabbithole_proto::dm::EncryptedPayload,
    ) -> Result<rabbithole_proto::dm::DmSent, ClientError> {
        self.request(&rabbithole_proto::dm::DmSend::new_encrypted(to, payload))
            .await
    }

    // ---- Wave 2.2b: rooms -------------------------------------------------

    pub async fn room_list(&mut self) -> Result<Vec<pchat::RoomInfo>, ClientError> {
        Ok(self
            .request::<_, pchat::RoomList>(&pchat::RoomListRequest)
            .await?
            .rooms)
    }

    pub async fn room_create(
        &mut self,
        req: &pchat::RoomCreate,
    ) -> Result<pchat::RoomInfo, ClientError> {
        Ok(self.request::<_, pchat::RoomInfoReply>(req).await?.room)
    }

    pub async fn room_join(&mut self, room: &str) -> Result<pchat::RoomInfo, ClientError> {
        Ok(self
            .request::<_, pchat::RoomInfoReply>(&pchat::RoomJoin::new(room))
            .await?
            .room)
    }

    pub async fn room_leave(&mut self, room: &str) -> Result<(), ClientError> {
        self.request_ack(&pchat::RoomLeave::new(room)).await
    }

    pub async fn room_invite(&mut self, room: &str, screen_name: &str) -> Result<(), ClientError> {
        self.request_ack(&pchat::RoomInvite::new(room, screen_name))
            .await
    }

    pub async fn room_topic(&mut self, room: &str, topic: &str) -> Result<(), ClientError> {
        self.request_ack(&pchat::RoomTopicSet::new(room, topic))
            .await
    }

    pub async fn room_kick(
        &mut self,
        room: &str,
        screen_name: &str,
        ban: bool,
    ) -> Result<(), ClientError> {
        self.request_ack(&pchat::RoomKick::new(room, screen_name, ban))
            .await
    }

    pub async fn room_members(&mut self, room: &str) -> Result<Vec<String>, ClientError> {
        Ok(self
            .request::<_, pchat::RoomMemberList>(&pchat::RoomMembersRequest::new(room))
            .await?
            .members)
    }

    // ---- Wave 2.3: welcome, theme, keyword --------------------------------

    pub async fn welcome_screen(
        &mut self,
    ) -> Result<rabbithole_proto::welcome::WelcomeScreen, ClientError> {
        self.request(&rabbithole_proto::welcome::WelcomeScreenRequest)
            .await
    }

    /// Fetch and verify the server theme bundle. Returns None when the
    /// server has no theme; errors only on transport/verification failure.
    pub async fn theme(
        &mut self,
    ) -> Result<Option<rabbithole_proto::welcome::ThemeBundle>, ClientError> {
        let reply: rabbithole_proto::welcome::ThemeReply = match self
            .request(&rabbithole_proto::welcome::ThemeGet)
            .await
        {
            Ok(r) => r,
            Err(ClientError::Refused(rabbithole_proto::ErrorCode::NotFound)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(crate::theme::verify_theme_bundle(
            &reply,
            &self.server.server_key,
        ))
    }

    pub async fn keyword_go(
        &mut self,
        word: &str,
    ) -> Result<rabbithole_proto::welcome::KeywordTarget, ClientError> {
        self.request(&rabbithole_proto::welcome::KeywordGo::new(word))
            .await
    }

    // ---- Wave 3.1: boards -------------------------------------------------

    pub async fn boards(&mut self) -> Result<Vec<rabbithole_proto::board::BoardInfo>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::board::BoardList>(
                &rabbithole_proto::board::BoardListRequest,
            )
            .await?
            .boards)
    }

    pub async fn board_create(
        &mut self,
        req: &rabbithole_proto::board::BoardCreate,
    ) -> Result<rabbithole_proto::board::BoardInfo, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::board::BoardCreated>(req)
            .await?
            .board)
    }

    pub async fn threads(
        &mut self,
        board: &str,
        limit: u32,
    ) -> Result<Vec<rabbithole_proto::board::ThreadSummary>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::board::ThreadList>(
                &rabbithole_proto::board::ThreadListRequest::new(board, limit),
            )
            .await?
            .threads)
    }

    pub async fn thread(
        &mut self,
        root: [u8; 32],
        limit: u32,
    ) -> Result<Vec<rabbithole_proto::board::PostView>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::board::ThreadPosts>(
                &rabbithole_proto::board::ThreadRequest::new(root, limit),
            )
            .await?
            .posts)
    }

    pub async fn post(
        &mut self,
        req: &rabbithole_proto::board::PostCreate,
    ) -> Result<rabbithole_proto::board::PostView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::board::PostReply>(req)
            .await?
            .post)
    }

    pub async fn post_edit(
        &mut self,
        target: [u8; 32],
        subject: &str,
        body: &str,
        mime: &str,
    ) -> Result<rabbithole_proto::board::PostView, ClientError> {
        let req = rabbithole_proto::board::PostEdit::new(target, subject, body, mime);
        Ok(self
            .request::<_, rabbithole_proto::board::PostReply>(&req)
            .await?
            .post)
    }

    pub async fn post_delete(&mut self, target: [u8; 32]) -> Result<(), ClientError> {
        self.request_ack(&rabbithole_proto::board::PostDelete::new(target))
            .await
    }

    pub async fn board_mark_read(&mut self, board: &str, up_to: i64) -> Result<(), ClientError> {
        self.request_ack(&rabbithole_proto::board::MarkRead::new(board, up_to))
            .await
    }

    // ---- Wave 3.2: the Wishing Well ---------------------------------------

    pub async fn wishes(
        &mut self,
        status: Option<u8>,
        limit: u32,
    ) -> Result<Vec<rabbithole_proto::wish::WishView>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::wish::WishList>(
                &rabbithole_proto::wish::WishListRequest::new(status, limit),
            )
            .await?
            .wishes)
    }

    pub async fn wish_create(
        &mut self,
        kind: u8,
        title: &str,
        details: &str,
    ) -> Result<rabbithole_proto::wish::WishView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::wish::WishReply>(
                &rabbithole_proto::wish::WishCreate::new(kind, title, details),
            )
            .await?
            .wish)
    }

    pub async fn wish_vote(
        &mut self,
        id: i64,
    ) -> Result<rabbithole_proto::wish::WishView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::wish::WishReply>(
                &rabbithole_proto::wish::WishVote::new(id),
            )
            .await?
            .wish)
    }

    pub async fn wish_set_status(
        &mut self,
        req: &rabbithole_proto::wish::WishSetStatus,
    ) -> Result<rabbithole_proto::wish::WishView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::wish::WishReply>(req)
            .await?
            .wish)
    }

    // ---- Wave 4.1: file libraries -----------------------------------------

    pub async fn file_areas(
        &mut self,
    ) -> Result<Vec<rabbithole_proto::filelib::FileAreaView>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::AreaList>(
                &rabbithole_proto::filelib::AreaListRequest,
            )
            .await?
            .areas)
    }

    pub async fn folder_list(
        &mut self,
        area: &str,
        path: Option<String>,
    ) -> Result<Vec<rabbithole_proto::filelib::FileNodeView>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeList>(
                &rabbithole_proto::filelib::FolderListRequest::new(area, path),
            )
            .await?
            .nodes)
    }

    pub async fn node_get(
        &mut self,
        id: i64,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeReply>(
                &rabbithole_proto::filelib::NodeGet::new(id),
            )
            .await?
            .node)
    }

    pub async fn area_create(
        &mut self,
        slug: &str,
        title: &str,
        description: &str,
    ) -> Result<rabbithole_proto::filelib::FileAreaView, ClientError> {
        let req =
            rabbithole_proto::filelib::AreaCreate::new(slug, title).with_description(description);
        Ok(self
            .request::<_, rabbithole_proto::filelib::AreaReply>(&req)
            .await?
            .area)
    }

    pub async fn folder_create(
        &mut self,
        req: &rabbithole_proto::filelib::FolderCreate,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeReply>(req)
            .await?
            .node)
    }

    pub async fn file_upload(
        &mut self,
        req: &rabbithole_proto::filelib::FileUpload,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeReply>(req)
            .await?
            .node)
    }

    pub async fn file_download(
        &mut self,
        id: i64,
    ) -> Result<rabbithole_proto::filelib::FileContent, ClientError> {
        self.request(&rabbithole_proto::filelib::FileDownloadRequest::new(id))
            .await
    }

    pub async fn node_delete(&mut self, id: i64) -> Result<(), ClientError> {
        self.request_ack(&rabbithole_proto::filelib::NodeDelete::new(id))
            .await
    }

    pub async fn set_file_metadata(
        &mut self,
        id: i64,
        icon: &str,
        comment: &str,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeReply>(
                &rabbithole_proto::filelib::SetMetadata::new(id, icon, comment),
            )
            .await?
            .node)
    }

    pub async fn file_search(
        &mut self,
        area: Option<String>,
        query: &str,
        limit: u32,
    ) -> Result<Vec<rabbithole_proto::filelib::FileNodeView>, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::SearchResults>(
                &rabbithole_proto::filelib::SearchRequest::new(area, query, limit),
            )
            .await?
            .nodes)
    }

    pub async fn rate_file(
        &mut self,
        id: i64,
        stars: u8,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeReply>(
                &rabbithole_proto::filelib::RateFile::new(id, stars),
            )
            .await?
            .node)
    }

    pub async fn alias_create(
        &mut self,
        req: &rabbithole_proto::filelib::AliasCreate,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        Ok(self
            .request::<_, rabbithole_proto::filelib::NodeReply>(req)
            .await?
            .node)
    }

    pub async fn totp_enroll(
        &mut self,
    ) -> Result<rabbithole_proto::persona::TotpEnrollInfo, ClientError> {
        self.request(&rabbithole_proto::persona::TotpEnrollBegin)
            .await
    }

    pub async fn totp_confirm(
        &mut self,
        code: &str,
    ) -> Result<rabbithole_proto::persona::RecoveryCodes, ClientError> {
        self.request(&rabbithole_proto::persona::TotpEnrollConfirm::new(code))
            .await
    }

    // ---- Wave 4.2: bulk transfers -----------------------------------------

    // ---- Swarm (the Warren, Wave 5) ------------------------------------

    /// Advertise files this client can serve (list-without-upload). Returns
    /// the ack with the granted TTL — re-announce before it lapses.
    pub async fn swarm_advertise(
        &mut self,
        entries: Vec<rabbithole_proto::swarm::AdvertEntry>,
        ttl_secs: u32,
    ) -> Result<rabbithole_proto::swarm::AdvertiseAck, ClientError> {
        self.request(&rabbithole_proto::swarm::AdvertiseFiles::new(
            entries, ttl_secs,
        ))
        .await
    }

    /// Withdraw advertisements (empty = all of this session's).
    pub async fn swarm_withdraw(&mut self, roots: Vec<[u8; 32]>) -> Result<(), ClientError> {
        self.request_ack(&rabbithole_proto::swarm::AdvertWithdraw::new(roots))
            .await
    }

    /// Who has this root? (Peers advertising it + whether the server does.)
    pub async fn swarm_find(
        &mut self,
        root: [u8; 32],
    ) -> Result<rabbithole_proto::swarm::SourceList, ClientError> {
        self.request(&rabbithole_proto::swarm::FindSources::new(root))
            .await
    }

    /// Register this session's peer-wire contact card (the QUIC port it
    /// serves swarm fetches on + its cert fingerprint). The server pairs the
    /// port with this connection's observed IP.
    pub async fn swarm_contact(&mut self, port: u16, cert_fp: [u8; 32]) -> Result<(), ClientError> {
        self.request_ack(&rabbithole_proto::swarm::PeerContact::new(port, cert_fp))
            .await
    }

    /// Ask the origin for a capability token authorizing this session to
    /// fetch `root` from peers (verify/decode via `rabbithole-swarm`).
    pub async fn swarm_ticket(
        &mut self,
        root: [u8; 32],
    ) -> Result<rabbithole_proto::swarm::SourceTicket, ClientError> {
        self.request(&rabbithole_proto::swarm::SourceTicketRequest::new(root))
            .await
    }

    /// Cap transfer bandwidth to `bytes_per_sec` (`None`/0 = unlimited). The
    /// queue driver uses this to throttle background transfers; it applies to
    /// the ranged-chunk and dedicated-stream paths alike.
    pub fn set_rate_limit(&mut self, bytes_per_sec: Option<u64>) {
        self.rate_limit = bytes_per_sec.filter(|&r| r > 0);
    }

    /// Chunk size for ranged transfers (kept under the 1 MiB control cap).
    const TRANSFER_CHUNK: usize = 256 * 1024;

    /// blake3-hash a local file incrementally, returning `(root, size)` —
    /// the same root transfers verify against and swarm adverts carry.
    pub fn hash_file(path: &std::path::Path) -> Result<([u8; 32], u64), ClientError> {
        use std::io::Read;
        let mut f = std::fs::File::open(path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        Ok((*hasher.finalize().as_bytes(), total))
    }

    /// Download a file node to `dest`, resuming from any partial already
    /// present. Verifies the finished file's blake3 root against the ticket
    /// before returning the byte count. Works over any transport (the ranged
    /// chunk path); dedicated QUIC bulk streams are a transparent optimization
    /// added on top of the same tickets.
    pub async fn transfer_download(
        &mut self,
        node_id: i64,
        dest: &std::path::Path,
    ) -> Result<u64, ClientError> {
        use std::io::{Seek, SeekFrom, Write};
        let ticket: rabbithole_proto::transfer::TransferTicket = self
            .request(&rabbithole_proto::transfer::TransferOpen::download(node_id))
            .await?;
        let rate = self.rate_limit;
        // Resume from the existing partial (clamped to the real size).
        let mut have = std::fs::metadata(dest)
            .map(|m| m.len())
            .unwrap_or(0)
            .min(ticket.size);
        // Resuming: keep the existing partial (never truncate); we seek past it.
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dest)?;
        file.seek(SeekFrom::Start(have))?;
        if let Some(bulk) = self.conn.bulk() {
            // Dedicated QUIC stream: bytes flow off the control channel.
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut send, mut recv) = bulk.open().await?;
            let pre = postcard::to_allocvec(&rabbithole_proto::transfer::BulkPreamble::new(
                ticket.transfer_id,
                ticket.token,
                have,
                rabbithole_proto::transfer::DIR_DOWNLOAD,
            ))
            .expect("preamble serializes");
            rabbithole_net::write_framed(&mut send, &pre).await?;
            send.shutdown().await?; // no more client→server on this stream
            let mut buf = vec![0u8; Self::TRANSFER_CHUNK];
            loop {
                let n = recv.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
                have += n as u64;
                throttle(rate, n).await;
            }
        } else {
            // WebSocket / no-multiplex: ranged chunks on the control stream.
            while have < ticket.size {
                let want = ((ticket.size - have).min(Self::TRANSFER_CHUNK as u64)) as u32;
                let chunk: rabbithole_proto::transfer::FileChunk = self
                    .request(&rabbithole_proto::transfer::FileChunkRequest::new(
                        ticket.transfer_id,
                        have,
                        want,
                    ))
                    .await?;
                if chunk.bytes.is_empty() {
                    break;
                }
                let n = chunk.bytes.len();
                file.write_all(&chunk.bytes)?;
                have += n as u64;
                throttle(rate, n).await;
                if chunk.last {
                    break;
                }
            }
        }
        file.flush()?;
        drop(file);
        let (root, _) = Self::hash_file(dest)?;
        if root != ticket.root {
            return Err(ClientError::IntegrityError);
        }
        Ok(have)
    }

    /// Upload a local file into `area` (optionally under `parent`), resuming
    /// from the server's staged prefix. Returns the created node.
    pub async fn transfer_upload(
        &mut self,
        area: &str,
        parent: Option<String>,
        name: &str,
        src: &std::path::Path,
        mime: &str,
        comment: &str,
    ) -> Result<rabbithole_proto::filelib::FileNodeView, ClientError> {
        use std::io::{Read, Seek, SeekFrom};
        let (root, size) = Self::hash_file(src)?;
        let open = rabbithole_proto::transfer::TransferOpen::upload(area, parent, name, size, root)
            .with_meta(mime, comment);
        let ticket: rabbithole_proto::transfer::TransferTicket = self.request(&open).await?;
        let rate = self.rate_limit;

        let mut have = ticket.server_have.min(size);
        let mut file = std::fs::File::open(src)?;
        file.seek(SeekFrom::Start(have))?;
        let mut buf = vec![0u8; Self::TRANSFER_CHUNK];
        if let Some(bulk) = self.conn.bulk() {
            // Dedicated QUIC stream: stream the remainder, then wait for the
            // server's one-byte ack so staging is durable before UploadFinish.
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut send, mut recv) = bulk.open().await?;
            let pre = postcard::to_allocvec(&rabbithole_proto::transfer::BulkPreamble::new(
                ticket.transfer_id,
                ticket.token,
                have,
                rabbithole_proto::transfer::DIR_UPLOAD,
            ))
            .expect("preamble serializes");
            rabbithole_net::write_framed(&mut send, &pre).await?;
            while have < size {
                let want = ((size - have).min(Self::TRANSFER_CHUNK as u64)) as usize;
                let mut filled = 0;
                while filled < want {
                    let n = file.read(&mut buf[filled..want])?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                if filled == 0 {
                    break;
                }
                send.write_all(&buf[..filled]).await?;
                have += filled as u64;
                throttle(rate, filled).await;
            }
            send.shutdown().await?; // FIN → server reads to EOF
            let mut ack = [0u8; 1];
            recv.read_exact(&mut ack).await?; // staging is now durable
        } else {
            // WebSocket / no-multiplex: ranged chunks on the control stream.
            while have < size {
                let want = ((size - have).min(Self::TRANSFER_CHUNK as u64)) as usize;
                // Read up to `want` bytes (files can short-read).
                let mut filled = 0;
                while filled < want {
                    let n = file.read(&mut buf[filled..want])?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                if filled == 0 {
                    break;
                }
                let last = have + filled as u64 >= size;
                self.request_ack(&rabbithole_proto::transfer::FileChunkPut::new(
                    ticket.transfer_id,
                    have,
                    last,
                    buf[..filled].to_vec(),
                ))
                .await?;
                have += filled as u64;
                throttle(rate, filled).await;
            }
        }
        let reply: rabbithole_proto::filelib::NodeReply = self
            .request(&rabbithole_proto::transfer::UploadFinish::new(
                ticket.transfer_id,
            ))
            .await?;
        Ok(reply.node)
    }

    /// Download a whole folder subtree into `dest_dir`, preserving structure.
    /// One manifest round trip, then each file transfers (and resumes)
    /// independently. Returns the number of files fetched.
    pub async fn folder_download(
        &mut self,
        area: &str,
        path: Option<String>,
        dest_dir: &std::path::Path,
    ) -> Result<usize, ClientError> {
        let manifest: rabbithole_proto::transfer::FolderManifest = self
            .request(&rabbithole_proto::transfer::FolderManifestRequest::new(
                area, path,
            ))
            .await?;
        for entry in &manifest.entries {
            // Build the destination path component-wise (rel_path uses '/').
            let mut dest = dest_dir.to_path_buf();
            for comp in entry.rel_path.split('/').filter(|c| !c.is_empty()) {
                dest.push(comp);
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.transfer_download(entry.node_id, &dest).await?;
        }
        Ok(manifest.entries.len())
    }

    pub async fn close(&mut self) {
        self.conn.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_trigger_reconnect_others_do_not() {
        // Network drops are recoverable by re-dial + resume.
        assert!(is_transient(&ClientError::Closed));
        assert!(is_transient(&ClientError::Net(NetError::Unsupported("ws"))));
        assert!(is_transient(&ClientError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        ))));
        // Server-level outcomes are not: retrying would just repeat them.
        assert!(!is_transient(&ClientError::Refused(ErrorCode::RateLimited)));
        assert!(!is_transient(&ClientError::UnexpectedReply {
            family: 2,
            message_type: 3,
        }));
        assert!(!is_transient(&ClientError::IntegrityError));
    }
}
