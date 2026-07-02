//! The persistent client transfer queue (Wave 4.3).
//!
//! Downloads and uploads are enqueued here and survive restart: the partial
//! file on disk holds the bytes, `bytes_done` records the resume offset, and
//! the queue is drained highest-priority-first. A client driver moves items
//! through the states and applies bandwidth caps / schedule windows on top;
//! this module is just the durable record and its scheduling order.

use rusqlite::{params, Connection, OptionalExtension};

use crate::StoreError;

/// Transfer direction (mirrors `rabbithole_proto::transfer`).
pub const DIR_DOWNLOAD: u8 = 0;
pub const DIR_UPLOAD: u8 = 1;

/// Queue item state.
pub const QUEUED: u8 = 0;
pub const ACTIVE: u8 = 1;
pub const DONE: u8 = 2;
pub const FAILED: u8 = 3;
pub const PAUSED: u8 = 4;

/// A queued transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferItem {
    pub id: i64,
    pub direction: u8,
    pub endpoint: String,
    pub node_id: Option<i64>,
    pub area: Option<String>,
    pub parent: Option<String>,
    pub name: Option<String>,
    pub local_path: String,
    pub size: i64,
    pub bytes_done: i64,
    pub priority: i64,
    pub state: u8,
    pub error: Option<String>,
}

/// A new transfer to enqueue.
#[derive(Debug, Clone, Default)]
pub struct NewTransfer {
    pub direction: u8,
    pub endpoint: String,
    pub node_id: Option<i64>,
    pub area: Option<String>,
    pub parent: Option<String>,
    pub name: Option<String>,
    pub local_path: String,
    pub size: i64,
    pub priority: i64,
}

fn row_to_item(r: &rusqlite::Row) -> rusqlite::Result<TransferItem> {
    Ok(TransferItem {
        id: r.get("id")?,
        direction: r.get::<_, i64>("direction")? as u8,
        endpoint: r.get("endpoint")?,
        node_id: r.get("node_id")?,
        area: r.get("area")?,
        parent: r.get("parent")?,
        name: r.get("name")?,
        local_path: r.get("local_path")?,
        size: r.get("size")?,
        bytes_done: r.get("bytes_done")?,
        priority: r.get("priority")?,
        state: r.get::<_, i64>("state")? as u8,
        error: r.get("error")?,
    })
}

const COLS: &str = "id, direction, endpoint, node_id, area, parent, name, local_path, \
                    size, bytes_done, priority, state, error";

/// The transfer queue, scoped to a client store connection.
pub struct TransferQueue<'a>(pub &'a Connection);

