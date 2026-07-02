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

    pub async fn close(&mut self) {
        self.conn.close().await;
    }
}
