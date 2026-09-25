//! Real TCP coverage for bounded HTTP/1.x reuse. The response reader obeys
//! Content-Length and HEAD framing instead of waiting for connection close.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use burrow::http::{MAX_CONNECTION_BYTES, MAX_CONNECTION_REQUESTS, MAX_HEAD_BYTES};
use burrow::Burrow;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn config(work: &std::path::Path) -> ServerConfig {
    let web = work.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), b"<html>shell</html>").unwrap();
    std::fs::write(web.join("app.js"), b"console.log('asset')").unwrap();
    ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        http_enabled: true,
        http_addr: "127.0.0.1:0".parse().unwrap(),
        http_web_root: web,
        data_dir: work.join("data"),
        // These tests isolate connection framing from rate budgets. Dedicated
        // tests below restore small budgets and prove both rate classes.
        ratelimit_conn_per_min: 6000,
        ratelimit_conn_burst: 1000,
        ratelimit_legacy_per_sec: 1000,
        ratelimit_legacy_burst: 1000,
        ..ServerConfig::default()
    }
}

struct Peer {
    socket: TcpStream,
    pending: Vec<u8>,
}

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Peer {
    async fn connect(addr: SocketAddr) -> Self {
        Self {
            socket: TcpStream::connect(addr).await.unwrap(),
            pending: Vec::new(),
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        tokio::time::timeout(Duration::from_secs(3), self.socket.write_all(bytes))
            .await
            .unwrap()
            .unwrap();
    }

    async fn read_more(&mut self) {
        let mut bytes = [0; 4096];
        let n = tokio::time::timeout(Duration::from_secs(3), self.socket.read(&mut bytes))
            .await
            .expect("response did not arrive")
            .unwrap();
        assert!(n > 0, "connection ended before complete response");
        self.pending.extend_from_slice(&bytes[..n]);
    }

    async fn reply(&mut self, head: bool) -> Reply {
        let end = loop {
            if let Some(end) = self.pending.windows(4).position(|w| w == b"\r\n\r\n") {
                break end;
            }
            self.read_more().await;
        };
        let text = std::str::from_utf8(&self.pending[..end]).unwrap();
        let mut lines = text.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: BTreeMap<String, String> = lines
            .map(|line| {
                let (key, value) = line.split_once(':').unwrap();
                (key.to_ascii_lowercase(), value.trim().to_string())
            })
            .collect();
        let len = if head {
            0
        } else {
            headers["content-length"].parse().unwrap()
        };
        let size = end + 4 + len;
        while self.pending.len() < size {
            self.read_more().await;
        }
        let body = self.pending[end + 4..size].to_vec();
        self.pending.drain(..size);
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn eof(&mut self) {
        assert!(
            self.pending.is_empty(),
            "unexpected trailing response bytes: {:?}",
            self.pending
        );
        let mut bytes = [0; 1];
        let n = tokio::time::timeout(Duration::from_secs(3), self.socket.read(&mut bytes))
            .await
            .expect("server should close promptly")
            .unwrap();
        assert_eq!(n, 0, "server executed an extra request");
    }
}

async fn tracked_download(b: &Burrow) -> i64 {
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    let bytes = b"download side effect";
    let id = b.shared.blobs.put(bytes).unwrap().0;
    b.shared
        .files
        .add_file(
            "public",
            None,
            "probe.txt",
            &id,
            bytes.len() as i64,
            "text/plain",
            "disk",
            "",
            "fixture",
            1,
        )
        .await
        .unwrap()
        .id
}

const INJECTED: &[u8] =
    b"GET /files/public/probe.txt HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn short_reads_and_sequential_get_head_reuse_one_socket() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    for part in [
        b"GE".as_slice(),
        b"T /lobby HTTP/1.1\r",
        b"\nHost: test\r\nContent-Length: 0\r\n",
        b"\r",
        b"\n",
    ] {
        peer.send(part).await;
        tokio::task::yield_now().await;
    }
    let first = peer.reply(false).await;
    assert_eq!(first.status, 200);
    assert_eq!(first.body, b"<html>shell</html>");
    assert_eq!(first.headers["connection"], "keep-alive");

    peer.send(b"HEAD /lobby HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    let head = peer.reply(true).await;
    assert_eq!(head.status, first.status);
    assert_eq!(head.headers, first.headers);
    assert!(head.body.is_empty());

    peer.send(b"GET /app.js HTTP/1.1\r\nHost: test\r\nConnection: CLOSE\r\n\r\n")
        .await;
    let asset = peer.reply(false).await;
    assert_eq!(asset.status, 200);
    assert_eq!(asset.headers["content-type"], "text/javascript");
    assert_eq!(asset.headers["connection"], "close");
    assert_eq!(asset.body, b"console.log('asset')");
    peer.eof().await;
    b.shutdown().await;
}

#[tokio::test]
async fn pipeline_preserves_order_and_head_does_not_consume_the_next_response() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    peer.send(b"HEAD /app.js HTTP/1.1\r\nHost: test\r\n\r\nGET /missing.css HTTP/1.1\r\nHost: test\r\n\r\nGET /boards/general HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").await;
    let first = peer.reply(true).await;
    assert_eq!(first.status, 200);
    assert_eq!(
        first.headers["content-length"],
        b"console.log('asset')".len().to_string()
    );
    let second = peer.reply(false).await;
    assert_eq!(second.status, 404);
    assert_eq!(second.headers["connection"], "keep-alive");
    let third = peer.reply(false).await;
    assert_eq!(third.status, 200);
    assert_eq!(third.body, b"<html>shell</html>");
    peer.eof().await;
    b.shutdown().await;
}

