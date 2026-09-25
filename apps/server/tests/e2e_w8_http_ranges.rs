//! Byte-exact HTTP resume and range failures over the real embedded listener.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_blobs::BlobId;
use rabbithole_proto::admin::subject_kind;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn config(work: &std::path::Path) -> ServerConfig {
    let web = work.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), b"<html>shell</html>").unwrap();
    std::fs::write(web.join("asset.bin"), b"0123456789").unwrap();
    ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        http_enabled: true,
        http_addr: "127.0.0.1:0".parse().unwrap(),
        http_web_root: web,
        data_dir: work.join("data"),
        ratelimit_conn_per_min: 6000,
        ratelimit_conn_burst: 1000,
        ratelimit_legacy_per_sec: 1000,
        ratelimit_legacy_burst: 1000,
        ..ServerConfig::default()
    }
}

async fn add_file(b: &Burrow, folder: Option<&str>, name: &str, bytes: &[u8]) -> (i64, BlobId) {
    let blob = b.shared.blobs.put(bytes).unwrap();
    let file = b
        .shared
        .files
        .add_file(
            "public",
            folder,
            name,
            &blob.0,
            bytes.len() as i64,
            "application/octet-stream",
            "disk",
            "",
            "fixture",
            1,
        )
        .await
        .unwrap();
    (file.id, blob)
}

async fn downloads(b: &Burrow, id: i64) -> i64 {
    b.shared.files.node(id).await.unwrap().unwrap().downloads
}

struct Peer {
    socket: TcpStream,
    pending: Vec<u8>,
}

#[derive(Debug)]
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
        let count = tokio::time::timeout(Duration::from_secs(3), self.socket.read(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0, "connection ended before a complete response");
        self.pending.extend_from_slice(&bytes[..count]);
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
        assert!(self.pending.is_empty(), "trailing response bytes");
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), self.socket.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}

async fn request(addr: SocketAddr, method: &str, path: &str, headers: &str) -> Reply {
    let mut peer = Peer::connect(addr).await;
    peer.send(
        format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{headers}\r\n")
            .as_bytes(),
    )
    .await;
    let response = peer.reply(method == "HEAD").await;
    peer.eof().await;
    response
}

#[tokio::test]
async fn full_partial_open_and_suffix_downloads_resume_to_the_original_bytes() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    let original: Vec<u8> = (0..1024).map(|n| (n % 256) as u8).collect();
    let (id, _) = add_file(&b, None, "resume.bin", &original).await;
    let path = "/files/public/resume.bin";
    let addr = b.http_addr.unwrap();
    let full = request(addr, "GET", path, "").await;
    assert_eq!(full.status, 200);
    assert_eq!(full.body, original);
    assert_eq!(full.headers["accept-ranges"], "bytes");
    assert_eq!(full.headers["content-length"], "1024");
    assert!(!full.headers.contains_key("content-range"));
    assert_eq!(
        full.headers["content-disposition"],
        "attachment; filename=\"resume.bin\""
    );

    let prefix = request(addr, "GET", path, "Range: bytes=0-127\r\n").await;
    assert_eq!(prefix.status, 206);
    assert_eq!(prefix.headers["content-range"], "bytes 0-127/1024");
    assert_eq!(prefix.headers["content-length"], "128");
    let rest = request(addr, "GET", path, "Range: bytes=128-\r\n").await;
    assert_eq!(rest.status, 206);
    assert_eq!(rest.headers["content-range"], "bytes 128-1023/1024");
    assert_eq!(
        [prefix.body, rest.body].concat(),
        original,
        "resume reconstructs every binary byte"
    );

    for (spec, start, end) in [
        ("bytes=4-9", 4, 10),
        ("bytes=1000-9999", 1000, 1024),
        ("bytes=-10", 1014, 1024),
        ("bytes=-9999", 0, 1024),
        ("BYTES= 0000-0000", 0, 1),
        ("bytes=1000-18446744073709551615", 1000, 1024),
        ("bytes=-18446744073709551615", 0, 1024),
    ] {
        let reply = request(addr, "GET", path, &format!("Range: {spec}\r\n")).await;
        assert_eq!(reply.status, 206, "{spec}");
        assert_eq!(reply.body, original[start..end], "{spec}");
        assert_eq!(
            reply.headers["content-range"],
            format!("bytes {start}-{}/1024", end - 1)
        );
        assert_eq!(reply.headers["content-length"], (end - start).to_string());
    }
    assert_eq!(
        downloads(&b, id).await,
        10,
        "each full or partial GET counts once"
    );
    b.shutdown().await;
}

