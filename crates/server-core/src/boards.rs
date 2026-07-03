//! Board service: mints signed post events, projects them into the store,
//! enforces retention, and tracks per-account read marks.
//!
//! Every post is a [`crate::events::SignedEvent`] — the store keeps the
//! signed blob as the federation source of truth and a denormalized
//! projection for querying. This service is the only place posts are
//! created, so all the invariants (root/parent linkage, author binding,
//! retention) live here.

use rabbithole_identity::keys::IdentityKey;
use rabbithole_store_server::repo4::{
    BoardRow, BoardsRepo, FollowupRow, FollowupsRepo, PostRow, PostsRepo, ReadMarksRepo,
};
use rabbithole_store_server::{SqlitePool, StoreError};

use crate::events::{mint, EventBody, SignedEvent};

/// The kind of a board follow-up event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowupKind {
    Edit,
    Tombstone,
}

impl FollowupKind {
    fn code(self) -> u8 {
        match self {
            Self::Edit => 1,
            Self::Tombstone => 2,
        }
    }
}

/// What ingesting a signed board event did to local state — the federation
/// caller uses it to decide which bus event to re-fire for the flood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// A new post projection was stored (re-fire `BoardPost`).
    Posted(PostRow),
    /// A follow-up applied to an existing post (re-fire `BoardEvent`).
    Applied {
        board: String,
        target: [u8; 32],
        kind: FollowupKind,
    },
    /// A follow-up stored ahead of its (missing) target post — it applies when
    /// the post arrives (re-fire `BoardEvent` so peers who hold the post get
    /// it too).
    Pending {
        board: String,
        target: [u8; 32],
        kind: FollowupKind,
    },
}