#[tokio::test]
async fn stored_mime_controls_cannot_split_get_head_or_the_next_response() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    let bytes = b"the real download body";
    let blob = b.shared.blobs.put(bytes).unwrap().0;
    for (index, mime) in [
        "text/plain\r\nContent-Length: 0\r\n\r\nHTTP/1.1 200 OK\r\nX-Injected: yes",
        "text/plain\nX-Injected: yes",
        "text/plain\0hidden",
        "text/plain\x7fhidden",
        "text/plain\thidden",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("untrusted-{index}.txt");
        let node = b
            .shared
            .files
            .add_file(
                "public",
                None,
                &name,
                &blob,
                bytes.len() as i64,
                mime,
                "disk",
                "",
                "fixture",
                1,
            )
            .await
            .unwrap();
        let mut peer = Peer::connect(b.http_addr.unwrap()).await;
        let pipeline = format!(
            "GET /files/public/{name} HTTP/1.1\r\nHost: test\r\n\r\nHEAD /files/public/{name} HTTP/1.1\r\nHost: test\r\n\r\nGET /app.js HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"
        );
        peer.send(pipeline.as_bytes()).await;
        let get = peer.reply(false).await;
        assert_eq!(get.status, 200);
        assert_eq!(get.headers["content-type"], "text/plain; charset=utf-8");
        assert_eq!(get.headers["content-length"], bytes.len().to_string());
        assert!(!get.headers.contains_key("x-injected"));
        assert_eq!(get.body, bytes);
        let head = peer.reply(true).await;
        assert_eq!(head.status, 200);
        assert_eq!(head.headers, get.headers);
        let next = peer.reply(false).await;
        assert_eq!(next.status, 200);
        assert_eq!(next.headers["content-type"], "text/javascript");
        assert_eq!(next.body, b"console.log('asset')");
        peer.eof().await;
        assert_eq!(
            b.shared
                .files
                .node(node.id)
                .await
                .unwrap()
                .unwrap()
                .downloads,
            1,
            "HEAD never counts a second download"
        );
    }
    b.shutdown().await;
}

