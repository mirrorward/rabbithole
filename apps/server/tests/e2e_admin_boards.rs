//! Boards, as an operator manages them over the wire: make one, change what it
//! says, remove one that is empty, and never one that is not.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::board::{
    BoardCreate, BoardCreated, BoardDelete, BoardList, BoardListRequest, BoardUpdate, PostCreate,
    PostDelete,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

const PW: &str = "pw-pw-pw-pw";

async fn start(dir: &std::path::Path) -> Burrow {
    let config = ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(config).await.unwrap();
    for (login, role) in [("mo", Role::Moderator), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(login, PW, role)
            .await
            .unwrap();
    }
    burrow
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    c.auth_password(user, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_board_is_made_edited_and_removed_only_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;

    // Made.
    let mut create = BoardCreate::new("tea-party", "Tea Party", 2);
    create.description = "Unbirthdays only.".into();
    let made: BoardCreated = mo.request(&create).await.unwrap();
    assert_eq!(made.board.slug, "tea-party");

    // A slug is an address: some things are not addresses. A title is required.
    for bad in ["Tea Party", "", "-dash-first", "UPPER", "sl/ash"] {
        refused(
            mo.request::<_, BoardCreated>(&BoardCreate::new(bad, "Title", 2))
                .await,
            ErrorCode::BadRequest,
        );
    }
    refused(
        mo.request::<_, BoardCreated>(&BoardCreate::new("untitled", "  ", 2))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        mo.request::<_, BoardCreated>(&BoardCreate::new("tea-party", "Again", 2))
            .await,
        ErrorCode::AlreadyExists,
    );
    let mut orphan = BoardCreate::new("orphan", "Orphan", 2);
    orphan.parent_slug = Some("no-such-category".into());
    refused(
        mo.request::<_, BoardCreated>(&orphan).await,
        ErrorCode::NotFound,
    );

    // Edited: everything but the slug.
    mo.request_ack(&BoardUpdate::new(
        "tea-party",
        "The Tea Party",
        "Clean cups.",
        Some(50),
    ))
    .await
    .unwrap();
    let list: BoardList = mo.request(&BoardListRequest).await.unwrap();
    let board = list.boards.iter().find(|b| b.slug == "tea-party").unwrap();
    assert_eq!(board.title, "The Tea Party");
    assert_eq!(board.description, "Clean cups.");
    // Retitling alone leaves the retention where it was.
    mo.request_ack(&BoardUpdate::new(
        "tea-party",
        "The Tea Party",
        "Clean cups.",
        None,
    ))
    .await
    .unwrap();
    let kept = burrow
        .shared
        .boards
        .board("tea-party")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.max_threads, 50);
    refused(
        mo.request_ack(&BoardUpdate::new("tea-party", "", "x", None))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        mo.request_ack(&BoardUpdate::new("nowhere", "Title", "", None))
            .await,
        ErrorCode::NotFound,
    );

    // A member does none of this.
    refused(
        alice
            .request::<_, BoardCreated>(&BoardCreate::new("mine", "Mine", 2))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        alice
            .request_ack(&BoardUpdate::new("tea-party", "Hacked", "", None))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        alice.request_ack(&BoardDelete::new("tea-party")).await,
        ErrorCode::Forbidden,
    );

    // With a post in it, it stays.
    let posted = alice
        .post(&PostCreate::new("tea-party", "First", "Is there any tea?"))
        .await
        .unwrap();
    refused(
        mo.request_ack(&BoardDelete::new("tea-party")).await,
        ErrorCode::BadRequest,
    );

    // A category with a board inside it stays too.
    mo.request::<_, BoardCreated>(&BoardCreate::new("garden", "Garden", 0))
        .await
        .unwrap();
    let mut inner = BoardCreate::new("roses", "Roses", 2);
    inner.parent_slug = Some("garden".into());
    mo.request::<_, BoardCreated>(&inner).await.unwrap();
    refused(
        mo.request_ack(&BoardDelete::new("garden")).await,
        ErrorCode::BadRequest,
    );
    mo.request_ack(&BoardDelete::new("roses")).await.unwrap();
    mo.request_ack(&BoardDelete::new("garden")).await.unwrap();
    refused(
        mo.request_ack(&BoardDelete::new("garden")).await,
        ErrorCode::NotFound,
    );

    // A moderator can take a post down; the thread list no longer shows it live.
    mo.request_ack(&PostDelete::new(posted.id)).await.unwrap();
    let threads = mo.threads("tea-party", 20).await.unwrap();
    assert!(threads
        .iter()
        .all(|t| t.root.id != posted.id || t.root.tombstoned));

    let list: BoardList = mo.request(&BoardListRequest).await.unwrap();
    assert!(list
        .boards
        .iter()
        .all(|b| b.slug != "garden" && b.slug != "roses"));
    burrow.shutdown().await;
}

