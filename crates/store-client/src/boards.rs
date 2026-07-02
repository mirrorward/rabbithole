//! The offline board cache and reply outbox (Wave 3.2).
//!
//! A local mirror of the server's message bases so a client can read boards
//! and threads with no connection, plus an outbox that holds replies
//! composed offline until the next sync flushes them.
//!
//! Sync is delta-based and idempotent: posts are keyed by their 32-byte
//! content id, so re-ingesting an overlapping batch is a no-op. Each board
//! carries a cursor — the newest `created_at` cached — that a caller uses to
//! ask the server only for what changed (PLAN §9.4).

use rabbithole_proto::board::{BoardInfo, PostView};
use rusqlite::{params, Connection, OptionalExtension};

use crate::StoreError;

/// A queued offline reply awaiting send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxPost {
    pub id: i64,
    pub board: String,
    pub parent: Option<[u8; 32]>,
    pub subject: String,
    pub body: String,
    pub mime: String,
    pub created_at: i64,
}

/// The board cache, scoped to a client store connection.
pub struct BoardCache<'a>(pub &'a Connection);

fn to_id(bytes: Vec<u8>) -> Option<[u8; 32]> {
    bytes.try_into().ok()
}

impl BoardCache<'_> {
    /// Cache (upsert) the board tree. `synced_at` is the caller's clock in
    /// unix seconds — the local store never invents time.
    pub fn put_boards(&self, boards: &[BoardInfo], synced_at: i64) -> Result<(), StoreError> {
        let mut stmt = self.0.prepare_cached(
            "INSERT INTO cached_boards
                 (slug, title, description, kind, parent_slug, unread, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(slug) DO UPDATE SET
                 title = excluded.title,
                 description = excluded.description,
                 kind = excluded.kind,
                 parent_slug = excluded.parent_slug,
                 unread = excluded.unread,
                 synced_at = excluded.synced_at",
        )?;
        for b in boards {
            stmt.execute(params![
                b.slug,
                b.title,
                b.description,
                b.kind as i64,
                b.parent_slug,
                b.unread as i64,
                synced_at,
            ])?;
        }
        Ok(())
    }

    /// The cached board tree, categories/bundles/boards intermixed, by slug.
    pub fn boards(&self) -> Result<Vec<BoardInfo>, StoreError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT slug, title, description, kind, parent_slug, unread
             FROM cached_boards ORDER BY slug",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let mut b = BoardInfo::new(
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(3)? as u8,
                );
                b.description = r.get(2)?;
                b.parent_slug = r.get(4)?;
                b.unread = r.get::<_, i64>(5)? as u64;
                Ok(b)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Ingest a batch of posts, idempotent by content id. Later copies of a
    /// post overwrite earlier ones so edits/tombstones settle in.
    pub fn put_posts(&self, posts: &[PostView]) -> Result<(), StoreError> {
        let mut stmt = self.0.prepare_cached(
            "INSERT INTO cached_posts
                 (id, board, root, parent, author, subject, body, mime,
                  created_at, edited, tombstoned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 subject = excluded.subject,
                 body = excluded.body,
                 mime = excluded.mime,
                 edited = excluded.edited,
                 tombstoned = excluded.tombstoned",
        )?;
        for p in posts {
            stmt.execute(params![
                &p.id[..],
                p.board,
                p.root.as_ref().map(|r| &r[..]),
                p.parent.as_ref().map(|r| &r[..]),
                p.author,
                p.subject,
                p.body,
                p.mime,
                p.created_at_unix_ms,
                p.edited as i64,
                p.tombstoned as i64,
            ])?;
        }
        Ok(())
    }

    /// The delta-download watermark for a board: the newest `created_at` we
    /// have cached (0 when the board is empty). Ask the server for posts
    /// after this to fetch only what changed.
    pub fn board_cursor(&self, board: &str) -> Result<i64, StoreError> {
        Ok(self
            .0
            .query_row(
                "SELECT COALESCE(MAX(created_at), 0) FROM cached_posts WHERE board = ?1",
                params![board],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    fn read_row(r: &rusqlite::Row) -> rusqlite::Result<PostView> {
        Ok(PostView::new(
            to_id(r.get::<_, Vec<u8>>(0)?).unwrap_or([0; 32]),
            r.get::<_, String>(1)?,
            r.get::<_, Option<Vec<u8>>>(2)?.and_then(to_id),
            r.get::<_, Option<Vec<u8>>>(3)?.and_then(to_id),
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, i64>(9)? != 0,
            r.get::<_, i64>(10)? != 0,
        ))
    }

    const POST_COLS: &'static str =
        "id, board, root, parent, author, subject, body, mime, created_at, edited, tombstoned";

    /// Cached top-level posts (thread roots) for a board, newest first.
    pub fn threads(&self, board: &str) -> Result<Vec<PostView>, StoreError> {
        let sql = format!(
            "SELECT {} FROM cached_posts
             WHERE board = ?1 AND (root IS NULL OR root = id)
             ORDER BY created_at DESC",
            Self::POST_COLS
        );
        let mut stmt = self.0.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![board], Self::read_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A cached thread: the root and its descendants, oldest first.
    pub fn thread(&self, root: [u8; 32]) -> Result<Vec<PostView>, StoreError> {
        let sql = format!(
            "SELECT {} FROM cached_posts
             WHERE id = ?1 OR root = ?1
             ORDER BY created_at ASC",
            Self::POST_COLS
        );
        let mut stmt = self.0.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![&root[..]], Self::read_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record the local read mark for a board (highest post time seen).
    pub fn set_read(&self, board: &str, up_to_ms: i64) -> Result<(), StoreError> {
        self.0.execute(
            "INSERT INTO board_read_marks (board, up_to_ms) VALUES (?1, ?2)
             ON CONFLICT(board) DO UPDATE SET up_to_ms = MAX(up_to_ms, excluded.up_to_ms)",
            params![board, up_to_ms],
        )?;
        Ok(())
    }

    /// The local read mark for a board (0 if never read).
    pub fn read_mark(&self, board: &str) -> Result<i64, StoreError> {
        Ok(self
            .0
            .query_row(
                "SELECT up_to_ms FROM board_read_marks WHERE board = ?1",
                params![board],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    // ---- Reply outbox ----------------------------------------------------

    /// Queue a reply composed offline. `created_at` is the caller's clock.
    /// Returns the outbox row id.
    pub fn enqueue(
        &self,
        board: &str,
        parent: Option<[u8; 32]>,
        subject: &str,
        body: &str,
        mime: &str,
        created_at: i64,
    ) -> Result<i64, StoreError> {
        self.0.execute(
            "INSERT INTO board_outbox (board, parent, subject, body, mime, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                board,
                parent.as_ref().map(|p| &p[..]),
                subject,
                body,
                mime,
                created_at
            ],
        )?;
        Ok(self.0.last_insert_rowid())
    }

    /// Unsent outbox entries, oldest first — flush these on reconnect.
    pub fn pending(&self) -> Result<Vec<OutboxPost>, StoreError> {
        let mut stmt = self.0.prepare_cached(
            "SELECT id, board, parent, subject, body, mime, created_at
             FROM board_outbox WHERE sent = 0 ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(OutboxPost {
                    id: r.get(0)?,
                    board: r.get(1)?,
                    parent: r.get::<_, Option<Vec<u8>>>(2)?.and_then(to_id),
                    subject: r.get(3)?,
                    body: r.get(4)?,
                    mime: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark an outbox entry sent, recording the server-assigned content id.
    pub fn mark_sent(&self, outbox_id: i64, event_id: [u8; 32]) -> Result<(), StoreError> {
        self.0.execute(
            "UPDATE board_outbox SET sent = 1, sent_event_id = ?2 WHERE id = ?1",
            params![outbox_id, &event_id[..]],
        )?;
        Ok(())
    }

    /// Count of replies still waiting to be sent.
    pub fn pending_count(&self) -> Result<i64, StoreError> {
        Ok(self.0.query_row(
            "SELECT COUNT(*) FROM board_outbox WHERE sent = 0",
            [],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    fn post(id: u8, board: &str, root: Option<[u8; 32]>, at: i64) -> PostView {
        PostView::new(
            [id; 32],
            board,
            root,
            root.map(|_| [1; 32]),
            "alice@home",
            "subj",
            "body",
            "text/plain",
            at,
            false,
            false,
        )
    }

    #[test]
    fn boards_upsert_and_read_back() {
        let conn = open_in_memory().unwrap();
        let cache = BoardCache(&conn);
        let mut b = BoardInfo::new("general", "General", 2);
        b.unread = 3;
        cache.put_boards(&[b.clone()], 1000).unwrap();
        // Upsert lowers unread.
        b.unread = 0;
        cache.put_boards(&[b], 2000).unwrap();
        let got = cache.boards().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].slug, "general");
        assert_eq!(got[0].unread, 0);
    }

    #[test]
    fn delta_ingest_is_idempotent_and_tracks_cursor() {
        let conn = open_in_memory().unwrap();
        let cache = BoardCache(&conn);
        let root = [1; 32];
        cache.put_posts(&[post(1, "general", None, 100)]).unwrap();
        cache
            .put_posts(&[post(2, "general", Some(root), 200)])
            .unwrap();
        // Re-ingest an overlapping batch: no duplicates.
        cache
            .put_posts(&[
                post(2, "general", Some(root), 200),
                post(3, "general", Some(root), 300),
            ])
            .unwrap();

        assert_eq!(cache.board_cursor("general").unwrap(), 300);
        let thread = cache.thread(root).unwrap();
        assert_eq!(thread.len(), 3, "root + two replies, deduped");
        assert_eq!(thread[0].created_at_unix_ms, 100, "oldest first");

        let roots = cache.threads("general").unwrap();
        assert_eq!(roots.len(), 1, "only the root is a top-level post");
        assert_eq!(roots[0].id, root);
    }

    #[test]
    fn edit_overwrites_cached_body() {
        let conn = open_in_memory().unwrap();
        let cache = BoardCache(&conn);
        cache.put_posts(&[post(1, "general", None, 100)]).unwrap();
        let mut edited = post(1, "general", None, 100);
        edited.body = "corrected".into();
        edited.edited = true;
        cache.put_posts(&[edited]).unwrap();
        let t = cache.thread([1; 32]).unwrap();
        assert_eq!(t[0].body, "corrected");
        assert!(t[0].edited);
    }

    #[test]
    fn read_marks_are_monotonic() {
        let conn = open_in_memory().unwrap();
        let cache = BoardCache(&conn);
        assert_eq!(cache.read_mark("general").unwrap(), 0);
        cache.set_read("general", 500).unwrap();
        cache.set_read("general", 200).unwrap(); // older, must not regress
        assert_eq!(cache.read_mark("general").unwrap(), 500);
    }

    #[test]
    fn outbox_queue_flush_cycle() {
        let conn = open_in_memory().unwrap();
        let cache = BoardCache(&conn);
        let root = [9; 32];
        let a = cache
            .enqueue("general", None, "hi", "first", "text/plain", 10)
            .unwrap();
        let b = cache
            .enqueue("general", Some(root), "re", "reply", "text/plain", 20)
            .unwrap();
        assert_eq!(cache.pending_count().unwrap(), 2);

        let pending = cache.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, a, "oldest first");
        assert_eq!(pending[1].parent, Some(root));

        // Flush the first; only the second remains pending.
        cache.mark_sent(a, [7; 32]).unwrap();
        let still = cache.pending().unwrap();
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].id, b);
        assert_eq!(cache.pending_count().unwrap(), 1);
    }
}
