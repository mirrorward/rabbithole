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
        // Last among its own: a new board is read after the ones already
        // there, wherever the operator has since put them.
        sqlx::query(
            "INSERT INTO boards
                 (slug, title, description, kind, parent_slug, max_threads, position, created_at)
             VALUES (
                 ?, ?, ?, ?, ?, ?,
                 (SELECT COALESCE(MAX(position) + 1, 0) FROM boards
                   WHERE COALESCE(parent_slug, '') = COALESCE(?, '')),
                 unixepoch()
             )",
        )
        .bind(slug)
        .bind(title)
        .bind(description)
        .bind(kind as i64)
        .bind(parent_slug)
        .bind(max_threads)
        .bind(parent_slug)
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

    /// Change what a board is called, says about itself, and keeps. The slug
    /// is its identity (routes, newsgroup names, gateway maps) and never moves.
    /// Returns whether there was such a board.
    pub async fn update(
        &self,
        slug: &str,
        title: &str,
        description: &str,
        max_threads: Option<i64>,
    ) -> Result<bool, StoreError> {
        // `None` keeps the retention it has.
        Ok(sqlx::query(
            "UPDATE boards SET title = ?, description = ?,
                    max_threads = COALESCE(?, max_threads)
             WHERE slug = ?",
        )
        .bind(title)
        .bind(description)
        .bind(max_threads)
        .bind(slug)
        .execute(self.0)
        .await?
        .rows_affected()
            > 0)
    }

    /// How much is in a board: `(posts, boards directly inside it)`.
    pub async fn contents(&self, slug: &str) -> Result<(i64, i64), StoreError> {
        let posts: i64 = sqlx::query("SELECT COUNT(*) AS n FROM posts WHERE board_slug = ?")
            .bind(slug)
            .fetch_one(self.0)
            .await?
            .get("n");
        let children: i64 = sqlx::query("SELECT COUNT(*) AS n FROM boards WHERE parent_slug = ?")
            .bind(slug)
            .fetch_one(self.0)
            .await?
            .get("n");
        Ok((posts, children))
    }

    /// Remove a board and the read marks that point at it. The caller has
    /// already made sure it is empty.
    pub async fn delete(&self, slug: &str) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM read_marks WHERE board_slug = ?")
            .bind(slug)
            .execute(self.0)
            .await?;
        Ok(sqlx::query("DELETE FROM boards WHERE slug = ?")
            .bind(slug)
            .execute(self.0)
            .await?
            .rows_affected()
            > 0)
    }

    /// Every board in reading order: each one where its operator put it
    /// among its own, with what is inside it straight after it. A position
    /// is only meaningful among siblings, so sorting the whole table by it
    /// would deal one parent's children in between another's.
    ///
    /// A board whose parent is gone reads as a top-level one rather than
    /// not at all: it is still somebody's board.
    pub async fn all(&self) -> Result<Vec<BoardRow>, StoreError> {
        Ok(sqlx::query(
            "WITH RECURSIVE ordered(slug, path) AS (
                 SELECT b.slug, printf('%08d/%s', b.position, b.slug)
                   FROM boards b
                  WHERE b.parent_slug IS NULL
                     OR b.parent_slug NOT IN (SELECT slug FROM boards)
                 UNION ALL
                 SELECT b.slug, ordered.path || '/' || printf('%08d/%s', b.position, b.slug)
                   FROM boards b
                   JOIN ordered ON b.parent_slug = ordered.slug
             )
             SELECT boards.* FROM boards
               JOIN ordered ON ordered.slug = boards.slug
              ORDER BY ordered.path",
        )
        .fetch_all(self.0)
        .await?
        .iter()
        .map(row_to_board)
        .collect())
    }

    /// Put `slug` straight after `after` among the boards under the same
    /// parent — or first, when there is no `after`. The others keep their
    /// order; positions are renumbered from 0 so they never run out.
    /// `Ok(false)` when either board is not there, or `after` sits under
    /// another parent (a board is only moved among its own).
    pub async fn move_after(&self, slug: &str, after: Option<&str>) -> Result<bool, StoreError> {
        let mut tx = self.0.begin().await?;
        // A slug is one slug in any case (the column is NOCASE), so the
        // burrow's own spelling is what the list work below compares.
        let found: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT slug, parent_slug FROM boards WHERE slug = ?")
                .bind(slug)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((mine, parent)) = found else {
            return Ok(false);
        };
        let slug = mine.as_str();
        let mut after_slug: Option<String> = None;
        if let Some(after) = after {
            let theirs: Option<(String, Option<String>)> =
                sqlx::query_as("SELECT slug, parent_slug FROM boards WHERE slug = ?")
                    .bind(after)
                    .fetch_optional(&mut *tx)
                    .await?;
            match theirs {
                Some((after, theirs)) if theirs == parent && after != slug => {
                    after_slug = Some(after)
                }
                _ => return Ok(false),
            }
        }
        let after = after_slug.as_deref();
        // The rest, in the order they are read now.
        let siblings: Vec<String> = sqlx::query_scalar(
            "SELECT slug FROM boards WHERE COALESCE(parent_slug, '') = COALESCE(?, '')
             ORDER BY position, slug",
        )
        .bind(parent.as_deref())
        .fetch_all(&mut *tx)
        .await?;
        let mut order: Vec<String> = siblings.into_iter().filter(|s| s != slug).collect();
        let at = match after {
            None => 0,
            Some(after) => match order.iter().position(|s| s == after) {
                Some(i) => i + 1,
                None => return Ok(false),
            },
        };
        order.insert(at, slug.to_string());
        for (i, s) in order.iter().enumerate() {
            sqlx::query("UPDATE boards SET position = ? WHERE slug = ?")
                .bind(i as i64)
                .bind(s)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(true)
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

/// A stored board follow-up: an `Edit` or `Tombstone` signed event, kept so
/// the federation flood can advertise/serve it and so out-of-order delivery
/// (a follow-up before its target post) can be reconciled later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupRow {
    pub event_id: [u8; 32],
    pub target_id: [u8; 32],
    pub root_id: [u8; 32],
    pub board_slug: String,
    /// 1 = edit, 2 = tombstone.
    pub kind: u8,
    pub origin: String,
    /// Whether it has been applied to the `posts` projection yet.
    pub applied: bool,
    pub created_at: i64,
    pub event_blob: Vec<u8>,
}

fn row_to_followup(r: &sqlx::sqlite::SqliteRow) -> FollowupRow {
    let id = |c| r.get::<Vec<u8>, _>(c).try_into().unwrap_or([0; 32]);
    FollowupRow {
        event_id: id("event_id"),
        target_id: id("target_id"),
        root_id: id("root_id"),
        board_slug: r.get("board_slug"),
        kind: r.get::<i64, _>("kind") as u8,
        origin: r.get("origin"),
        applied: r.get::<i64, _>("applied") != 0,
        created_at: r.get("created_at"),
        event_blob: r.get("event_blob"),
    }
}

pub struct FollowupsRepo<'a>(pub &'a SqlitePool);