impl TransferQueue<'_> {
    /// Enqueue a transfer at state `QUEUED`. `now` is the caller's unix clock.
    pub fn enqueue(&self, t: &NewTransfer, now: i64) -> Result<i64, StoreError> {
        self.0.execute(
            "INSERT INTO transfer_queue
                 (direction, endpoint, node_id, area, parent, name, local_path,
                  size, bytes_done, priority, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, 0, ?10, ?10)",
            params![
                t.direction as i64,
                t.endpoint,
                t.node_id,
                t.area,
                t.parent,
                t.name,
                t.local_path,
                t.size,
                t.priority,
                now,
            ],
        )?;
        Ok(self.0.last_insert_rowid())
    }

    pub fn get(&self, id: i64) -> Result<Option<TransferItem>, StoreError> {
        let sql = format!("SELECT {COLS} FROM transfer_queue WHERE id = ?1");
        Ok(self
            .0
            .query_row(&sql, params![id], row_to_item)
            .optional()?)
    }

    /// All items, highest priority first (newest-first within a priority).
    pub fn all(&self) -> Result<Vec<TransferItem>, StoreError> {
        let sql = format!("SELECT {COLS} FROM transfer_queue ORDER BY priority DESC, id");
        let mut stmt = self.0.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_item)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The next runnable item (state QUEUED), highest priority first.
    pub fn next_queued(&self) -> Result<Option<TransferItem>, StoreError> {
        let sql = format!(
            "SELECT {COLS} FROM transfer_queue WHERE state = {QUEUED}
             ORDER BY priority DESC, id LIMIT 1"
        );
        Ok(self.0.query_row(&sql, [], row_to_item).optional()?)
    }

    pub fn set_state(&self, id: i64, state: u8, now: i64) -> Result<(), StoreError> {
        self.0.execute(
            "UPDATE transfer_queue SET state = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, state as i64, now],
        )?;
        Ok(())
    }

    /// Record a failure with a message.
    pub fn fail(&self, id: i64, error: &str, now: i64) -> Result<(), StoreError> {
        self.0.execute(
            "UPDATE transfer_queue SET state = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, FAILED as i64, error, now],
        )?;
        Ok(())
    }

    /// Update resume progress (bytes transferred so far).
    pub fn set_progress(&self, id: i64, bytes_done: i64, now: i64) -> Result<(), StoreError> {
        self.0.execute(
            "UPDATE transfer_queue SET bytes_done = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, bytes_done, now],
        )?;
        Ok(())
    }

    pub fn set_priority(&self, id: i64, priority: i64, now: i64) -> Result<(), StoreError> {
        self.0.execute(
            "UPDATE transfer_queue SET priority = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, priority, now],
        )?;
        Ok(())
    }

    pub fn remove(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .0
            .execute("DELETE FROM transfer_queue WHERE id = ?1", params![id])?
            > 0)
    }

    /// Drop all completed items; returns how many were removed.
    pub fn clear_done(&self) -> Result<usize, StoreError> {
        Ok(self.0.execute(
            "DELETE FROM transfer_queue WHERE state = ?1",
            params![DONE as i64],
        )?)
    }

    /// On startup, any item left ACTIVE from a previous run is re-queued so
    /// the driver resumes it (from `bytes_done`).
    pub fn requeue_active(&self, now: i64) -> Result<usize, StoreError> {
        Ok(self.0.execute(
            "UPDATE transfer_queue SET state = ?1, updated_at = ?2 WHERE state = ?3",
            params![QUEUED as i64, now, ACTIVE as i64],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    fn dl(endpoint: &str, node_id: i64, path: &str, priority: i64) -> NewTransfer {
        NewTransfer {
            direction: DIR_DOWNLOAD,
            endpoint: endpoint.into(),
            node_id: Some(node_id),
            local_path: path.into(),
            priority,
            ..Default::default()
        }
    }

    #[test]
    fn enqueue_and_priority_order() {
        let conn = open_in_memory().unwrap();
        let q = TransferQueue(&conn);
        let low = q.enqueue(&dl("h", 1, "/a", 0), 100).unwrap();
        let high = q.enqueue(&dl("h", 2, "/b", 10), 101).unwrap();
        let _mid = q.enqueue(&dl("h", 3, "/c", 5), 102).unwrap();

        // next_queued honors priority.
        assert_eq!(q.next_queued().unwrap().unwrap().id, high);
        // all() is priority-ordered.
        let ids: Vec<i64> = q.all().unwrap().iter().map(|t| t.id).collect();
        assert_eq!(ids[0], high);
        assert_eq!(*ids.last().unwrap(), low);
    }

    #[test]
    fn lifecycle_progress_and_states() {
        let conn = open_in_memory().unwrap();
        let q = TransferQueue(&conn);
        let id = q.enqueue(&dl("h", 1, "/a", 0), 100).unwrap();

        q.set_state(id, ACTIVE, 101).unwrap();
        q.set_progress(id, 4096, 102).unwrap();
        assert_eq!(q.get(id).unwrap().unwrap().bytes_done, 4096);
        // ACTIVE is not returned as next_queued.
        assert!(q.next_queued().unwrap().is_none());

        q.set_state(id, DONE, 103).unwrap();
        assert_eq!(q.clear_done().unwrap(), 1);
        assert!(q.get(id).unwrap().is_none());
    }

    #[test]
    fn requeue_active_on_restart() {
        let conn = open_in_memory().unwrap();
        let q = TransferQueue(&conn);
        let id = q.enqueue(&dl("h", 1, "/a", 0), 100).unwrap();
        q.set_state(id, ACTIVE, 101).unwrap();
        q.set_progress(id, 2048, 101).unwrap();

        // Simulate restart: active → queued, progress preserved for resume.
        assert_eq!(q.requeue_active(200).unwrap(), 1);
        let item = q.next_queued().unwrap().unwrap();
        assert_eq!(item.id, id);
        assert_eq!(item.state, QUEUED);
        assert_eq!(item.bytes_done, 2048);
    }

    #[test]
    fn fail_records_error() {
        let conn = open_in_memory().unwrap();
        let q = TransferQueue(&conn);
        let id = q.enqueue(&dl("h", 1, "/a", 0), 100).unwrap();
        q.fail(id, "connection refused", 101).unwrap();
        let item = q.get(id).unwrap().unwrap();
        assert_eq!(item.state, FAILED);
        assert_eq!(item.error.as_deref(), Some("connection refused"));
    }
}
