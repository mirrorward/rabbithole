//! Wave 3.1 repositories: boards, posts, read marks.

use sqlx::Row;

use crate::{SqlitePool, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardRow {
    pub slug: String,
    pub title: String,
    pub description: String,
    /// 0 category, 1 bundle, 2 board.
    pub kind: u8,
    pub parent_slug: Option<String>,
    pub max_threads: i64,
}

fn row_to_board(r: &sqlx::sqlite::SqliteRow) -> BoardRow {
    BoardRow {
        slug: r.get("slug"),
        title: r.get("title"),
        description: r.get("description"),
        kind: r.get::<i64, _>("kind") as u8,
        parent_slug: r.get("parent_slug"),
        max_threads: r.get("max_threads"),
    }
}

pub struct BoardsRepo<'a>(pub &'a SqlitePool);

impl BoardsRepo<'_> {
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        slug: &str,
        title: &str,
        description: &str,
        kind: u8,
        parent_slug: Option<&str>,
        max_threads: i64,
    ) -> Result<BoardRow, StoreError> {
        sqlx::query(
            "INSERT INTO boards (slug, title, description, kind, parent_slug, max_threads, created_at)
             VALUES (?, ?, ?, ?, ?, ?, unixepoch())",
        )
        .bind(slug)
        .bind(title)
        .bind(description)
        .bind(kind as i64)
        .bind(parent_slug)
        .bind(max_threads)
        .execute(self.0)
        .await?;
        Ok(self.by_slug(slug).await?.expect("just inserted"))
    }

    pub async fn by_slug(&self, slug: &str) -> Result<Option<BoardRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM boards WHERE slug = ?")
            .bind(slug)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_board(&r)))
    }

    pub async fn all(&self) -> Result<Vec<BoardRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM boards ORDER BY slug")
            .fetch_all(self.0)
            .await?
            .iter()
            .map(row_to_board)
            .collect())
    }
}

/// A denormalized post projection (the signed blob is `event_blob`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostRow {
    pub event_id: [u8; 32],
    pub board_slug: String,
    pub root_id: Option<[u8; 32]>,
    pub parent_id: Option<[u8; 32]>,
    pub author: String,
    pub subject: String,
    pub body: String,
    pub mime: String,
    pub created_at: i64,
    pub edited: bool,
    pub tombstoned: bool,
    pub event_blob: Vec<u8>,
}

fn opt_id(bytes: Option<Vec<u8>>) -> Option<[u8; 32]> {
    bytes.and_then(|b| b.try_into().ok())
}

fn row_to_post(r: &sqlx::sqlite::SqliteRow) -> PostRow {
    PostRow {
        event_id: r
            .get::<Vec<u8>, _>("event_id")
            .try_into()
            .unwrap_or([0; 32]),
        board_slug: r.get("board_slug"),
        root_id: opt_id(r.get("root_id")),
        parent_id: opt_id(r.get("parent_id")),
        author: r.get("author"),
        subject: r.get("subject"),
        body: r.get("body"),
        mime: r.get("mime"),
        created_at: r.get("created_at"),
        edited: r.get::<i64, _>("edited") != 0,
        tombstoned: r.get::<i64, _>("tombstoned") != 0,
        event_blob: r.get("event_blob"),
    }
}

pub struct PostsRepo<'a>(pub &'a SqlitePool);