/// The federation authorization gate for a **remote** follow-up: apply an
/// Edit/Tombstone to `target` only if the **same author** is acting on their
/// own post (`author_key` matches), or the post's **home server** is
/// moderating its own content (`origin` matches). A third party editing or
/// retracting someone else's cross-origin post is refused. Authenticity
/// (the dual signature) is verified separately, before this gate.
pub fn followup_authorized(followup: &SignedEvent, target: &SignedEvent) -> bool {
    followup.author_key == target.author_key || followup.origin == target.origin
}

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

        // Retention: drop oldest overflow threads in this board, and their
        // follow-ups with them.
        if parent.is_none() && max_threads > 0 {
            for old in PostsRepo(&self.pool)
                .overflow_threads(board, max_threads)
                .await?
            {
                PostsRepo(&self.pool).delete_thread(&old).await?;
                FollowupsRepo(&self.pool).delete_for_root(&old).await?;
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
    /// Mints a signed Edit follow-up event, **stores** it (so it federates),
    /// and applies it to the projection. Returns the updated projection plus
    /// the Edit event's content id — the caller floods it as a `BoardEvent`.
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
    ) -> Result<(PostRow, [u8; 32]), BoardError> {
        let Some(row) = PostsRepo(&self.pool).by_id(&target).await? else {
            return Err(BoardError::NoSuchPost);
        };
        let editor_key = IdentityKey::from_seed(editor_seed);
        let event = mint(
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
        let event_id = event.id;
        let root = row.root_id.unwrap_or(row.event_id);
        self.store_local_followup(&event, &row.board_slug, target, root, FollowupKind::Edit)
            .await?;
        let updated = PostsRepo(&self.pool)
            .by_id(&target)
            .await?
            .ok_or(BoardError::NoSuchPost)?;
        Ok((updated, event_id))
    }

    /// Retract a post: authorization is the caller's job (author or moderator).
    /// Mints a signed Tombstone follow-up event, **stores** it (so it
    /// federates), and applies it. Returns the Tombstone event's content id
    /// for the caller to flood as a `BoardEvent`.
    pub async fn tombstone(
        &self,
        target: [u8; 32],
        actor_display: &str,
        actor_seed: &[u8; 32],
        now_ms: i64,
    ) -> Result<[u8; 32], BoardError> {
        let Some(row) = PostsRepo(&self.pool).by_id(&target).await? else {
            return Err(BoardError::NoSuchPost);
        };
        let actor_key = IdentityKey::from_seed(actor_seed);
        let event = mint(
            actor_display,
            &actor_key,
            &self.origin,
            &self.origin_key(),
            now_ms,
            EventBody::Tombstone { target },
        );
        let event_id = event.id;
        let root = row.root_id.unwrap_or(row.event_id);
        self.store_local_followup(
            &event,
            &row.board_slug,
            target,
            root,
            FollowupKind::Tombstone,
        )
        .await?;
        Ok(event_id)
    }

    /// Ingest a **verified** signed board event of any kind — the federation
    /// entry point. A Post inserts a projection and reconciles any follow-ups
    /// that were waiting for it; an Edit/Tombstone runs the **authorization
    /// gate** ([`followup_authorized`]) against its target and applies it if
    /// the target is present, else parks it pending (the gate re-runs at
    /// reconcile when the target lands). `board` is the delivery's named board
    /// (used only to store a pending follow-up whose target we don't hold).
    ///
    /// Signature verification is the caller's job (`federation.rs`); this
    /// method owns the authorization gate so it is applied consistently on
    /// both the present-now and out-of-order paths.
    pub async fn ingest_event(
        &self,
        event: &SignedEvent,
        board: &str,
        max_threads: i64,
    ) -> Result<IngestOutcome, BoardError> {
        let (target, kind) = match &event.body {
            EventBody::Post { .. } => {
                let row = self.ingest(event, max_threads).await?;
                self.reconcile_followups(&row).await?;
                return Ok(IngestOutcome::Posted(row));
            }
            EventBody::Edit { target, .. } => (*target, FollowupKind::Edit),
            EventBody::Tombstone { target } => (*target, FollowupKind::Tombstone),
        };

        match PostsRepo(&self.pool).by_id(&target).await? {
            // Target present: authorize against it, then store applied.
            Some(p) => {
                let target_event: SignedEvent =
                    postcard::from_bytes(&p.event_blob).map_err(|_| BoardError::NoSuchPost)?;
                if !followup_authorized(event, &target_event) {
                    return Err(BoardError::Forbidden);
                }
                let root = p.root_id.unwrap_or(p.event_id);
                self.store_followup(event, &p.board_slug, target, root, kind, true)
                    .await?;
                self.apply_followup(&event.body).await?;
                Ok(IngestOutcome::Applied {
                    board: p.board_slug,
                    target,
                    kind,
                })
            }
            // Target absent: park pending (authenticity is proven; the
            // authorization gate re-runs at reconcile, dropping it if it
            // fails then).
            None => {
                self.store_followup(event, board, target, target, kind, false)
                    .await?;
                Ok(IngestOutcome::Pending {
                    board: board.to_string(),
                    target,
                    kind,
                })
            }
        }
    }

    /// Store + apply a **locally authored** follow-up (from [`edit`] /
    /// [`tombstone`]). The caller already permission-checked the actor, and a
    /// local moderator may act on any post we hold (including federated ones),
    /// so the federation authorization gate does **not** apply here. The
    /// target is always present (the caller checked).
    async fn store_local_followup(
        &self,
        event: &SignedEvent,
        board: &str,
        target: [u8; 32],
        root: [u8; 32],
        kind: FollowupKind,
    ) -> Result<(), BoardError> {
        self.store_followup(event, board, target, root, kind, true)
            .await?;
        self.apply_followup(&event.body).await
    }

    /// A stored follow-up event by content id (the flood pull-serve path).
    pub async fn followup_by_id(&self, id: &[u8; 32]) -> Result<Option<FollowupRow>, BoardError> {
        Ok(FollowupsRepo(&self.pool).by_id(id).await?)
    }

    /// Insert one follow-up row (idempotent on content id).
    async fn store_followup(
        &self,
        event: &SignedEvent,
        board: &str,
        target: [u8; 32],
        root: [u8; 32],
        kind: FollowupKind,
        applied: bool,
    ) -> Result<(), BoardError> {
        FollowupsRepo(&self.pool)
            .insert(&FollowupRow {
                event_id: event.id,
                target_id: target,
                root_id: root,
                board_slug: board.to_string(),
                kind: kind.code(),
                origin: event.origin.clone(),
                applied,
                created_at: event.created_at_unix_ms,
                event_blob: postcard::to_allocvec(event).expect("serializable"),
            })
            .await?;
        Ok(())
    }

    /// Apply a follow-up's effect to the projection (idempotent).
    async fn apply_followup(&self, body: &EventBody) -> Result<(), BoardError> {
        match body {
            EventBody::Edit {
                target,
                subject,
                body,
                mime,
            } => {
                PostsRepo(&self.pool)
                    .apply_edit(target, subject, body, mime)
                    .await?;
            }
            EventBody::Tombstone { target } => {
                PostsRepo(&self.pool).apply_tombstone(target).await?;
            }
            EventBody::Post { .. } => {}
        }
        Ok(())
    }

    /// Apply follow-ups that were waiting for a just-ingested `post`, oldest
    /// first (an edit chain resolves latest-wins; a tombstone is terminal).
    /// The authorization gate re-runs here now that the target is present:
    /// authorized follow-ups apply; unauthorized ones are dropped as junk.
    /// Local projection catch-up only — never re-floods (each follow-up
    /// already re-fired when first ingested).
    async fn reconcile_followups(&self, post: &PostRow) -> Result<(), BoardError> {
        let target_event: SignedEvent = match postcard::from_bytes(&post.event_blob) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        let repo = FollowupsRepo(&self.pool);
        for f in repo.pending_for(&post.event_id).await? {
            let Ok(signed) = postcard::from_bytes::<SignedEvent>(&f.event_blob) else {
                repo.delete_one(&f.event_id).await?;
                continue;
            };
            if followup_authorized(&signed, &target_event) {
                self.apply_followup(&signed.body).await?;
                repo.mark_applied(&f.event_id).await?;
            } else {
                repo.delete_one(&f.event_id).await?; // unauthorized: junk, drop
            }
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

    /// Mint a signed board event for the federation-ingest tests (the
    /// `origin_key` never matters here — `ingest_event` trusts the event; the
    /// caller verifies signatures).
    fn event(author_seed: &[u8; 32], origin: &str, now: i64, body: EventBody) -> SignedEvent {
        mint(
            &format!("actor@{origin}"),
            &IdentityKey::from_seed(author_seed),
            origin,
            &IdentityKey::from_seed(&[9; 32]),
            now,
            body,
        )
    }

    #[tokio::test]
    async fn edit_and_tombstone_store_verifiable_followups() {
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

        // Edit applies to the projection AND stores a signed, verifiable event.
        let (edited, edit_id) = svc
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
        let f = svc
            .followup_by_id(&edit_id)
            .await
            .unwrap()
            .expect("edit stored");
        assert!(f.applied && f.kind == 1 && f.board_slug == "rabbit.general");
        let ev: SignedEvent = postcard::from_bytes(&f.event_blob).unwrap();
        assert!(ev
            .verify(&IdentityKey::from_seed(&[9; 32]).public().0)
            .is_ok());

        // Tombstone likewise mints + stores a signed event and retracts.
        let tomb_id = svc
            .tombstone(root.event_id, "alice@home", &[1; 32], 1600)
            .await
            .unwrap();
        let t = svc.post_by_id(&root.event_id).await.unwrap().unwrap();
        assert!(t.tombstoned && t.body.is_empty());
        assert_eq!(svc.followup_by_id(&tomb_id).await.unwrap().unwrap().kind, 2);
    }

    #[test]
    fn authorization_gate_allows_author_or_home_server_only() {
        let post = event(
            &[1; 32],
            "home",
            1,
            EventBody::Post {
                board: "b".into(),
                root: None,
                parent: None,
                subject: "s".into(),
                body: "x".into(),
                mime: "text/plain".into(),
            },
        );
        let tomb = |seed: &[u8; 32], origin: &str| {
            event(seed, origin, 2, EventBody::Tombstone { target: post.id })
        };
        // Same author (any origin) — allowed.
        assert!(followup_authorized(&tomb(&[1; 32], "elsewhere"), &post));
        // Same home server, different author (a moderator) — allowed.
        assert!(followup_authorized(&tomb(&[2; 32], "home"), &post));
        // Third party from another origin — refused.
        assert!(!followup_authorized(&tomb(&[2; 32], "evil"), &post));
    }

    #[tokio::test]
    async fn federated_ingest_enforces_authorization() {
        let svc = service().await;
        let post = svc
            .post(
                "rabbit.general",
                None,
                "alice@home",
                &[1; 32],
                "orig",
                "body",
                "text/plain",
                1000,
            )
            .await
            .unwrap();
        let target = post.event_id;
        let edit = |seed: &[u8; 32], origin: &str, body: &str| {
            event(
                seed,
                origin,
                2000,
                EventBody::Edit {
                    target,
                    subject: "s".into(),
                    body: body.into(),
                    mime: "text/plain".into(),
                },
            )
        };

        // A third-party forge (different author + origin) is refused, applies
        // nothing, and stores no follow-up.
        let forge = edit(&[7; 32], "evil", "hacked");
        assert!(matches!(
            svc.ingest_event(&forge, "rabbit.general", 0).await,
            Err(BoardError::Forbidden)
        ));
        assert!(svc.followup_by_id(&forge.id).await.unwrap().is_none());
        assert_eq!(svc.post_by_id(&target).await.unwrap().unwrap().body, "body");

        // The author's own edit applies.
        let ok = edit(&[1; 32], "home", "authored fix");
        assert!(matches!(
            svc.ingest_event(&ok, "rabbit.general", 0).await.unwrap(),
            IngestOutcome::Applied { .. }
        ));
        assert_eq!(
            svc.post_by_id(&target).await.unwrap().unwrap().body,
            "authored fix"
        );
    }

    #[tokio::test]
    async fn out_of_order_followup_reconciles_when_post_arrives() {
        let svc = service().await;
        // A remote post + an authored edit of it, both from origin "home"
        // (via mint), delivered edit-first.
        let post = event(
            &[1; 32],
            "home",
            1000,
            EventBody::Post {
                board: "rabbit.general".into(),
                root: None,
                parent: None,
                subject: "s".into(),
                body: "orig".into(),
                mime: "text/plain".into(),
            },
        );
        let good_edit = event(
            &[1; 32], // same author → authorized once the post lands
            "home",
            2000,
            EventBody::Edit {
                target: post.id,
                subject: "s".into(),
                body: "fixed".into(),
                mime: "text/plain".into(),
            },
        );
        let forge_edit = event(
            &[7; 32], // different author + origin → unauthorized at reconcile
            "evil",
            2500,
            EventBody::Edit {
                target: post.id,
                subject: "s".into(),
                body: "hacked".into(),
                mime: "text/plain".into(),
            },
        );

        // Both edits arrive before the post → pending (no target yet).
        assert!(matches!(
            svc.ingest_event(&good_edit, "rabbit.general", 0)
                .await
                .unwrap(),
            IngestOutcome::Pending { .. }
        ));
        assert!(matches!(
            svc.ingest_event(&forge_edit, "rabbit.general", 0)
                .await
                .unwrap(),
            IngestOutcome::Pending { .. }
        ));

        // The post lands → reconcile applies the authorized edit and drops the
        // forge.
        assert!(matches!(
            svc.ingest_event(&post, "rabbit.general", 0).await.unwrap(),
            IngestOutcome::Posted(_)
        ));
        let p = svc.post_by_id(&post.id).await.unwrap().unwrap();
        assert_eq!(p.body, "fixed", "authorized pending edit applied");
        assert!(
            svc.followup_by_id(&good_edit.id)
                .await
                .unwrap()
                .unwrap()
                .applied
        );
        assert!(
            svc.followup_by_id(&forge_edit.id).await.unwrap().is_none(),
            "unauthorized pending edit dropped at reconcile"
        );
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
