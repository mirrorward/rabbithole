//! Wave 2.2 end-to-end tests: presence states (incl. Cheshire mode),
//! buddy lists, blocks, and DMs with attachments/receipts/auto-response.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::blob::BlobPurpose;
use rabbithole_proto::dm::{DmReadReceipt, DmReceived, DmSend};
use rabbithole_proto::presence::PresenceState;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "W22 Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str, pw: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, pw).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

/// Pump pushes until `pred` matches (bounded), returning the frame.
async fn wait_push_named<F: Fn(&rabbithole_proto::Frame) -> bool>(
    label: &str,
    c: &mut Client,
    pred: F,
) -> rabbithole_proto::Frame {
    for _ in 0..20 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), c.next_push())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for push: {label}"))
            .unwrap()
            .expect("push");
        if pred(&frame) {
            return frame;
        }
    }
    panic!("expected push not seen: {label}");
}

#[tokio::test]
async fn away_status_and_cheshire_mode() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for (u, r) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("mod", Role::Moderator),
    ] {
        burrow
            .shared
            .auth
            .create_account(u, "pw-pw-pw", r)
            .await
            .unwrap();
    }

    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;
    let mut bob = login(&burrow, "bob", "pw-pw-pw").await;
    let mut modr = login(&burrow, "mod", "pw-pw-pw").await;

    // Away with a status message shows in who.
    alice
        .presence_set(PresenceState::Away, Some("gone fishing".into()))
        .await
        .unwrap();
    let who = bob.who().await.unwrap();
    let a = who.iter().find(|u| u.screen_name == "alice").unwrap();
    assert_eq!(a.state, PresenceState::Away);
    assert_eq!(a.status.as_deref(), Some("gone fishing"));

    // Cheshire mode: alice vanishes for bob (UserLeft push + gone from who)…
    alice
        .presence_set(PresenceState::Invisible, None)
        .await
        .unwrap();
    wait_push_named("push", &mut bob, |f| {
        f.decode::<rabbithole_proto::presence::UserLeft>()
            .and_then(Result::ok)
            .is_some_and(|l| l.screen_name == "alice")
    })
    .await;
    assert!(!bob
        .who()
        .await
        .unwrap()
        .iter()
        .any(|u| u.screen_name == "alice"));

    // …but moderators still see her, marked invisible.
    let who_mod = modr.who().await.unwrap();
    let a = who_mod.iter().find(|u| u.screen_name == "alice").unwrap();
    assert_eq!(a.state, PresenceState::Invisible);

    // And she still sees herself.
    assert!(alice
        .who()
        .await
        .unwrap()
        .iter()
        .any(|u| u.screen_name == "alice"));

    // Reappearing emits UserJoined for bob.
    alice
        .presence_set(PresenceState::Online, None)
        .await
        .unwrap();
    wait_push_named("push", &mut bob, |f| {
        f.decode::<rabbithole_proto::presence::UserJoined>()
            .and_then(Result::ok)
            .is_some_and(|j| j.user.screen_name == "alice")
    })
    .await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn buddy_list_groups_and_invisibility() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;

    // Add online buddy + unknown persona fails.
    let mut bob = login(&burrow, "bob", "pw-pw-pw").await;
    alice.buddy_add("bob", "Co-Workers").await.unwrap();
    assert!(matches!(
        alice.buddy_add("nobody", "X").await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    let list = alice.buddy_list().await.unwrap();
    assert_eq!(list.buddies.len(), 1);
    assert_eq!(list.buddies[0].group, "Co-Workers");
    assert!(list.buddies[0].online);

    // Bob goes Cheshire → buddy list shows him offline.
    bob.presence_set(PresenceState::Invisible, None)
        .await
        .unwrap();
    let list = alice.buddy_list().await.unwrap();
    assert!(!list.buddies[0].online, "invisible buddies read as offline");

    // Offline buddy after disconnect too.
    bob.close().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let list = alice.buddy_list().await.unwrap();
    assert!(!list.buddies[0].online);

    alice.buddy_remove("bob").await.unwrap();
    assert!(alice.buddy_list().await.unwrap().buddies.is_empty());

    burrow.shutdown().await;
}

#[tokio::test]
async fn dm_flow_attachments_receipts_offline() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;
    let mut bob = login(&burrow, "bob", "pw-pw-pw").await;

    // Live DM with an attachment.
    let blob = alice
        .blob_put(BlobPurpose::Avatar, b"attachment-bytes".to_vec())
        .await
        .unwrap();
    let mut msg = DmSend::new("bob", "check this out");
    msg.attachments = vec![blob];
    let sent = alice.dm_send(&msg).await.unwrap();
    assert!(sent.id > 0);

    let frame = wait_push_named("for-bob", &mut bob, |f| f.decode::<DmReceived>().is_some()).await;
    let received = frame.decode::<DmReceived>().unwrap().unwrap().message;
    assert_eq!(received.from, "alice");
    assert_eq!(received.attachments, vec![blob]);

    // Threads show 1 unread; mark read sends alice a receipt.
    let threads = bob.dm_threads().await.unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].unread, 1);
    bob.dm_mark_read("alice", received.id).await.unwrap();
    let frame = wait_push_named("for-alice", &mut alice, |f| {
        f.decode::<DmReadReceipt>().is_some()
    })
    .await;
    let receipt = frame.decode::<DmReadReceipt>().unwrap().unwrap();
    assert_eq!(receipt.by, "bob");

    // Offline queueing: bob logs out, alice writes, bob logs in → delivered.
    bob.close().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    alice
        .dm_send(&DmSend::new("bob", "while you were out"))
        .await
        .unwrap();
    let mut bob2 = login(&burrow, "bob", "pw-pw-pw").await;
    let frame = wait_push_named("for-bob2", &mut bob2, |f| {
        f.decode::<DmReceived>().is_some()
    })
    .await;
    let m = frame.decode::<DmReceived>().unwrap().unwrap().message;
    assert_eq!(m.text, "while you were out");

    // History pagination sees all three messages (2 alice→bob, 1 auto? none).
    let history = bob2.dm_history("alice", 0, 50).await.unwrap();
    assert_eq!(history.len(), 2);

    burrow.shutdown().await;
}

