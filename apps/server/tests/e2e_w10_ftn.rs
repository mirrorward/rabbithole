//! Wave 10 end-to-end tests: the FidoNet (FTN) binkp gateway wired into
//! `burrow`. The PKT/binkp codecs and the tosser/scanner are unit-tested in
//! their own crates; here we prove burrow binds the mailer, tosses inbound mail
//! onto real boards/DMs, scans local posts back out into BSO packets, and drives
//! a live binkp session over a socket — and that it stays off by default.

use std::collections::HashMap;
use std::time::Duration;

use burrow::ftn::{self, FtnGateway};
use burrow::Burrow;
use rabbithole_legacy_binkp::{Address as BinkpAddress, FileInfo};
use rabbithole_legacy_ftn::{Message as FtnMessage, PackedMessage, Packet, PacketHeader};
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo3::DmsRepo;
use tokio::net::TcpStream;

/// A test config with the FTN gateway toggled and an `R20.GENERAL → rabbit`
/// echo-area mapping. Spool dirs are absolute (under `dir`) so they resolve
/// without depending on `data_dir`.
fn ftn_config(dir: &std::path::Path, enabled: bool) -> ServerConfig {
    let mut areas = HashMap::new();
    areas.insert("R20.GENERAL".to_string(), "rabbit".to_string());
    ServerConfig {
        name: "FTN Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ftn_enabled: enabled,
        ftn_addr: "127.0.0.1:0".parse().unwrap(),
        ftn_node: "2:280/1".into(),
        ftn_uplink: "2:280/464".into(),
        ftn_areas: areas,
        ftn_inbound_dir: dir.join("in"),
        ftn_outbound_dir: dir.join("out"),
        data_dir: dir.join("srv"),
        ..ServerConfig::default()
    }
}

/// Build a one-message echomail `.PKT` in the given area.
fn echo_pkt(area: &str, from: &str, subject: &str, body: &str, msgid: &str) -> Vec<u8> {
    let mut m = PackedMessage {
        orig_net: 280,
        orig_node: 464,
        dest_net: 280,
        dest_node: 1,
        to: "All".into(),
        from: from.into(),
        subject: subject.into(),
        ..Default::default()
    };
    m.set_body(&FtnMessage {
        area: Some(area.into()),
        kludges: vec![format!("MSGID: 2:280/464 {msgid}")],
        text: body.as_bytes().to_vec(),
        ..Default::default()
    });
    let header = PacketHeader {
        orig_zone: 2,
        dest_zone: 2,
        orig_net: 280,
        orig_node: 464,
        dest_net: 280,
        dest_node: 1,
        ..Default::default()
    };
    Packet {
        header,
        messages: vec![m],
    }
    .encode()
}

/// Build a one-message netmail `.PKT` addressed to `to`.
fn netmail_pkt(to: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
    let mut m = PackedMessage {
        orig_net: 280,
        orig_node: 464,
        dest_net: 280,
        dest_node: 1,
        to: to.into(),
        from: from.into(),
        subject: subject.into(),
        ..Default::default()
    };
    m.set_body(&FtnMessage {
        kludges: vec!["MSGID: 2:280/464 nnnn0001".into()],
        text: body.as_bytes().to_vec(),
        ..Default::default()
    });
    let header = PacketHeader {
        orig_zone: 2,
        dest_zone: 2,
        orig_net: 280,
        orig_node: 464,
        dest_net: 280,
        dest_node: 1,
        ..Default::default()
    };
    Packet {
        header,
        messages: vec![m],
    }
    .encode()
}

