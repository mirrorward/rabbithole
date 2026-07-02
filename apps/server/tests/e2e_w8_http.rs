//! Wave 8 end-to-end tests: the embedded HTTP server — the `/files/...`
//! download handoff telnet's `get` mints links for, plus the static SPA
//! shell. We prove that:
//!
//! - a publicly-listable file downloads with the exact stored bytes and the
//!   right headers (`Content-Length`, `Content-Type`,
//!   `Content-Disposition: attachment`), and the download counter bumps at
//!   this byte-serving hop;
//! - `HEAD` mirrors `GET` (status + headers) with no body and no counter
//!   bump;
//! - drop-box contents, quarantined blobs, and deny-listed hashes are all
//!   the same plain 404 — no existence distinctions leak;
//! - traversal attempts (plain and percent-encoded) are refused, and
//!   non-GET/HEAD methods get 405;
//! - with `http_web_root` configured the SPA shell is served (`/` =
//!   `index.html`, typed assets, a generated `/manifest.webmanifest`, no
//!   directory listings);
//! - the surface is off by default.
//!
//! Deterministic: every request is a fresh `Connection: close` exchange
//! against a port the OS picked; no sleeps, no polls.

use std::net::SocketAddr;

use burrow::Burrow;
use rabbithole_proto::admin::subject_kind;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn http_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Warren Web".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        http_enabled: true,
        http_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        // Every request here is a fresh connection from one IP; leave the
        // default per-IP connection burst (10) out of the test's way.
        ratelimit_conn_per_min: 600,
        ratelimit_conn_burst: 100,
        ..ServerConfig::default()
    }
}

/// One raw HTTP exchange: write the request, read to EOF, split the response
/// into (status, lowercased headers, body). Hand-rolled because `HEAD`
/// responses carry a `Content-Length` with no body, which a framing-strict
/// parser would call truncated.
async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(
        format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let head_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("complete response head");
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers, raw[head_end + 4..].to_vec())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Store `bytes` as a blob and add them as a library file; returns the blob
/// hash (= blob id) and the node id.
async fn add_file(
    b: &Burrow,
    area: &str,
    folder: Option<&str>,
    name: &str,
    bytes: &[u8],
) -> ([u8; 32], i64) {
    let blob_id = b.shared.blobs.put(bytes).unwrap().0;
    let node = b
        .shared
        .files
        .add_file(
            area,
            folder,
            name,
            &blob_id,
            bytes.len() as i64,
            "application/zip",
            "disk",
            "",
            "op@warren",
            1,
        )
        .await
        .unwrap();
    (blob_id, node.id)
}

#[tokio::test]
async fn public_download_serves_bytes_headers_and_counts() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(http_config(work.path())).await.unwrap();
    let addr = b.http_addr.expect("http enabled");

    b.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let payload = b"the cool demo bytes \x00\x01\x02";
    let (_, node_id) = add_file(&b, "warez", None, "cool demo.zip", payload).await;

    // GET via the exact link shape telnet mints: percent-encoded path.
    let (status, headers, body) = request(addr, "GET", "/files/warez/cool%20demo.zip").await;
    assert_eq!(status, 200);
    assert_eq!(body, payload, "exact stored bytes");
    assert_eq!(
        header(&headers, "content-length").unwrap(),
        payload.len().to_string()
    );
    assert_eq!(header(&headers, "content-type"), Some("application/zip"));
    assert_eq!(
        header(&headers, "content-disposition"),
        Some("attachment; filename=\"cool demo.zip\"")
    );
    assert_eq!(header(&headers, "connection"), Some("close"));
    let node = b.shared.files.node(node_id).await.unwrap().unwrap();
    assert_eq!(node.downloads, 1, "the GET counted the download");

    // HEAD mirrors GET: same status and headers, no body, no counter bump.
    let (status, headers, body) = request(addr, "HEAD", "/files/warez/cool%20demo.zip").await;
    assert_eq!(status, 200);
    assert!(body.is_empty(), "HEAD has no body");
    assert_eq!(
        header(&headers, "content-length").unwrap(),
        payload.len().to_string()
    );
    let node = b.shared.files.node(node_id).await.unwrap().unwrap();
    assert_eq!(node.downloads, 1, "HEAD serves no bytes, counts nothing");

    // Missing files, bare areas, and folder paths are all plain 404s.
    let (status, _, _) = request(addr, "GET", "/files/warez/missing.zip").await;
    assert_eq!(status, 404);
    let (status, _, _) = request(addr, "GET", "/files/warez").await;
    assert_eq!(status, 404, "no area listings");
    let (status, _, _) = request(addr, "GET", "/files/nope/x.zip").await;
    assert_eq!(status, 404);

    b.shutdown().await;
}