#[tokio::test]
async fn explicit_close_and_http10_never_execute_a_pipelined_request() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let download = tracked_download(&b).await;
    for first in [
        "GET / HTTP/1.1\r\nHost: test\r\nConnection: keep-alive, Close\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: test\r\nConnection: keep-alive\r\nConnection: close\r\n\r\n",
        "GET / HTTP/1.0\r\n\r\n",
        "GET / HTTP/1.0\r\nHost: test\r\nConnection: keep-alive\r\n\r\n",
    ] {
        let mut peer = Peer::connect(b.http_addr.unwrap()).await;
        peer.send(&[first.as_bytes(), INJECTED].concat()).await;
        let reply = peer.reply(false).await;
        assert_eq!(reply.status, 200, "{first:?}");
        assert_eq!(reply.headers["connection"], "close");
        peer.eof().await;
    }
    assert_eq!(
        b.shared
            .files
            .node(download)
            .await
            .unwrap()
            .unwrap()
            .downloads,
        0
    );
    b.shutdown().await;
}

#[tokio::test]
async fn ambiguous_or_body_bearing_requests_close_without_executing_trailing_bytes() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let download = tracked_download(&b).await;
    for (first, status) in [
        ("GET / HTTP/1.1\r\nHost: test\r\nContent-Length: 1\r\n\r\nx", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nContent-Length: 0, 0\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nContent-Length: +0\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nContent-Length: 99999999999999999999999\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\nTransfer-Encoding: identity\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nHost: other\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: \r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost : test\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\n folded: header\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\nInjected: x\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nBad\0Name: x\r\n\r\n", 400),
        ("GET / HTTP/1.2\r\nHost: test\r\n\r\n", 400),
        ("GET / HTTP/1.1 extra\r\nHost: test\r\n\r\n", 400),
        ("GET  / HTTP/1.1\r\nHost: test\r\n\r\n", 400),
        ("GET /%2e%2e/secret HTTP/1.1\r\nHost: test\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: test\r\nExpect: 100-continue\r\n\r\n", 417),
        ("GET / HTTP/1.1\r\nHost: test\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n", 400),
        ("POST / HTTP/1.1\r\nHost: test\r\n\r\n", 405),
        ("HEAD / HTTP/1.1\r\nHost: test\r\nContent-Length: 1\r\n\r\nx", 400),
        ("HEAD /%2e%2e/secret HTTP/1.1\r\nHost: test\r\n\r\n", 400),
    ] {
        let mut peer = Peer::connect(b.http_addr.unwrap()).await;
        peer.send(&[first.as_bytes(), INJECTED].concat()).await;
        let reply = peer.reply(first.starts_with("HEAD ")).await;
        assert_eq!(reply.status, status, "{first:?}");
        assert_eq!(reply.headers["connection"], "close", "{first:?}");
        peer.eof().await;
    }
    assert_eq!(
        b.shared
            .files
            .node(download)
            .await
            .unwrap()
            .unwrap()
            .downloads,
        0
    );
    b.shutdown().await;
}

fn padded_head(size: usize) -> Vec<u8> {
    let prefix = b"GET / HTTP/1.1\r\nHost: test\r\nX-Padding: ";
    [
        prefix.as_slice(),
        &vec![b'x'; size - prefix.len() - 4],
        b"\r\n\r\n",
    ]
    .concat()
}

#[tokio::test]
async fn per_head_and_total_byte_limits_bound_pipelined_input() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let download = tracked_download(&b).await;
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    peer.send(&[padded_head(MAX_HEAD_BYTES + 1), INJECTED.to_vec()].concat())
        .await;
    assert_eq!(peer.reply(false).await.status, 400);
    peer.eof().await;

    // Exact per-head and connection-byte boundaries are accepted, with the
    // last accepted response explicitly closing before the injected request.
    assert_eq!(MAX_CONNECTION_BYTES % MAX_HEAD_BYTES, 0);
    let count = MAX_CONNECTION_BYTES / MAX_HEAD_BYTES;
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    let mut pipeline = padded_head(MAX_HEAD_BYTES).repeat(count);
    pipeline.extend_from_slice(INJECTED);
    peer.send(&pipeline).await;
    for index in 0..count {
        let reply = peer.reply(false).await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.headers["connection"],
            if index + 1 == count {
                "close"
            } else {
                "keep-alive"
            }
        );
    }
    peer.eof().await;
    assert_eq!(
        b.shared
            .files
            .node(download)
            .await
            .unwrap()
            .unwrap()
            .downloads,
        0
    );
    b.shutdown().await;
}

