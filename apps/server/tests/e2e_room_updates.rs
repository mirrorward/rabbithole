//! RH-12: room invalidations on live sockets, followed by authorized snapshots.
//! A bus sentinel fences each mutation, so absence assertions never use sleeps.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::chat::{RoomCreate, RoomModeration, RoomModerationRequest, RoomsChanged};
use rabbithole_proto::presence::PresenceState;
use rabbithole_proto::session::ServerNotice;
use rabbithole_proto::{ErrorCode, Frame, RequestId};
use rabbithole_server_core::{Role, ServerConfig, ServerEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PASS: &str = "room-update-password";

async fn server(path: &std::path::Path) -> Burrow {
    let b = Burrow::start(ServerConfig {
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        hotline_enabled: true,
        hotline_addr: "127.0.0.1:0".parse().unwrap(),
        keywords: [("den".into(), "room:den".into())].into(),
        ratelimit_enabled: false,
        ..Default::default()
    })
    .await
    .unwrap();
    for (name, role) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("mo", Role::Moderator),
    ] {
        b.shared
            .auth
            .create_account(name, PASS, role)
            .await
            .unwrap();
    }
    b
}

async fn login(b: &Burrow, name: &str) -> Client {
    let mut c = Client::connect(&format!("ws://{}", b.ws_addr), None, None, "rooms", "0")
        .await
        .unwrap();
    c.auth_password(name, PASS).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn keeping(c: &mut Client, room: &str) -> RoomModeration {
    c.request(&RoomModerationRequest::new(room)).await.unwrap()
}

async fn count(c: &mut Client, name: &str) -> Option<u32> {
    c.room_list()
        .await
        .unwrap()
        .iter()
        .find(|room| room.name == name)
        .map(|room| room.member_count)
}

async fn fence(b: &Burrow, c: &mut Client, marker: &str) -> Vec<Frame> {
    b.shared.bus.publish(ServerEvent::Notice {
        text: marker.into(),
        from: "room-test".into(),
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut updates = Vec::new();
        loop {
            let frame = c.next_push().await.unwrap().expect("live socket");
            if matches!(frame.decode::<ServerNotice>(), Some(Ok(notice)) if notice.text == marker) {
                return updates;
            }
            if matches!(frame.decode::<RoomsChanged>(), Some(Ok(_))) {
                assert!(
                    frame.payload.0.is_empty(),
                    "no room names or roster in a push"
                );
                assert_eq!(frame.id, RequestId::PUSH, "not stamped for offline replay");
                updates.push(frame);
            }
        }
    })
    .await
    .expect("bus sentinel arrives")
}

async fn gone(b: &Burrow, session: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while b.shared.presence.get(session).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session cleanup bounded");
}

#[tokio::test]
async fn public_lifecycle_refreshes_other_sessions_and_keeper_rosters() {
    let dir = tempfile::tempdir().unwrap();
    let b = server(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut bob = login(&b, "bob").await;
    fence(&b, &mut alice, "initial alice").await;
    fence(&b, &mut bob, "initial bob").await;

    alice
        .room_create(&RoomCreate::new("den", false))
        .await
        .unwrap();
    assert_eq!(fence(&b, &mut bob, "created").await.len(), 1);
    assert_eq!(count(&mut bob, "den").await, Some(1));
    fence(&b, &mut alice, "drain create").await;
    bob.room_join("den").await.unwrap();
    assert_eq!(fence(&b, &mut alice, "joined").await.len(), 1);
    assert_eq!(count(&mut alice, "den").await, Some(2));
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice", "bob"]);
    let ordinary = keeping(&mut bob, "den").await;
    assert!(!ordinary.may_moderate);
    assert!(
        ordinary.members.is_empty(),
        "ordinary members receive no keeper roster"
    );
    bob.room_join("DEN").await.unwrap();
    assert!(fence(&b, &mut alice, "noop join").await.is_empty());
    bob.room_leave("den").await.unwrap();
    assert_eq!(fence(&b, &mut alice, "left").await.len(), 1);
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice"]);
    bob.room_leave("den").await.unwrap();
    assert!(fence(&b, &mut alice, "noop leave").await.is_empty());

    bob.room_join("den").await.unwrap();
    alice.room_leave("den").await.unwrap();
    fence(&b, &mut alice, "before disconnect").await;
    let session = b
        .shared
        .presence
        .is_screen_name_online("bob")
        .unwrap()
        .session_id;
    bob.close().await;
    gone(&b, session).await;
    assert_eq!(
        fence(&b, &mut alice, "disconnected and reaped").await.len(),
        1
    );
    assert_eq!(count(&mut alice, "den").await, None);
    assert_eq!(count(&mut alice, "lobby").await, Some(1));
    assert!(bob.reconnect().await.unwrap().resumed);
    bob.expect_welcome().await.unwrap();
    assert_eq!(
        count(&mut bob, "den").await,
        None,
        "reconnect reads current state"
    );
    assert_eq!(count(&mut bob, "lobby").await, Some(2));
    b.shutdown().await;
}

#[tokio::test]
async fn private_changes_never_notify_outsiders_and_removed_invitees_resync() {
    let dir = tempfile::tempdir().unwrap();
    let b = server(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut bob = login(&b, "bob").await;
    let mut outsider = login(&b, "mo").await;
    fence(&b, &mut bob, "initial bob").await;
    fence(&b, &mut outsider, "initial outsider").await;
    alice
        .room_create(&RoomCreate::new("secret", true))
        .await
        .unwrap();
    assert!(fence(&b, &mut bob, "private creation hidden")
        .await
        .is_empty());
    assert!(
        fence(&b, &mut outsider, "private creation hidden from moderator")
            .await
            .is_empty()
    );
    assert_eq!(count(&mut bob, "secret").await, None);
    assert!(matches!(
        outsider
            .request::<_, RoomModeration>(&RoomModerationRequest::new("secret"))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    alice.room_invite("secret", "bob").await.unwrap();
    assert_eq!(fence(&b, &mut bob, "invited").await.len(), 1);
    assert_eq!(count(&mut bob, "secret").await, Some(1));
    bob.room_join("secret").await.unwrap();
    assert!(fence(&b, &mut outsider, "private membership hidden")
        .await
        .is_empty());
    fence(&b, &mut bob, "drain private join").await;
    alice.room_kick("secret", "bob", true).await.unwrap();
    assert_eq!(fence(&b, &mut bob, "banned viewer resyncs").await.len(), 1);
    assert_eq!(count(&mut bob, "secret").await, None);
    alice.room_leave("secret").await.unwrap();
    assert!(fence(&b, &mut outsider, "private reaping hidden")
        .await
        .is_empty());
    assert!(fence(&b, &mut bob, "revoked invite excluded")
        .await
        .is_empty());
    assert_eq!(count(&mut alice, "secret").await, None);
    b.shutdown().await;
}

#[tokio::test]
async fn an_older_hello_keeps_ordinary_reads_without_receiving_room_invalidations() {
    use rabbithole_proto::{CapabilitySet, Hello, HelloAck};
    let dir = tempfile::tempdir().unwrap();
    let b = server(dir.path()).await;
    let mut old = Client::connect(&format!("ws://{}", b.ws_addr), None, None, "old", "0")
        .await
        .unwrap();
    // Re-Hello before authentication replaces the core client's offer with
    // the exact empty capability set of an older implementation.
    let ack: HelloAck = old
        .request(&Hello::new("old", "0", CapabilitySet::default()))
        .await
        .unwrap();
    assert!(ack
        .capabilities
        .contains(rabbithole_proto::hello::caps::ROOM_UPDATES));
    old.auth_password("bob", PASS).await.unwrap();
    old.expect_welcome().await.unwrap();
    let mut alice = login(&b, "alice").await;
    alice
        .room_create(&RoomCreate::new("den", false))
        .await
        .unwrap();
    alice
        .presence_set(PresenceState::Invisible, None)
        .await
        .unwrap();
    assert!(fence(&b, &mut old, "older client reads only")
        .await
        .is_empty());
    assert_eq!(count(&mut old, "den").await, Some(0));
    old.room_join("den").await.unwrap();
    assert_eq!(count(&mut old, "den").await, Some(1));
    assert!(fence(&b, &mut old, "older join gets no invalidation")
        .await
        .is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn invisible_counts_and_live_persona_labels_match_each_viewers_roster() {
    let dir = tempfile::tempdir().unwrap();
    let b = server(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut bob = login(&b, "bob").await;
    let mut moderator = login(&b, "mo").await;
    alice
        .room_create(&RoomCreate::new("den", false))
        .await
        .unwrap();
    bob.presence_set(PresenceState::Invisible, None)
        .await
        .unwrap();
    bob.room_join("den").await.unwrap();
    fence(&b, &mut alice, "invisible joined").await;
    assert_eq!(count(&mut alice, "lobby").await, Some(2));
    assert_eq!(count(&mut alice, "den").await, Some(1));
    assert_eq!(
        alice.room_join("den").await.unwrap().member_count,
        1,
        "join reply also filtered"
    );
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice"]);
    assert_eq!(count(&mut bob, "den").await, Some(2), "one sees oneself");
    assert_eq!(
        keeping(&mut moderator, "den").await.members,
        ["alice", "bob"]
    );
    bob.presence_set(PresenceState::Online, None).await.unwrap();
    assert!(!fence(&b, &mut alice, "became visible").await.is_empty());
    assert_eq!(count(&mut alice, "den").await, Some(2));
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice", "bob"]);
    let persona = bob.persona_create("robert").await.unwrap().persona;
    bob.persona_switch(persona.id).await.unwrap();
    assert!(!fence(&b, &mut alice, "persona changed").await.is_empty());
    assert_eq!(
        keeping(&mut alice, "den").await.members,
        ["alice", "robert"]
    );
    bob.presence_set(PresenceState::Invisible, None)
        .await
        .unwrap();
    fence(&b, &mut alice, "hidden again").await;
    let session = b
        .shared
        .presence
        .is_screen_name_online("robert")
        .unwrap()
        .session_id;
    bob.close().await;
    gone(&b, session).await;
    assert!(!fence(&b, &mut alice, "invisible disconnected")
        .await
        .is_empty());
    assert_eq!(count(&mut alice, "den").await, Some(1));
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice"]);
    b.shutdown().await;
}

struct Telnet {
    socket: TcpStream,
    bytes: Vec<u8>,
    pos: usize,
}

impl Telnet {
    async fn expect(&mut self, needle: &[u8]) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(at) = self.bytes[self.pos..]
                    .windows(needle.len())
                    .position(|part| part == needle)
                {
                    self.pos += at + needle.len();
                    return;
                }
                let mut buf = [0; 4096];
                let read = self.socket.read(&mut buf).await.unwrap();
                assert_ne!(read, 0, "telnet stayed connected");
                self.bytes.extend_from_slice(&buf[..read]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "missing telnet marker {:?}",
                String::from_utf8_lossy(needle)
            )
        });
    }
    async fn send(&mut self, line: &str) {
        self.socket
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn real_telnet_membership_and_disconnect_refresh_native_keeper() {
    let dir = tempfile::tempdir().unwrap();
    let b = server(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    alice
        .room_create(&RoomCreate::new("den", false))
        .await
        .unwrap();
    fence(&b, &mut alice, "before telnet").await;
    let mut telnet = Telnet {
        socket: TcpStream::connect(b.telnet_addr.unwrap()).await.unwrap(),
        bytes: Vec::new(),
        pos: 0,
    };
    telnet.expect(b"login: ").await;
    telnet.send("bob").await;
    telnet.expect(b"password: ").await;
    telnet.send(PASS).await;
    telnet.expect(b"Command: ").await;
    assert!(!fence(&b, &mut alice, "telnet lobby").await.is_empty());
    assert_eq!(count(&mut alice, "lobby").await, Some(2));
    telnet.send("/go den").await;
    telnet.expect(b"--- Chat: den ---").await;
    telnet.expect(b"(no recent chat)\r\n").await;
    assert_eq!(fence(&b, &mut alice, "telnet joined").await.len(), 1);
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice", "bob"]);
    telnet.send("/q").await;
    telnet.expect(b"Command: ").await;
    assert_eq!(fence(&b, &mut alice, "telnet left").await.len(), 1);
    assert_eq!(keeping(&mut alice, "den").await.members, ["alice"]);
    let session = b
        .shared
        .presence
        .is_screen_name_online("bob")
        .unwrap()
        .session_id;
    telnet.socket.shutdown().await.unwrap();
    gone(&b, session).await;
    assert_eq!(fence(&b, &mut alice, "telnet disconnected").await.len(), 1);
    assert_eq!(count(&mut alice, "lobby").await, Some(1));
    b.shutdown().await;
}

struct Hotline {
    socket: TcpStream,
    id: u32,
}

impl Hotline {
    async fn send(&mut self, kind: u16, fields: Vec<rabbithole_legacy_hotline::Field>) {
        self.id += 1;
        self.socket
            .write_all(
                &rabbithole_legacy_hotline::Transaction::request(kind, self.id, fields).encode(),
            )
            .await
            .unwrap();
    }

    async fn receive(&mut self, kind: u16) -> rabbithole_legacy_hotline::Transaction {
        use rabbithole_legacy_hotline::{Transaction, TransactionHeader};
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut header = [0; TransactionHeader::LEN];
                self.socket.read_exact(&mut header).await.unwrap();
                let decoded = TransactionHeader::decode(&header).unwrap();
                let mut wire = header.to_vec();
                wire.resize(TransactionHeader::LEN + decoded.data_size as usize, 0);
                self.socket
                    .read_exact(&mut wire[TransactionHeader::LEN..])
                    .await
                    .unwrap();
                let transaction = Transaction::decode(&wire).unwrap();
                if transaction.header.type_ == kind {
                    return transaction;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Hotline transaction {kind} bounded"))
    }
}

#[tokio::test]
async fn real_hotline_private_membership_updates_native_keeper_without_outsider_leak() {
    use rabbithole_legacy_hotline::constants::{field, transaction};
    use rabbithole_legacy_hotline::{Field, Handshake, HandshakeReply};
    let dir = tempfile::tempdir().unwrap();
    let b = server(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut outsider = login(&b, "mo").await;
    alice
        .room_create(&RoomCreate::new("secret", true))
        .await
        .unwrap();
    let mut legacy = Hotline {
        socket: TcpStream::connect(b.hotline_addr.unwrap()).await.unwrap(),
        id: 0,
    };
    legacy
        .socket
        .write_all(&Handshake::hotl().encode())
        .await
        .unwrap();
    let mut ack = [0; HandshakeReply::LEN];
    legacy.socket.read_exact(&mut ack).await.unwrap();
    assert!(HandshakeReply::decode(&ack).unwrap().is_ok());
    legacy
        .send(
            transaction::LOGIN,
            vec![
                Field::new(
                    field::LOGIN,
                    "bob".bytes().map(|byte| !byte).collect::<Vec<_>>(),
                ),
                Field::new(
                    field::PASSWORD,
                    PASS.bytes().map(|byte| !byte).collect::<Vec<_>>(),
                ),
                Field::text(field::USER_NAME, "bob"),
            ],
        )
        .await;
    assert_eq!(legacy.receive(transaction::LOGIN).await.header.error, 0);
    // A round-trip crosses Hotline's registration/subscription boundary.
    legacy
        .send(transaction::GET_USER_NAME_LIST, Vec::new())
        .await;
    legacy.receive(transaction::GET_USER_NAME_LIST).await;
    fence(&b, &mut alice, "before legacy invitation").await;
    fence(&b, &mut outsider, "before legacy invitation outsider").await;
    alice.room_invite("secret", "bob").await.unwrap();
    let invited = legacy.receive(transaction::INVITE_TO_CHAT).await;
    let chat_id = invited
        .fields
        .iter()
        .find(|field_| field_.id == field::CHAT_ID)
        .and_then(|field_| rabbithole_legacy_hotline::read_int(&field_.data).ok())
        .unwrap();
    fence(&b, &mut alice, "drain invitation").await;
    legacy
        .send(
            transaction::JOIN_CHAT,
            vec![Field::int(field::CHAT_ID, chat_id)],
        )
        .await;
    assert_eq!(legacy.receive(transaction::JOIN_CHAT).await.header.error, 0);
    assert_eq!(fence(&b, &mut alice, "legacy joined").await.len(), 1);
    assert_eq!(
        keeping(&mut alice, "secret").await.members,
        ["alice", "bob"]
    );
    assert!(fence(&b, &mut outsider, "legacy private activity hidden")
        .await
        .is_empty());
    legacy
        .send(
            transaction::LEAVE_CHAT,
            vec![Field::int(field::CHAT_ID, chat_id)],
        )
        .await;
    // LeaveChat is a one-way notify. Fence it with a handled request.
    legacy
        .send(transaction::GET_USER_NAME_LIST, Vec::new())
        .await;
    legacy.receive(transaction::GET_USER_NAME_LIST).await;
    assert_eq!(fence(&b, &mut alice, "legacy left").await.len(), 1);
    assert_eq!(keeping(&mut alice, "secret").await.members, ["alice"]);
    assert!(fence(&b, &mut outsider, "legacy private leave hidden")
        .await
        .is_empty());
    let session = b
        .shared
        .presence
        .is_screen_name_online("bob")
        .unwrap()
        .session_id;
    legacy.socket.shutdown().await.unwrap();
    gone(&b, session).await;
    assert_eq!(fence(&b, &mut alice, "legacy disconnected").await.len(), 1);
    assert_eq!(count(&mut alice, "lobby").await, Some(2));
    b.shutdown().await;
}
