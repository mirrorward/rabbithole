//! Wave 10 end-to-end tests: QWK offline mail wired into `burrow`. The
//! byte-level codec is unit-tested in `rabbithole-legacy-qwk`; here we prove
//! the server glue: `qwk-build` writes spool members that decode back to the
//! right posts under the stable conference numbering, read pointers advance
//! (a second build is empty), `.REP` ingest posts as the uploading user and
//! dedupes a re-upload, the telnet `qwk` command mints HTTP handoff links,
//! and everything is off by default.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_qwk::{ControlDat, MessagesDat, ReplyMessage, ReplyPacket};
use rabbithole_server_core::{Role, ServerConfig};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "QWK Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        qwk_enabled: true,
        ..ServerConfig::default()
    }
}

/// Two postable boards (slug order fixes the conference numbering:
/// `alpha` = 1, `beta` = 2) plus a category that must never be a conference.
async fn seed_boards(burrow: &Burrow) {
    let b = &burrow.shared.boards;
    b.create_board("rabbit", "Rabbit", "", 0, None, 0)
        .await
        .unwrap();
    b.create_board("beta", "Beta", "", 2, None, 0)
        .await
        .unwrap();
    b.create_board("alpha", "Alpha", "", 2, None, 0)
        .await
        .unwrap();
}

/// Post with a fixed timestamp for deterministic ordering/pointers.
async fn seed_post(burrow: &Burrow, board: &str, subject: &str, body: &str, ts: i64) {
    burrow
        .shared
        .boards
        .post(
            board,
            None,
            "seeder@qwk-warren",
            &[7u8; 32],
            subject,
            body,
            "text/plain",
            ts,
        )
        .await
        .unwrap();
}

/// Member path from a `qwk-build` response, by canonical name.
fn member_path(data: &Value, name: &str) -> std::path::PathBuf {
    let m = data["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("member {name} missing: {data}"));
    std::path::PathBuf::from(m["path"].as_str().unwrap())
}

#[tokio::test]
async fn qwk_build_members_decode_and_pointers_advance() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    seed_boards(&burrow).await;
    seed_post(&burrow, "alpha", "First", "hello from alpha", 1000).await;
    seed_post(&burrow, "beta", "Cross", "hello from beta", 1500).await;
    seed_post(&burrow, "alpha", "Second", "more alpha", 2000).await;

    // First build: everything is unread.
    let r = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "alice"}),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let data = &r["data"];
    assert_eq!(data["total_messages"], 3, "{data}");

    // Stable numbering: postable boards sorted by slug, 1..=N; the category
    // is not a conference. The mapping is also in CONTROL.DAT.
    let confs = data["conferences"].as_array().unwrap();
    assert_eq!(confs.len(), 2, "{data}");
    assert_eq!(confs[0], json!({"conference": 1, "board": "alpha"}));
    assert_eq!(confs[1], json!({"conference": 2, "board": "beta"}));

    let names: Vec<&str> = data["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "MESSAGES.DAT",
            "CONTROL.DAT",
            "DOOR.ID",
            "001.NDX",
            "002.NDX"
        ],
        "members in the packet's stable order"
    );

    // The delivered `.QWK` is a real STORE-method ZIP of the members: a valid
    // envelope whose entries (name + bytes) round-trip against the raw
    // spooled members.
    let zip = std::fs::read(data["packet"].as_str().unwrap()).unwrap();
    assert!(
        zip.starts_with(&0x0403_4b50u32.to_le_bytes()),
        "local file header signature"
    );
    assert!(
        zip.windows(4).any(|w| w == 0x0605_4b50u32.to_le_bytes()),
        "end-of-central-directory record present"
    );
    for name in ["MESSAGES.DAT", "CONTROL.DAT", "001.NDX"] {
        let raw = std::fs::read(member_path(data, name)).unwrap();
        assert!(
            zip.windows(name.len()).any(|w| w == name.as_bytes()),
            "{name} entry present in the ZIP"
        );
        assert!(
            raw.is_empty() || zip.windows(raw.len()).any(|w| w == raw.as_slice()),
            "{name} bytes stored (uncompressed) in the ZIP"
        );
    }

    // MESSAGES.DAT decodes back (with the dev-dep codec) to the seeded posts
    // under the right conference numbers, oldest first per conference.
    let bytes = std::fs::read(member_path(data, "MESSAGES.DAT")).unwrap();
    let back = MessagesDat::decode(&bytes).unwrap();
    assert_eq!(back.messages.len(), 3);
    let got: Vec<(u16, &str, &str)> = back
        .messages
        .iter()
        .map(|m| (m.conference, m.subject.as_str(), m.body.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (1, "First", "hello from alpha"),
            (1, "Second", "more alpha"),
            (2, "Cross", "hello from beta"),
        ]
    );
    assert!(
        back.messages[0].from.starts_with("seeder@"),
        "author carried into From: {:?}",
        back.messages[0].from
    );

    // CONTROL.DAT carries the identity, the target user, and the mapping.
    let control =
        ControlDat::parse(&std::fs::read(member_path(data, "CONTROL.DAT")).unwrap()).unwrap();
    assert_eq!(control.bbs_name, "QWK Warren");
    assert_eq!(control.username, "ALICE");
    assert_eq!(control.total_messages, 3);
    assert_eq!(
        control.conferences,
        vec![(1, "alpha".to_string()), (2, "beta".to_string())]
    );

    // Second build: pointers advanced, nothing new → an empty packet.
    let r2 = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "alice"}),
    )
    .await;
    assert_eq!(r2["ok"], true, "{r2}");
    assert_eq!(r2["data"]["total_messages"], 0, "{r2}");
    let bytes2 = std::fs::read(member_path(&r2["data"], "MESSAGES.DAT")).unwrap();
    assert!(
        MessagesDat::decode(&bytes2).unwrap().messages.is_empty(),
        "second packet decodes empty"
    );

    // New mail after the high-water mark shows up in the third build — and
    // only it.
    seed_post(&burrow, "alpha", "Third", "fresh", 3000).await;
    let r3 = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "alice"}),
    )
    .await;
    assert_eq!(r3["data"]["total_messages"], 1, "{r3}");
    let bytes3 = std::fs::read(member_path(&r3["data"], "MESSAGES.DAT")).unwrap();
    let back3 = MessagesDat::decode(&bytes3).unwrap();
    assert_eq!(back3.messages.len(), 1);
    assert_eq!(back3.messages[0].subject, "Third");
    assert_eq!(back3.messages[0].conference, 1);

    burrow.shutdown().await;
}

