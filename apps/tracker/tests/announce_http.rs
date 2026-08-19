//! A burrow-shaped `POST /api/announce` lands in INDEX.

use std::time::Duration;

use looking_glass::{ingest_announce, service, Registry};
use rabbithole_identity::IdentityKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

async fn spawn_glass() -> (std::net::SocketAddr, std::net::SocketAddr) {
    let registry = std::sync::Arc::new(Registry::new(Duration::from_secs(360)));
    let announce = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let status = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let announce_addr = announce.local_addr().unwrap();
    let status_addr = status.local_addr().unwrap();
    tokio::spawn(service::run_announce_http(announce, registry.clone()));
    tokio::spawn(service::run_status_tcp(status, registry));
    (announce_addr, status_addr)
}

async fn post(addr: std::net::SocketAddr, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text.into_owned())
}

async fn index(addr: std::net::SocketAddr) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(b"INDEX\n").await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    String::from_utf8_lossy(&raw).into_owned()
}

#[tokio::test]
async fn a_signed_announce_is_what_index_lists() {
    let (announce_addr, status_addr) = spawn_glass().await;
    let key = IdentityKey::from_seed(&[11u8; 32]);
    let descriptor = serde_json::json!({
        "name": "alice@127.0.0.1",
        "publicKey": hex::encode(key.public().0),
        "timestamp": 1_700_000_000_000_i64,
        "ttl": 300,
        "description": "local just-up burrow",
        "endpoints": { "quic": "quic://127.0.0.1:4653" },
    });
    let signature = key.sign(looking_glass::canonical_json(&descriptor).as_bytes());
    let body = serde_json::json!({
        "descriptor": descriptor,
        "signature": hex::encode(signature.0),
    })
    .to_string();

    // The same ingest the HTTP handler uses — pin the document first.
    let entry = ingest_announce(&body).expect("ingest");
    assert_eq!(entry.addr, "127.0.0.1:4653".parse().unwrap());

    let (status, _) = timeout(
        Duration::from_secs(2),
        post(announce_addr, "/api/announce", &body),
    )
    .await
    .expect("post");
    assert_eq!(status, 200);

    let listing = timeout(Duration::from_secs(2), index(status_addr))
        .await
        .expect("index");
    assert!(
        listing.contains("alice@127.0.0.1\t127.0.0.1:4653"),
        "INDEX should list the announced burrow, got: {listing:?}"
    );

    let (missing, _) = post(announce_addr, "/nope", &body).await;
    assert_eq!(missing, 404);

    let (bad, reply) = post(announce_addr, "/api/announce", "not-json").await;
    assert_eq!(bad, 400);
    assert!(reply.contains("not JSON"), "{reply}");
}
