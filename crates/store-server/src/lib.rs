//! Burrow's persistence layer: SQLite (WAL) via sqlx.
//!
//! Repositories for each domain (accounts, personas, boards, files, …)
//! land with their waves, each behind a trait defined next to its domain
//! logic so Postgres can be slotted in later without touching callers.
//! Wave 0 delivers the pool/migration harness those repositories share.

#![forbid(unsafe_code)]

pub mod repo;
pub mod repo2;

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
}