impl FollowupsRepo<'_> {
    /// Insert a follow-up. Idempotent on the content id — a duplicate (same
    /// event flooded twice) is a no-op returning `false`.
    pub async fn insert(&self, f: &FollowupRow) -> Result<bool, StoreError> {
        let affected = sqlx::query(
            "INSERT INTO board_followups
                 (event_id, target_id, root_id, board_slug, kind, origin, applied,
                  created_at, event_blob)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(f.event_id.as_slice())
        .bind(f.target_id.as_slice())
        .bind(f.root_id.as_slice())
        .bind(&f.board_slug)
        .bind(f.kind as i64)
        .bind(&f.origin)
        .bind(f.applied as i64)
        .bind(f.created_at)
        .bind(&f.event_blob)
        .execute(self.0)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    pub async fn by_id(&self, id: &[u8; 32]) -> Result<Option<FollowupRow>, StoreError> {
        Ok(
            sqlx::query("SELECT * FROM board_followups WHERE event_id = ?")
                .bind(id.as_slice())
                .fetch_optional(self.0)
                .await?
                .map(|r| row_to_followup(&r)),
        )
    }

    /// Not-yet-applied follow-ups for a target post, oldest first — applied in
    /// order when the target lands (an edit chain resolves latest-wins; a
    /// tombstone is terminal).
    pub async fn pending_for(&self, target: &[u8; 32]) -> Result<Vec<FollowupRow>, StoreError> {
        Ok(sqlx::query(
            "SELECT * FROM board_followups WHERE target_id = ? AND applied = 0
             ORDER BY created_at ASC",
        )
        .bind(target.as_slice())
        .fetch_all(self.0)
        .await?
        .iter()
        .map(row_to_followup)
        .collect())
    }

    /// Drop a single follow-up (e.g. a pending one whose target, once it
    /// arrived, refused it at the authorization gate).
    pub async fn delete_one(&self, id: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM board_followups WHERE event_id = ?")
            .bind(id.as_slice())
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// Mark a follow-up applied to the projection.
    pub async fn mark_applied(&self, id: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query("UPDATE board_followups SET applied = 1 WHERE event_id = ?")
            .bind(id.as_slice())
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// Drop every follow-up targeting a thread — the retention cascade called
    /// alongside `PostsRepo::delete_thread`.
    pub async fn delete_for_root(&self, root: &[u8; 32]) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM board_followups WHERE root_id = ?")
            .bind(root.as_slice())
            .execute(self.0)
            .await?
            .rows_affected())
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
    async fn boards_sit_where_they_are_put() {
        let pool = open_in_memory().await.unwrap();
        let boards = BoardsRepo(&pool);
        for slug in ["alpha", "bravo", "charlie"] {
            boards.create(slug, slug, "", 2, None, 0).await.unwrap();
        }
        boards
            .create("alpha.one", "One", "", 2, Some("alpha"), 0)
            .await
            .unwrap();
        let order = |rows: Vec<BoardRow>| -> Vec<String> {
            rows.into_iter()
                .filter(|b| b.parent_slug.is_none())
                .map(|b| b.slug)
                .collect()
        };
        // Until anybody moves one, they read as they always did.
        assert_eq!(
            order(boards.all().await.unwrap()),
            ["alpha", "bravo", "charlie"]
        );

        // Last to first, and back to the middle.
        assert!(boards.move_after("charlie", None).await.unwrap());
        assert_eq!(
            order(boards.all().await.unwrap()),
            ["charlie", "alpha", "bravo"]
        );
        assert!(boards.move_after("charlie", Some("alpha")).await.unwrap());
        assert_eq!(
            order(boards.all().await.unwrap()),
            ["alpha", "charlie", "bravo"]
        );

        // A board is moved among its own: not after one under another
        // parent, not after itself, and not if it is not there.
        assert!(!boards
            .move_after("charlie", Some("alpha.one"))
            .await
            .unwrap());
        assert!(!boards.move_after("charlie", Some("charlie")).await.unwrap());
        assert!(!boards.move_after("nowhere", None).await.unwrap());
        assert!(!boards.move_after("charlie", Some("nowhere")).await.unwrap());
        assert_eq!(
            order(boards.all().await.unwrap()),
            ["alpha", "charlie", "bravo"],
            "a refused move changes nothing"
        );

        // A slug is one slug in any case, so moving "CHARLIE" moves charlie
        // rather than making a second place for a board of that name.
        assert!(boards.move_after("CHARLIE", None).await.unwrap());
        assert_eq!(
            order(boards.all().await.unwrap()),
            ["charlie", "alpha", "bravo"]
        );
        assert!(!boards.move_after("charlie", Some("CHARLIE")).await.unwrap());

        // A board made now is read after the ones already there, wherever
        // the operator has since put them.
        boards
            .create("delta", "delta", "", 2, None, 0)
            .await
            .unwrap();
        assert_eq!(
            order(boards.all().await.unwrap()),
            ["charlie", "alpha", "bravo", "delta"]
        );

        // What is inside a board is read straight after it, whatever the
        // positions happen to be: a position only means something among
        // the boards that share a parent.
        boards
            .create("bravo.inner", "Inner", "", 2, Some("bravo"), 0)
            .await
            .unwrap();
        boards
            .create("charlie.inner", "Inner", "", 2, Some("charlie"), 0)
            .await
            .unwrap();
        let all: Vec<String> = boards
            .all()
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.slug)
            .collect();
        assert_eq!(
            all,
            [
                "charlie",
                "charlie.inner",
                "alpha",
                "alpha.one",
                "bravo",
                "bravo.inner",
                "delta"
            ],
            "each board, then what is inside it"
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

    fn followup(id: u8, target: [u8; 32], root: [u8; 32], kind: u8, at: i64) -> FollowupRow {
        FollowupRow {
            event_id: [id; 32],
            target_id: target,
            root_id: root,
            board_slug: "b".into(),
            kind,
            origin: "home".into(),
            applied: false,
            created_at: at,
            event_blob: vec![id; 8],
        }
    }

    #[tokio::test]
    async fn followups_insert_pending_apply_and_cascade() {
        let pool = open_in_memory().await.unwrap();
        let f = FollowupsRepo(&pool);

        // Two follow-ups target post [1;32] (thread root [1;32]).
        assert!(f
            .insert(&followup(10, [1; 32], [1; 32], 1, 2000))
            .await
            .unwrap());
        assert!(
            !f.insert(&followup(10, [1; 32], [1; 32], 1, 2000))
                .await
                .unwrap(),
            "dedup"
        );
        f.insert(&followup(11, [1; 32], [1; 32], 2, 3000))
            .await
            .unwrap();
        // One targets a different thread.
        f.insert(&followup(20, [5; 32], [5; 32], 1, 4000))
            .await
            .unwrap();

        // pending_for is oldest-first and scoped to the target.
        let pending = f.pending_for(&[1; 32]).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].event_id, [10; 32]);
        assert_eq!(pending[1].event_id, [11; 32]);
        assert_eq!(f.by_id(&[10; 32]).await.unwrap().unwrap().kind, 1);

        // Applying removes it from the pending set.
        f.mark_applied(&[10; 32]).await.unwrap();
        let pending = f.pending_for(&[1; 32]).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(f.by_id(&[10; 32]).await.unwrap().unwrap().applied);

        // Retention cascade drops the whole thread's follow-ups, not others'.
        assert_eq!(f.delete_for_root(&[1; 32]).await.unwrap(), 2);
        assert!(f.by_id(&[10; 32]).await.unwrap().is_none());
        assert!(
            f.by_id(&[20; 32]).await.unwrap().is_some(),
            "other thread untouched"
        );
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
