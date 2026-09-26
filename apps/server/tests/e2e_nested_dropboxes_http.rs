//! Anonymous HTTP reads follow every drop-box ancestor of both alias and target.

use std::net::SocketAddr;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn request(addr: SocketAddr, method: &str, path: &str, range: bool) -> (u16, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut socket = TcpStream::connect(addr).await.unwrap();
        let range = if range { "Range: bytes=0-2\r\n" } else { "" };
        socket
            .write_all(
                format!(
                    "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{range}\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&bytes[..end]).unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, bytes[end + 4..].to_vec())
    })
    .await
    .expect("bounded HTTP exchange")
}

async fn add_file(b: &Burrow, folder: &str, name: &str, bytes: &[u8]) -> i64 {
    let blob = b.shared.blobs.put(bytes).unwrap();
    b.shared
        .files
        .add_file(
            "public",
            Some(folder),
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
        .unwrap()
        .id
}

#[tokio::test]
async fn nested_files_and_both_alias_locations_are_hidden_for_get_head_and_range() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        http_enabled: true,
        http_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("data"),
        ratelimit_conn_per_min: 6000,
        ratelimit_conn_burst: 1000,
        ratelimit_legacy_per_sec: 1000,
        ratelimit_legacy_burst: 1000,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    let addr = b.http_addr.unwrap();
    let files = &b.shared.files;
    files.create_area("public", "Public", "").await.unwrap();
    files.mkdir("public", None, "inbox", true).await.unwrap();
    files
        .mkdir("public", Some("inbox"), "nested", false)
        .await
        .unwrap();
    let deep = files
        .mkdir("public", Some("inbox/nested"), "deep", false)
        .await
        .unwrap();
    files.mkdir("public", None, "open", false).await.unwrap();
    let hidden = add_file(&b, "inbox/nested/deep", "secret.bin", b"private payload").await;
    let public = add_file(&b, "open", "public.bin", b"public payload").await;
    files
        .add_alias(
            "public",
            Some("open"),
            "hidden-link",
            "inbox/nested/deep/secret.bin",
        )
        .await
        .unwrap();
    files
        .add_alias(
            "public",
            Some("inbox/nested/deep"),
            "public-link",
            "open/public.bin",
        )
        .await
        .unwrap();
    files
        .add_alias("public", Some("open"), "public-link", "open/public.bin")
        .await
        .unwrap();

    for (method, range) in [("GET", false), ("HEAD", false), ("GET", true)] {
        let missing = request(addr, method, "/files/public/missing", range).await;
        assert_eq!(missing.0, 404);
        for path in [
            "/files/public/inbox/nested/deep/secret.bin",
            "/files/public/open/hidden-link",
            "/files/public/inbox/nested/deep/public-link",
        ] {
            assert_eq!(
                request(addr, method, path, range).await,
                missing,
                "{method} {path}, range={range}"
            );
        }
    }
    assert_eq!(files.node(hidden).await.unwrap().unwrap().downloads, 0);
    assert_eq!(files.node(public).await.unwrap().unwrap().downloads, 0);
    for path in [
        "/files/public/open/public.bin",
        "/files/public/open/public-link",
    ] {
        assert_eq!(
            request(addr, "GET", path, false).await,
            (200, b"public payload".to_vec())
        );
        assert_eq!(
            request(addr, "GET", path, true).await,
            (206, b"pub".to_vec())
        );
    }

    // A parent move changes both the descendant path and the alias target's
    // eligibility immediately; no cached parent snapshot may retain access.
    files.move_to(deep.id, Some("open")).await.unwrap();
    for path in [
        "/files/public/open/deep/secret.bin",
        "/files/public/open/hidden-link",
    ] {
        assert_eq!(
            request(addr, "GET", path, false).await,
            (200, b"private payload".to_vec())
        );
    }
    assert_eq!(
        request(addr, "GET", "/files/public/open/deep/public-link", false).await,
        (200, b"public payload".to_vec())
    );
    files.move_to(deep.id, Some("inbox/nested")).await.unwrap();
    for path in [
        "/files/public/inbox/nested/deep/secret.bin",
        "/files/public/open/hidden-link",
        "/files/public/inbox/nested/deep/public-link",
    ] {
        assert_eq!(request(addr, "GET", path, false).await.0, 404);
    }
    assert_eq!(files.node(hidden).await.unwrap().unwrap().downloads, 2);
    b.shutdown().await;
}
