//! Board service: mints signed post events, projects them into the store,
//! enforces retention, and tracks per-account read marks.
//!
//! Every post is a [`crate::events::SignedEvent`] — the store keeps the
//! signed blob as the federation source of truth and a denormalized
//! projection for querying. This service is the only place posts are
//! created, so all the invariants (root/parent linkage, author binding,
//! retention) live here.

use rabbithole_identity::keys::IdentityKey;
use rabbithole_store_server::repo4::{BoardRow, BoardsRepo, PostRow, PostsRepo, ReadMarksRepo};
use rabbithole_store_server::{SqlitePool, StoreError};

use crate::events::{mint, EventBody, SignedEvent};

#[derive(Debug, thiserror::Error)]
pub enum BoardError {
    #[error("no such board")]
    NoSuchBoard,
    #[error("board is not postable (it's a category/bundle)")]
    NotPostable,
    #[error("no such post")]
    NoSuchPost,
    #[error("not permitted")]
    Forbidden,
    #[error("empty post")]
    Empty,
    #[error("slug already exists")]
    SlugExists,
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

pub struct BoardService {
    pool: SqlitePool,
    origin: String,
    /// Origin server signing key (rebuilt from the seed in `Shared`).
    origin_seed: [u8; 32],
}

impl BoardService {
    pub fn new(pool: SqlitePool, origin: String, origin_seed: [u8; 32]) -> Self {
        Self {
            pool,
            origin,
            origin_seed,
        }
    }

    fn origin_key(&self) -> IdentityKey {
        IdentityKey::from_seed(&self.origin_seed)
    }

    pub async fn create_board(
        &self,
        slug: &str,
        title: &str,
        description: &str,
        kind: u8,
        parent_slug: Option<&str>,
        max_threads: i64,
    ) -> Result<BoardRow, BoardError> {
        let slug = slug.trim();
        if slug.is_empty() || slug.len() > 64 {
            return Err(BoardError::NoSuchBoard);
        }
        if BoardsRepo(&self.pool).by_slug(slug).await?.is_some() {
            return Err(BoardError::SlugExists);
        }
        Ok(BoardsRepo(&self.pool)
            .create(slug, title, description, kind, parent_slug, max_threads)
            .await?)
    }

    pub async fn boards(&self) -> Result<Vec<BoardRow>, BoardError> {
        Ok(BoardsRepo(&self.pool).all().await?)
    }

    pub async fn board(&self, slug: &str) -> Result<Option<BoardRow>, BoardError> {
        Ok(BoardsRepo(&self.pool).by_slug(slug).await?)
    }

    /// Mint + store a post. `author_seed` is the author's identity key
    /// (Wave 3 derives a stable per-account key; Wave 9 uses enrolled
    /// keys). Returns the stored projection and the root id.
    #[allow(clippy::too_many_arguments)]
    pub async fn post(
        &self,
        board: &str,
        parent: Option<[u8; 32]>,
        author_display: &str,
        author_seed: &[u8; 32],
        subject: &str,
        body: &str,
        mime: &str,
        now_ms: i64,
    ) -> Result<PostRow, BoardError> {
        let Some(board_row) = BoardsRepo(&self.pool).by_slug(board).await? else {
            return Err(BoardError::NoSuchBoard);
        };
        if board_row.kind != 2 {
            return Err(BoardError::NotPostable);
        }
        if subject.trim().is_empty() && body.trim().is_empty() {
            return Err(BoardError::Empty);
        }

        // Resolve the thread root from the parent (a reply inherits the
        // parent's root; a top-level post is its own root once minted).
        let root = match parent {
            Some(pid) => {
                let parent_row = PostsRepo(&self.pool)
                    .by_id(&pid)
                    .await?
                    .ok_or(BoardError::NoSuchPost)?;
                Some(parent_row.root_id.unwrap_or(parent_row.event_id))
            }
            None => None,
        };

        let author_key = IdentityKey::from_seed(author_seed);
        let event = mint(
            author_display,
            &author_key,
            &self.origin,
            &self.origin_key(),
            now_ms,
            EventBody::Post {
                board: board.to_string(),
                root,
                parent,
                subject: subject.to_string(),
                body: body.to_string(),
                mime: mime.to_string(),
            },
        );

        let row = self.ingest(&event, board_row.max_threads).await?;
        Ok(row)
    }