#[tokio::test]
async fn rep_ingest_posts_threads_dedupes_and_rejects() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    seed_boards(&burrow).await;
    seed_post(&burrow, "alpha", "Hello", "root post", 1000).await;

    // A REP with: a good reply threaded under alpha article #1, and a reply
    // to a conference that doesn't exist.
    let mut good = ReplyMessage::new(1, "ALL", "BOB", "Re: Hello", "good reply");
    good.reference = 1; // alpha article #1 = the "Hello" root
    let bad = ReplyMessage::new(99, "ALL", "BOB", "Lost", "no such conference");
    let rep = ReplyPacket::new(vec![good, bad]).encode();
    let rep_path = work.path().join("upload.msg");
    std::fs::write(&rep_path, &rep).unwrap();

    let ingest = |path: String| {
        let shared = burrow.shared.clone();
        async move {
            burrow::ctl::handle(
                &shared,
                &json!({"cmd": "qwk-ingest", "login": "bob", "path": path}),
            )
            .await
        }
    };

    let r = ingest(rep_path.display().to_string()).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["accepted"], 1, "{r}");
    assert_eq!(r["data"]["duplicates"], 0, "{r}");
    let rejected = r["data"]["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1, "{r}");
    assert_eq!(rejected[0]["subject"], "Lost");
    assert!(
        rejected[0]["reason"]
            .as_str()
            .unwrap()
            .contains("unknown conference 99"),
        "{r}"
    );

    // The reply landed on the mapped board, threaded under the root, and is
    // authored by the *uploading user* (not the REP's From field).
    let threads = burrow.shared.boards.threads("alpha", 100).await.unwrap();
    assert_eq!(threads.len(), 1, "reply threaded, not a new thread");
    let root = &threads[0].0;
    let full = burrow
        .shared
        .boards
        .thread(&root.event_id, 100)
        .await
        .unwrap();
    assert_eq!(full.len(), 2);
    let reply = full.iter().find(|p| p.subject == "Re: Hello").unwrap();
    assert_eq!(reply.parent_id, Some(root.event_id));
    assert!(
        reply.author.starts_with("bob@"),
        "authored by the uploader: {}",
        reply.author
    );
    assert_eq!(reply.body, "good reply");

    // Re-uploading the same REP double-posts nothing: content-hash dedupe.
    let r2 = ingest(rep_path.display().to_string()).await;
    assert_eq!(r2["ok"], true, "{r2}");
    assert_eq!(r2["data"]["accepted"], 0, "{r2}");
    assert_eq!(r2["data"]["duplicates"], 1, "{r2}");
    let full2 = burrow
        .shared
        .boards
        .thread(&root.event_id, 100)
        .await
        .unwrap();
    assert_eq!(full2.len(), 2, "no double post on re-ingest");

    burrow.shutdown().await;
}

