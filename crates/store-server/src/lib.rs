//! Burrow's persistence layer: SQLite (WAL) via sqlx.
//!
//! Repositories for each domain (accounts, personas, boards, files, …)
//! land with their waves, each behind a trait defined next to its domain
//! logic so Postgres can be slotted in later without touching callers.
//! Wave 0 delivers the pool/migration harness those repositories share.

#![forbid(unsafe_code)]

pub mod repo;
pub mod repo2;
pub mod repo3;
pub mod repo4;
pub mod repo5;
pub mod repo6;
pub mod repo7;
pub mod repo8;

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
pub use sqlx::SqlitePool;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Open (creating if needed) the server database and run pending
/// migrations. WAL journaling for concurrent readers.
pub async fn open(path: &Path) -> Result<SqlitePool, StoreError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(16)
        .connect_with(options)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// In-memory database for tests.
pub async fn open_in_memory() -> Result<SqlitePool, StoreError> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .in_memory(true)
                .foreign_keys(true),
        )
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> Result<(), StoreError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// Online backup: `VACUUM INTO` writes a consistent point-in-time copy of
/// the database to `dest` (which must not exist yet). It runs inside a read
/// transaction, so it's safe under WAL with concurrent readers *and*
/// writers — writes that land after the vacuum's snapshot simply aren't in
/// the copy. The `INTO` target is an SQL expression, so the path binds as a
/// regular parameter (no string splicing).
pub async fn vacuum_into(pool: &SqlitePool, dest: &Path) -> Result<(), StoreError> {
    sqlx::query("VACUUM INTO ?1")
        .bind(dest.to_string_lossy().into_owned())
        .execute(pool)
        .await?;
    Ok(())
}

