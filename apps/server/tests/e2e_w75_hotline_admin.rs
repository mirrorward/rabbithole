//! Hotline account-admin end-to-end tests: the classic admin transactions
//! (NewUser/DeleteUser/GetUser/SetUser 350-353, DisconnectUser 110,
//! UserBroadcast 355) exercised by scripted vintage-style clients against the
//! shared account service and RBAC classes. Mirrors the structure of
//! `e2e_w74_hotline_news.rs`.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{
    AccessMask, Field, Handshake, HandshakeReply, Privilege, Transaction, TransactionHeader,
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

    /// Expect the server to close the connection (EOF or reset), then shut
    /// our own write half down gracefully.
    async fn expect_closed(mut self) {
        let mut buf = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut buf))
            .await
            .expect("timed out waiting for the server to close the connection");
        match read {
            Ok(0) | Err(_) => {}
            Ok(n) => panic!("expected the connection to close, got {n} more byte(s)"),
        }
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

fn field_mask(txn: &Transaction, id: u16) -> AccessMask {
    AccessMask::decode(field_bytes(txn, id).expect("access field present")).expect("valid mask")
}

/// A member-shaped access bitmap (maps to the `member` role/class).
fn member_mask() -> AccessMask {
    [
        Privilege::ReadChat,
        Privilege::SendChat,
        Privilege::NewsReadArticle,
        Privilege::NewsPostArticle,
        Privilege::DownloadFiles,
        Privilege::UploadFiles,
        Privilege::SendPrivateMessages,
    ]
    .into_iter()
    .collect()
}

