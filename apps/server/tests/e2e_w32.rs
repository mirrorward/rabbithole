//! Wave 3.2 end-to-end tests: the Wishing Well — make/vote/list/claim/
//! fulfill/withdraw, plus the requester's status-change push.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::wish::{WishSetStatus, WishUpdated};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

// Status codes (mirror handlers7).
const OPEN: u8 = 0;
const CLAIMED: u8 = 1;
const FULFILLED: u8 = 2;
const DECLINED: u8 = 3;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Wishing Warren".into(),
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

async fn guest(burrow: &Burrow) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_guest(Some("Dormouse".into())).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

/// Await a `WishUpdated` push (skipping any other pushes), or fail.
async fn expect_wish_push(c: &mut Client) -> WishUpdated {
    for _ in 0..20 {
        if let Ok(Some(frame)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), c.next_push())
                .await
                .map(|r| r.unwrap())
        {
            if let Some(Ok(u)) = frame.decode::<WishUpdated>() {
                return u;
            }
        }
    }
    panic!("no WishUpdated push arrived");
}

#[tokio::test]
async fn wishing_well_lifecycle_and_push() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for (name, role) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("mod", Role::Moderator),
    ] {
        burrow
            .shared
            .auth
            .create_account(name, "pw-pw-pw", role)
            .await
            .unwrap();
    }

    // Guests may browse but not wish.
    let mut anon = guest(&burrow).await;
    assert!(anon.wishes(None, 50).await.unwrap().is_empty());
    assert!(matches!(
        anon.wish_create(0, "want stuff", "").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;

    // Alice makes a wish.
    let wish = alice
        .wish_create(0, "Want the 1997 shareware CD", "the good one")
        .await
        .unwrap();
    assert_eq!(wish.status, OPEN);
    assert_eq!(wish.votes, 0);
    assert!(
        wish.requester.starts_with("alice@"),
        "requester is persona@origin"
    );

    // Voting toggles and counts.
    assert_eq!(bob.wish_vote(wish.id).await.unwrap().votes, 1);
    assert_eq!(alice.wish_vote(wish.id).await.unwrap().votes, 2);
    assert_eq!(
        bob.wish_vote(wish.id).await.unwrap().votes,
        1,
        "bob un-votes"
    );

    // Guests can't vote either.
    assert!(matches!(
        anon.wish_vote(wish.id).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Listing: appears under "all" and "open", not under "claimed".
    assert_eq!(alice.wishes(None, 50).await.unwrap().len(), 1);
    assert_eq!(alice.wishes(Some(OPEN), 50).await.unwrap().len(), 1);
    assert!(alice.wishes(Some(CLAIMED), 50).await.unwrap().is_empty());

    // Bob claims it (regular users have FILE_UPLOAD). Alice, the requester,
    // gets a push.
    let claimed = bob
        .wish_set_status(&WishSetStatus::new(wish.id, CLAIMED))
        .await
        .unwrap();
    assert_eq!(claimed.status, CLAIMED);
    assert!(claimed.claimed_by.as_deref().unwrap().starts_with("bob@"));
    let push = expect_wish_push(&mut alice).await;
    assert_eq!(push.wish.id, wish.id);
    assert_eq!(push.wish.status, CLAIMED);

    // Now it's under "claimed", not "open".
    assert!(alice.wishes(Some(OPEN), 50).await.unwrap().is_empty());
    assert_eq!(alice.wishes(Some(CLAIMED), 50).await.unwrap().len(), 1);

    // Bob (the claimer) fulfills it with a link; claim is preserved.
    let done = bob
        .wish_set_status(
            &WishSetStatus::new(wish.id, FULFILLED).with_fulfillment("rabbit://host/abc"),
        )
        .await
        .unwrap();
    assert_eq!(done.status, FULFILLED);
    assert_eq!(done.fulfillment.as_deref(), Some("rabbit://host/abc"));
    assert!(done.claimed_by.as_deref().unwrap().starts_with("bob@"));
    let push = expect_wish_push(&mut alice).await;
    assert_eq!(push.wish.status, FULFILLED);

    burrow.shutdown().await;
}

#[tokio::test]
async fn withdraw_and_decline_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for (name, role) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("mod", Role::Moderator),
    ] {
        burrow
            .shared
            .auth
            .create_account(name, "pw-pw-pw", role)
            .await
            .unwrap();
    }

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let mut modr = login(&burrow, "mod").await;

    let w1 = alice
        .wish_create(2, "feature: dark mode everywhere", "")
        .await
        .unwrap();
    let w2 = alice
        .wish_create(2, "feature: keybindings", "")
        .await
        .unwrap();

    // A non-owner, non-moderator can't decline someone else's wish.
    assert!(matches!(
        bob.wish_set_status(&WishSetStatus::new(w1.id, DECLINED))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // The requester may withdraw her own (decline).
    let withdrawn = alice
        .wish_set_status(&WishSetStatus::new(w1.id, DECLINED))
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DECLINED);

    // A moderator may decline another's wish.
    let declined = modr
        .wish_set_status(&WishSetStatus::new(w2.id, DECLINED))
        .await
        .unwrap();
    assert_eq!(declined.status, DECLINED);

    // Unknown wish id → NotFound.
    assert!(matches!(
        alice
            .wish_set_status(&WishSetStatus::new(999_999, OPEN))
            .await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    burrow.shutdown().await;
}
