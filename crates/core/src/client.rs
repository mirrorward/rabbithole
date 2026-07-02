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
}

/// A connected, hello-negotiated session.
pub struct Client {
    conn: Box<dyn Connection>,
    next_id: u64,
    pushes: VecDeque<Frame>,
    /// Highest push sequence seen (the resume replay cursor).
    pub replay_cursor: u64,
    pub server: HelloAck,
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
            server: HelloAck::new(
                rabbithole_proto::PROTOCOL_VERSION,
                CapabilitySet::default(),
                "",
                "",
                [0; 32],
            ),
        };
        let hello = Hello::new(client_name, client_version, CapabilitySet::default());
        let ack: HelloAck = client.request(&hello).await?;
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
        self.request(&psess::AuthPassword::new(login, password))
            .await
    }

    pub async fn auth_guest(
        &mut self,
        desired_name: Option<String>,
    ) -> Result<psess::AuthOk, ClientError> {
        self.request(&psess::AuthGuest::new(desired_name)).await
    }

    pub async fn auth_resume(
        &mut self,
        token: &str,
        replay_cursor: u64,
    ) -> Result<psess::AuthOk, ClientError> {
        self.request(&psess::AuthResume::new(token, replay_cursor))
            .await
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

    pub async fn close(&mut self) {
        self.conn.close().await;
    }
}