    /// Store a signed event's projection (used by `post` and, later, by the
    /// federation tosser). Idempotent on content id. Enforces retention for
    /// new top-level threads. Returns the projected row.
    pub async fn ingest(
        &self,
        event: &SignedEvent,
        max_threads: i64,
    ) -> Result<PostRow, BoardError> {
        let EventBody::Post {
            board,
            root,
            parent,
            subject,
            body,
            mime,
        } = &event.body
        else {
            return Err(BoardError::Empty); // ingest() is for Post events
        };
        let blob = postcard::to_allocvec(event).expect("serializable");
        // A top-level post is its own root.
        let root_id = root.or(if parent.is_none() {
            Some(event.id)
        } else {
            None
        });
        let row = PostRow {
            event_id: event.id,
            board_slug: board.clone(),
            root_id,
            parent_id: *parent,
            author: event.author.clone(),
            subject: subject.clone(),
            body: body.clone(),
            mime: mime.clone(),
            created_at: event.created_at_unix_ms,
            edited: false,
            tombstoned: false,
            event_blob: blob,
        };
        PostsRepo(&self.pool).insert(&row).await?;

        // Retention: drop oldest overflow threads in this board.
        if parent.is_none() && max_threads > 0 {
            for old in PostsRepo(&self.pool)
                .overflow_threads(board, max_threads)
                .await?
            {
                PostsRepo(&self.pool).delete_thread(&old).await?;
            }
        }
        Ok(row)
    }

    pub async fn threads(
        &self,
        board: &str,
        limit: i64,
    ) -> Result<Vec<(PostRow, i64, i64)>, BoardError> {
        Ok(PostsRepo(&self.pool).threads(board, limit).await?)
    }

    pub async fn thread(&self, root: &[u8; 32], limit: i64) -> Result<Vec<PostRow>, BoardError> {
        Ok(PostsRepo(&self.pool).thread(root, limit).await?)
    }

    pub async fn post_by_id(&self, id: &[u8; 32]) -> Result<Option<PostRow>, BoardError> {
        Ok(PostsRepo(&self.pool).by_id(id).await?)
    }

    /// Edit a post: authorization is the caller's job (author or moderator).
    /// Mints an Edit follow-up event and applies it to the projection.
    #[allow(clippy::too_many_arguments)]
    pub async fn edit(
        &self,
        target: [u8; 32],
        editor_display: &str,
        editor_seed: &[u8; 32],
        subject: &str,
        body: &str,
        mime: &str,
        now_ms: i64,
    ) -> Result<PostRow, BoardError> {
        if PostsRepo(&self.pool).by_id(&target).await?.is_none() {
            return Err(BoardError::NoSuchPost);
        }
        let editor_key = IdentityKey::from_seed(editor_seed);
        let _event = mint(
            editor_display,
            &editor_key,
            &self.origin,
            &self.origin_key(),
            now_ms,
            EventBody::Edit {
                target,
                subject: subject.to_string(),
                body: body.to_string(),
                mime: mime.to_string(),
            },
        );
        // (The Edit event blob will be stored/flooded in W9; W3 applies it.)
        PostsRepo(&self.pool)
            .apply_edit(&target, subject, body, mime)
            .await?;
        PostsRepo(&self.pool)
            .by_id(&target)
            .await?
            .ok_or(BoardError::NoSuchPost)
    }

    pub async fn tombstone(&self, target: [u8; 32]) -> Result<(), BoardError> {
        if !PostsRepo(&self.pool).apply_tombstone(&target).await? {
            return Err(BoardError::NoSuchPost);
        }
        Ok(())
    }

    pub async fn unread(&self, account_id: i64, board: &str) -> Result<i64, BoardError> {
        let mark = ReadMarksRepo(&self.pool).get(account_id, board).await?;
        Ok(PostsRepo(&self.pool).count_after(board, mark).await?)
    }