#[tokio::test]
async fn dm_blocks_and_away_autoresponse() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("pest", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice", "pw-pw-pw").await;
    let mut pest = login(&burrow, "pest", "pw-pw-pw").await;

    // Away auto-response, exactly once per away period.
    alice
        .presence_set(PresenceState::Away, Some("at the tea party".into()))
        .await
        .unwrap();
    pest.dm_send(&DmSend::new("alice", "hello?")).await.unwrap();
    let frame = wait_push_named("for-pest", &mut pest, |f| {
        f.decode::<DmReceived>()
            .and_then(Result::ok)
            .is_some_and(|m| m.message.is_auto)
    })
    .await;
    let auto = frame.decode::<DmReceived>().unwrap().unwrap().message;
    assert_eq!(auto.text, "at the tea party");
    // Second DM: no second auto-response.
    pest.dm_send(&DmSend::new("alice", "hello??"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // (Would have arrived by now; the next push we see must not be auto.)

    // Blocking: alice blocks pest; DMs both ways are Forbidden.
    alice.block_add("pest").await.unwrap();
    assert!(matches!(
        pest.dm_send(&DmSend::new("alice", "let me in")).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice.dm_send(&DmSend::new("pest", "no")).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    let list = alice.buddy_list().await.unwrap();
    assert_eq!(list.blocked, vec!["pest".to_string()]);

    // Unblock restores DMs.
    alice.block_remove("pest").await.unwrap();
    alice
        .presence_set(PresenceState::Online, None)
        .await
        .unwrap();
    pest.dm_send(&DmSend::new("alice", "truce?")).await.unwrap();

    burrow.shutdown().await;
}
