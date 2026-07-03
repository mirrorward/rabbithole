//! Wave 12 end-to-end: transport-resilience — the client's reconnect + session
//! resume helper (`rabbithole_core::Client::reconnect`).
//!
//! We prove that a **password** session survives a connection drop: re-dialing
//! the same endpoint and `AuthResume`-ing with the remembered token + replay
//! cursor restores a live, working session (`resumed == true`, requests and
//! sends round-trip afterward, the cursor never rewinds). **Guest** sessions
//! carry no token and are correctly refused a resume.
//!
//! Deterministic: the drop is driven explicitly (`reconnect()`), against a
//! burrow on an OS-picked port; no sleeps.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_server_core::{Role, ServerConfig, LOBBY};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "W12 Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn connect(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .expect("connect")
}

#[tokio::test]
async fn password_session_reconnects_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "wonderland", Role::User)
        .await
        .unwrap();

    let mut alice = connect(&burrow).await;
    alice.auth_password("alice", "wonderland").await.unwrap();
    alice.expect_welcome().await.unwrap();
    assert!(
        alice.is_resumable(),
        "a password session carries a resume token"
    );

    // The connection works before the drop, and we note the replay cursor.
    alice.who().await.expect("who before reconnect");
    let cursor_before = alice.replay_cursor;

    // Simulate a drop + recovery: re-dial the same endpoint and resume.
    let ok = alice.reconnect().await.expect("reconnect");
    assert!(ok.resumed, "the resume path ran (AuthResume, resumed=true)");
    assert_eq!(ok.screen_name, "alice", "same identity after resume");
    assert!(
        alice.replay_cursor >= cursor_before,
        "the replay cursor never rewinds across a reconnect"
    );
    assert!(alice.is_resumable(), "still resumable after a resume");

    // The session is live on the fresh connection: a read round-trips and a
    // write (lobby chat) is accepted — proof the resumed session has its
    // capabilities, not a half-open socket.
    alice.who().await.expect("who after reconnect");
    alice
        .chat_send(LOBBY, "back online")
        .await
        .expect("chat send after reconnect");

    alice.close().await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn guest_sessions_cannot_resume() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();

    let mut guest = connect(&burrow).await;
    guest
        .auth_guest(Some("wanderer".into()))
        .await
        .expect("guest sign-in");
    guest.expect_welcome().await.unwrap();

    assert!(
        !guest.is_resumable(),
        "guest sessions get an empty token — not resumable"
    );
    // reconnect refuses before dialing (no token to resume with).
    assert!(
        matches!(guest.reconnect().await, Err(ClientError::Closed)),
        "reconnect refuses a non-resumable session"
    );

    guest.close().await;
    burrow.shutdown().await;
}