#[tokio::test]
async fn request_count_limit_closes_after_last_accepted_request() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let download = tracked_download(&b).await;
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    let mut pipeline = b"GET / HTTP/1.1\r\nHost: test\r\n\r\n".repeat(MAX_CONNECTION_REQUESTS);
    pipeline.extend_from_slice(INJECTED);
    peer.send(&pipeline).await;
    for index in 0..MAX_CONNECTION_REQUESTS {
        let reply = peer.reply(false).await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.headers["connection"],
            if index + 1 == MAX_CONNECTION_REQUESTS {
                "close"
            } else {
                "keep-alive"
            }
        );
    }
    peer.eof().await;
    assert_eq!(
        b.shared
            .files
            .node(download)
            .await
            .unwrap()
            .unwrap()
            .downloads,
        0
    );
    b.shutdown().await;
}

#[tokio::test]
async fn reused_requests_spend_the_request_budget_and_head_429_has_no_body() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    peer.send(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n").await;
    assert_eq!(peer.reply(false).await.status, 200);
    // A zero-capacity live policy deterministically refuses the next request,
    // without depending on how many milliseconds the previous request took.
    b.shared.config.update(|c| c.ratelimit_legacy_burst = 0);
    peer.send(b"HEAD / HTTP/1.1\r\nHost: test\r\n\r\nGET / HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    let reply = peer.reply(true).await;
    assert_eq!(reply.status, 429);
    assert_eq!(reply.headers["connection"], "close");
    assert!(reply.headers["content-length"].parse::<usize>().unwrap() > 0);
    peer.eof().await;
    b.shutdown().await;
}

#[tokio::test]
async fn connection_budget_is_spent_at_accept_not_on_reused_requests() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = config(work.path());
    cfg.ratelimit_conn_per_min = 1;
    cfg.ratelimit_conn_burst = 1;
    let b = Burrow::start(cfg).await.unwrap();
    let addr = b.http_addr.unwrap();
    let mut peer = Peer::connect(addr).await;
    for _ in 0..3 {
        peer.send(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n").await;
        assert_eq!(peer.reply(false).await.status, 200);
    }
    let mut refused = TcpStream::connect(addr).await.unwrap();
    let _ = refused
        .write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    let mut bytes = [0; 1];
    let closed = tokio::time::timeout(Duration::from_secs(3), refused.read(&mut bytes))
        .await
        .unwrap();
    assert!(
        matches!(closed, Ok(0)) || closed.is_err(),
        "extra accepted connection must not answer"
    );
    peer.send(b"GET / HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await;
    assert_eq!(peer.reply(false).await.status, 200);
    peer.eof().await;
    b.shutdown().await;
}

#[tokio::test]
async fn clean_and_partial_disconnects_do_not_damage_other_connections() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let addr = b.http_addr.unwrap();
    let mut clean = Peer::connect(addr).await;
    clean.socket.shutdown().await.unwrap();
    clean.eof().await;

    let mut partial = Peer::connect(addr).await;
    partial.send(b"HEAD / HTTP/1.1\r\nHost: unfinished").await;
    partial.socket.shutdown().await.unwrap();
    assert_eq!(partial.reply(true).await.status, 400);
    partial.eof().await;

    let mut half_closed = Peer::connect(addr).await;
    half_closed
        .send(b"GET / HTTP/1.1\r\nHost: test\r\n\r\nHEAD /app.js HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    half_closed.socket.shutdown().await.unwrap();
    assert_eq!(half_closed.reply(false).await.status, 200);
    assert_eq!(half_closed.reply(true).await.status, 200);
    half_closed.eof().await;

    let mut abandoned = Peer::connect(addr).await;
    abandoned
        .send(b"GET /app.js HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    drop(abandoned);
    let mut healthy = Peer::connect(addr).await;
    healthy
        .send(b"GET /lobby HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await;
    assert_eq!(healthy.reply(false).await.status, 200);
    healthy.eof().await;
    b.shutdown().await;
}
