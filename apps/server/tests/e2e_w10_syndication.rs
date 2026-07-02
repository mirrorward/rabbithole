//! Wave 10 end-to-end tests: the RSS/Atom syndication service wired into
//! `burrow`. The parser, mapping, seen-set, and poll state machine are
//! unit-tested in `rabbithole-legacy-syndication`; here we prove the server
//! glue: a real HTTP fetch loop against a local canned feed server (200 with
//! validators, then 304), redirect following, items landing on a real board
//! exactly once (no dupes on re-poll, no dupes across a service restart), and
//! that the whole surface stays off by default.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use burrow::syndication::SyndicationService;
use burrow::Burrow;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RSS: &str = r#"<rss version="2.0">
  <channel>
    <title>The Warren Wire</title>
    <link>https://warren.example/</link>
    <description>news</description>
    <item>
      <title>Burrow 1.0 released</title>
      <link>https://warren.example/1</link>
      <guid>urn:warren:1</guid>
      <description>Down the hole we go.</description>
    </item>
    <item>
      <title>Carrots up 40%</title>
      <link>https://warren.example/2</link>
      <guid>urn:warren:2</guid>
      <description>Market report.</description>
    </item>
  </channel>
</rss>"#;

/// Canned feed HTTP server: `/feed.xml` 301-redirects to `/real.xml`, which
/// serves the RSS with `ETag: "v1"` — or `304` when the client replays the
/// validator. Every request head is appended to `log`.
async fn spawn_feed_server(log: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _peer)) = listener.accept().await else {
                break;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                log.lock().unwrap().push(head.clone());
                let _ = sock.write_all(respond(&head).as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    addr
}

fn respond(head: &str) -> String {
    let path = head.split_whitespace().nth(1).unwrap_or("/");
    if path == "/feed.xml" {
        return "HTTP/1.1 301 Moved Permanently\r\nLocation: /real.xml\r\nContent-Length: 0\r\n\r\n"
            .to_string();
    }
    if head.to_ascii_lowercase().contains("if-none-match: \"v1\"") {
        return "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\n\r\n".to_string();
    }
    format!(
        "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\n\r\n{}",
        RSS.len(),
        RSS
    )
}

/// A test config mapping the canned server's redirecting URL onto the `news`
/// board. `enabled` gates whether *burrow itself* spawns the background task.
fn syn_config(dir: &std::path::Path, feed_url: &str, enabled: bool) -> ServerConfig {
    let mut feeds = HashMap::new();
    feeds.insert(feed_url.to_string(), "news".to_string());
    ServerConfig {
        name: "Feed Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        syndication_enabled: enabled,
        syndication_feeds: feeds,
        data_dir: dir.join("srv"),
        ..ServerConfig::default()
    }
}

async fn wait_for_threads(shared: &burrow::Shared, slug: &str, want: usize) -> bool {
    for _ in 0..100 {
        if let Ok(threads) = shared.boards.threads(slug, 100).await {
            if threads.len() >= want {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn syndication_off_by_default() {
    let cfg = ServerConfig::default();
    assert!(!cfg.syndication_enabled, "must be opt-in");
    assert!(cfg.syndication_feeds.is_empty());
    assert_eq!(cfg.syndication_poll_secs, 1800);

    // A burrow with feeds configured but the switch off boots cleanly and
    // fetches nothing (the canned server sees zero requests).
    let log = Arc::new(Mutex::new(Vec::new()));
    let addr = spawn_feed_server(log.clone()).await;
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(syn_config(
        work.path(),
        &format!("http://{addr}/feed.xml"),
        false,
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(log.lock().unwrap().is_empty(), "no fetch while disabled");
    burrow.shutdown().await;
}

#[tokio::test]
async fn feed_items_post_once_and_repolls_dedupe() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let addr = spawn_feed_server(log.clone()).await;
    let url = format!("http://{addr}/feed.xml");
    let work = tempfile::tempdir().unwrap();

    // Keep burrow's own background task off: the test drives the service
    // directly with a deterministic clock.
    let burrow = Burrow::start(syn_config(work.path(), &url, false))
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("news", "News", "", 2, None, 0)
        .await
        .unwrap();

    let state_dir = work.path().join("synd-state");
    let mut svc = SyndicationService::new(burrow.shared.clone(), state_dir.clone())
        .await
        .unwrap();
    assert_eq!(svc.feed_count(), 1);

    // First poll: 301 → 200 body; both items land on the board.
    let now = chrono::Utc::now().timestamp();
    let posted = svc.poll_due(now).await;
    assert_eq!(posted, 2, "both fresh items posted");
    let threads = burrow.shared.boards.threads("news", 100).await.unwrap();
    assert_eq!(threads.len(), 2);
    let mut subjects: Vec<&str> = threads.iter().map(|t| t.0.subject.as_str()).collect();
    subjects.sort();
    assert_eq!(subjects, ["Burrow 1.0 released", "Carrots up 40%"]);
    assert!(threads.iter().all(|t| t.0.author.ends_with("@rss")));
    assert!(
        threads[0]
            .0
            .body
            .contains("Source: https://warren.example/"),
        "source link appended: {}",
        threads[0].0.body
    );
    {
        let heads = log.lock().unwrap();
        assert_eq!(heads.len(), 2, "redirect hop then the real fetch");
        assert!(heads[0].starts_with("GET /feed.xml HTTP/1.1\r\n"));
        assert!(heads[1].starts_with("GET /real.xml HTTP/1.1\r\n"));
        assert!(!heads[1].to_ascii_lowercase().contains("if-none-match"));
    }

    // Not due yet: nothing happens before the scheduled next poll.
    assert_eq!(svc.poll_due(now + 1).await, 0);
    assert_eq!(log.lock().unwrap().len(), 2);

    // Second poll (due): the stored ETag is replayed, the server answers 304,
    // and the board stays at two threads.
    let posted = svc.poll_due(now + 100_000).await;
    assert_eq!(posted, 0, "304 posts nothing");
    {
        let heads = log.lock().unwrap();
        assert_eq!(heads.len(), 4);
        assert!(
            heads[3]
                .to_ascii_lowercase()
                .contains("if-none-match: \"v1\""),
            "conditional GET replayed the validator: {}",
            heads[3]
        );
    }
    assert_eq!(
        burrow
            .shared
            .boards
            .threads("news", 100)
            .await
            .unwrap()
            .len(),
        2,
        "no dupes on re-poll"
    );

    // Service restart: validators are gone (a fresh 200 with the same body),
    // but the durable seen file still suppresses every item.
    let mut svc2 = SyndicationService::new(burrow.shared.clone(), state_dir)
        .await
        .unwrap();
    let posted = svc2.poll_due(chrono::Utc::now().timestamp()).await;
    assert_eq!(posted, 0, "durable seen-set survives a restart");
    assert_eq!(
        burrow
            .shared
            .boards
            .threads("news", 100)
            .await
            .unwrap()
            .len(),
        2
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn enabled_burrow_polls_in_the_background() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let addr = spawn_feed_server(log.clone()).await;
    let url = format!("http://{addr}/real.xml");
    let work = tempfile::tempdir().unwrap();

    // Boot once (ingest off) to create the target board, then reboot with
    // syndication enabled so the very first background poll finds it.
    let first = Burrow::start(syn_config(work.path(), &url, false))
        .await
        .unwrap();
    first
        .shared
        .boards
        .create_board("news", "News", "", 2, None, 0)
        .await
        .unwrap();
    first.shutdown().await;

    let burrow = Burrow::start(syn_config(work.path(), &url, true))
        .await
        .unwrap();
    assert!(
        wait_for_threads(&burrow.shared, "news", 2).await,
        "background poller posted the feed items"
    );
    burrow.shutdown().await;
}
