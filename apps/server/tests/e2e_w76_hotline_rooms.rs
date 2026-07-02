//! Hotline private-chat + IM end-to-end tests (Wave 7.6): the classic
//! private-chat transactions (InviteNewChat/InviteToChat 112-113,
//! RejectChatInvite 114, JoinChat 115, LeaveChat 116, the 117-119 pushes, and
//! SetChatSubject 120) riding the shared rooms service, plus instant-message
//! quoting relay and the away auto-response. Mirrors the scripted-client
//! structure of `e2e_w75_hotline_admin.rs`.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{
    read_int, Field, Handshake, HandshakeReply, Transaction, TransactionHeader,
};
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Hotline Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        hotline_enabled: true,
        hotline_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// A scripted Hotline client over a raw TCP socket.
struct Client {
    stream: TcpStream,
    next_id: u32,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(&Handshake::hotl().encode()).await.unwrap();
        let mut reply = [0u8; HandshakeReply::LEN];
        stream.read_exact(&mut reply).await.unwrap();
        assert!(HandshakeReply::decode(&reply).unwrap().is_ok());
        Client { stream, next_id: 1 }
    }

    fn take_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn send(&mut self, type_: u16, fields: Vec<Field>) -> u32 {
        let id = self.take_id();
        let txn = Transaction::request(type_, id, fields);
        self.stream.write_all(&txn.encode()).await.unwrap();
        id
    }

    async fn read_txn(&mut self) -> Transaction {
        let mut hdr = [0u8; TransactionHeader::LEN];
        self.stream.read_exact(&mut hdr).await.unwrap();
        let header = TransactionHeader::decode(&hdr).unwrap();
        let mut buf = hdr.to_vec();
        buf.resize(TransactionHeader::LEN + header.data_size as usize, 0);
        self.stream
            .read_exact(&mut buf[TransactionHeader::LEN..])
            .await
            .unwrap();
        Transaction::decode(&buf).unwrap()
    }

    async fn read_until(&mut self, type_: u16) -> Transaction {
        loop {
            let txn = tokio::time::timeout(Duration::from_secs(5), self.read_txn())
                .await
                .expect("timed out waiting for transaction");
            if txn.header.type_ == type_ {
                return txn;
            }
        }
    }

    /// Send + wait for the matching reply type.
    async fn roundtrip(&mut self, type_: u16, fields: Vec<Field>) -> Transaction {
        let id = self.send(type_, fields).await;
        let reply = self.read_until(type_).await;
        assert_eq!(reply.header.id, id, "reply echoes the request id");
        reply
    }

    async fn login(&mut self, user: &str, pass: &str, name: &str) -> Transaction {
        let fields = vec![
            Field::new(field::LOGIN, obfuscate(user)),
            Field::new(field::PASSWORD, obfuscate(pass)),
            Field::text(field::USER_NAME, name),
            Field::int(field::USER_ICON_ID, 200),
        ];
        self.roundtrip(transaction::LOGIN, fields).await
    }

    /// Graceful goodbye: FIN the write half instead of a bare drop.
    async fn close(mut self) {
        let _ = self.stream.shutdown().await;
    }
}

fn obfuscate(s: &str) -> Vec<u8> {
    s.bytes().map(|b| !b).collect()
}

fn field_bytes(txn: &Transaction, id: u16) -> Option<&[u8]> {
    txn.fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.data.as_slice())
}

fn field_text(txn: &Transaction, id: u16) -> Option<String> {
    field_bytes(txn, id).map(|b| String::from_utf8_lossy(b).into_owned())
}

fn field_int(txn: &Transaction, id: u16) -> Option<u32> {
    field_bytes(txn, id).and_then(|b| read_int(b).ok())
}

/// Parse the classic user list (`id(2) icon(2) flags(2) len(2) name`) into
/// `(uid, name)` pairs.
fn parse_users(txn: &Transaction) -> Vec<(u16, String)> {
    txn.fields
        .iter()
        .filter(|f| f.id == 300)
        .filter_map(|f| {
            let d = &f.data;
            if d.len() < 8 {
                return None;
            }
            let uid = u16::from_be_bytes([d[0], d[1]]);
            let len = u16::from_be_bytes([d[6], d[7]]) as usize;
            let name = String::from_utf8_lossy(&d[8..(8 + len).min(d.len())]).into_owned();
            Some((uid, name))
        })
        .collect()
}

