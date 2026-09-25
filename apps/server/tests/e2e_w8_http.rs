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
//! - with `http_web_root` configured the SPA shell answers direct client
//!   routes as well as `/`, while typed assets and generated manifests keep
//!   their behavior, missing assets stay 404, and directories never list;
//! - `/.well-known/rabbithole/server` returns the signed, self-certifying
//!   discovery descriptor as JSON (verifies against the key it names, carries
//!   the server name / advertised endpoints / feature tags), only the exact
//!   path resolves, and HEAD mirrors GET;
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
    let web = work.path().join("web");
    std::fs::create_dir_all(web.join("files").join("warez")).unwrap();
    std::fs::write(web.join("index.html"), "<html>the shell</html>").unwrap();
    // Files in the web root must never shadow or bypass library downloads.
    std::fs::write(
        web.join("files/warez/cool demo.zip"),
        "not the library file",
    )
    .unwrap();
    std::fs::write(web.join("files/warez/static-only.zip"), "not public").unwrap();
    let b = Burrow::start(ServerConfig {
        http_web_root: web,
        ..http_config(&work.path().join("data"))
    })
    .await
    .unwrap();
    let addr = b.http_addr.expect("http enabled");

    // Exactly `/files` is the client page, even with a static files folder.
    let (status, headers, body) = request(addr, "GET", "/files").await;
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(body, b"<html>the shell</html>");

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
    let (status, _, _) = request(addr, "GET", "/files/warez/static-only.zip").await;
    assert_eq!(
        status, 404,
        "download URLs never use static files or the shell"
    );

    b.shutdown().await;
}