#[tokio::test]
async fn unsatisfiable_and_rejected_ranges_never_count_as_downloads() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    let (id, _) = add_file(&b, None, "tiny.bin", b"abc").await;
    let addr = b.http_addr.unwrap();
    let path = "/files/public/tiny.bin";
    for value in [
        "bytes=3-",
        "bytes=99-100",
        "bytes=-0",
        "bytes=18446744073709551615-",
    ] {
        let reply = request(addr, "GET", path, &format!("Range: {value}\r\n")).await;
        assert_eq!(reply.status, 416, "{value}");
        assert_eq!(reply.headers["content-range"], "bytes */3");
        assert_eq!(reply.headers["accept-ranges"], "bytes");
        assert_eq!(
            reply.headers["content-length"],
            reply.body.len().to_string()
        );
    }
    for headers in [
        "Range: bytes=\r\n".to_string(),
        "Range: bytes=-\r\n".to_string(),
        "Range: bytes=2-1\r\n".to_string(),
        "Range: bytes=+0-1\r\n".to_string(),
        "Range: bytes=0 - 1\r\n".to_string(),
        "Range: bytes=0-1-2\r\n".to_string(),
        "Range: bytes=18446744073709551616-\r\n".to_string(),
        "Range: bytes=0-18446744073709551616\r\n".to_string(),
        "Range: bytes=-18446744073709551616\r\n".to_string(),
        "Range: bytes=0-0,2-2\r\n".to_string(),
        "Range: bytes=0-0\r\nRange: bytes=2-2\r\n".to_string(),
        format!("Range: bytes={}-\r\n", "0".repeat(256)),
    ] {
        let reply = request(addr, "GET", path, &headers).await;
        assert_eq!(reply.status, 400, "{headers}");
        assert!(!reply.headers.contains_key("content-range"));
        assert_eq!(
            reply.headers["content-length"],
            reply.body.len().to_string()
        );
    }
    assert_eq!(downloads(&b, id).await, 0);
    b.shutdown().await;
}

#[tokio::test]
async fn head_unknown_units_if_range_and_empty_files_use_full_responses() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    let (id, _) = add_file(&b, None, "data.bin", b"abcdef").await;
    let (empty_id, _) = add_file(&b, None, "empty.bin", b"").await;
    let addr = b.http_addr.unwrap();
    let path = "/files/public/data.bin";
    let full = request(addr, "HEAD", path, "").await;
    for headers in [
        "Range: bytes=1-2\r\n",
        "Range: bytes=999-\r\n",
        "Range: bytes=bad\r\n",
        "Range: bytes=0-0,2-2\r\n",
        "Range: bytes=0-0\r\nRange: bytes=1-1\r\n",
    ] {
        let head = request(addr, "HEAD", path, headers).await;
        assert_eq!(head.status, 200);
        assert_eq!(head.headers, full.headers, "HEAD ignores {headers}");
        assert!(head.body.is_empty());
    }
    assert_eq!(downloads(&b, id).await, 0);
    for headers in [
        "Range: widgets=1-2\r\n",
        "Range: bytes=1-2\r\nIf-Range: \"unknown-etag\"\r\n",
        "Range: bytes=1-2\r\nIf-Range: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
    ] {
        let reply = request(addr, "GET", path, headers).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"abcdef");
        assert!(!reply.headers.contains_key("content-range"));
    }
    for spec in [
        "bytes=0-0",
        "bytes=-1",
        "bytes=-0",
        "bytes=999-",
        "bytes=bad",
    ] {
        let reply = request(
            addr,
            "GET",
            "/files/public/empty.bin",
            &format!("Range: {spec}\r\n"),
        )
        .await;
        assert_eq!(reply.status, 200, "empty representation ignores {spec}");
        assert_eq!(reply.headers["content-length"], "0");
        assert!(reply.body.is_empty());
        assert!(!reply.headers.contains_key("content-range"));
    }
    assert_eq!(downloads(&b, id).await, 3);
    assert_eq!(downloads(&b, empty_id).await, 5);
    b.shutdown().await;
}

#[tokio::test]
async fn file_ranges_leave_spa_generated_documents_and_missing_assets_unchanged() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    let addr = b.http_addr.unwrap();
    let partial = request(addr, "GET", "/asset.bin", "Range: bytes=3-6\r\n").await;
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, b"3456");
    assert_eq!(partial.headers["content-range"], "bytes 3-6/10");
    let head = request(addr, "HEAD", "/asset.bin", "Range: bytes=3-6\r\n").await;
    assert_eq!(head.status, 200);
    assert_eq!(head.headers["content-length"], "10");
    for path in [
        "/",
        "/lobby",
        "/manifest.webmanifest",
        "/.well-known/rabbithole/server",
    ] {
        let reply = request(addr, "GET", path, "Range: bytes=0-0,3-3\r\n").await;
        assert_eq!(reply.status, 200, "{path} ignores Range");
        assert!(!reply.headers.contains_key("content-range"));
        assert!(!reply.headers.contains_key("accept-ranges"));
        assert!(reply.body.len() > 1);
    }
    let missing = request(addr, "GET", "/missing.bin", "Range: bytes=999-\r\n").await;
    assert_eq!(missing.status, 404);
    assert!(!missing.headers.contains_key("content-range"));
    b.shutdown().await;
}