/// A logged-in client's wire user id, from its own user-name list. Retries
/// briefly: a peer's login reply lands just before it joins shared presence,
/// so the freshest arrival can be one beat away from the roster.
async fn uid_of(client: &mut Client, name: &str) -> u32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let list = client
            .roundtrip(transaction::GET_USER_NAME_LIST, vec![])
            .await;
        if let Some((uid, _)) = parse_users(&list).into_iter().find(|(_, n)| n == name) {
            return u32::from(uid);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{name} never appeared in the user list"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait (bounded) until `pred` holds over the shared presence snapshot.
async fn wait_presence(
    burrow: &Burrow,
    what: &str,
    pred: impl Fn(&[rabbithole_server_core::PresenceEntry]) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if pred(&burrow.shared.presence.snapshot()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for presence: {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn hotline_private_chat_invite_join_chat_subject_and_leave() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for (login, pass) in [
        ("alice", "curiouser-and-curiouser"),
        ("bob", "swordfish-swordfish"),
        ("carol", "jam-tomorrow-jam-today"),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, pass, Role::User)
            .await
            .unwrap();
    }

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut alice = Client::connect(addr).await;
    assert_eq!(
        alice
            .login("alice", "curiouser-and-curiouser", "Alice")
            .await
            .header
            .error,
        0
    );
    let mut bob = Client::connect(addr).await;
    assert_eq!(
        bob.login("bob", "swordfish-swordfish", "Bob")
            .await
            .header
            .error,
        0
    );
    // Carol is a lobby bystander: she must never see the private room's chat.
    let mut carol = Client::connect(addr).await;
    assert_eq!(
        carol
            .login("carol", "jam-tomorrow-jam-today", "Carol")
            .await
            .header
            .error,
        0
    );

    // Carol must be fully in the shared world (bus-subscribed) before any
    // chat flows, or her bystander reads below would race her own join.
    wait_presence(&burrow, "carol online", |all| {
        all.iter().any(|e| e.screen_name == "Carol")
    })
    .await;
    let bob_uid = uid_of(&mut alice, "Bob").await;
    let alice_uid = uid_of(&mut bob, "Alice").await;

    // InviteNewChat: alice opens a private chat with bob. The reply names the
    // new chat id and echoes the inviter's identity.
    let created = alice
        .roundtrip(
            transaction::INVITE_NEW_CHAT,
            vec![Field::int(field::USER_ID, bob_uid)],
        )
        .await;
    assert_eq!(created.header.error, 0, "InviteNewChat accepted");
    let chat_id = field_int(&created, field::CHAT_ID).expect("chat id in the reply");
    assert_eq!(field_int(&created, field::USER_ID), Some(alice_uid));

    // Bob receives the classic InviteToChat push naming chat and inviter.
    let invite = bob.read_until(transaction::INVITE_TO_CHAT).await;
    assert_eq!(field_int(&invite, field::CHAT_ID), Some(chat_id));
    assert_eq!(
        field_text(&invite, field::USER_NAME).as_deref(),
        Some("Alice")
    );
    assert_eq!(field_int(&invite, field::USER_ID), Some(alice_uid));

    // JoinChat: the reply carries the subject and both members; alice gets
    // the 117 join push.
    let joined = bob
        .roundtrip(
            transaction::JOIN_CHAT,
            vec![Field::int(field::CHAT_ID, chat_id)],
        )
        .await;
    assert_eq!(joined.header.error, 0, "invited user may join");
    assert!(
        field_text(&joined, field::CHAT_SUBJECT).is_some(),
        "join reply carries the subject"
    );
    let mut names: Vec<String> = parse_users(&joined).into_iter().map(|(_, n)| n).collect();
    names.sort();
    assert_eq!(names, vec!["Alice", "Bob"], "join reply lists both members");

    let join_push = alice.read_until(transaction::NOTIFY_CHAT_CHANGE_USER).await;
    assert_eq!(field_int(&join_push, field::CHAT_ID), Some(chat_id));
    assert_eq!(field_int(&join_push, field::USER_ID), Some(bob_uid));
    assert_eq!(
        field_text(&join_push, field::USER_NAME).as_deref(),
        Some("Bob")
    );

    // Chat inside the room: SEND_CHAT with the CHAT_ID field. Both members
    // see it tagged with the chat id; the lobby does not.
    alice
        .send(
            transaction::CHAT_SEND,
            vec![
                Field::int(field::CHAT_ID, chat_id),
                Field::new(field::CHAT_TEXT, b"no room! no room!".to_vec()),
            ],
        )
        .await;
    for member in [&mut alice, &mut bob] {
        let line = member.read_until(transaction::CHAT_MSG).await;
        assert_eq!(
            field_int(&line, field::CHAT_ID),
            Some(chat_id),
            "room chat is tagged with the chat id"
        );
        assert!(
            field_text(&line, field::CHAT_TEXT)
                .unwrap_or_default()
                .contains("no room! no room!"),
            "both members see the room line"
        );
    }
    // Now a lobby line: the FIRST chat message carol sees must be this lobby
    // line (bus order is delivery order, so the private line — sent earlier —
    // would have arrived first had it leaked).
    alice
        .send(
            transaction::CHAT_SEND,
            vec![Field::new(field::CHAT_TEXT, b"hello lobby".to_vec())],
        )
        .await;
    let public = carol.read_until(transaction::CHAT_MSG).await;
    assert!(
        field_int(&public, field::CHAT_ID).is_none(),
        "lobby chat carries no chat id"
    );
    assert!(
        field_text(&public, field::CHAT_TEXT)
            .unwrap_or_default()
            .contains("hello lobby"),
        "the private-room line never reached the lobby bystander"
    );

    // SetChatSubject (by the creator) pushes the 119 subject notify.
    alice
        .send(
            transaction::SET_CHAT_SUBJECT,
            vec![
                Field::int(field::CHAT_ID, chat_id),
                Field::text(field::CHAT_SUBJECT, "a very merry unbirthday"),
            ],
        )
        .await;
    let subject = bob.read_until(transaction::NOTIFY_CHAT_SUBJECT).await;
    assert_eq!(field_int(&subject, field::CHAT_ID), Some(chat_id));
    assert_eq!(
        field_text(&subject, field::CHAT_SUBJECT).as_deref(),
        Some("a very merry unbirthday")
    );

    // LeaveChat: the remaining member gets the 118 leave push.
    bob.send(
        transaction::LEAVE_CHAT,
        vec![Field::int(field::CHAT_ID, chat_id)],
    )
    .await;
    let left = alice.read_until(transaction::NOTIFY_CHAT_DELETE_USER).await;
    assert_eq!(field_int(&left, field::CHAT_ID), Some(chat_id));
    assert_eq!(field_int(&left, field::USER_ID), Some(bob_uid));

    alice.close().await;
    bob.close().await;
    carol.close().await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_rejected_invite_notifies_the_chat_and_bars_outsiders() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for (login, pass) in [
        ("alice", "curiouser-and-curiouser"),
        ("bob", "swordfish-swordfish"),
        ("carol", "jam-tomorrow-jam-today"),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, pass, Role::User)
            .await
            .unwrap();
    }

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut alice = Client::connect(addr).await;
    assert_eq!(
        alice
            .login("alice", "curiouser-and-curiouser", "Alice")
            .await
            .header
            .error,
        0
    );
    let mut bob = Client::connect(addr).await;
    assert_eq!(
        bob.login("bob", "swordfish-swordfish", "Bob")
            .await
            .header
            .error,
        0
    );
    let mut carol = Client::connect(addr).await;
    assert_eq!(
        carol
            .login("carol", "jam-tomorrow-jam-today", "Carol")
            .await
            .header
            .error,
        0
    );

    let bob_uid = uid_of(&mut alice, "Bob").await;

    let created = alice
        .roundtrip(
            transaction::INVITE_NEW_CHAT,
            vec![Field::int(field::USER_ID, bob_uid)],
        )
        .await;
    assert_eq!(created.header.error, 0);
    let chat_id = field_int(&created, field::CHAT_ID).expect("chat id");

    // Bob declines: the chat's members see the classic decline notice.
    let invite = bob.read_until(transaction::INVITE_TO_CHAT).await;
    assert_eq!(field_int(&invite, field::CHAT_ID), Some(chat_id));
    bob.send(
        transaction::REJECT_CHAT_INVITE,
        vec![Field::int(field::CHAT_ID, chat_id)],
    )
    .await;
    let notice = alice.read_until(transaction::CHAT_MSG).await;
    assert_eq!(field_int(&notice, field::CHAT_ID), Some(chat_id));
    assert!(
        field_text(&notice, field::CHAT_TEXT)
            .unwrap_or_default()
            .contains("Bob declined"),
        "decline notice names the invitee"
    );

    // The room is invite-only: carol (never invited) may not join it.
    let barred = carol
        .roundtrip(
            transaction::JOIN_CHAT,
            vec![Field::int(field::CHAT_ID, chat_id)],
        )
        .await;
    assert_ne!(barred.header.error, 0, "uninvited join is refused");

    alice.close().await;
    bob.close().await;
    carol.close().await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_im_relays_quoting_and_auto_responds_when_target_is_away() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for (login, pass) in [
        ("alice", "curiouser-and-curiouser"),
        ("bob", "swordfish-swordfish"),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, pass, Role::User)
            .await
            .unwrap();
    }

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut alice = Client::connect(addr).await;
    assert_eq!(
        alice
            .login("alice", "curiouser-and-curiouser", "Alice")
            .await
            .header
            .error,
        0
    );
    let mut bob = Client::connect(addr).await;
    assert_eq!(
        bob.login("bob", "swordfish-swordfish", "Bob")
            .await
            .header
            .error,
        0
    );

    let bob_uid = uid_of(&mut alice, "Bob").await;

    // Bob sets an automatic response (SetClientUserInfo field 215): the
    // session goes away in shared presence with that text as its status.
    bob.send(
        transaction::SET_CLIENT_USER_INFO,
        vec![
            Field::text(field::USER_NAME, "Bob"),
            Field::text(field::AUTOMATIC_RESPONSE, "gone to the tea party"),
        ],
    )
    .await;
    wait_presence(&burrow, "bob away with a status", |all| {
        all.iter().any(|e| {
            e.screen_name == "Bob"
                && e.state == 1
                && e.status.as_deref() == Some("gone to the tea party")
        })
    })
    .await;

    // Alice IMs bob with a quoted original. Bob still receives the IM (with
    // the quote relayed verbatim); alice gets the auto-response back, marked
    // with the automatic-response option.
    let sent = alice
        .roundtrip(
            transaction::SEND_INSTANT_MSG,
            vec![
                Field::int(field::USER_ID, bob_uid),
                Field::new(field::DATA, b"are you coming?".to_vec()),
                Field::new(field::QUOTING_MSG, b"> the hatter waits".to_vec()),
            ],
        )
        .await;
    assert_eq!(sent.header.error, 0, "IM accepted");

    let im = bob.read_until(transaction::SERVER_MSG).await;
    assert_eq!(field_int(&im, field::OPTIONS), Some(1), "a user message");
    assert_eq!(field_text(&im, field::USER_NAME).as_deref(), Some("Alice"));
    assert_eq!(
        field_text(&im, field::DATA).as_deref(),
        Some("are you coming?")
    );
    assert_eq!(
        field_text(&im, field::QUOTING_MSG).as_deref(),
        Some("> the hatter waits"),
        "the quoted original rides along"
    );

    let auto = alice.read_until(transaction::SERVER_MSG).await;
    assert_eq!(
        field_int(&auto, field::OPTIONS),
        Some(4),
        "marked as an automatic response"
    );
    assert_eq!(field_text(&auto, field::USER_NAME).as_deref(), Some("Bob"));
    assert_eq!(field_int(&auto, field::USER_ID), Some(bob_uid));
    assert_eq!(
        field_text(&auto, field::DATA).as_deref(),
        Some("gone to the tea party")
    );

    // A second IM this away period gets no second auto-response: the next
    // ServerMsg alice sees is bob's own reply, not another echo. (Had a
    // duplicate been sent, it would have been written by alice's own session
    // before bob's IM could arrive.)
    let sent = alice
        .roundtrip(
            transaction::SEND_INSTANT_MSG,
            vec![
                Field::int(field::USER_ID, bob_uid),
                Field::new(field::DATA, b"hello?".to_vec()),
            ],
        )
        .await;
    assert_eq!(sent.header.error, 0);
    let alice_uid = uid_of(&mut bob, "Alice").await;
    bob.send(
        transaction::SEND_INSTANT_MSG,
        vec![
            Field::int(field::USER_ID, alice_uid),
            Field::new(field::DATA, b"back now".to_vec()),
        ],
    )
    .await;
    let next = alice.read_until(transaction::SERVER_MSG).await;
    assert_eq!(
        field_int(&next, field::OPTIONS),
        Some(1),
        "no duplicate auto-response this away period"
    );
    assert_eq!(field_text(&next, field::DATA).as_deref(), Some("back now"));

    // Clearing the automatic response returns the session to online.
    bob.send(
        transaction::SET_CLIENT_USER_INFO,
        vec![
            Field::text(field::USER_NAME, "Bob"),
            Field::new(field::AUTOMATIC_RESPONSE, Vec::new()),
        ],
    )
    .await;
    wait_presence(&burrow, "bob back online", |all| {
        all.iter()
            .any(|e| e.screen_name == "Bob" && e.state == 0 && e.status.is_none())
    })
    .await;

    alice.close().await;
    bob.close().await;
    burrow.shutdown().await;
}
