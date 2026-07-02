//! Wave 6 end-to-end tests: the legacy telnet + finger listeners wired into
//! `burrow`. The telnet protocol depth and finger formatting are unit-tested
//! in their own crates; here we prove burrow binds them, adapts them to real
//! accounts/personas/presence, and serves live sockets.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Legacy Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        finger_enabled: true,
        finger_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn finger_serves_profiles_from_personas() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.finger_addr.expect("finger enabled");

    // A user query returns the persona view (no .plan set → "No Plan.").
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"alice\r\n").await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("alice"), "profile names the user: {text:?}");
    assert!(text.contains("No Plan."), "planless user: {text:?}");

    // An unknown user is reported, not matched.
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"nobody\r\n").await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_lowercase();
    assert!(
        text.contains("no such") || text.contains("not found") || text.contains("nobody"),
        "unknown user handled: {text:?}"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn telnet_listener_accepts_and_negotiates() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    // On connect, the shell starts telnet option negotiation immediately —
    // the first bytes contain IAC (0xFF). This proves the listener accepts
    // and hands the socket to the BBS shell (protocol depth is covered by
    // the legacy-telnet crate's own tests).
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(3), s.read(&mut buf))
        .await
        .expect("server sent initial bytes")
        .unwrap();
    assert!(n > 0, "server greeted the connection");
    assert!(
        buf[..n].contains(&0xFF),
        "telnet negotiation (IAC) present in the greeting"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn legacy_surfaces_off_by_default() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        name: "Quiet Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    assert!(burrow.telnet_addr.is_none(), "telnet off by default");
    assert!(burrow.finger_addr.is_none(), "finger off by default");
    burrow.shutdown().await;
}