impl PostsRepo<'_> {
    /// Insert a new post projection. Idempotent on the content id — a
    /// duplicate (same event flooded twice) is a no-op returning false.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert(&self, post: &PostRow) -> Result<bool, StoreError> {
        let affected = sqlx::query(
            "INSERT INTO posts (event_id, board_slug, root_id, parent_id, author, author_key,
                                origin, subject, body, mime, created_at, event_blob)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(post.event_id.as_slice())
        .bind(&post.board_slug)
        .bind(post.root_id.map(|r| r.to_vec()))
        .bind(post.parent_id.map(|p| p.to_vec()))
        .bind(&post.author)
        .bind(Vec::<u8>::new()) // author_key lives in the blob; column reserved
        .bind("")
        .bind(&post.subject)
        .bind(&post.body)
        .bind(&post.mime)
        .bind(post.created_at)
        .bind(&post.event_blob)
        .execute(self.0)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    pub async fn by_id(&self, id: &[u8; 32]) -> Result<Option<PostRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM posts WHERE event_id = ?")
            .bind(id.as_slice())
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_post(&r)))
    }

    /// Apply an edit follow-up: replace projected text, mark edited.
    pub async fn apply_edit(
        &self,
        target: &[u8; 32],
        subject: &str,
        body: &str,
        mime: &str,
    ) -> Result<bool, StoreError> {
        Ok(sqlx::query(
            "UPDATE posts SET subject = ?, body = ?, mime = ?, edited = 1 WHERE event_id = ?",
        )
        .bind(subject)
        .bind(body)
        .bind(mime)
        .bind(target.as_slice())
        .execute(self.0)
        .await?
        .rows_affected()
            > 0)
    }

    pub async fn apply_tombstone(&self, target: &[u8; 32]) -> Result<bool, StoreError> {
        Ok(sqlx::query(
            "UPDATE posts SET tombstoned = 1, subject = '', body = '' WHERE event_id = ?",
        )
        .bind(target.as_slice())
        .execute(self.0)
        .await?
        .rows_affected()
            > 0)
    }

    /// Top-level threads in a board, newest activity first, with a reply
    /// count and the latest activity time.
    pub async fn threads(
        &self,
        board: &str,
        limit: i64,
    ) -> Result<Vec<(PostRow, i64, i64)>, StoreError> {
        let roots = sqlx::query(
            "SELECT * FROM posts WHERE board_slug = ? AND parent_id IS NULL
             ORDER BY created_at DESC LIMIT ?",
        )
        .bind(board)
        .bind(limit)
        .fetch_all(self.0)
        .await?;
        let mut out = Vec::new();
        for r in &roots {
            let root = row_to_post(r);
            // Replies only: exclude the root itself, which (by convention)
            // carries root_id == its own id.
            let stats = sqlx::query(
                "SELECT COUNT(*) AS n, COALESCE(MAX(created_at), ?2) AS last
                 FROM posts WHERE root_id = ?1 AND event_id != ?1",
            )
            .bind(root.event_id.as_slice())
            .bind(root.created_at)
            .fetch_one(self.0)
            .await?;
            out.push((root, stats.get::<i64, _>("n"), stats.get::<i64, _>("last")));
        }
        Ok(out)
    }

    /// All posts in a thread (root + descendants), oldest first.
    pub async fn thread(&self, root: &[u8; 32], limit: i64) -> Result<Vec<PostRow>, StoreError> {
        Ok(sqlx::query(
            "SELECT * FROM posts WHERE event_id = ?1 OR root_id = ?1
             ORDER BY created_at ASC LIMIT ?2",
        )
        .bind(root.as_slice())
        .bind(limit)
        .fetch_all(self.0)
        .await?
        .iter()
        .map(row_to_post)
        .collect())
    }

    /// Count posts in a board created after a timestamp (unread counting).
    pub async fn count_after(&self, board: &str, after_ms: i64) -> Result<i64, StoreError> {
        Ok(sqlx::query(
            "SELECT COUNT(*) AS n FROM posts WHERE board_slug = ? AND created_at > ? AND tombstoned = 0",
        )
        .bind(board)
        .bind(after_ms)
        .fetch_one(self.0)
        .await?
        .get("n"))
    }

    /// Delta sync: posts across all boards created after a cursor, oldest
    /// first — the offline-download and (later) federation feed primitive.
    pub async fn since(&self, after_ms: i64, limit: i64) -> Result<Vec<PostRow>, StoreError> {
        Ok(
            sqlx::query("SELECT * FROM posts WHERE created_at > ? ORDER BY created_at ASC LIMIT ?")
                .bind(after_ms)
                .bind(limit)
                .fetch_all(self.0)
                .await?
                .iter()
                .map(row_to_post)
                .collect(),
        )
    }

    /// Enforce a board's thread-retention cap: returns root ids to drop
    /// (oldest top-level threads beyond `max_threads`).
    pub async fn overflow_threads(
        &self,
        board: &str,
        max_threads: i64,
    ) -> Result<Vec<[u8; 32]>, StoreError> {
        if max_threads <= 0 {
            return Ok(Vec::new());
        }
        Ok(sqlx::query(
            "SELECT event_id FROM posts WHERE board_slug = ? AND parent_id IS NULL
             ORDER BY created_at DESC LIMIT -1 OFFSET ?",
        )
        .bind(board)
        .bind(max_threads)
        .fetch_all(self.0)
        .await?
        .iter()
        .filter_map(|r| r.get::<Vec<u8>, _>("event_id").try_into().ok())
        .collect())
    }

    pub async fn delete_thread(&self, root: &[u8; 32]) -> Result<u64, StoreError> {
        Ok(
            sqlx::query("DELETE FROM posts WHERE event_id = ?1 OR root_id = ?1")
                .bind(root.as_slice())
                .execute(self.0)
                .await?
                .rows_affected(),
        )
    }
}

pub struct ReadMarksRepo<'a>(pub &'a SqlitePool);

