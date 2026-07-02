//! Wave 2.2b end-to-end tests: rooms — public/private, invites,
//! membership-scoped delivery, moderation.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::chat::{ChatMessage, RoomCreate, RoomInvited, RoomKicked};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Rooms Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

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
async fn public_rooms_scoped_delivery_and_topic() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for u in ["alice", "bob", "carol"] {
        burrow
            .shared
            .auth
            .create_account(u, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let mut carol = login(&burrow, "carol").await;

    // Create a categorized public room; listing shows it to everyone.
    let mut create = RoomCreate::new("Tea Party", false);
    create.category = "Social".into();
    create.topic = "unbirthdays".into();
    let room = alice.room_create(&create).await.unwrap();
    assert_eq!(room.category, "Social");
    let listed = bob.room_list().await.unwrap();
    assert!(listed
        .iter()
        .any(|r| r.name == "Tea Party" && r.topic == "unbirthdays"));
    assert_eq!(listed[0].name, "lobby", "lobby sorts first");

    // Bob joins (case-insensitive); carol does not.
    bob.room_join("tea party").await.unwrap();
    assert_eq!(
        alice.room_members("Tea Party").await.unwrap(),
        vec!["alice", "bob"]
    );

    // Sending without membership is refused.
    assert!(matches!(
        carol.chat_send("Tea Party", "crashing").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Members get room chat; non-members don't (carol only sees lobby chat).
    alice.chat_send("Tea Party", "clean cups!").await.unwrap();
    wait_push_named("bob-room-chat", &mut bob, |f| {
        f.decode::<ChatMessage>()
            .and_then(Result::ok)
            .is_some_and(|m| m.room == "Tea Party" && m.text == "clean cups!")
    })
    .await;
    alice.chat_send("lobby", "hello everyone").await.unwrap();
    let frame = wait_push_named("carol-chat", &mut carol, |f| {
        f.decode::<ChatMessage>().is_some()
    })
    .await;
    let m = frame.decode::<ChatMessage>().unwrap().unwrap();
    assert_eq!(m.room, "lobby", "carol must not receive Tea Party chat");

    // Topic: only creator or moderator.
    assert!(bob.room_topic("Tea Party", "hijack").await.is_err());
    alice
        .room_topic("Tea Party", "very merry unbirthday")
        .await
        .unwrap();

    // History respects the same visibility as membership (public = open).
    let h = carol.chat_history("Tea Party", 10).await.unwrap();
    assert!(h.iter().any(|m| m.text == "clean cups!"));

    burrow.shutdown().await;
}

#[tokio::test]
async fn private_rooms_invite_flow_and_reaping() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for u in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(u, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;

    alice
        .room_create(&RoomCreate::new("conspiracy", true))
        .await
        .unwrap();

    // Hidden from bob's list; join refused.
    assert!(!bob
        .room_list()
        .await
        .unwrap()
        .iter()
        .any(|r| r.name == "conspiracy"));
    assert!(matches!(
        bob.room_join("conspiracy").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Invite: bob gets the push, sees the room, joins, chats.
    alice.room_invite("conspiracy", "bob").await.unwrap();
    let frame = wait_push_named("bob-invited", &mut bob, |f| {
        f.decode::<RoomInvited>().is_some()
    })
    .await;
    let invite = frame.decode::<RoomInvited>().unwrap().unwrap();
    assert_eq!(invite.from, "alice");
    assert!(bob
        .room_list()
        .await
        .unwrap()
        .iter()
        .any(|r| r.name == "conspiracy"));
    bob.room_join("conspiracy").await.unwrap();
    bob.chat_send("conspiracy", "the walrus was paul")
        .await
        .unwrap();

    // Both leave → ad-hoc room reaps; the lobby cannot be left.
    assert!(matches!(
        alice.room_leave("lobby").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    alice.room_leave("conspiracy").await.unwrap();
    bob.room_leave("conspiracy").await.unwrap();
    assert!(matches!(
        bob.room_join("conspiracy").await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn room_kick_ban_and_guest_limits() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for u in ["alice", "pest"] {
        burrow
            .shared
            .auth
            .create_account(u, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let mut alice = login(&burrow, "alice").await;
    let mut pest = login(&burrow, "pest").await;

    alice
        .room_create(&RoomCreate::new("garden", false))
        .await
        .unwrap();
    pest.room_join("garden").await.unwrap();

    // Non-creator can't kick.
    assert!(pest.room_kick("garden", "alice", false).await.is_err());

    // Creator kicks + bans: pest gets the push and can't rejoin or send.
    alice.room_kick("garden", "pest", true).await.unwrap();
    let frame = wait_push_named("pest-kicked", &mut pest, |f| {
        f.decode::<RoomKicked>().is_some()
    })
    .await;
    let kicked = frame.decode::<RoomKicked>().unwrap().unwrap();
    assert!(kicked.banned);
    assert!(matches!(
        pest.room_join("garden").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        pest.chat_send("garden", "im back").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // A fresh invite forgives the ban.
    alice.room_invite("garden", "pest").await.unwrap();
    pest.room_join("garden").await.unwrap();

    // Guests can join public rooms but not create rooms.
    let mut guest = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("Dormouse".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    guest.room_join("garden").await.unwrap();
    assert!(matches!(
        guest.room_create(&RoomCreate::new("nope", false)).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    burrow.shutdown().await;
}