/// An admin-shaped access bitmap (any user-admin bit maps to `admin`).
fn admin_mask() -> AccessMask {
    let mut m = member_mask();
    m.grant(Privilege::CreateUsers);
    m.grant(Privilege::DeleteUsers);
    m.grant(Privilege::ModifyUsers);
    m.grant(Privilege::DisconnectUsers);
    m
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

#[tokio::test]
async fn hotline_admin_account_lifecycle_roundtrips_access_mask() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("root", "queen-of-hearts", Role::Admin)
        .await
        .unwrap();

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut root = Client::connect(addr).await;
    let login = root.login("root", "queen-of-hearts", "Root").await;
    assert_eq!(login.header.error, 0);
    // The login reply carries the projected access bitmap: an admin holds
    // the user-admin bits, so real clients enable their admin menus.
    let mask = field_mask(&login, field::USER_ACCESS);
    assert!(mask.has(Privilege::CreateUsers), "admin login mask");
    assert!(mask.has(Privilege::DisconnectUsers), "admin login mask");

    // NewUser: create mallory with a member-shaped bitmap.
    let created = root
        .roundtrip(
            transaction::NEW_USER,
            vec![
                Field::text(field::USER_NAME, "Mallory"),
                Field::credential(field::USER_LOGIN, "mallory"),
                Field::credential(field::USER_PASSWORD, "off-with-their-heads"),
                Field::new(field::USER_ACCESS, member_mask().to_bytes().to_vec()),
            ],
        )
        .await;
    assert_eq!(created.header.error, 0, "NewUser accepted");

    // Creating the same login again is refused.
    let dup = root
        .roundtrip(
            transaction::NEW_USER,
            vec![
                Field::credential(field::USER_LOGIN, "mallory"),
                Field::credential(field::USER_PASSWORD, "x"),
            ],
        )
        .await;
    assert_ne!(dup.header.error, 0, "duplicate login refused");
    assert!(
        field_text(&dup, field::ERROR_TEXT)
            .unwrap_or_default()
            .contains("exists"),
        "duplicate error names the cause"
    );

    // GetUser round-trips the account: login, name, and the projected mask.
    let got = root
        .roundtrip(
            transaction::GET_USER,
            vec![Field::credential(field::USER_LOGIN, "mallory")],
        )
        .await;
    assert_eq!(got.header.error, 0, "GetUser succeeds");
    assert_eq!(
        got.fields
            .iter()
            .find(|f| f.id == field::USER_LOGIN)
            .map(|f| f.as_credential_text_lossy())
            .as_deref(),
        Some("mallory"),
        "login round-trips (obfuscated on the wire)"
    );
    assert_eq!(
        field_text(&got, field::USER_NAME).as_deref(),
        Some("mallory"),
        "account name present"
    );
    assert!(
        field_bytes(&got, field::USER_PASSWORD)
            .map(|b| b.is_empty())
            .unwrap_or(false),
        "the password is never disclosed (empty placeholder)"
    );
    let got_mask = field_mask(&got, field::USER_ACCESS);
    for p in [
        Privilege::DownloadFiles,
        Privilege::UploadFiles,
        Privilege::NewsPostArticle,
        Privilege::SendPrivateMessages,
        Privilege::ChangeOwnPassword,
    ] {
        assert!(got_mask.has(p), "member mask holds {p:?}");
    }
    assert!(!got_mask.has(Privilege::CreateUsers), "member is no admin");

    // The created account is a real, working login.
    let mut mallory = Client::connect(addr).await;
    let m_login = mallory
        .login("mallory", "off-with-their-heads", "Mallory")
        .await;
    assert_eq!(m_login.header.error, 0, "created account can log in");
    assert!(
        !field_mask(&m_login, field::USER_ACCESS).has(Privilege::CreateUsers),
        "member session gets a member mask"
    );
    mallory.close().await;

    // SetUser with an admin-shaped mask promotes to the admin role/class.
    let promoted = root
        .roundtrip(
            transaction::SET_USER,
            vec![
                Field::credential(field::USER_LOGIN, "mallory"),
                Field::new(field::USER_PASSWORD, Vec::new()), // unchanged
                Field::new(field::USER_ACCESS, admin_mask().to_bytes().to_vec()),
            ],
        )
        .await;
    assert_eq!(promoted.header.error, 0, "SetUser accepted");
    let got = root
        .roundtrip(
            transaction::GET_USER,
            vec![Field::credential(field::USER_LOGIN, "mallory")],
        )
        .await;
    assert!(
        field_mask(&got, field::USER_ACCESS).has(Privilege::CreateUsers),
        "promotion round-trips through the mask"
    );

    // Role ordering: an admin cannot delete a fellow admin...
    let refused = root
        .roundtrip(
            transaction::DELETE_USER,
            vec![Field::credential(field::USER_LOGIN, "mallory")],
        )
        .await;
    assert_ne!(refused.header.error, 0, "cannot delete an equal role");

    // ...so demote back to member first, then delete.
    let demoted = root
        .roundtrip(
            transaction::SET_USER,
            vec![
                Field::credential(field::USER_LOGIN, "mallory"),
                Field::new(field::USER_ACCESS, member_mask().to_bytes().to_vec()),
            ],
        )
        .await;
    assert_eq!(demoted.header.error, 0);
    let deleted = root
        .roundtrip(
            transaction::DELETE_USER,
            vec![Field::credential(field::USER_LOGIN, "mallory")],
        )
        .await;
    assert_eq!(deleted.header.error, 0, "DeleteUser accepted");

    // The deleted account reads as absent and can no longer log in.
    let gone = root
        .roundtrip(
            transaction::GET_USER,
            vec![Field::credential(field::USER_LOGIN, "mallory")],
        )
        .await;
    assert_ne!(gone.header.error, 0, "deleted account is absent");
    let mut mallory = Client::connect(addr).await;
    let m_login = mallory
        .login("mallory", "off-with-their-heads", "Mallory")
        .await;
    assert_ne!(m_login.header.error, 0, "deleted account cannot log in");
    mallory.close().await;

    root.close().await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_non_admin_is_refused_admin_transactions() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "swordfish-swordfish", Role::User)
        .await
        .unwrap();

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut bob = Client::connect(addr).await;
    let login = bob.login("bob", "swordfish-swordfish", "Bob").await;
    assert_eq!(login.header.error, 0);
    assert!(
        !field_mask(&login, field::USER_ACCESS).has(Privilege::CreateUsers),
        "member login mask has no admin bits"
    );

    let create = bob
        .roundtrip(
            transaction::NEW_USER,
            vec![
                Field::credential(field::USER_LOGIN, "eve"),
                Field::credential(field::USER_PASSWORD, "pw-pw-pw-pw"),
            ],
        )
        .await;
    assert_ne!(create.header.error, 0, "NewUser refused");

    let get = bob
        .roundtrip(
            transaction::GET_USER,
            vec![Field::credential(field::USER_LOGIN, "bob")],
        )
        .await;
    assert_ne!(get.header.error, 0, "GetUser refused");

    let set = bob
        .roundtrip(
            transaction::SET_USER,
            vec![
                Field::credential(field::USER_LOGIN, "bob"),
                Field::new(field::USER_ACCESS, admin_mask().to_bytes().to_vec()),
            ],
        )
        .await;
    assert_ne!(set.header.error, 0, "SetUser (self-promotion) refused");

    let del = bob
        .roundtrip(
            transaction::DELETE_USER,
            vec![Field::credential(field::USER_LOGIN, "bob")],
        )
        .await;
    assert_ne!(del.header.error, 0, "DeleteUser refused");

    let kick = bob
        .roundtrip(
            transaction::DISCONNECT_USER,
            vec![Field::int(field::USER_ID, 1)],
        )
        .await;
    assert_ne!(kick.header.error, 0, "DisconnectUser refused");

    let bcast = bob
        .roundtrip(
            transaction::USER_BROADCAST,
            vec![Field::text(field::DATA, "free candy")],
        )
        .await;
    assert_ne!(bcast.header.error, 0, "UserBroadcast refused");

    // The refused NewUser really did nothing.
    assert!(
        burrow
            .shared
            .auth
            .login_password("eve", "pw-pw-pw-pw", None)
            .await
            .is_err(),
        "no account was created"
    );

    bob.close().await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_broadcast_kick_and_ban() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("root", "queen-of-hearts", Role::Admin)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("mallory", "off-with-their-heads", Role::User)
        .await
        .unwrap();

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut root = Client::connect(addr).await;
    assert_eq!(
        root.login("root", "queen-of-hearts", "Root")
            .await
            .header
            .error,
        0
    );
    let mut mallory = Client::connect(addr).await;
    assert_eq!(
        mallory
            .login("mallory", "off-with-their-heads", "Mallory")
            .await
            .header
            .error,
        0
    );

    // UserBroadcast reaches every connected Hotline client as a ServerMsg.
    let bcast = root
        .roundtrip(
            transaction::USER_BROADCAST,
            vec![Field::text(field::DATA, "the trial is starting")],
        )
        .await;
    assert_eq!(bcast.header.error, 0, "broadcast accepted");
    let push = mallory.read_until(transaction::SERVER_MSG).await;
    assert_eq!(
        field_text(&push, field::DATA).as_deref(),
        Some("the trial is starting"),
        "broadcast delivered to the other client"
    );

    // Find mallory's wire user id from the classic user list.
    let list = root
        .roundtrip(transaction::GET_USER_NAME_LIST, vec![])
        .await;
    let (target, _) = parse_users(&list)
        .into_iter()
        .find(|(_, name)| name == "Mallory")
        .expect("mallory in the user list");

    // Kick with the ban option: the target gets the DisconnectMsg, then the
    // server closes the connection.
    let kicked = root
        .roundtrip(
            transaction::DISCONNECT_USER,
            vec![
                Field::int(field::USER_ID, u32::from(target)),
                Field::int(field::OPTIONS, 1), // temporary ban
            ],
        )
        .await;
    assert_eq!(kicked.header.error, 0, "kick accepted");

    let notice = mallory.read_until(transaction::DISCONNECT_MSG).await;
    assert_eq!(
        field_text(&notice, field::DATA).as_deref(),
        Some("banned"),
        "the kicked client is told why before the close"
    );
    mallory.expect_closed().await;

    // The session leaves shared presence.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let online = burrow.shared.presence.snapshot();
        if !online.iter().any(|e| e.screen_name == "Mallory") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "kicked session still in presence: {online:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The temporary ban blocks a fresh login (same account, same address).
    let mut again = Client::connect(addr).await;
    let refused = again
        .login("mallory", "off-with-their-heads", "Mallory")
        .await;
    assert_ne!(refused.header.error, 0, "banned account is refused");
    assert_eq!(
        field_text(&refused, field::ERROR_TEXT).as_deref(),
        Some("you are banned")
    );
    again.close().await;

    root.close().await;
    burrow.shutdown().await;
}