#[tokio::test]
async fn qwk_disabled_by_default_refuses_both_ctl_surfaces() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        name: "Quiet Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    };
    assert!(!cfg.qwk_enabled, "off by default");
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let r = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "alice"}),
    )
    .await;
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap().contains("not enabled"), "{r}");

    let rep_path = work.path().join("upload.msg");
    std::fs::write(
        &rep_path,
        ReplyPacket::new(vec![ReplyMessage::new(1, "ALL", "A", "s", "b")]).encode(),
    )
    .unwrap();
    let r = burrow::ctl::handle(
        &burrow.shared,
        &json!({
            "cmd": "qwk-ingest",
            "login": "alice",
            "path": rep_path.display().to_string(),
        }),
    )
    .await;
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap().contains("not enabled"), "{r}");

    burrow.shutdown().await;
}

// ---------------------------------------------------------------------------
// Telnet surface: the same deterministic marker-driven client the other
// telnet e2e suites use (no blind sleeps).

struct Client {
    sock: TcpStream,
    buf: Vec<u8>,
    pos: usize,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        Client {
            sock: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
            pos: 0,
        }
    }

    async fn send(&mut self, line: &str) {
        self.sock
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    async fn expect(&mut self, needle: &[u8]) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(at) = find(&self.buf[self.pos..], needle) {
                    self.pos += at + needle.len();
                    return;
                }
                let mut chunk = [0u8; 4096];
                let n = self.sock.read(&mut chunk).await.expect("telnet read");
                assert!(
                    n > 0,
                    "EOF while waiting for {:?}; unconsumed: {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.buf[self.pos..])
                );
                self.buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {:?}; unconsumed: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.buf[self.pos..])
            )
        })
    }

    async fn login(&mut self, user: &str, pass: &str) {
        self.expect(b"login: ").await;
        self.send(user).await;
        self.expect(b"password: ").await;
        self.send(pass).await;
        self.expect(b"Command: ").await;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn telnet_qwk_mints_links_and_refuses_politely() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(&work.path().join("srv"));
    cfg.telnet_enabled = true;
    cfg.telnet_addr = "127.0.0.1:0".parse().unwrap();
    cfg.files_http_base = "http://dl.example:8080".into();
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    seed_boards(&burrow).await;
    seed_post(&burrow, "alpha", "Hi", "body", 1000).await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    // The menu advertises QWK while enabled.
    c.send("").await;
    c.expect(b"[M] QWK offline mail").await;
    c.expect(b"Command: ").await;

    // `qwk` builds the packet and mints one link per raw member.
    c.send("qwk").await;
    c.expect(b"1 new message(s)").await;
    c.expect(b"http://dl.example:8080/qwk/alice/MESSAGES.DAT")
        .await;
    c.expect(b"http://dl.example:8080/qwk/alice/CONTROL.DAT")
        .await;
    c.expect(b"http://dl.example:8080/qwk/alice/DOOR.ID").await;
    c.expect(b"http://dl.example:8080/qwk/alice/001.NDX").await;
    c.expect(b"Command: ").await;

    // The spool holds the members the links point at.
    let spool = work.path().join("srv").join("qwk").join("alice");
    assert!(spool.join("MESSAGES.DAT").is_file(), "spooled member");

    // No handoff base → the polite no-transfers notice (before any build).
    burrow.shared.config.set_key("files_http_base", "").unwrap();
    c.send("qwk").await;
    c.expect(b"not available on telnet").await;
    c.expect(b"Command: ").await;

    // Disabled live → the not-enabled notice.
    burrow
        .shared
        .config
        .set_key("qwk_enabled", "false")
        .unwrap();
    c.send("qwk").await;
    c.expect(b"not enabled on this system").await;
    c.expect(b"Command: ").await;

    c.send("q").await;
    c.expect(b"Goodbye").await;
    burrow.shutdown().await;
}