#[tokio::test]
async fn dropbox_quarantined_and_denied_content_reads_as_missing() {
    let work = tempfile::tempdir().unwrap();
    let web = work.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html>the shell</html>").unwrap();
    let b = Burrow::start(ServerConfig {
        http_web_root: web,
        ..http_config(&work.path().join("data"))
    })
    .await
    .unwrap();
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
        "/lobby/../outside.txt",
        "/people/%2e%2e",
        "/boards/a%2Fb",
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

/// The burrow never serves its own data directory, however the web root
/// was arrived at. `http_web_root` is an ordinary config key that anybody
/// with `CONFIG_ADMIN` can set live, and pointed at the data directory the
/// static route would answer `GET /identity/server_ed25519.seed` for
/// anybody at all: the burrow's signing key, with no session and nothing in
/// the audit log. Set here through the config the server starts with, which
/// is the way round no validation can catch.
#[tokio::test]
async fn the_burrows_own_folders_are_never_served() {
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("index.html"), "private index must not be served").unwrap();
    let b = Burrow::start(ServerConfig {
        // The whole data directory as the web root: what an operator gets
        // by typing the wrong path, and what a stolen console session would
        // choose on purpose. Set in the config the server starts with,
        // which is the way round no validation can catch.
        http_web_root: data.clone(),
        backup_dir: "snapshots".into(),
        ..http_config(&data)
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();

    // The burrow minted its own identity on the way up; that is the file
    // this test is about.
    let seed_path = data.join("identity").join("server_ed25519.seed");
    let seed = std::fs::read(&seed_path).expect("the burrow keeps a signing seed");
    assert!(!seed.is_empty());
    std::fs::create_dir_all(data.join("snapshots").join("snapshot-1")).unwrap();
    std::fs::write(
        data.join("snapshots").join("snapshot-1").join("burrow.db"),
        b"a copy of everything",
    )
    .unwrap();

    for path in [
        "/identity/server_ed25519.seed",
        "/identity/tls_key.der",
        "/burrow.db",
        "/snapshots/snapshot-1/burrow.db",
        "/index.html",
        "/lobby",
        "/boards/general",
        "/files",
    ] {
        let (status, _, body) = request(addr, "GET", path).await;
        assert_eq!(status, 404, "{path} was served");
        assert!(body != seed, "{path} leaked the signing seed");
    }
    // And the same 404 as anything else missing: no existence distinctions.
    let (missing, _, _) = request(addr, "GET", "/nothing-here").await;
    assert_eq!(missing, 404);

    b.shutdown().await;
}

/// The other half: an operator who tries to set the web root over the
/// console is told why, rather than finding out later.
#[test]
fn a_web_root_over_the_burrows_own_folders_is_refused() {
    let mut cfg = ServerConfig {
        data_dir: "/srv/burrow/data".into(),
        backup_dir: "/srv/burrow/snapshots".into(),
        ..ServerConfig::default()
    };
    for bad in ["/srv/burrow/data", "/srv/burrow", "/srv/burrow/data/blobs"] {
        assert!(
            cfg.set_key("http_web_root", bad).is_err(),
            "{bad} was accepted as a web root"
        );
    }
    assert!(cfg
        .set_key("http_web_root", "/srv/burrow/snapshots")
        .is_err());
    // A sibling that merely reads like one of them is fine.
    assert!(cfg.set_key("http_web_root", "/srv/burrow-web").is_ok());
    // A relative data directory is the same place as its absolute self: a
    // burrow started with `--data-dir target/run` must still refuse the
    // full path to it.
    let here = std::env::current_dir().unwrap();
    let mut relative = ServerConfig {
        data_dir: "target/a-burrow".into(),
        ..ServerConfig::default()
    };
    assert!(
        relative
            .set_key(
                "http_web_root",
                here.join("target/a-burrow").to_str().unwrap()
            )
            .is_err(),
        "the same folder, written two ways, was accepted"
    );
    // And snapshots cannot be moved under the web root afterwards.
    assert!(cfg.set_key("backup_dir", "/srv/burrow-web/snaps").is_err());
    assert!(cfg.set_key("backup_dir", "/srv/burrow/snapshots").is_ok());
}

#[tokio::test]
async fn web_root_serves_the_spa_shell_and_generated_manifest() {
    let work = tempfile::tempdir().unwrap();
    let web = work.path().join("dist"); // e.g. a `trunk build` output dir
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<html>the shell</html>").unwrap();
    std::fs::write(web.join("assets").join("app.js"), "console.log(1)").unwrap();
    // An existing static file still wins at a name also owned by the router.
    std::fs::write(web.join("about"), "operator's static about page").unwrap();
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
    let (status, headers, body) = request(addr, "GET", "/about").await;
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(body, b"operator's static about page");

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
    for path in [
        "/assets",
        "/nope.png",
        "/assets/missing.js",
        "/missing_bg.wasm",
        "/missing.css",
        "/api/status",
        "/.well-known/missing",
        "/unknown-page",
        "/lobby/extra",
        "/boards/general/extra",
        "/people/alice/extra",
        "/admin/theme/extra",
    ] {
        let (status, headers, body) = request(addr, "GET", path).await;
        assert_eq!(status, 404, "{path} must not receive the shell");
        assert_eq!(
            header(&headers, "content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_ne!(body, b"<html>the shell</html>");
    }
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
async fn direct_client_routes_serve_the_same_shell_for_get_and_head() {
    let work = tempfile::tempdir().unwrap();
    let web = work.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    let shell = b"<html>direct route shell</html>";
    std::fs::write(web.join("index.html"), shell).unwrap();
    let b = Burrow::start(ServerConfig {
        http_web_root: web,
        ..http_config(&work.path().join("data"))
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();

    // Every registered route shape, including valid dotted parameters and
    // the query/trailing-slash normalization performed before routing.
    for path in [
        "/",
        "/about",
        "/settings",
        "/people",
        "/people/alice.smith",
        "/transfers",
        "/you",
        "/lobby",
        "/boards",
        "/boards/comp.lang.rust",
        "/dms",
        "/directory",
        "/files",
        "/radio",
        "/servers",
        "/art",
        "/wishing-well",
        "/admin",
        "/admin/theme",
        "/lobby?from=signin",
        "/files/",
        "/people/alice%20smith",
        "/boards//general/",
    ] {
        let (get_status, get_headers, get_body) = request(addr, "GET", path).await;
        assert_eq!(get_status, 200, "GET {path}");
        assert_eq!(get_body, shell, "GET {path}");
        assert_eq!(
            header(&get_headers, "content-type"),
            Some("text/html; charset=utf-8")
        );
        let (head_status, head_headers, head_body) = request(addr, "HEAD", path).await;
        assert_eq!(head_status, get_status, "HEAD {path}");
        assert_eq!(head_headers, get_headers, "HEAD {path} headers match GET");
        assert!(head_body.is_empty(), "HEAD {path} has no body");
    }
    b.shutdown().await;
}

#[tokio::test]
async fn client_routes_require_a_readable_index_file() {
    let work = tempfile::tempdir().unwrap();
    let web = work.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    let b = Burrow::start(ServerConfig {
        http_web_root: web.clone(),
        ..http_config(&work.path().join("data"))
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();
    for index_is_directory in [false, true] {
        if index_is_directory {
            std::fs::create_dir(web.join("index.html")).unwrap();
        }
        for path in ["/", "/lobby", "/files", "/boards/general"] {
            let (status, _, _) = request(addr, "GET", path).await;
            assert_eq!(status, 404, "no readable index: {path}");
        }
    }
    b.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn client_route_fallback_never_follows_an_unsafe_index_symlink() {
    use std::os::unix::fs::symlink;
    let work = tempfile::tempdir().unwrap();
    let web = work.path().join("web");
    let data = web.join("private-data");
    let backups = web.join("private-backups");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&backups).unwrap();
    let outside = work.path().join("outside.html");
    let private = data.join("private.html");
    let snapshot = backups.join("snapshot.html");
    for target in [&outside, &private, &snapshot] {
        std::fs::write(target, "must not be a public shell").unwrap();
    }
    // Set through startup config to exercise runtime checks even when the
    // config setter's web-root validation was bypassed.
    let b = Burrow::start(ServerConfig {
        http_web_root: web.clone(),
        backup_dir: backups,
        ..http_config(&data)
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();
    for target in [&outside, &private, &snapshot] {
        symlink(target, web.join("index.html")).unwrap();
        for path in ["/", "/lobby", "/files", "/people/alice"] {
            for method in ["GET", "HEAD"] {
                let (status, _, body) = request(addr, method, path).await;
                assert_eq!(status, 404, "{method} {path}, index points at {target:?}");
                assert_ne!(body, b"must not be a public shell");
            }
        }
        std::fs::remove_file(web.join("index.html")).unwrap();
    }
    b.shutdown().await;
}

#[tokio::test]
async fn well_known_serves_a_signed_self_certifying_descriptor() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(ServerConfig {
        advertise_host: "warren.test".into(),
        ws_public_url: "wss://warren.test/rhp".into(),
        ..http_config(work.path())
    })
    .await
    .unwrap();
    let addr = b.http_addr.expect("http enabled");

    let (status, headers, body) = request(addr, "GET", "/.well-known/rabbithole/server").await;
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("application/json"));

    let desc: rabbithole_federation::PeerDescriptor =
        serde_json::from_slice(&body).expect("valid descriptor JSON");
    // Self-certifying: the signature verifies against the key it names…
    assert_eq!(desc.verify(), Ok(()), "descriptor signature verifies");
    // …and that key is this burrow's federation/signing identity.
    let identity = rabbithole_identity::IdentityKey::from_seed(&b.shared.server_signing_seed)
        .public()
        .0;
    assert_eq!(
        desc.body.server_key, identity,
        "names the server identity key"
    );

    assert_eq!(desc.body.name, "Warren Web");
    // advertise_host drives the host-based QUIC/HTTP surfaces (with the
    // harness's ephemeral :0); WebSocket comes only from its explicit public
    // WSS URL because the plaintext backend bind is not publication truth.
    for scheme in ["quic://warren.test:", "http://warren.test:"] {
        assert!(
            desc.body.addresses.iter().any(|a| a.starts_with(scheme)),
            "{scheme} not advertised: {:?}",
            desc.body.addresses
        );
    }
    assert!(
        desc.body
            .addresses
            .iter()
            .any(|address| address == "wss://warren.test/rhp"),
        "explicit WSS URL not advertised: {:?}",
        desc.body.addresses
    );
    // Core features are always present; guests are on by default.
    for tag in ["boards", "chat", "dm", "files", "swarm", "guest"] {
        assert!(
            desc.body.features.iter().any(|f| f == tag),
            "missing feature {tag}"
        );
    }
    assert!(desc.body.issued_at > 0, "stamped with an issue time");

    // Only the exact path answers; other `.well-known` subpaths are 404.
    for path in [
        "/.well-known/rabbithole/nope",
        "/.well-known/other",
        "/.well-known",
    ] {
        let (status, _, _) = request(addr, "GET", path).await;
        assert_eq!(status, 404, "{path} must not resolve to the descriptor");
    }

    // HEAD mirrors GET: same status/headers, a real Content-Length, no body.
    let (status, headers, hbody) = request(addr, "HEAD", "/.well-known/rabbithole/server").await;
    assert_eq!(status, 200);
    assert!(hbody.is_empty(), "HEAD has no body");
    assert_eq!(header(&headers, "content-type"), Some("application/json"));
    assert!(
        header(&headers, "content-length")
            .unwrap()
            .parse::<usize>()
            .unwrap()
            > 0
    );

    b.shutdown().await;
}

#[tokio::test]
async fn well_known_never_publishes_a_websocket_backend_bind() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(http_config(work.path())).await.unwrap();
    let addr = b.http_addr.expect("http enabled");

    let (status, _, body) = request(addr, "GET", "/.well-known/rabbithole/server").await;
    assert_eq!(status, 200);
    let desc: rabbithole_federation::PeerDescriptor =
        serde_json::from_slice(&body).expect("valid descriptor JSON");
    assert!(
        desc.body
            .addresses
            .iter()
            .all(|address| !address.starts_with("ws://") && !address.starts_with("wss://")),
        "WebSocket discovery must require ws_public_url: {:?}",
        desc.body.addresses
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
    for path in ["/", "/lobby", "/files", "/boards/general"] {
        let (status, _, _) = request(addr, "GET", path).await;
        assert_eq!(status, 404, "no web root: no shell for {path}");
    }
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
