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

/// One response per fetch, including an empty response for a transport
/// failure. Request counts prove scheduling without negative wall-clock waits.
async fn spawn_scripted_feed(
    log: Arc<Mutex<Vec<String>>>,
    responses: Vec<String>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut tmp).await.unwrap();
                assert!(n > 0, "complete request header");
                buf.extend_from_slice(&tmp[..n]);
                assert!(buf.len() < 16_384);
            }
            log.lock().unwrap().push(String::from_utf8(buf).unwrap());
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        }
    });
    (addr, task)
}

fn feed_response(body: &str, etag: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nETag: \"{etag}\"\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

async fn assert_next_fetch(
    svc: &mut SyndicationService,
    log: &Mutex<Vec<String>>,
    due: i64,
    previous_requests: usize,
) {
    assert_eq!(svc.poll_due(due - 1).await, 0);
    assert_eq!(log.lock().unwrap().len(), previous_requests, "not due yet");
    assert_eq!(svc.poll_due(due).await, 0);
    assert_eq!(log.lock().unwrap().len(), previous_requests + 1, "due now");
    assert_eq!(svc.poll_due(due).await, 0);
    assert_eq!(
        log.lock().unwrap().len(),
        previous_requests + 1,
        "no busy loop"
    );
}

#[tokio::test]
async fn publisher_intervals_survive_304_and_failures_then_refresh() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ttl_rss = RSS.replace("<channel>", "<channel><ttl>120</ttl>");
    let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"
        xmlns:poll="http://purl.org/rss/1.0/modules/syndication/">
        <poll:updatePeriod>daily</poll:updatePeriod>
        <poll:updateFrequency>4</poll:updateFrequency></feed>"#;
    let responses = vec![
        feed_response(&ttl_rss, "v1"),
        "HTTP/1.1 304 Not Modified\r\n\r\n".into(),
        "HTTP/1.1 503 Unavailable\r\nETag: \"error\"\r\nContent-Length: 0\r\n\r\n".into(),
        String::new(),
        feed_response(atom, "v2"),
        feed_response("<html>upstream failure</html>", "invalid"),
        feed_response(RSS, "v3"),
        "HTTP/1.1 304 Not Modified\r\n\r\n".into(),
    ];
    let (addr, server) = spawn_scripted_feed(log.clone(), responses).await;
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(syn_config(
        work.path(),
        &format!("http://{addr}/feed"),
        false,
    ))
    .await
    .unwrap();
    burrow
        .shared
        .boards
        .create_board("news", "News", "", 2, None, 0)
        .await
        .unwrap();
    let mut svc = SyndicationService::new(burrow.shared.clone(), work.path().join("state"))
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    assert_eq!(svc.poll_due(now).await, 2);
    assert_eq!(log.lock().unwrap().len(), 1);

    // 7200-second TTL survives 304, HTTP failure (2x), and transport
    // failure (4x). New Atom metadata resets backoff at 21600 seconds;
    // an unparseable 200 retains that hint and the last good validators.
    for (previous, offset) in [7_200, 14_400, 28_800, 57_600, 79_200, 122_400, 124_200]
        .into_iter()
        .enumerate()
    {
        assert_next_fetch(&mut svc, &log, now + offset, previous + 1).await;
    }
    let heads = log.lock().unwrap().clone();
    assert!(!heads[0].to_ascii_lowercase().contains("if-none-match"));
    for head in &heads[1..5] {
        assert!(head.to_ascii_lowercase().contains("if-none-match: \"v1\""));
    }
    for head in &heads[5..7] {
        assert!(head.to_ascii_lowercase().contains("if-none-match: \"v2\""));
    }
    assert!(heads[7]
        .to_ascii_lowercase()
        .contains("if-none-match: \"v3\""));
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
    server.await.unwrap();
    burrow.shutdown().await;
}

#[tokio::test]
async fn bounded_hints_and_live_operator_interval_preserve_existing_deadlines() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let fast = r#"<feed xmlns:sy="http://purl.org/rss/1.0/modules/syndication/">
        <sy:updatePeriod>hourly</sy:updatePeriod>
        <sy:updateFrequency>4294967295</sy:updateFrequency></feed>"#;
    let huge = "<rss><channel><ttl>9223372036854775807</ttl></channel></rss>";
    let invalid = r#"<rss xmlns:sy="http://purl.org/rss/1.0/modules/syndication/"><channel>
        <ttl>-9</ttl><sy:updatePeriod>hourly</sy:updatePeriod>
        <sy:updateFrequency>0</sy:updateFrequency></channel></rss>"#;
    let (addr, server) = spawn_scripted_feed(
        log.clone(),
        vec![
            feed_response(fast, "fast"),
            "HTTP/1.1 304 Not Modified\r\n\r\n".into(),
            feed_response(huge, "huge"),
            feed_response(invalid, "invalid-hints"),
            "HTTP/1.1 304 Not Modified\r\n\r\n".into(),
        ],
    )
    .await;
    let work = tempfile::tempdir().unwrap();
    let mut cfg = syn_config(work.path(), &format!("http://{addr}/feed"), false);
    cfg.syndication_poll_secs = 1;
    let burrow = Burrow::start(cfg).await.unwrap();
    let mut svc = SyndicationService::new(burrow.shared.clone(), work.path().join("state"))
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    assert_eq!(svc.poll_due(now).await, 0);
    assert_eq!(log.lock().unwrap().len(), 1);

    // Raising the live operator base leaves the first 300-second deadline
    // intact, then the 304 uses the new base, which dominates the feed hint.
    burrow
        .shared
        .config
        .set_key("syndication_poll_secs", "10000")
        .unwrap();
    assert_next_fetch(&mut svc, &log, now + 300, 1).await;
    burrow
        .shared
        .config
        .set_key("syndication_poll_secs", "1")
        .unwrap();
    assert_next_fetch(&mut svc, &log, now + 10_300, 2).await;
    // Extreme TTL is capped at one day. Malformed hints on a fresh parsed
    // feed clear the old hint and return to the five-minute floor, also on 304.
    assert_next_fetch(&mut svc, &log, now + 96_700, 3).await;
    assert_next_fetch(&mut svc, &log, now + 97_000, 4).await;
    assert_eq!(svc.poll_due(now + 97_299).await, 0);
    assert_eq!(log.lock().unwrap().len(), 5);
    server.await.unwrap();
    burrow.shutdown().await;
}
