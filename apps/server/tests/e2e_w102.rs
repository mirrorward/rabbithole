//! Wave 10.2 end-to-end test: the NNTP reader/poster gateway wired into
//! `burrow`. The NNTP wire codec is unit-tested in its own crate; here we prove
//! burrow binds the listener, projects a board as a newsgroup, serves articles
//! and overviews, and — once authenticated — accepts a `POST` that lands as a
//! real board post.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "News Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_enabled: true,
        nntp_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// A minimal scripted NNTP client over the wire.
struct NntpClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl NntpClient {
    async fn connect(addr: std::net::SocketAddr) -> NntpClient {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (r, w) = stream.into_split();
        NntpClient {
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
}

#[tokio::test]
async fn nntp_gateway_reads_and_posts() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();

    // An account for AUTHINFO, and a postable board with one seeded post.
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit.general", "General", "General chatter", 2, None, 0)
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .post(
            "rabbit.general",
            None,
            "alice@news-warren",
            &[7u8; 32],
            "First light",
            "The warren wakes.",
            "text/plain",
            1_700_000_000_000,
        )
        .await
        .unwrap();

    let addr = burrow.nntp_addr.expect("nntp enabled");
    let mut client = NntpClient::connect(addr).await;

    // Greeting.
    let greeting = client.read_line().await;
    assert!(greeting.starts_with("200"), "greeting: {greeting:?}");

    // CAPABILITIES.
    client.send("CAPABILITIES").await;
    assert!(client.read_line().await.starts_with("101"));
    let caps = client.read_block().await;
    assert!(
        caps.iter().any(|l| l.contains("VERSION 2")),
        "caps: {caps:?}"
    );
    assert!(caps.iter().any(|l| l == "POST"), "caps advertise POST");
    assert!(caps.iter().any(|l| l == "READER"), "caps advertise READER");

    // MODE READER + DATE.
    client.send("MODE READER").await;
    assert!(client.read_line().await.starts_with("200"));
    client.send("DATE").await;
    assert!(client.read_line().await.starts_with("111"));

    // LIST ACTIVE — our board appears as a newsgroup.
    client.send("LIST").await;
    assert!(client.read_line().await.starts_with("215"));
    let groups = client.read_block().await;
    assert!(
        groups.iter().any(|l| l.starts_with("rabbit.general ")),
        "LIST includes the group: {groups:?}"
    );

    // GROUP selects it and reports one article.
    client.send("GROUP rabbit.general").await;
    let sel = client.read_line().await;
    assert!(sel.starts_with("211"), "group selected: {sel:?}");
    // "211 <count> <low> <high> <group>"
    let fields: Vec<&str> = sel.split_whitespace().collect();
    assert_eq!(fields[1], "1", "one article in group: {sel:?}");

    // ARTICLE 1 renders a netnews article.
    client.send("ARTICLE 1").await;
    let art = client.read_line().await;
    assert!(art.starts_with("220"), "article follows: {art:?}");
    let body = client.read_block().await;
    assert!(
        body.iter().any(|l| l.starts_with("Subject: First light")),
        "subject header present: {body:?}"
    );
    assert!(
        body.iter().any(|l| l.starts_with("Message-ID: <")),
        "message-id present: {body:?}"
    );
    assert!(
        body.iter().any(|l| l == "The warren wakes."),
        "body present: {body:?}"
    );

    // OVER 1 returns an overview record.
    client.send("OVER 1").await;
    assert!(client.read_line().await.starts_with("224"));
    let over = client.read_block().await;
    assert_eq!(over.len(), 1, "one overview line: {over:?}");
    assert!(
        over[0].contains("First light"),
        "overview names the article: {over:?}"
    );

    // Posting is refused before authentication.
    client.send("POST").await;
    assert!(
        client.read_line().await.starts_with("480"),
        "post refused until auth"
    );

    // AUTHINFO USER/PASS.
    client.send("AUTHINFO USER alice").await;
    assert!(client.read_line().await.starts_with("381"));
    client.send("AUTHINFO PASS pw-pw-pw").await;
    assert!(client.read_line().await.starts_with("281"));

    // POST a new article.
    client.send("POST").await;
    assert!(client.read_line().await.starts_with("340"));
    client
        .writer
        .write_all(
            b"Newsgroups: rabbit.general\r\n\
              Subject: Via NNTP\r\n\
              From: alice\r\n\
              \r\n\
              Hello from the newsreader.\r\n\
              .\r\n",
        )
        .await
        .unwrap();
    let posted = client.read_line().await;
    assert!(posted.starts_with("240"), "post accepted: {posted:?}");

    client.send("QUIT").await;
    assert!(client.read_line().await.starts_with("205"));

    // The article landed as a real board post, authored by the authed persona.
    let threads = burrow
        .shared
        .boards
        .threads("rabbit.general", 100)
        .await
        .unwrap();
    assert_eq!(threads.len(), 2, "seeded post + NNTP post");
    let posted = threads
        .iter()
        .map(|(root, _, _)| root)
        .find(|p| p.subject == "Via NNTP")
        .expect("NNTP post is a board post");
    assert_eq!(posted.body, "Hello from the newsreader.");
    assert!(
        posted.author.starts_with("alice@"),
        "authored by the authed persona: {:?}",
        posted.author
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn nntp_off_by_default() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        name: "Quiet Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    assert!(burrow.nntp_addr.is_none(), "nntp off by default");
    burrow.shutdown().await;
}
