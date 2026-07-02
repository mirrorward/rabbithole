//! End-to-end tests: a real burrow on ephemeral ports, driven by the real
//! client over both transports.

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_proto::chat::ChatMessage;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "E2E Warren".into(),
        motd: "welcome to the test warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn start(dir: &std::path::Path) -> Burrow {
    Burrow::start(test_config(dir))
        .await
        .expect("burrow starts")
}

async fn quic_client(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("127.0.0.1:{}", burrow.quic_addr.port()),
        Some("localhost"),
        Some(&burrow.fingerprint.to_hex()),
        "e2e",
        "0",
    )
    .await
    .expect("client connects")
}

#[tokio::test]
async fn guest_login_chat_and_who_over_quic() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;

    let mut alice = quic_client(&burrow).await;
    assert_eq!(alice.server.server_name, "E2E Warren");
    assert_ne!(alice.server.server_key, [0u8; 32], "real persistent key");

    let ok = alice.auth_guest(Some("Alice".into())).await.unwrap();
    assert_eq!(ok.screen_name, "Alice (guest)");
    assert!(ok.token.is_empty(), "guests are not resumable");
    let welcome = alice.expect_welcome().await.unwrap();
    assert_eq!(welcome.motd, "welcome to the test warren");

    // Second client sees Alice in who and receives her chat line.
    let mut bob = quic_client(&burrow).await;
    bob.auth_guest(Some("Bob".into())).await.unwrap();
    bob.expect_welcome().await.unwrap();

    let who = bob.who().await.unwrap();
    let names: Vec<&str> = who.iter().map(|u| u.screen_name.as_str()).collect();
    assert!(names.contains(&"Alice (guest)"), "who list: {names:?}");

    alice
        .chat_send("lobby", "curiouser and curiouser")
        .await
        .unwrap();

    // Bob receives the line as a push (bounded wait — a hang is a failure).
    let mut got = None;
    for _ in 0..10 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), bob.next_push())
            .await
            .expect("push within 5s")
            .unwrap()
            .expect("push");
        if let Some(Ok(m)) = frame.decode::<ChatMessage>() {
            got = Some(m);
            break;
        }
    }
    let m = got.expect("chat push arrived");
    assert_eq!(m.from, "Alice (guest)");
    assert_eq!(m.text, "curiouser and curiouser");

    // History has it too.
    let history = bob.chat_history("lobby", 10).await.unwrap();
    assert!(history.iter().any(|m| m.text == "curiouser and curiouser"));

    burrow.shutdown().await;
}

#[tokio::test]
async fn password_login_resume_and_replay_over_ws() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    burrow
        .shared
        .auth
        .create_account("alice", "looking-glass", Role::User)
        .await
        .unwrap();

    let ws = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());

    // Login, observe cursor, disconnect.
    let mut c1 = Client::connect(&ws, None, None, "e2e", "0").await.unwrap();
    let ok = c1.auth_password("alice", "looking-glass").await.unwrap();
    assert!(!ok.token.is_empty());
    assert!(!ok.resumed);
    assert_eq!(ok.role, Role::User as u8);
    c1.expect_welcome().await.unwrap();
    let cursor = c1.replay_cursor;
    let token = ok.token.clone();
    c1.close().await;

    // While alice is away, someone chats (pushes stamped into her log).
    let mut guest = Client::connect(&ws, None, None, "e2e", "0").await.unwrap();
    guest.auth_guest(None).await.unwrap();
    guest.expect_welcome().await.unwrap();
    guest.chat_send("lobby", "missed line").await.unwrap();
    // Give the (async, best-effort) offline-replay recorder a beat to stamp.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Resume: AuthOk.resumed, and the missed chat line is replayed.
    let mut c2 = Client::connect(&ws, None, None, "e2e", "0").await.unwrap();
    let ok2 = c2.auth_resume(&token, cursor).await.unwrap();
    assert!(ok2.resumed);
    assert_eq!(ok2.screen_name, "alice");

    let mut saw_missed = false;
    for _ in 0..10 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), c2.next_push())
            .await
            .expect("replay push within 5s")
            .unwrap()
            .expect("push");
        if let Some(Ok(m)) = frame.decode::<ChatMessage>() {
            if m.text == "missed line" {
                saw_missed = true;
                break;
            }
        }
    }
    assert!(saw_missed, "replay delivered the missed chat line");

    // Bad password and bad token are refused.
    let mut c3 = Client::connect(&ws, None, None, "e2e", "0").await.unwrap();
    assert!(c3.auth_password("alice", "wrong").await.is_err());
    assert!(c3.auth_resume("bogus-token", 0).await.is_err());

    burrow.shutdown().await;
}

#[tokio::test]
async fn guests_disabled_and_agreement_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.guest_enabled = false;
    cfg.agreement = "be excellent to each other".into();
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("mad-hatter", "tea-time", Role::User)
        .await
        .unwrap();

    let mut c = quic_client(&burrow).await;
    // Guests are off.
    assert!(c.auth_guest(None).await.is_err());

    // Password user must accept the agreement before chatting.
    let mut h = quic_client(&burrow).await;
    h.auth_password("mad-hatter", "tea-time").await.unwrap();
    let welcome = h.expect_welcome().await.unwrap();
    assert_eq!(
        welcome.agreement.as_deref(),
        Some("be excellent to each other")
    );

    let denied = h.chat_send("lobby", "no thanks").await;
    assert!(
        denied.is_err(),
        "chat before accepting the agreement is refused"
    );

    h.agreement_accept().await.unwrap();
    h.chat_send("lobby", "tea time!").await.unwrap();

    burrow.shutdown().await;
}

#[tokio::test]
async fn ctl_surface_config_and_accounts() {
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let shared = &burrow.shared;

    // config get/set through the ctl handler.
    let r = burrow::ctl::handle(shared, &json!({"cmd": "config-get", "key": "name"})).await;
    assert_eq!(r["data"], "E2E Warren");

    let r = burrow::ctl::handle(
        shared,
        &json!({"cmd": "config-set", "key": "motd", "value": "fresh motd"}),
    )
    .await;
    assert_eq!(r["ok"], true);
    assert_eq!(r["data"]["applied_live"], true);
    assert_eq!(shared.config.read().motd, "fresh motd");

    // account-create, then that account can log in.
    let r = burrow::ctl::handle(
        shared,
        &json!({"cmd": "account-create", "login": "dormouse", "password": "zzz", "role": "moderator"}),
    )
    .await;
    assert_eq!(r["ok"], true, "ctl error: {r}");

    let mut c = quic_client(&burrow).await;
    let ok = c.auth_password("dormouse", "zzz").await.unwrap();
    assert_eq!(ok.role, Role::Moderator as u8);

    // Unknown command errors cleanly.
    let r = burrow::ctl::handle(shared, &json!({"cmd": "frobnicate"})).await;
    assert_eq!(r["ok"], false);

    burrow.shutdown().await;
}