impl ReadMarksRepo<'_> {
    pub async fn get(&self, account_id: i64, board: &str) -> Result<i64, StoreError> {
        Ok(sqlx::query(
            "SELECT last_read_ms FROM read_marks WHERE account_id = ? AND board_slug = ?",
        )
        .bind(account_id)
        .bind(board)
        .fetch_optional(self.0)
        .await?
        .map(|r| r.get::<i64, _>("last_read_ms"))
        .unwrap_or(0))
    }

    /// Advance the high-water mark (never moves backwards).
    pub async fn set(&self, account_id: i64, board: &str, ms: i64) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO read_marks (account_id, board_slug, last_read_ms) VALUES (?, ?, ?)
             ON CONFLICT (account_id, board_slug)
             DO UPDATE SET last_read_ms = MAX(last_read_ms, excluded.last_read_ms)",
        )
        .bind(account_id)
        .bind(board)
        .bind(ms)
        .execute(self.0)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    fn post(
        id: u8,
        board: &str,
        root: Option<[u8; 32]>,
        parent: Option<[u8; 32]>,
        at: i64,
    ) -> PostRow {
        PostRow {
            event_id: [id; 32],
            board_slug: board.into(),
            root_id: root,
            parent_id: parent,
            author: "alice@home".into(),
            subject: format!("subject {id}"),
            body: format!("body {id}"),
            mime: "text/plain".into(),
            created_at: at,
            edited: false,
            tombstoned: false,
            event_blob: vec![id; 8],
        }
    }

    #[tokio::test]
    async fn board_tree_and_post_dedup() {
        let pool = open_in_memory().await.unwrap();
        let boards = BoardsRepo(&pool);
        boards
            .create("rabbit", "Rabbit", "", 0, None, 0)
            .await
            .unwrap();
        let b = boards
            .create("rabbit.general", "General", "chat", 2, Some("rabbit"), 0)
            .await
            .unwrap();
        assert_eq!(b.parent_slug.as_deref(), Some("rabbit"));
        assert!(boards
            .create("rabbit", "dup", "", 0, None, 0)
            .await
            .is_err());

        let posts = PostsRepo(&pool);
        let p = post(1, "rabbit.general", None, None, 1000);
        assert!(posts.insert(&p).await.unwrap());
        assert!(!posts.insert(&p).await.unwrap(), "same content id dedups");
        assert_eq!(
            posts.by_id(&[1; 32]).await.unwrap().unwrap().subject,
            "subject 1"
        );
    }

    #[tokio::test]
    async fn threading_and_counts() {
        let pool = open_in_memory().await.unwrap();
        let posts = PostsRepo(&pool);
        // Root + two replies.
        posts.insert(&post(1, "b", None, None, 1000)).await.unwrap();
        posts
            .insert(&post(2, "b", Some([1; 32]), Some([1; 32]), 2000))
            .await
            .unwrap();
        posts
            .insert(&post(3, "b", Some([1; 32]), Some([2; 32]), 3000))
            .await
            .unwrap();
        // A second thread.
        posts.insert(&post(4, "b", None, None, 4000)).await.unwrap();

        let threads = posts.threads("b", 10).await.unwrap();
        assert_eq!(threads.len(), 2);
        // Newest thread first (id 4), then id 1 with 2 replies + last=3000.
        assert_eq!(threads[0].0.event_id, [4; 32]);
        let (root, replies, last) = &threads[1];
        assert_eq!(root.event_id, [1; 32]);
        assert_eq!(*replies, 2);
        assert_eq!(*last, 3000);

        let full = posts.thread(&[1; 32], 100).await.unwrap();
        assert_eq!(full.len(), 3); // root + 2 replies, oldest first
        assert_eq!(full[0].event_id, [1; 32]);
    }

    #[tokio::test]
    async fn edit_tombstone_and_since() {
        let pool = open_in_memory().await.unwrap();
        let posts = PostsRepo(&pool);
        posts.insert(&post(1, "b", None, None, 1000)).await.unwrap();

        assert!(posts
            .apply_edit(&[1; 32], "new subj", "new body", "text/markdown")
            .await
            .unwrap());
        let p = posts.by_id(&[1; 32]).await.unwrap().unwrap();
        assert!(p.edited && p.subject == "new subj");

        posts.insert(&post(2, "b", None, None, 2000)).await.unwrap();
        assert!(posts.apply_tombstone(&[2; 32]).await.unwrap());
        let t = posts.by_id(&[2; 32]).await.unwrap().unwrap();
        assert!(t.tombstoned && t.body.is_empty());

        // count_after ignores tombstoned; since() is the raw delta feed.
        assert_eq!(posts.count_after("b", 500).await.unwrap(), 1); // only #1
        assert_eq!(posts.since(1500, 100).await.unwrap().len(), 1); // only #2
    }

    #[tokio::test]
    async fn retention_overflow() {
        let pool = open_in_memory().await.unwrap();
        let posts = PostsRepo(&pool);
        for i in 1..=5u8 {
            posts
                .insert(&post(i, "b", None, None, i as i64 * 1000))
                .await
                .unwrap();
        }
        // Keep 3 newest threads → the 2 oldest overflow (ids 1,2).
        let overflow = posts.overflow_threads("b", 3).await.unwrap();
        assert_eq!(overflow.len(), 2);
        assert!(overflow.contains(&[1; 32]) && overflow.contains(&[2; 32]));
        assert!(posts.overflow_threads("b", 0).await.unwrap().is_empty()); // 0 = unlimited
    }

    #[tokio::test]
    async fn read_marks_high_water() {
        let pool = open_in_memory().await.unwrap();
        crate::repo::AccountsRepo(&pool)
            .create("a", None, "a", 1, None)
            .await
            .unwrap();
        let marks = ReadMarksRepo(&pool);
        assert_eq!(marks.get(1, "b").await.unwrap(), 0);
        marks.set(1, "b", 5000).await.unwrap();
        marks.set(1, "b", 3000).await.unwrap(); // never goes backwards
        assert_eq!(marks.get(1, "b").await.unwrap(), 5000);
    }
}
