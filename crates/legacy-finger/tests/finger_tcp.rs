//! End-to-end finger tests: a real TCP listener on 127.0.0.1:0 answering a
//! stub directory, exercised with raw socket clients like a period finger
//! client would.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use rabbithole_legacy_finger::{FingerDirectory, FingerServer, Profile, WhoEntry};

struct StubDirectory;

#[async_trait]
impl FingerDirectory for StubDirectory {
    async fn who(&self) -> Vec<WhoEntry> {
        vec![
            WhoEntry {
                screen_name: "alice".to_string(),
                idle_secs: 0,
                location: Some("Wonderland".to_string()),
            },
            WhoEntry {
                screen_name: "madhatter".to_string(),
                idle_secs: 2 * 3600 + 5 * 60,
                location: None,
            },
        ]
    }

    async fn lookup(&self, user: &str) -> Option<Profile> {
        match user {
            "alice" => Some(Profile {
                screen_name: "alice".to_string(),
                real_name: Some("Alice Liddell".to_string()),
                pronouns: Some("she/her".to_string()),
                location: Some("Wonderland".to_string()),
                interests: Some("croquet, tea".to_string()),
                quote: Some("Curiouser and curiouser!".to_string()),
                plan: Some("1. Follow the white rabbit\n2. Tea at six\x1b[31m".to_string()),
            }),
            "dormouse" => Some(Profile {
                screen_name: "dormouse".to_string(),
                ..Profile::default()
            }),
            _ => None,
        }
    }
}

async fn start_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let server = FingerServer::new(Arc::new(StubDirectory));
    tokio::spawn(server.serve(listener));
    addr
}

async fn finger(addr: SocketAddr, query: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(query).await.expect("write query");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    String::from_utf8(response).expect("utf-8 response")
}

fn assert_crlf_only(response: &str) {
    assert!(!response.is_empty(), "response should not be empty");
    assert!(
        !response.replace("\r\n", "").contains(['\r', '\n']),
        "every line ending must be CRLF: {response:?}"
    );
    assert!(response.ends_with("\r\n"), "response ends with CRLF");
}

#[tokio::test]
async fn empty_query_lists_who_is_online() {
    let addr = start_server().await;
    let response = finger(addr, b"\r\n").await;
    assert_crlf_only(&response);
    let lines: Vec<&str> = response.lines().collect();
    assert!(lines[0].starts_with("Login"), "table header: {response:?}");
    assert!(lines
        .iter()
        .any(|l| l.starts_with("alice") && l.ends_with("Wonderland")));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("madhatter") && l.contains("2h05m")));
}

#[tokio::test]
async fn user_query_returns_profile_and_plan() {
    let addr = start_server().await;
    let response = finger(addr, b"alice\r\n").await;
    assert_crlf_only(&response);
    assert!(response.starts_with("Login: alice\r\n"));
    assert!(response.contains("Real name: Alice Liddell\r\n"));
    assert!(response.contains("Quote: Curiouser and curiouser!\r\n"));
    assert!(
        response.contains("Plan:\r\n1. Follow the white rabbit\r\n2. Tea at six"),
        "plan rendered under heading: {response:?}"
    );
    assert!(
        !response.contains('\x1b'),
        "escape stripped from hostile plan"
    );
}

#[tokio::test]
async fn verbose_flag_is_accepted() {
    let addr = start_server().await;
    let response = finger(addr, b"/W alice\r\n").await;
    assert!(response.starts_with("Login: alice\r\n"));
}

#[tokio::test]
async fn planless_user_gets_no_plan() {
    let addr = start_server().await;
    let response = finger(addr, b"dormouse\r\n").await;
    assert_crlf_only(&response);
    assert!(response.ends_with("No Plan.\r\n"), "{response:?}");
}

#[tokio::test]
async fn unknown_user_is_reported() {
    let addr = start_server().await;
    let response = finger(addr, b"cheshire\r\n").await;
    assert_crlf_only(&response);
    assert_eq!(response, "finger: cheshire: no such user.\r\n");
}

#[tokio::test]
async fn forwarding_is_refused() {
    let addr = start_server().await;
    let response = finger(addr, b"alice@example.com\r\n").await;
    assert_crlf_only(&response);
    assert_eq!(response, "finger: forwarding service denied.\r\n");
}

#[tokio::test]
async fn oversized_query_is_rejected_politely() {
    let addr = start_server().await;
    // Exactly the server's read limit, so every sent byte is consumed before
    // the server replies and closes (no RST racing the response).
    let long = vec![b'a'; rabbithole_legacy_finger::MAX_QUERY_BYTES + 2];
    let response = finger(addr, &long).await;
    assert_eq!(response, "finger: query too long.\r\n");
}