async fn wait_for_thread(shared: &burrow::Shared, slug: &str) -> bool {
    for _ in 0..60 {
        if let Ok(threads) = shared.boards.threads(slug, 100).await {
            if !threads.is_empty() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn ftn_off_by_default() {
    // The bare default keeps the mailer disabled.
    assert!(!ServerConfig::default().ftn_enabled);
    assert_eq!(
        ServerConfig::default().ftn_addr.port(),
        24554,
        "IANA binkp port default"
    );

    // A burrow started without opting in binds no FTN listener.
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ftn_config(work.path(), false)).await.unwrap();
    assert!(burrow.ftn_addr.is_none(), "FTN listener stays down");
    burrow.shutdown().await;
}

#[tokio::test]
async fn inbound_echomail_becomes_board_post() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ftn_config(work.path(), true)).await.unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit", "Rabbit", "", 2, None, 0)
        .await
        .unwrap();

    let gw = FtnGateway::from_shared(
        burrow.shared.clone(),
        work.path().join("g-in"),
        work.path().join("g-out"),
    );
    let pkt = echo_pkt(
        "R20.GENERAL",
        "Kevin",
        "Hello echo",
        "Greetings, Warren!",
        "aaaa0001",
    );
    let (posted, delivered) = gw.ingest_pkt_bytes(&pkt).await.unwrap();
    assert_eq!((posted, delivered), (1, 0));

    let threads = burrow.shared.boards.threads("rabbit", 100).await.unwrap();
    assert_eq!(threads.len(), 1, "one echomail thread landed on the board");
    assert_eq!(threads[0].0.subject, "Hello echo");
    assert!(threads[0].0.author.ends_with("@fidonet"));

    // A re-toss of the same MSGID is a dupe: no second post.
    let (again, _) = gw.ingest_pkt_bytes(&pkt).await.unwrap();
    assert_eq!(again, 0, "MSGID dupe is not posted twice");
    assert_eq!(
        burrow
            .shared
            .boards
            .threads("rabbit", 100)
            .await
            .unwrap()
            .len(),
        1
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn inbound_netmail_becomes_dm() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ftn_config(work.path(), true)).await.unwrap();
    let alice = burrow
        .shared
        .auth
        .create_account("alice", "hunter2hunter2", Role::User)
        .await
        .unwrap();

    let gw = FtnGateway::from_shared(
        burrow.shared.clone(),
        work.path().join("g-in"),
        work.path().join("g-out"),
    );
    let pkt = netmail_pkt("alice", "Kevin", "psst", "a private note");
    let (posted, delivered) = gw.ingest_pkt_bytes(&pkt).await.unwrap();
    assert_eq!((posted, delivered), (0, 1));

    // The DM landed for alice, from the gateway pseudo-account.
    let rows = DmsRepo(&burrow.shared.pool)
        .thread(0, alice.id, 0, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, "a private note");

    // An unmapped recipient is dropped, not delivered.
    let miss = netmail_pkt("nobody", "Kevin", "?", "into the void");
    let (_, delivered) = gw.ingest_pkt_bytes(&miss).await.unwrap();
    assert_eq!(delivered, 0);

    burrow.shutdown().await;
}

#[tokio::test]
async fn scan_local_post_stages_outbound_packet() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ftn_config(work.path(), true)).await.unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit", "Rabbit", "", 2, None, 0)
        .await
        .unwrap();

    let gw = FtnGateway::from_shared(
        burrow.shared.clone(),
        work.path().join("g-in"),
        work.path().join("g-out"),
    );

    // A locally authored post (author domain == origin) in a mapped board.
    let now = chrono::Utc::now().timestamp_millis();
    let row = burrow
        .shared
        .boards
        .post(
            "rabbit",
            None,
            "kevin@ftn-warren",
            &[7u8; 32],
            "Outbound hello",
            "going to fidonet",
            "text/plain",
            now,
        )
        .await
        .unwrap();

    let path = gw
        .scan_local_post(&row)
        .await
        .unwrap()
        .expect("post scanned to a staged packet");
    assert!(path.exists(), "staged BSO file exists");
    // BSO name for dest 2:280/464 net/node, Normal packet flavor.
    assert_eq!(path.file_name().unwrap().to_string_lossy(), "011801d0.out");

    // The staged bytes are a valid PKT carrying the echomail in the right area.
    let bytes = tokio::fs::read(&path).await.unwrap();
    let pkt = Packet::decode(&bytes).unwrap();
    assert_eq!(pkt.messages.len(), 1);
    let parsed = pkt.messages[0].parse_body();
    assert_eq!(parsed.area.as_deref(), Some("R20.GENERAL"));
    assert_eq!(parsed.text_str(), "going to fidonet");

    burrow.shutdown().await;
}

#[tokio::test]
async fn binkp_socket_roundtrip_tosses_to_board() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ftn_config(work.path(), true)).await.unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit", "Rabbit", "", 2, None, 0)
        .await
        .unwrap();
    let server = burrow.ftn_addr.expect("ftn listener bound");

    // Act as an originating mailer: connect and send one echomail PKT.
    let pkt = echo_pkt(
        "R20.GENERAL",
        "Kevin",
        "Over the wire",
        "delivered by binkp",
        "bbbb0002",
    );
    let stream = TcpStream::connect(server).await.unwrap();
    let files = vec![(FileInfo::new("mail.pkt", pkt.len() as u64, 1000), pkt)];
    let received = ftn::run_originating(
        stream,
        vec![BinkpAddress::new(2, 280, 464, 0).with_domain("fidonet")],
        String::new(),
        files,
        &work.path().join("client-in"),
    )
    .await
    .unwrap();
    assert!(
        received.is_empty(),
        "server sent us nothing back this batch"
    );

    // The server tosses it asynchronously; the post shows up shortly.
    assert!(
        wait_for_thread(&burrow.shared, "rabbit").await,
        "binkp-delivered echomail became a board post"
    );
    let threads = burrow.shared.boards.threads("rabbit", 100).await.unwrap();
    assert_eq!(threads[0].0.subject, "Over the wire");

    burrow.shutdown().await;
}