/// Boards sit where an operator puts them: the order is theirs to set,
/// and a slug — which addresses and other burrows carry — never changes
/// for it. Moderators only, among a board's own siblings.
#[tokio::test]
async fn boards_are_read_in_the_order_an_operator_puts_them() {
    use rabbithole_proto::board::{BoardList, BoardListRequest, BoardMove};

    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;

    for slug in ["announcements", "general", "swaps"] {
        let _: rabbithole_proto::board::BoardCreated =
            mo.request(&BoardCreate::new(slug, slug, 2)).await.unwrap();
    }
    async fn order(c: &mut Client) -> Vec<String> {
        let list: BoardList = c.request(&BoardListRequest).await.unwrap();
        list.boards.into_iter().map(|b| b.slug).collect()
    }
    assert_eq!(
        order(&mut mo).await,
        ["announcements", "general", "swaps"],
        "as they always were: by name"
    );

    // Swaps to the top, then after announcements.
    mo.request_ack(&BoardMove::new("swaps", None))
        .await
        .unwrap();
    assert_eq!(order(&mut mo).await, ["swaps", "announcements", "general"]);
    mo.request_ack(&BoardMove::new("swaps", Some("announcements".into())))
        .await
        .unwrap();
    assert_eq!(order(&mut mo).await, ["announcements", "swaps", "general"]);
    // Everybody reads them in that order, not only the operator.
    assert_eq!(
        order(&mut alice).await,
        ["announcements", "swaps", "general"]
    );

    // Not anybody's to arrange, and not a board that is not there.
    refused(
        alice.request_ack(&BoardMove::new("swaps", None)).await,
        ErrorCode::Forbidden,
    );
    refused(
        mo.request_ack(&BoardMove::new("nowhere", None)).await,
        ErrorCode::NotFound,
    );
    assert_eq!(order(&mut mo).await, ["announcements", "swaps", "general"]);

    burrow.shutdown().await;
}

/// What a board keeps is something the console can see as well as set: a
/// board listing carries neither the limit nor how many threads there are,
/// so retention could be changed blind. Moderators only, and a board that
/// is over its limit drops its oldest thread when the next one starts.
#[tokio::test]
async fn what_a_board_keeps_is_shown_and_set() {
    use rabbithole_proto::board::{
        BoardKeeping, BoardKeepingRequest, BoardUpdate, PostCreate, PostReply,
    };

    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;

    let _: rabbithole_proto::board::BoardCreated = mo
        .request(&BoardCreate::new("swaps", "Swaps", 2))
        .await
        .unwrap();
    async fn kept(c: &mut Client) -> rabbithole_proto::board::BoardKept {
        let list: BoardKeeping = c.request(&BoardKeepingRequest).await.unwrap();
        list.boards
            .into_iter()
            .find(|b| b.slug == "swaps")
            .expect("the board")
    }

    // Nothing set, nothing posted.
    let now = kept(&mut mo).await;
    assert_eq!((now.max_threads, now.threads), (0, 0));

    // Three threads, and a limit of two.
    for subject in ["one", "two", "three"] {
        let _: PostReply = alice
            .request(&PostCreate::new("swaps", subject, "hello"))
            .await
            .unwrap();
    }
    let now = kept(&mut mo).await;
    assert_eq!((now.max_threads, now.threads), (0, 3));
    mo.request_ack(&BoardUpdate::new("swaps", "Swaps", "", Some(2)))
        .await
        .unwrap();
    let now = kept(&mut mo).await;
    assert_eq!(
        (now.max_threads, now.threads),
        (2, 3),
        "the limit is shown, not only kept, and lowering it takes nothing \
         away until the next thread starts"
    );

    // The next thread pushes the oldest out, and the count says so.
    let _: PostReply = alice
        .request(&PostCreate::new("swaps", "four", "hello"))
        .await
        .unwrap();
    let now = kept(&mut mo).await;
    assert_eq!(now.threads, 2, "kept to what it keeps: {now:?}");

    // Not everybody's to read.
    let refused = alice.request::<_, BoardKeeping>(&BoardKeepingRequest).await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::Forbidden))),
        "{refused:?}"
    );

    burrow.shutdown().await;
}