    pub async fn mark_read(
        &self,
        account_id: i64,
        board: &str,
        up_to_ms: i64,
    ) -> Result<(), BoardError> {
        ReadMarksRepo(&self.pool)
            .set(account_id, board, up_to_ms)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_store_server::open_in_memory;

    async fn service() -> BoardService {
        let pool = open_in_memory().await.unwrap();
        let svc = BoardService::new(pool, "home".into(), [9u8; 32]);
        svc.create_board("rabbit", "Rabbit", "", 0, None, 0)
            .await
            .unwrap();
        svc.create_board("rabbit.general", "General", "", 2, Some("rabbit"), 0)
            .await
            .unwrap();
        svc
    }

    #[tokio::test]
    async fn post_reply_threading_and_signed_verify() {
        let svc = service().await;
        let seed = [1u8; 32];

        // Can't post to a category.
        assert!(matches!(
            svc.post(
                "rabbit",
                None,
                "alice@home",
                &seed,
                "s",
                "b",
                "text/plain",
                1000
            )
            .await,
            Err(BoardError::NotPostable)
        ));

        let root = svc
            .post(
                "rabbit.general",
                None,
                "alice@home",
                &seed,
                "Hello",
                "world",
                "text/plain",
                1000,
            )
            .await
            .unwrap();
        assert_eq!(
            root.root_id,
            Some(root.event_id),
            "top-level post is its own root"
        );

        let reply = svc
            .post(
                "rabbit.general",
                Some(root.event_id),
                "bob@home",
                &[2u8; 32],
                "re: Hello",
                "hi",
                "text/plain",
                2000,
            )
            .await
            .unwrap();
        assert_eq!(reply.root_id, Some(root.event_id));
        assert_eq!(reply.parent_id, Some(root.event_id));

        // The stored blob verifies under the origin key.
        let ev: SignedEvent = postcard::from_bytes(&root.event_blob).unwrap();
        let origin_pub = IdentityKey::from_seed(&[9u8; 32]).public().0;
        assert!(ev.verify(&origin_pub).is_ok());

        // Thread view: root + 1 reply.
        let full = svc.thread(&root.event_id, 100).await.unwrap();
        assert_eq!(full.len(), 2);
    }

    #[tokio::test]
    async fn unread_tracks_read_mark() {
        let pool_svc = service().await;
        // Need an account for the read-mark FK.
        rabbithole_store_server::repo::AccountsRepo(pool_svc_pool(&pool_svc))
            .create("alice", None, "alice", 1, None)
            .await
            .unwrap();

        pool_svc
            .post(
                "rabbit.general",
                None,
                "alice@home",
                &[1; 32],
                "a",
                "b",
                "text/plain",
                1000,
            )
            .await
            .unwrap();
        pool_svc
            .post(
                "rabbit.general",
                None,
                "alice@home",
                &[1; 32],
                "c",
                "d",
                "text/plain",
                2000,
            )
            .await
            .unwrap();
        assert_eq!(pool_svc.unread(1, "rabbit.general").await.unwrap(), 2);
        pool_svc.mark_read(1, "rabbit.general", 1500).await.unwrap();
        assert_eq!(pool_svc.unread(1, "rabbit.general").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn edit_and_tombstone() {
        let svc = service().await;
        let root = svc
            .post(
                "rabbit.general",
                None,
                "alice@home",
                &[1; 32],
                "s",
                "b",
                "text/plain",
                1000,
            )
            .await
            .unwrap();
        let edited = svc
            .edit(
                root.event_id,
                "alice@home",
                &[1; 32],
                "s2",
                "b2",
                "text/markdown",
                1500,
            )
            .await
            .unwrap();
        assert!(edited.edited && edited.subject == "s2");

        svc.tombstone(root.event_id).await.unwrap();
        let t = svc.post_by_id(&root.event_id).await.unwrap().unwrap();
        assert!(t.tombstoned && t.body.is_empty());
    }

    #[tokio::test]
    async fn retention_caps_threads() {
        let pool = open_in_memory().await.unwrap();
        let svc = BoardService::new(pool, "home".into(), [9u8; 32]);
        svc.create_board("b", "B", "", 2, None, 2).await.unwrap(); // keep 2 threads
        for i in 0..4i64 {
            svc.post(
                "b",
                None,
                "a@home",
                &[1; 32],
                &format!("t{i}"),
                "x",
                "text/plain",
                1000 + i,
            )
            .await
            .unwrap();
        }
        let threads = svc.threads("b", 100).await.unwrap();
        assert_eq!(threads.len(), 2, "retention capped to newest 2 threads");
    }

    // Small helper to reach the pool for account setup in a test.
    fn pool_svc_pool(svc: &BoardService) -> &SqlitePool {
        &svc.pool
    }
}