/// Open the database at `path` read-only (no migrations, no writes) and run
/// `PRAGMA integrity_check`, returning its first result row — `"ok"` when
/// the file is sound. Used to vet backup snapshots without touching them.
pub async fn integrity_check(path: &Path) -> Result<String, StoreError> {
    let options = SqliteConnectOptions::new().filename(path).read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let (result,): (String,) = sqlx::query_as("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    pool.close().await;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_are_idempotent() {
        let pool = open_in_memory().await.unwrap();
        let (epoch,): (String,) =
            sqlx::query_as("SELECT value FROM server_meta WHERE key = 'schema_epoch'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(epoch, "wave-0");
        // Running migrate again is a no-op, not an error.
        migrate(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn canonical_board_migration_repairs_projections_without_rewriting_history() {
        use crate::repo4::{BoardsRepo, FollowupRow, FollowupsRepo, PostRow, PostsRepo};

        let pool = open_in_memory().await.unwrap();
        // Migration 18 changes data only. Removing its applied marker lets
        // this fixture exercise the same upgrade path as an existing store.
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 18")
            .execute(&pool)
            .await
            .unwrap();
        BoardsRepo(&pool)
            .create("Rabbit.General", "General", "", 2, None, 1)
            .await
            .unwrap();
        let posts = PostsRepo(&pool);
        let followups = FollowupsRepo(&pool);
        for (id, board, parent) in [
            (1, "RABBIT.GENERAL", None),
            (2, "rabbit.general", Some([1; 32])),
            (3, "Rabbit.General", None),
            (4, "Unknown.Board", Some([77; 32])),
        ] {
            posts
                .insert(&PostRow {
                    event_id: [id; 32],
                    board_slug: board.into(),
                    root_id: Some(parent.unwrap_or([id; 32])),
                    parent_id: parent,
                    author: "remote author".into(),
                    subject: format!("subject {id}"),
                    body: format!("body {id}"),
                    mime: "text/plain".into(),
                    created_at: i64::from(id) * 1000,
                    edited: false,
                    tombstoned: false,
                    event_blob: vec![id; 64],
                })
                .await
                .unwrap();
        }
        posts
            .apply_edit(&[1; 32], "edited", "kept", "text/markdown")
            .await
            .unwrap();
        posts.apply_tombstone(&[2; 32]).await.unwrap();
        sqlx::query("UPDATE posts SET author_key = X'1234', origin = 'remote' WHERE event_id = ?")
            .bind([1u8; 32].as_slice())
            .execute(&pool)
            .await
            .unwrap();
        for (id, target, board, applied) in [
            (10, 2, "RABBIT.GENERAL", true),
            (11, 99, "rabbit.general", false),
            (12, 2, "Unknown.Board", false),
            (13, 4, "rabbit.general", false),
        ] {
            followups
                .insert(&FollowupRow {
                    event_id: [id; 32],
                    target_id: [target; 32],
                    root_id: [target; 32],
                    board_slug: board.into(),
                    kind: 1,
                    origin: "remote".into(),
                    applied,
                    created_at: 9000,
                    event_blob: vec![id; 64],
                })
                .await
                .unwrap();
        }
        let mut expected_posts = Vec::new();
        for id in 1..=4 {
            let mut row = posts.by_id(&[id; 32]).await.unwrap().unwrap();
            if id != 4 {
                row.board_slug = "Rabbit.General".into();
            }
            expected_posts.push(row);
        }
        let mut expected_followups = Vec::new();
        for id in 10..=13 {
            let mut row = followups.by_id(&[id; 32]).await.unwrap().unwrap();
            if id != 12 {
                row.board_slug = "Rabbit.General".into();
            }
            if id == 10 {
                row.root_id = [1; 32];
            }
            expected_followups.push(row);
        }

        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();
        // The repair itself also remains harmless if deliberately reapplied.
        sqlx::raw_sql(include_str!(
            "../migrations/0018_canonical_board_projection.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        for expected in expected_posts {
            assert_eq!(
                posts.by_id(&expected.event_id).await.unwrap(),
                Some(expected)
            );
        }
        for expected in expected_followups {
            assert_eq!(
                followups.by_id(&expected.event_id).await.unwrap(),
                Some(expected)
            );
        }
        let provenance: (Vec<u8>, String) =
            sqlx::query_as("SELECT author_key, origin FROM posts WHERE event_id = ?")
                .bind([1u8; 32].as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(provenance, (vec![0x12, 0x34], "remote".into()));
        assert_eq!(
            BoardsRepo(&pool).keeping().await.unwrap(),
            vec![("Rabbit.General".into(), 1, 2)]
        );
        assert_eq!(posts.threads("Rabbit.General", 10).await.unwrap().len(), 2);
        assert_eq!(posts.count_after("Rabbit.General", 0).await.unwrap(), 2);
        // No history was pruned by upgrade; ordinary retention now sees the
        // formerly invisible root and cascades to the repaired reply follow-up.
        assert_eq!(
            posts.overflow_threads("Rabbit.General", 1).await.unwrap(),
            vec![[1; 32]]
        );
        posts.delete_thread(&[1; 32]).await.unwrap();
        assert_eq!(followups.delete_for_root(&[1; 32]).await.unwrap(), 1);
        assert!(followups.by_id(&[11; 32]).await.unwrap().is_some());
        assert!(posts.by_id(&[4; 32]).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn open_on_disk_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.db");
        let pool = open(&path).await.unwrap();
        drop(pool);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn vacuum_into_produces_sound_copy() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open(&dir.path().join("live.db")).await.unwrap();
        sqlx::query("INSERT INTO server_meta (key, value) VALUES ('probe', 'x')")
            .execute(&pool)
            .await
            .unwrap();

        let copy = dir.path().join("copy.db");
        vacuum_into(&pool, &copy).await.unwrap();
        assert!(copy.exists());
        assert_eq!(integrity_check(&copy).await.unwrap(), "ok");

        // Vacuuming into an existing file is refused by SQLite (the online
        // backup never clobbers).
        assert!(vacuum_into(&pool, &copy).await.is_err());
    }
}
