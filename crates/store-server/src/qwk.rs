//! Durable QWK REP receipts, committed with the signed post and projection.
//!
//! The database is the server scope and this table is the network namespace.
//! Account and canonical board scope prevent one uploader or board from
//! suppressing another. Receipts outlive post retention, but are removed on
//! account deletion or after the documented 30-day replay window.

use crate::repo4::{PostRow, PostsRepo};
use crate::{SqlitePool, StoreError};

/// Match the shared in-memory seen store's existing 30-day window. A replay
/// exactly on the boundary is still a duplicate; older receipts expire.
pub const REPLY_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub struct QwkRepliesRepo<'a>(pub &'a SqlitePool);

impl QwkRepliesRepo<'_> {
    /// Store a validated, signed post and its successful-import receipt in
    /// one transaction. Returns true only for a newly stored post; duplicates
    /// do not update the receipt's original acceptance time.
    ///
    /// The write lock precedes every read/check so concurrent importers cannot
    /// both claim a receipt. Any error, including retention, rolls back all
    /// changes. Notifications belong after this method returns successfully.
    pub async fn post_once(
        &self,
        account_id: i64,
        digest: &[u8; 32],
        post: &PostRow,
        max_threads: i64,
    ) -> Result<bool, StoreError> {
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM qwk_reply_receipts WHERE accepted_at < ?")
            .bind(post.created_at.saturating_sub(REPLY_RETENTION_MS))
            .execute(&mut *tx)
            .await?;
        let claimed = sqlx::query(
            "INSERT INTO qwk_reply_receipts
                 (account_id, board_slug, digest, event_id, accepted_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (account_id, board_slug, digest) DO NOTHING",
        )
        .bind(account_id)
        .bind(&post.board_slug)
        .bind(digest.as_slice())
        .bind(post.event_id.as_slice())
        .bind(post.created_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            tx.commit().await?;
            return Ok(false);
        }
        // If this exact signed event already exists, keep the receipt as a
        // successful association but report a duplicate, not another post.
        let inserted = PostsRepo::insert_on(&mut tx, post).await?;

        // Match BoardService's ordinary post retention, but keep both thread
        // and follow-up deletion in this transaction with the new post.
        if post.parent_id.is_none() && max_threads > 0 {
            let roots: Vec<Vec<u8>> = sqlx::query_scalar(
                "SELECT event_id FROM posts WHERE board_slug = ? AND parent_id IS NULL
                 ORDER BY created_at DESC LIMIT -1 OFFSET ?",
            )
            .bind(&post.board_slug)
            .bind(max_threads)
            .fetch_all(&mut *tx)
            .await?;
            for root in roots {
                sqlx::query("DELETE FROM posts WHERE event_id = ?1 OR root_id = ?1")
                    .bind(&root)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM board_followups WHERE root_id = ?")
                    .bind(&root)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(inserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::AccountsRepo;
    use crate::repo4::{BoardRow, BoardsRepo, FollowupRow, FollowupsRepo};

    async fn account(pool: &SqlitePool, login: &str) -> i64 {
        AccountsRepo(pool)
            .create(login, None, login, 1, None)
            .await
            .unwrap()
            .id
    }

    async fn board(pool: &SqlitePool) -> BoardRow {
        BoardsRepo(pool)
            .create("alpha", "Alpha", "", 2, None, 1)
            .await
            .unwrap()
    }

    fn post(id: u8, at: i64) -> PostRow {
        PostRow {
            event_id: [id; 32],
            board_slug: "alpha".into(),
            root_id: Some([id; 32]),
            parent_id: None,
            author: "alice@home".into(),
            subject: format!("post {id}"),
            body: "body".into(),
            mime: "text/plain".into(),
            created_at: at,
            edited: false,
            tombstoned: false,
            event_blob: vec![id; 8],
        }
    }

    async fn receipts(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM qwk_reply_receipts")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn scopes_expiry_and_account_cleanup() {
        let pool = crate::open_in_memory().await.unwrap();
        board(&pool).await;
        let alice = account(&pool, "alice").await;
        let bob = account(&pool, "bob").await;
        let repo = QwkRepliesRepo(&pool);
        let at = 1000;
        assert!(repo
            .post_once(alice, &[1; 32], &post(1, at), 0)
            .await
            .unwrap());
        assert!(repo
            .post_once(bob, &[1; 32], &post(2, at), 0)
            .await
            .unwrap());
        let mut other_board = post(3, at);
        other_board.board_slug = "beta".into();
        assert!(repo
            .post_once(alice, &[1; 32], &other_board, 0)
            .await
            .unwrap());

        // Pruning the original post cannot erase its import receipt.
        PostsRepo(&pool).delete_thread(&[1; 32]).await.unwrap();
        let mut replay = post(4, at + REPLY_RETENTION_MS);
        replay.board_slug = "ALPHA".into();
        assert!(!repo.post_once(alice, &[1; 32], &replay, 0).await.unwrap());
        assert!(PostsRepo(&pool).by_id(&[4; 32]).await.unwrap().is_none());
        let accepted: i64 = sqlx::query_scalar(
            "SELECT accepted_at FROM qwk_reply_receipts WHERE account_id = ? AND board_slug = 'alpha'",
        )
        .bind(alice)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(accepted, at, "replays do not extend retention");
        replay.created_at += 1;
        assert!(repo.post_once(alice, &[1; 32], &replay, 0).await.unwrap());
        assert_eq!(receipts(&pool).await, 1, "expired rows were pruned");
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(alice)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(receipts(&pool).await, 0);
        assert!(PostsRepo(&pool).by_id(&[4; 32]).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn failed_post_and_failed_retention_roll_back_and_remain_retryable() {
        let pool = crate::open_in_memory().await.unwrap();
        board(&pool).await;
        let alice = account(&pool, "alice").await;
        let repo = QwkRepliesRepo(&pool);
        sqlx::query(
            "CREATE TRIGGER fail_post BEFORE INSERT ON posts
             BEGIN SELECT RAISE(ABORT, 'injected post failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(repo
            .post_once(alice, &[1; 32], &post(1, 1000), 1)
            .await
            .is_err());
        assert_eq!(receipts(&pool).await, 0);
        assert!(PostsRepo(&pool).by_id(&[1; 32]).await.unwrap().is_none());
        sqlx::query("DROP TRIGGER fail_post")
            .execute(&pool)
            .await
            .unwrap();
        assert!(repo
            .post_once(alice, &[1; 32], &post(1, 1000), 1)
            .await
            .unwrap());
        FollowupsRepo(&pool)
            .insert(&FollowupRow {
                event_id: [9; 32],
                target_id: [1; 32],
                root_id: [1; 32],
                board_slug: "alpha".into(),
                kind: 1,
                origin: "home".into(),
                applied: true,
                created_at: 1001,
                event_blob: vec![9],
            })
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_retention BEFORE DELETE ON board_followups
             BEGIN SELECT RAISE(ABORT, 'injected retention failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(repo
            .post_once(alice, &[2; 32], &post(2, 2000), 1)
            .await
            .is_err());
        assert_eq!(receipts(&pool).await, 1);
        assert!(PostsRepo(&pool).by_id(&[2; 32]).await.unwrap().is_none());
        assert!(PostsRepo(&pool).by_id(&[1; 32]).await.unwrap().is_some());
        assert!(FollowupsRepo(&pool)
            .by_id(&[9; 32])
            .await
            .unwrap()
            .is_some());
        sqlx::query("DROP TRIGGER fail_retention")
            .execute(&pool)
            .await
            .unwrap();
        assert!(repo
            .post_once(alice, &[2; 32], &post(2, 2000), 1)
            .await
            .unwrap());
        assert_eq!(
            receipts(&pool).await,
            2,
            "old post's receipt survives retention"
        );
        assert!(PostsRepo(&pool).by_id(&[1; 32]).await.unwrap().is_none());
        assert!(FollowupsRepo(&pool)
            .by_id(&[9; 32])
            .await
            .unwrap()
            .is_none());
        assert!(!repo
            .post_once(alice, &[1; 32], &post(3, 3000), 1)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn existing_signed_event_gets_receipt_without_being_posted_again() {
        let pool = crate::open_in_memory().await.unwrap();
        let alice = account(&pool, "alice").await;
        let row = post(1, 1000);
        PostsRepo(&pool).insert(&row).await.unwrap();
        assert!(!QwkRepliesRepo(&pool)
            .post_once(alice, &[1; 32], &row, 0)
            .await
            .unwrap());
        assert_eq!(receipts(&pool).await, 1);
        assert!(!QwkRepliesRepo(&pool)
            .post_once(alice, &[1; 32], &post(2, 2000), 0)
            .await
            .unwrap());
        assert!(PostsRepo(&pool).by_id(&[2; 32]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn concurrent_importers_commit_one_post_and_reopen_keeps_receipt() {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("qwk.sqlite3");
        let pool = crate::open(&path).await.unwrap();
        let alice = account(&pool, "alice").await;
        let repo = QwkRepliesRepo(&pool);
        let first = post(1, 1000);
        let second = post(2, 1001);
        let (a, b) = tokio::join!(
            repo.post_once(alice, &[1; 32], &first, 0),
            repo.post_once(alice, &[1; 32], &second, 0),
        );
        assert_ne!(a.unwrap(), b.unwrap(), "exactly one importer wins");
        assert_eq!(receipts(&pool).await, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        pool.close().await;
        let pool = crate::open(&path).await.unwrap();
        assert!(!QwkRepliesRepo(&pool)
            .post_once(alice, &[1; 32], &post(3, 2000), 0)
            .await
            .unwrap());
        assert!(PostsRepo(&pool).by_id(&[3; 32]).await.unwrap().is_none());
        pool.close().await;
    }

    #[tokio::test]
    async fn upgrade_adds_receipts_without_changing_existing_posts() {
        let pool = crate::open_in_memory().await.unwrap();
        let alice = account(&pool, "alice").await;
        let row = post(1, 1000);
        PostsRepo(&pool).insert(&row).await.unwrap();
        sqlx::query("DROP TABLE qwk_reply_receipts")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 19")
            .execute(&pool)
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        assert_eq!(PostsRepo(&pool).by_id(&[1; 32]).await.unwrap(), Some(row));
        assert_eq!(receipts(&pool).await, 0);
        assert!(QwkRepliesRepo(&pool)
            .post_once(alice, &[2; 32], &post(2, 2000), 0)
            .await
            .unwrap());
    }
}
