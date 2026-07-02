//! The client's local store (rusqlite, bundled SQLite).
//!
//! Holds session cache, offline board copies, the outbox, the persistent
//! transfer queue, and prefs — the machinery behind offline mode (PLAN
//! §9.4) and resumable transfers (§9.5). Wave 0 delivers the migration
//! harness; tables land with their waves.
//!
//! Migrations are ordered SQL scripts applied under `PRAGMA user_version`:
//! simple, dependency-free, and adequate for an embedded store.

#![forbid(unsafe_code)]

pub mod boards;

use std::path::Path;

pub use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Ordered migration scripts. Index + 1 == resulting `user_version`.
const MIGRATIONS: &[&str] = &[
    // 0001: schema bootstrap (real tables arrive with their waves).
    "CREATE TABLE client_meta (
         key   TEXT PRIMARY KEY NOT NULL,
         value TEXT NOT NULL
     ) STRICT;
     INSERT INTO client_meta (key, value) VALUES ('schema_epoch', 'wave-0');",
    // 0002: offline board cache + reply outbox (Wave 3.2). Posts are keyed
    // by their 32-byte content id so a delta download can merge idempotently;
    // the outbox holds replies composed offline until they can be sent.
    "CREATE TABLE cached_boards (
         slug        TEXT PRIMARY KEY NOT NULL,
         title       TEXT NOT NULL,
         description TEXT NOT NULL DEFAULT '',
         kind        INTEGER NOT NULL,
         parent_slug TEXT,
         unread      INTEGER NOT NULL DEFAULT 0,
         synced_at   INTEGER NOT NULL DEFAULT 0
     ) STRICT;

     CREATE TABLE cached_posts (
         id           BLOB PRIMARY KEY NOT NULL,
         board        TEXT NOT NULL,
         root         BLOB,
         parent       BLOB,
         author       TEXT NOT NULL,
         subject      TEXT NOT NULL,
         body         TEXT NOT NULL,
         mime         TEXT NOT NULL,
         created_at   INTEGER NOT NULL,
         edited       INTEGER NOT NULL DEFAULT 0,
         tombstoned   INTEGER NOT NULL DEFAULT 0
     ) STRICT;
     CREATE INDEX cached_posts_board ON cached_posts(board, created_at);
     CREATE INDEX cached_posts_root  ON cached_posts(root, created_at);

     CREATE TABLE board_read_marks (
         board    TEXT PRIMARY KEY NOT NULL,
         up_to_ms INTEGER NOT NULL
     ) STRICT;

     CREATE TABLE board_outbox (
         id            INTEGER PRIMARY KEY AUTOINCREMENT,
         board         TEXT NOT NULL,
         parent        BLOB,
         subject       TEXT NOT NULL,
         body          TEXT NOT NULL,
         mime          TEXT NOT NULL DEFAULT 'text/plain',
         created_at    INTEGER NOT NULL,
         sent          INTEGER NOT NULL DEFAULT 0,
         sent_event_id BLOB
     ) STRICT;
     CREATE INDEX board_outbox_pending ON board_outbox(sent, id);",
];

/// Open (creating if needed) the local store and apply pending migrations.
pub fn open(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    init(&conn)?;
    Ok(conn)
}

/// In-memory store for tests.
pub fn open_in_memory() -> Result<Connection, StoreError> {
    let conn = Connection::open_in_memory()?;
    init(&conn)?;
    Ok(conn)
}

fn init(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "journal_mode", "WAL").ok(); // no-op in memory
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(conn)
}

fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, script) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        conn.execute_batch(script)?;
        conn.pragma_update(None, "user_version", i as u32 + 1)?;
    }
    Ok(())
}

/// The store's current schema version (for diagnostics).
pub fn schema_version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let conn = open_in_memory().unwrap();
        assert_eq!(schema_version(&conn).unwrap() as usize, MIGRATIONS.len());
        let epoch: String = conn
            .query_row(
                "SELECT value FROM client_meta WHERE key = 'schema_epoch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(epoch, "wave-0");
        migrate(&conn).unwrap();
    }

    #[test]
    fn reopening_on_disk_preserves_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rabbit.db");
        {
            let _ = open(&path).unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap() as usize, MIGRATIONS.len());
    }
}
