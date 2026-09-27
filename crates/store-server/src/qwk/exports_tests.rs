use super::exports::*;
use super::*;
use crate::repo::AccountsRepo;
use crate::repo4::{BoardsRepo, ReadMarksRepo};
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
async fn setup() -> (SqlitePool, i64, Vec<ExportBoard>) {
    let pool = crate::open_in_memory().await.unwrap();
    let account = AccountsRepo(&pool)
        .create("alice", None, "Alice", 1, None)
        .await
        .unwrap()
        .id;
    BoardsRepo(&pool)
        .create("alpha", "Alpha", "", 2, None, 0)
        .await
        .unwrap();
    let boards = QwkExportsRepo(&pool).conferences().await.unwrap();
    (pool, account, boards)
}
async fn record(
    pool: &SqlitePool,
    account: i64,
    boards: &[ExportBoard],
    refs: &[ExportReference],
    at: i64,
) -> String {
    let repo = QwkExportsRepo(pool);
    let id = repo.new_id().await.unwrap();
    repo.record(&id, account, "WARREN", boards, refs, &[], at)
        .await
        .unwrap()
        .unwrap();
    id
}
fn row() -> PostRow {
    PostRow {
        event_id: [2; 32],
        board_slug: "alpha".into(),
        root_id: Some([2; 32]),
        parent_id: None,
        author: "alice@home".into(),
        subject: "reply".into(),
        body: "body".into(),
        mime: "text/plain".into(),
        created_at: now(),
        edited: false,
        tombstoned: false,
        event_blob: vec![1],
    }
}
#[tokio::test]
async fn exports_bound_account_expiry_and_read_marks_atomically() {
    let (pool, account, boards) = setup().await;
    let repo = QwkExportsRepo(&pool);
    let at = now();
    let first = record(&pool, account, &boards, &[], at).await;
    assert_eq!(
        repo.bbs_id(account, &first, at).await.unwrap().as_deref(),
        Some("WARREN")
    );
    assert!(repo
        .bbs_id(account + 1, &first, at)
        .await
        .unwrap()
        .is_none());
    for i in 1..9 {
        record(&pool, account, &boards, &[], at + i).await;
    }
    assert!(repo.bbs_id(account, &first, at).await.unwrap().is_none());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM qwk_exports WHERE account_id=?")
        .bind(account)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, MAX_EXPORTS);
    // A failure after writing the new snapshot rolls back both it and marks.
    sqlx::query("CREATE TRIGGER fail_marks BEFORE INSERT ON read_marks BEGIN SELECT RAISE(ABORT,'failure'); END").execute(&pool).await.unwrap();
    let id = repo.new_id().await.unwrap();
    assert!(repo
        .record(
            &id,
            account,
            "WARREN",
            &boards,
            &[],
            &[("alpha".into(), 123)],
            at
        )
        .await
        .is_err());
    assert!(repo.bbs_id(account, &id, at).await.unwrap().is_none());
    assert_eq!(ReadMarksRepo(&pool).get(account, "alpha").await.unwrap(), 0);
    sqlx::query("DROP TRIGGER fail_marks")
        .execute(&pool)
        .await
        .unwrap();
    assert!(repo
        .record(
            &id,
            account,
            "WARREN",
            &boards,
            &[],
            &[("alpha".into(), 123)],
            at
        )
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        ReadMarksRepo(&pool).get(account, "alpha").await.unwrap(),
        123
    );
    assert!(repo
        .bbs_id(account, &id, at + EXPORT_TTL_MS)
        .await
        .unwrap()
        .is_none());
    sqlx::query("DELETE FROM accounts WHERE id=?")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM qwk_export_boards")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[tokio::test]
async fn immutable_board_parent_and_transaction_recheck() {
    let (pool, account, boards) = setup().await;
    let repo = QwkExportsRepo(&pool);
    let parent = row();
    PostsRepo(&pool).insert(&parent).await.unwrap();
    let id = record(
        &pool,
        account,
        &boards,
        &[ExportReference {
            conference: 1,
            number: 4,
            event_id: parent.event_id,
        }],
        now(),
    )
    .await;
    let mut g = ExportGuard {
        export_id: id,
        conference: 1,
        reference: 4,
    };
    let r = repo.resolve(account, &g, now()).await.unwrap().unwrap();
    assert_eq!(r.parent, Some(parent.event_id));
    let mut reply = row();
    reply.event_id = [3; 32];
    reply.parent_id = Some(parent.event_id);
    reply.root_id = Some(parent.event_id);
    // Delete after resolution, before the receipt transaction: no post/receipt.
    PostsRepo(&pool)
        .delete_thread(&parent.event_id)
        .await
        .unwrap();
    assert!(QwkRepliesRepo(&pool)
        .post_once_for_export(account, &[3; 32], &reply, 0, &g)
        .await
        .unwrap()
        .is_none());
    g.reference = 0;
    assert!(repo.resolve(account, &g, now()).await.unwrap().is_some());
    sqlx::query("DELETE FROM boards WHERE slug='alpha'")
        .execute(&pool)
        .await
        .unwrap();
    BoardsRepo(&pool)
        .create("alpha", "New", "", 2, None, 0)
        .await
        .unwrap();
    assert!(repo.resolve(account, &g, now()).await.unwrap().is_none());
    let id = repo.new_id().await.unwrap();
    assert!(repo
        .record(
            &id,
            account,
            "WARREN",
            &boards,
            &[],
            &[("alpha".into(), 999)],
            now()
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(ReadMarksRepo(&pool).get(account, "alpha").await.unwrap(), 0);
}
#[tokio::test]
async fn stale_event_timestamp_cannot_extend_export_expiry() {
    let (pool, account, boards) = setup().await;
    let at = now() - EXPORT_TTL_MS - 1000;
    let id = record(&pool, account, &boards, &[], at).await;
    // Reinsert an already-expired header to model a packet resolved before a
    // writer-lock wait. Minted event time is old, commit time must be fresh.
    sqlx::query("UPDATE qwk_exports SET expires_at=? WHERE id=?")
        .bind(now() - 1)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let mut post = row();
    post.created_at = at;
    let g = ExportGuard {
        export_id: id,
        conference: 1,
        reference: 0,
    };
    assert!(QwkRepliesRepo(&pool)
        .post_once_for_export(account, &[1; 32], &post, 0, &g)
        .await
        .unwrap()
        .is_none());
    assert!(PostsRepo(&pool)
        .by_id(&post.event_id)
        .await
        .unwrap()
        .is_none());
}