#[tokio::test]
async fn dropbox_quarantined_and_denied_content_reads_as_missing() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(http_config(work.path())).await.unwrap();
    let addr = b.http_addr.unwrap();

    b.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();

    // Drop-box contents never serve anonymously.
    b.shared
        .files
        .mkdir("warez", None, "inbox", true)
        .await
        .unwrap();
    let (_, dropped_id) = add_file(&b, "warez", Some("inbox"), "secret.zip", b"secret").await;
    let (status, _, body) = request(addr, "GET", "/files/warez/inbox/secret.zip").await;
    assert_eq!(status, 404, "drop-box content refused");
    assert!(!body.windows(6).any(|w| w == b"secret"), "no byte leak");
    let node = b.shared.files.node(dropped_id).await.unwrap().unwrap();
    assert_eq!(node.downloads, 0, "refused download never counts");

    // Quarantined-for-review blobs vanish from the anonymous surface…
    let (q_blob, _) = add_file(&b, "warez", None, "review-me.zip", b"under review").await;
    let (status, _, _) = request(addr, "GET", "/files/warez/review-me.zip").await;
    assert_eq!(status, 200, "public before quarantine");
    b.shared
        .moderation
        .quarantine_set(subject_kind::FILE, &q_blob, "reported", "mod")
        .await
        .unwrap();
    let (status, _, _) = request(addr, "GET", "/files/warez/review-me.zip").await;
    assert_eq!(status, 404, "quarantined content refused");

    // …and deny-listed hashes refuse too (both checks are consulted).
    let (d_blob, _) = add_file(&b, "warez", None, "banned.zip", b"banned bytes").await;
    b.shared
        .moderation
        .deny_add(&d_blob, "dmca", "mod")
        .await
        .unwrap();
    let (status, _, _) = request(addr, "GET", "/files/warez/banned.zip").await;
    assert_eq!(status, 404, "denied hash refused");

    b.shutdown().await;
}

#[tokio::test]
async fn traversal_is_refused_and_only_get_head_are_allowed() {
    let work = tempfile::tempdir().unwrap();
    // A web root proves traversal can't reach files beside it either.
    let web = work.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<h1>hi</h1>").unwrap();
    std::fs::write(work.path().join("outside.txt"), "you cannot see me").unwrap();
    let b = Burrow::start(ServerConfig {
        http_web_root: web,
        ..http_config(&work.path().join("data"))
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();

    // Plain, encoded (`%2e%2e`), and encoded-slash traversal all refuse.
    for path in [
        "/../outside.txt",
        "/files/warez/../../outside.txt",
        "/files/%2e%2e/%2e%2e/outside.txt",
        "/%2E%2E/outside.txt",
        "/..%2Foutside.txt",
        "/files/a%2F..%2Fb.zip",
        "/a%5Cb.txt",
        "/nul%00.txt",
        "/bad%zzescape",
    ] {
        let (status, _, body) = request(addr, "GET", path).await;
        assert_eq!(status, 400, "{path} must be refused");
        assert!(
            !body.windows(6).any(|w| w == b"cannot"),
            "{path} leaked bytes"
        );
    }

    // Methods other than GET/HEAD: 405 with an Allow header.
    for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
        let (status, headers, _) = request(addr, method, "/").await;
        assert_eq!(status, 405, "{method}");
        assert_eq!(header(&headers, "allow"), Some("GET, HEAD"));
    }

    // An oversized request head is a 400, not a hang or a crash.
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    sock.write_all(&vec![b'x'; 10 * 1024]).await.unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    assert!(raw.starts_with(b"HTTP/1.1 400 "), "oversized head refused");

    b.shutdown().await;
}

#[tokio::test]
async fn web_root_serves_the_spa_shell_and_generated_manifest() {
    let work = tempfile::tempdir().unwrap();
    let web = work.path().join("dist"); // e.g. a `trunk build` output dir
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<html>the shell</html>").unwrap();
    std::fs::write(web.join("assets").join("app.js"), "console.log(1)").unwrap();
    let b = Burrow::start(ServerConfig {
        http_web_root: web,
        ..http_config(&work.path().join("data"))
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();

    // `/` answers index.html with the html content type.
    let (status, headers, body) = request(addr, "GET", "/").await;
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(body, b"<html>the shell</html>");

    // Nested assets serve with types from the extension map.
    let (status, headers, body) = request(addr, "GET", "/assets/app.js").await;
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("text/javascript"));
    assert_eq!(body, b"console.log(1)");

    // The web root ships no manifest, so one is generated from server config.
    let (status, headers, body) = request(addr, "GET", "/manifest.webmanifest").await;
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/manifest+json")
    );
    let manifest: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(manifest["name"], "Warren Web");
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["start_url"], "/");

    // No directory listings; unknown assets are 404; HEAD carries no body.
    let (status, _, _) = request(addr, "GET", "/assets").await;
    assert_eq!(status, 404, "directories never list");
    let (status, _, _) = request(addr, "GET", "/nope.png").await;
    assert_eq!(status, 404);
    let (status, headers, body) = request(addr, "HEAD", "/").await;
    assert_eq!(status, 200);
    assert!(body.is_empty());
    assert_eq!(
        header(&headers, "content-length").unwrap(),
        b"<html>the shell</html>".len().to_string()
    );

    b.shutdown().await;
}

#[tokio::test]
async fn http_surface_is_off_by_default_and_static_off_without_web_root() {
    let work = tempfile::tempdir().unwrap();

    // Off by default: no listener, no bound address.
    let b = Burrow::start(ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("default"),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    assert!(b.http_addr.is_none(), "http is opt-in");
    assert!(!ServerConfig::default().http_enabled);
    b.shutdown().await;

    // Enabled without a web root: /files answers, the shell does not.
    let b = Burrow::start(http_config(&work.path().join("noroot")))
        .await
        .unwrap();
    let addr = b.http_addr.unwrap();
    let (status, _, _) = request(addr, "GET", "/").await;
    assert_eq!(status, 404, "no web root: no shell");
    let (status, _, _) = request(addr, "GET", "/manifest.webmanifest").await;
    assert_eq!(status, 404, "manifest belongs to the shell surface");

    b.shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();
    add_file(&b, "pub", None, "still-works.txt", b"handoff!").await;
    let (status, headers, body) = request(addr, "GET", "/files/pub/still-works.txt").await;
    assert_eq!(status, 200, "the handoff route needs no web root");
    assert_eq!(body, b"handoff!");
    assert!(header(&headers, "content-disposition")
        .unwrap()
        .contains("still-works.txt"));

    b.shutdown().await;
}
