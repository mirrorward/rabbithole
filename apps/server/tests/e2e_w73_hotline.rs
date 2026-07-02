//! Wave 7.3 end-to-end tests: the Hotline-compatible surface wired into
//! `burrow`. The wire codec is unit-tested in `rabbithole-legacy-hotline`; here
//! we prove burrow binds it, adapts it to real accounts/presence/chat, and
//! serves live sockets — a scripted vintage-style client handshakes, logs in,
//! sets its user info, appears in the user list, exchanges public chat with a
//! second client, and sends a private instant message. A bad login is rejected.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{Field, Handshake, HandshakeReply, Transaction, TransactionHeader};
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
        // Handshake: send the 12-byte TRTP/HOTL frame, read the 8-byte reply.
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

    /// Read exactly one transaction frame off the wire (single-frame bodies).
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

    /// Read transactions until one of `type_` arrives (skipping pushes),
    /// bounded by a timeout so a missing message fails fast.
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

    /// Perform a login. Credentials are byte-complemented on the wire, per the
    /// classic obfuscation.
    async fn login(&mut self, user: &str, pass: &str, name: &str) -> Transaction {
        let fields = vec![
            Field::new(field::LOGIN, obfuscate(user)),
            Field::new(field::PASSWORD, obfuscate(pass)),
            Field::text(field::USER_NAME, name),
            Field::int(field::USER_ICON_ID, 200),
        ];
        let id = self.send(transaction::LOGIN, fields).await;
        let reply = self.read_until(transaction::LOGIN).await;
        assert_eq!(reply.header.id, id, "login reply echoes the request id");
        reply
    }
}

fn obfuscate(s: &str) -> Vec<u8> {
    s.bytes().map(|b| !b).collect()
}

fn field_text(txn: &Transaction, id: u16) -> Option<String> {
    txn.fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.as_text_lossy().into_owned())
}

/// Parse the packed "user name with info" records (field 300) into
/// `(user_id, name)` pairs.
fn parse_user_list(txn: &Transaction) -> Vec<(u16, String)> {
    txn.fields
        .iter()
        .filter(|f| f.id == 300)
        .filter_map(|f| {
            let d = &f.data;
            if d.len() < 8 {
                return None;
            }
            let uid = u16::from_be_bytes([d[0], d[1]]);
            let name_len = u16::from_be_bytes([d[6], d[7]]) as usize;
            let name = String::from_utf8_lossy(&d[8..(8 + name_len).min(d.len())]).into_owned();
            Some((uid, name))
        })
        .collect()
}

#[tokio::test]
async fn hotline_login_presence_chat_and_im() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "hunter2hunter2", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "swordfish-swordfish", Role::User)
        .await
        .unwrap();
    let addr = burrow.hotline_addr.expect("hotline enabled");

    // Alice logs in successfully.
    let mut alice = Client::connect(addr).await;
    let reply = alice.login("alice", "hunter2hunter2", "Alice").await;
    assert_eq!(reply.header.error, 0, "good login accepted");

    // Alice sets her user info (name + icon).
    alice
        .send(
            transaction::SET_CLIENT_USER_INFO,
            vec![
                Field::text(field::USER_NAME, "Alice"),
                Field::int(field::USER_ICON_ID, 201),
            ],
        )
        .await;

    // Bob logs in too.
    let mut bob = Client::connect(addr).await;
    let reply = bob.login("bob", "swordfish-swordfish", "Bob").await;
    assert_eq!(reply.header.error, 0);
    // Let the join propagate to presence before Alice queries the list.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice's user name list shows both users.
    alice.send(transaction::GET_USER_NAME_LIST, vec![]).await;
    let list = alice.read_until(transaction::GET_USER_NAME_LIST).await;
    let users = parse_user_list(&list);
    let names: Vec<&str> = users.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"Alice"), "self in list: {names:?}");
    assert!(names.contains(&"Bob"), "peer in list: {names:?}");
    let bob_id = users
        .iter()
        .find(|(_, n)| n == "Bob")
        .map(|(id, _)| *id)
        .expect("bob has a user id");

    // Alice sends a public chat line; Bob receives it (shared lobby room).
    alice
        .send(
            transaction::CHAT_SEND,
            vec![Field::text(field::CHAT_TEXT, "down the rabbit hole")],
        )
        .await;
    let chat = bob.read_until(transaction::CHAT_MSG).await;
    let line = field_text(&chat, field::CHAT_TEXT).unwrap_or_default();
    assert!(
        line.contains("down the rabbit hole") && line.contains("Alice"),
        "chat delivered to peer: {line:?}"
    );

    // The native ChatService sees the same line in the lobby history.
    let history = burrow
        .shared
        .chat
        .history(rabbithole_server_core::LOBBY, 0, 10)
        .unwrap();
    assert!(
        history.iter().any(|l| l.text == "down the rabbit hole"),
        "line landed in the shared lobby"
    );

    // Alice sends Bob a private instant message; Bob receives a server message.
    alice
        .send(
            transaction::SEND_INSTANT_MSG,
            vec![
                Field::int(field::USER_ID, u32::from(bob_id)),
                Field::new(field::DATA, b"psst, meet me in the tea party".to_vec()),
            ],
        )
        .await;
    let im = bob.read_until(transaction::SERVER_MSG).await;
    let body = field_text(&im, field::DATA).unwrap_or_default();
    assert!(
        body.contains("tea party"),
        "instant message delivered: {body:?}"
    );
    assert_eq!(
        field_text(&im, field::USER_NAME).as_deref(),
        Some("Alice"),
        "IM names the sender"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_bad_login_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("carol", "correct-horse", Role::User)
        .await
        .unwrap();
    let addr = burrow.hotline_addr.expect("hotline enabled");

    let mut client = Client::connect(addr).await;
    let reply = client.login("carol", "wrong-password", "Carol").await;
    assert_ne!(reply.header.error, 0, "bad login rejected");
    assert!(
        field_text(&reply, field::ERROR_TEXT).is_some(),
        "rejection carries error text"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_off_by_default() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        name: "Quiet Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    assert!(burrow.hotline_addr.is_none(), "hotline off by default");
    burrow.shutdown().await;
}
