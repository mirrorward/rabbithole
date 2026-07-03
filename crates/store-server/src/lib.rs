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