#[tokio::test]
async fn ranged_get_full_get_head_and_errors_keep_pipeline_boundaries() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(work.path())).await.unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    let (id, _) = add_file(&b, None, "data.bin", b"abcdef").await;
    let mut peer = Peer::connect(b.http_addr.unwrap()).await;
    peer.send(b"GET /files/public/data.bin HTTP/1.1\r\nHost: test\r\nRange: bytes=2-4\r\n\r\nGET /files/public/data.bin HTTP/1.1\r\nHost: test\r\n\r\nHEAD /files/public/data.bin HTTP/1.1\r\nHost: test\r\nRange: bytes=99-\r\n\r\nGET /asset.bin HTTP/1.1\r\nHost: test\r\nRange: bytes=99-\r\n\r\nGET /asset.bin HTTP/1.1\r\nHost: test\r\nRange: bytes=bad\r\n\r\nGET /asset.bin HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").await;
    let part = peer.reply(false).await;
    assert_eq!(part.status, 206);
    assert_eq!(part.body, b"cde");
    assert_eq!(peer.reply(false).await.body, b"abcdef");
    let head = peer.reply(true).await;
    assert_eq!(head.status, 200);
    assert_eq!(head.headers["content-length"], "6");
    assert_eq!(peer.reply(false).await.status, 416);
    assert_eq!(peer.reply(false).await.status, 400);
    let last = peer.reply(false).await;
    assert_eq!(last.status, 200);
    assert_eq!(last.body, b"0123456789");
    peer.eof().await;
    assert_eq!(downloads(&b, id).await, 2);
    b.shutdown().await;
}

#[tokio::test]
async fn ranges_never_reveal_hidden_file_lengths_or_bypass_blob_verification() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = config(work.path());
    // Runtime checks must hold even when startup config puts the private data
    // under the web root rather than using the validated config setter.
    cfg.http_web_root = work.path().to_path_buf();
    let b = Burrow::start(cfg).await.unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    b.shared
        .files
        .mkdir("public", None, "inbox", true)
        .await
        .unwrap();
    let (drop_id, _) = add_file(&b, Some("inbox"), "secret.bin", b"dropbox secret").await;
    b.shared
        .files
        .add_alias("public", None, "alias.bin", "inbox/secret.bin")
        .await
        .unwrap();
    let (held_id, held_blob) = add_file(&b, None, "held.bin", b"quarantined").await;
    b.shared
        .moderation
        .quarantine_set(subject_kind::FILE, &held_blob.0, "held", "fixture")
        .await
        .unwrap();
    let (denied_id, denied_blob) = add_file(&b, None, "denied.bin", b"deny-listed").await;
    b.shared
        .moderation
        .deny_add(&denied_blob.0, "denied", "fixture")
        .await
        .unwrap();
    let (corrupt_id, corrupt_blob) = add_file(&b, None, "corrupt.bin", b"expected bytes").await;
    std::fs::write(b.shared.blobs.file_path(&corrupt_blob), b"tampered bytes").unwrap();
    for path in [
        "/files/public/inbox/secret.bin",
        "/files/public/alias.bin",
        "/files/public/held.bin",
        "/files/public/denied.bin",
        "/files/public/corrupt.bin",
        "/files/public/missing.bin",
        "/data/identity/server_ed25519.seed",
        "/data/burrow.db",
    ] {
        for range in ["bytes=0-1", "bytes=999-", "bytes=bad", "bytes=0-0,2-2"] {
            let reply = request(
                b.http_addr.unwrap(),
                "GET",
                path,
                &format!("Range: {range}\r\n"),
            )
            .await;
            assert_eq!(reply.status, 404, "{path}, {range}");
            assert!(
                !reply.headers.contains_key("content-range"),
                "no private length hint"
            );
            assert!(!reply.headers.contains_key("accept-ranges"));
        }
    }
    let traversal = request(
        b.http_addr.unwrap(),
        "GET",
        "/web/%2e%2e/data/burrow.db",
        "Range: bytes=0-1\r\n",
    )
    .await;
    assert_eq!(traversal.status, 400);
    assert!(!traversal.headers.contains_key("content-range"));
    for id in [drop_id, held_id, denied_id, corrupt_id] {
        assert_eq!(downloads(&b, id).await, 0);
    }
    b.shutdown().await;
}
