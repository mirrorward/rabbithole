//! Wave 10 end-to-end tests: the NNTP **peer-feed** (transit) surface wired
//! into `burrow`. The transit state machine and codec are unit-tested in
//! `rabbithole-legacy-nntp`; here we prove burrow binds the feed listener,
//! gates every transit verb behind the peer allowlist, drives a streaming
//! CHECK/TAKETHIS session (and classic IHAVE) into real board posts, refuses
//! re-offered ids via the shared dedupe, and answers NEWNEWS.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;

fn feed_config(dir: &std::path::Path) -> ServerConfig {
    let mut peers = std::collections::HashMap::new();
    peers.insert("hub".to_string(), "feed-pw".to_string());
    ServerConfig {
        name: "Feed Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_feed_enabled: true,
        nntp_feed_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_feed_peers: peers,
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// A minimal scripted NNTP peer over the wire (same shape as the reader-side
/// client in `e2e_w102`).
struct Peer {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Peer {
    async fn connect(addr: std::net::SocketAddr) -> Peer {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (r, w) = stream.into_split();
        Peer {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// Read one status line (CRLF stripped).
    async fn read_line(&mut self) -> String {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .expect("server responded")
            .unwrap();
        assert!(n > 0, "server closed unexpectedly");
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    /// Read a multi-line data block terminated by a "." line (un-dot-stuffed).
    async fn read_block(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let line = self.read_line().await;
            if line == "." {
                break;
            }
            out.push(line.strip_prefix('.').unwrap_or(&line).to_string());
        }
        out
    }

    async fn send(&mut self, cmd: &str) {
        self.writer
            .write_all(format!("{cmd}\r\n").as_bytes())
            .await
            .unwrap();
    }

    /// Send a dot-terminated article data block.
    async fn send_article(&mut self, lines: &[&str]) {
        let mut wire = String::new();
        for l in lines {
            wire.push_str(l);
            wire.push_str("\r\n");
        }
        wire.push_str(".\r\n");
        self.writer.write_all(wire.as_bytes()).await.unwrap();
    }

    /// AUTHINFO as the configured test peer.
    async fn auth(&mut self) {
        self.send("AUTHINFO USER hub").await;
        assert!(self.read_line().await.starts_with("381"));
        self.send("AUTHINFO PASS feed-pw").await;
        assert!(self.read_line().await.starts_with("281"));
    }
}

#[tokio::test]
async fn streaming_feed_accepts_posts_and_dedupes() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(feed_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit.general", "General", "General chatter", 2, None, 0)
        .await
        .unwrap();

    let addr = burrow.nntp_feed_addr.expect("feed enabled");
    let mut peer = Peer::connect(addr).await;
    assert!(peer.read_line().await.starts_with("200"), "greeting");

    // CAPABILITIES advertises the transit surface.
    peer.send("CAPABILITIES").await;
    assert!(peer.read_line().await.starts_with("101"));
    let caps = peer.read_block().await;
    assert!(caps.iter().any(|l| l == "IHAVE"), "caps: {caps:?}");
    assert!(caps.iter().any(|l| l == "STREAMING"), "caps: {caps:?}");
    assert!(caps.iter().any(|l| l == "MODE STREAM"), "caps: {caps:?}");

    peer.auth().await;
    peer.send("MODE STREAM").await;
    assert!(peer.read_line().await.starts_with("203"), "streaming ok");

    // CHECK an unknown id: wanted, with the id echoed (RFC 4644).
    peer.send("CHECK <article-1@peer.example>").await;
    assert_eq!(
        peer.read_line().await,
        "238 <article-1@peer.example>",
        "unknown id is wanted"
    );

    // TAKETHIS with the article: accepted and posted to the board.
    peer.send("TAKETHIS <article-1@peer.example>").await;
    peer.send_article(&[
        "Newsgroups: rabbit.general",
        "Subject: Over the wire",
        "From: alice",
        "Message-ID: <article-1@peer.example>",
        "",
        "Hello from a peer feed.",
    ])
    .await;
    assert_eq!(peer.read_line().await, "239 <article-1@peer.example>");

    let threads = burrow
        .shared
        .boards
        .threads("rabbit.general", 100)
        .await
        .unwrap();
    assert_eq!(threads.len(), 1, "the transferred article is a board post");
    let (root, _, _) = &threads[0];
    assert_eq!(root.subject, "Over the wire");
    assert_eq!(root.body, "Hello from a peer feed.");
    assert_eq!(
        root.author, "alice@usenet",
        "gateway authorship, never @origin"
    );

    // Re-offering the same id is refused by the shared dedupe: CHECK says
    // don't send (438), a stubborn TAKETHIS is rejected (439), and the
    // classic IHAVE offer is not wanted (435).
    peer.send("CHECK <article-1@peer.example>").await;
    assert_eq!(peer.read_line().await, "438 <article-1@peer.example>");
    peer.send("TAKETHIS <article-1@peer.example>").await;
    peer.send_article(&[
        "Newsgroups: rabbit.general",
        "Subject: Over the wire",
        "",
        "Dupe.",
    ])
    .await;
    assert_eq!(peer.read_line().await, "439 <article-1@peer.example>");
    peer.send("IHAVE <article-1@peer.example>").await;
    assert!(peer.read_line().await.starts_with("435"), "IHAVE dupe");
    let threads = burrow
        .shared
        .boards
        .threads("rabbit.general", 100)
        .await
        .unwrap();
    assert_eq!(threads.len(), 1, "no duplicate post landed");

    // Classic IHAVE for a fresh id: 335 send-it, then 235 transferred.
    peer.send("IHAVE <article-2@peer.example>").await;
    assert!(peer.read_line().await.starts_with("335"));
    peer.send_article(&[
        "Newsgroups: rabbit.general",
        "Subject: Via IHAVE",
        "From: Bob Warren <bob@peer.example>",
        "",
        "Second article.",
    ])
    .await;
    assert!(peer.read_line().await.starts_with("235"));

    // A TAKETHIS whose article maps to no known group is rejected (439).
    peer.send("TAKETHIS <article-3@peer.example>").await;
    peer.send_article(&["Newsgroups: no.such.group", "Subject: Lost", "", "Body."])
        .await;
    assert_eq!(peer.read_line().await, "439 <article-3@peer.example>");

    // NEWNEWS since the epoch lists the message-ids of both landed posts.
    peer.send("NEWNEWS rabbit.* 19700101 000000 GMT").await;
    assert!(peer.read_line().await.starts_with("230"));
    let ids = peer.read_block().await;
    assert_eq!(ids.len(), 2, "both posts are listed: {ids:?}");
    let origin_tag = "@feed-warren>";
    assert!(
        ids.iter()
            .all(|l| l.starts_with('<') && l.ends_with(origin_tag)),
        "native message-ids: {ids:?}"
    );
    // A non-matching wildmat lists nothing.
    peer.send("NEWNEWS warren.* 19700101 000000 GMT").await;
    assert!(peer.read_line().await.starts_with("230"));
    assert!(peer.read_block().await.is_empty());

    peer.send("QUIT").await;
    assert!(peer.read_line().await.starts_with("205"));
    burrow.shutdown().await;
}

#[tokio::test]
async fn unauthenticated_transit_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(feed_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit.general", "General", "", 2, None, 0)
        .await
        .unwrap();

    let addr = burrow.nntp_feed_addr.expect("feed enabled");
    let mut peer = Peer::connect(addr).await;
    assert!(peer.read_line().await.starts_with("200"));

    // Every transit verb answers 480 before AUTHINFO.
    peer.send("MODE STREAM").await;
    assert!(peer.read_line().await.starts_with("480"));
    peer.send("CHECK <x@peer.example>").await;
    assert!(peer.read_line().await.starts_with("480"));
    peer.send("IHAVE <x@peer.example>").await;
    assert!(peer.read_line().await.starts_with("480"));
    peer.send("NEWNEWS * 19700101 000000 GMT").await;
    assert!(peer.read_line().await.starts_with("480"));
    // TAKETHIS still consumes its unconditional article body, then refuses —
    // and nothing lands on the board.
    peer.send("TAKETHIS <x@peer.example>").await;
    peer.send_article(&["Newsgroups: rabbit.general", "Subject: Nope", "", "Body."])
        .await;
    assert!(peer.read_line().await.starts_with("480"));
    assert!(burrow
        .shared
        .boards
        .threads("rabbit.general", 100)
        .await
        .unwrap()
        .is_empty());

    // Wrong password is rejected; reader verbs are not served here either.
    peer.send("AUTHINFO USER hub").await;
    assert!(peer.read_line().await.starts_with("381"));
    peer.send("AUTHINFO PASS wrong").await;
    assert!(peer.read_line().await.starts_with("481"));
    peer.send("GROUP rabbit.general").await;
    assert!(peer.read_line().await.starts_with("502"));

    // An unlisted user is refused even with some password.
    peer.send("AUTHINFO USER nobody").await;
    assert!(peer.read_line().await.starts_with("381"));
    peer.send("AUTHINFO PASS feed-pw").await;
    assert!(peer.read_line().await.starts_with("481"));

    peer.send("QUIT").await;
    assert!(peer.read_line().await.starts_with("205"));
    burrow.shutdown().await;
}

#[tokio::test]
async fn feed_off_by_default_and_empty_allowlist_refuses() {
    let work = tempfile::tempdir().unwrap();

    // Off by default.
    let cfg = ServerConfig {
        name: "Quiet Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv-a"),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    assert!(burrow.nntp_feed_addr.is_none(), "feed off by default");
    burrow.shutdown().await;

    // Enabled with an empty allowlist: every AUTHINFO is refused.
    let cfg = ServerConfig {
        name: "Locked Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_feed_enabled: true,
        nntp_feed_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv-b"),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    let mut peer = Peer::connect(burrow.nntp_feed_addr.unwrap()).await;
    assert!(peer.read_line().await.starts_with("200"));
    peer.send("AUTHINFO USER anyone").await;
    assert!(peer.read_line().await.starts_with("381"));
    peer.send("AUTHINFO PASS anything").await;
    assert!(
        peer.read_line().await.starts_with("481"),
        "empty allowlist refuses all peers"
    );
    peer.send("QUIT").await;
    assert!(peer.read_line().await.starts_with("205"));
    burrow.shutdown().await;
}
