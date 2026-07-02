//! Wave 3.1 end-to-end tests: boards — hierarchy, signed posts, threading,
//! read pointers, edit/tombstone, moderation.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::board::{BoardCreate, PostCreate, PostPosted};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Boards Warren".into(),
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

#[tokio::test]
async fn board_tree_post_thread_unread() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
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

    // Admin builds the tree: a category and a board under it.
    let mut admin = login(&burrow, "admin").await;
    admin
        .board_create(&BoardCreate::new("rabbit", "Rabbit", 0))
        .await
        .unwrap();
    let mut board = BoardCreate::new("rabbit.general", "General", 2);
    board.parent_slug = Some("rabbit".into());
    admin.board_create(&board).await.unwrap();
    // Duplicate slug rejected.
    assert!(matches!(
        admin
            .board_create(&BoardCreate::new("rabbit", "dup", 0))
            .await,
        Err(ClientError::Refused(ErrorCode::AlreadyExists))
    ));

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;

    // Can't post to a category.
    assert!(matches!(
        alice.post(&PostCreate::new("rabbit", "s", "b")).await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    // Alice posts a thread; bob replies. Bob's client gets the PostPosted push.
    let root = alice
        .post(&PostCreate::new("rabbit.general", "Hello", "world"))
        .await
        .unwrap();
    assert_eq!(root.root, Some(root.id), "top-level post is its own root");
    assert!(root.author.starts_with("alice@"));

    let mut saw_push = false;
    for _ in 0..10 {
        if let Ok(Some(frame)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), bob.next_push())
                .await
                .map(|r| r.unwrap())
        {
            if frame.decode::<PostPosted>().is_some() {
                saw_push = true;
                break;
            }
        }
    }
    assert!(saw_push, "board post push delivered");

    let reply = bob
        .post(&PostCreate::new("rabbit.general", "re: Hello", "hi alice").reply_to(root.id))
        .await
        .unwrap();
    assert_eq!(reply.root, Some(root.id));

    // Thread listing: one thread with 1 reply.
    let threads = alice.threads("rabbit.general", 50).await.unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].replies, 1);

    // Full thread: root + reply, oldest first.
    let posts = alice.thread(root.id, 100).await.unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].subject, "Hello");

    // Unread: bob has 2 unread in the board; mark read clears it.
    let boards = bob.boards().await.unwrap();
    let gen = boards.iter().find(|b| b.slug == "rabbit.general").unwrap();
    assert_eq!(gen.unread, 2);
    bob.board_mark_read("rabbit.general", 0).await.unwrap();
    let boards = bob.boards().await.unwrap();
    assert_eq!(
        boards
            .iter()
            .find(|b| b.slug == "rabbit.general")
            .unwrap()
            .unread,
        0
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn signed_posts_verify_under_origin_key() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;
    alice
        .board_create(&BoardCreate::new("b", "B", 2))
        .await
        .unwrap();
    let post = alice.post(&PostCreate::new("b", "s", "b")).await.unwrap();

    // Reach into the server: the stored blob verifies under the server key.
    let row = burrow
        .shared
        .boards
        .post_by_id(&post.id)
        .await
        .unwrap()
        .unwrap();
    assert!(burrow::handlers6::verify_blob(
        &row.event_blob,
        &burrow.shared.server_key
    ));
    // A wrong key rejects.
    assert!(!burrow::handlers6::verify_blob(&row.event_blob, &[0u8; 32]));

    burrow.shutdown().await;
}

#[tokio::test]
async fn edit_tombstone_and_moderation() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("mod", "pw-pw-pw", Role::Moderator)
        .await
        .unwrap();
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

    let mut modr = login(&burrow, "mod").await;
    modr.board_create(&BoardCreate::new("b", "B", 2))
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let post = alice
        .post(&PostCreate::new("b", "orig", "orig body"))
        .await
        .unwrap();

    // Author edits her own post.
    let edited = alice
        .post_edit(post.id, "edited", "new body", "text/markdown")
        .await
        .unwrap();
    assert!(edited.edited && edited.subject == "edited");

    // Bob (not author, not mod) can't edit or delete it.
    assert!(matches!(
        bob.post_edit(post.id, "hijack", "x", "text/plain").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        bob.post_delete(post.id).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Moderator tombstones it; body is cleared, flagged tombstoned.
    modr.post_delete(post.id).await.unwrap();
    let posts = alice.thread(post.id, 10).await.unwrap();
    assert!(posts[0].tombstoned && posts[0].body.is_empty());

    // Guests can't post at all.
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
    assert!(matches!(
        guest.post(&PostCreate::new("b", "s", "b")).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    burrow.shutdown().await;
}
