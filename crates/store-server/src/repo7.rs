//! Wave 13 repository: the moderation suite — user reports, the quarantine
//! set, and the blake3 hash-deny list. The audit trail is the existing
//! `audit_log` table via [`crate::repo::AuditRepo`].

use sqlx::Row;

use crate::{SqlitePool, StoreError};

/// Report states. 0 open, 1 reviewing (claimed), 2 resolved, 3 dismissed.
pub const REPORT_OPEN: u8 = 0;
pub const REPORT_REVIEWING: u8 = 1;
pub const REPORT_RESOLVED: u8 = 2;
pub const REPORT_DISMISSED: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    pub id: i64,
    pub reporter_account: i64,
    /// 0 post, 1 dm, 2 file, 3 user (see `rabbithole_proto::admin::subject_kind`).
    pub subject_kind: u8,
    pub subject_ref: Vec<u8>,
    pub reason: String,
    pub created_at: i64,
    pub state: u8,
    /// Login of the moderator who claimed/closed it; empty = none yet.
    pub resolver: String,
    pub resolved_at: Option<i64>,
    pub resolution: String,
}

fn row_to_report(r: &sqlx::sqlite::SqliteRow) -> ReportRow {
    ReportRow {
        id: r.get("id"),
        reporter_account: r.get("reporter_account"),
        subject_kind: r.get::<i64, _>("subject_kind") as u8,
        subject_ref: r.get("subject_ref"),
        reason: r.get("reason"),
        created_at: r.get("created_at"),
        state: r.get::<i64, _>("state") as u8,
        resolver: r.get("resolver"),
        resolved_at: r.get("resolved_at"),
        resolution: r.get("resolution"),
    }
}

pub struct ReportsRepo<'a>(pub &'a SqlitePool);

impl ReportsRepo<'_> {
    pub async fn create(
        &self,
        reporter_account: i64,
        subject_kind: u8,
        subject_ref: &[u8],
        reason: &str,
    ) -> Result<ReportRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO reports (reporter_account, subject_kind, subject_ref, reason, created_at)
             VALUES (?, ?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(reporter_account)
        .bind(subject_kind as i64)
        .bind(subject_ref)
        .bind(reason)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.by_id(id).await?.expect("just inserted"))
    }

    pub async fn by_id(&self, id: i64) -> Result<Option<ReportRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM reports WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_report(&r)))
    }

    /// The reporter's still-open (open/reviewing) report on the same
    /// subject, if any — the dedupe check for `ReportCreate`.
    pub async fn open_duplicate(
        &self,
        reporter_account: i64,
        subject_kind: u8,
        subject_ref: &[u8],
    ) -> Result<Option<ReportRow>, StoreError> {
        Ok(sqlx::query(
            "SELECT * FROM reports
             WHERE reporter_account = ? AND subject_kind = ? AND subject_ref = ?
               AND state IN (0, 1)
             ORDER BY id LIMIT 1",
        )
        .bind(reporter_account)
        .bind(subject_kind as i64)
        .bind(subject_ref)
        .fetch_optional(self.0)
        .await?
        .map(|r| row_to_report(&r)))
    }

    /// Page reports, optionally filtered by state, oldest first (a queue).
    /// Returns the page and the total count under the same filter.
    pub async fn list(
        &self,
        state: Option<u8>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<ReportRow>, i64), StoreError> {
        let (rows, total) = match state {
            Some(s) => {
                let rows = sqlx::query(
                    "SELECT * FROM reports WHERE state = ? ORDER BY id LIMIT ? OFFSET ?",
                )
                .bind(s as i64)
                .bind(limit)
                .bind(offset)
                .fetch_all(self.0)
                .await?;
                let total: i64 = sqlx::query("SELECT COUNT(*) AS n FROM reports WHERE state = ?")
                    .bind(s as i64)
                    .fetch_one(self.0)
                    .await?
                    .get("n");
                (rows, total)
            }
            None => {
                let rows = sqlx::query("SELECT * FROM reports ORDER BY id LIMIT ? OFFSET ?")
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(self.0)
                    .await?;
                let total: i64 = sqlx::query("SELECT COUNT(*) AS n FROM reports")
                    .fetch_one(self.0)
                    .await?
                    .get("n");
                (rows, total)
            }
        };
        Ok((rows.iter().map(row_to_report).collect(), total))
    }

    /// Move a report into `state`. Terminal states (resolved/dismissed)
    /// stamp `resolved_at`; a claim leaves it NULL. Returns `false` when the
    /// report doesn't exist or isn't in one of `from` (state machine guard).
    pub async fn set_state(
        &self,
        id: i64,
        from: &[u8],
        state: u8,
        resolver: &str,
        resolution: &str,
    ) -> Result<bool, StoreError> {
        let terminal = state == REPORT_RESOLVED || state == REPORT_DISMISSED;
        // `from` has at most a few entries; build the IN list inline.
        let placeholders = vec!["?"; from.len().max(1)].join(", ");
        let sql = format!(
            "UPDATE reports SET state = ?, resolver = ?, resolution = ?,
                 resolved_at = CASE WHEN ? THEN unixepoch() ELSE NULL END
             WHERE id = ? AND state IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql)
            .bind(state as i64)
            .bind(resolver)
            .bind(resolution)
            .bind(terminal)
            .bind(id);
        for s in from {
            q = q.bind(*s as i64);
        }
        Ok(q.execute(self.0).await?.rows_affected() > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRow {
    pub subject_kind: u8,
    pub subject_ref: Vec<u8>,
    pub reason: String,
    pub added_by: String,
    pub created_at: i64,
}

pub struct QuarantineRepo<'a>(pub &'a SqlitePool);

impl QuarantineRepo<'_> {
    /// Add (or refresh) a quarantine entry. Idempotent.
    pub async fn set(
        &self,
        subject_kind: u8,
        subject_ref: &[u8],
        reason: &str,
        added_by: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO quarantine (subject_kind, subject_ref, reason, added_by, created_at)
             VALUES (?, ?, ?, ?, unixepoch())
             ON CONFLICT(subject_kind, subject_ref)
             DO UPDATE SET reason = excluded.reason, added_by = excluded.added_by",
        )
        .bind(subject_kind as i64)
        .bind(subject_ref)
        .bind(reason)
        .bind(added_by)
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn clear(&self, subject_kind: u8, subject_ref: &[u8]) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("DELETE FROM quarantine WHERE subject_kind = ? AND subject_ref = ?")
                .bind(subject_kind as i64)
                .bind(subject_ref)
                .execute(self.0)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn all(&self) -> Result<Vec<QuarantineRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM quarantine ORDER BY created_at, subject_ref")
            .fetch_all(self.0)
            .await?;
        Ok(rows
            .iter()
            .map(|r| QuarantineRow {
                subject_kind: r.get::<i64, _>("subject_kind") as u8,
                subject_ref: r.get("subject_ref"),
                reason: r.get("reason"),
                added_by: r.get("added_by"),
                created_at: r.get("created_at"),
            })
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyHashRow {
    pub hash: [u8; 32],
    pub reason: String,
    pub added_by: String,
    pub created_at: i64,
}

pub struct DenyHashesRepo<'a>(pub &'a SqlitePool);

impl DenyHashesRepo<'_> {
    /// Add (or refresh) a denied hash. Idempotent.
    pub async fn add(
        &self,
        hash: &[u8; 32],
        reason: &str,
        added_by: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO deny_hashes (hash, reason, added_by, created_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT(hash) DO UPDATE SET reason = excluded.reason,
                 added_by = excluded.added_by",
        )
        .bind(&hash[..])
        .bind(reason)
        .bind(added_by)
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn remove(&self, hash: &[u8; 32]) -> Result<bool, StoreError> {
        Ok(sqlx::query("DELETE FROM deny_hashes WHERE hash = ?")
            .bind(&hash[..])
            .execute(self.0)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn all(&self) -> Result<Vec<DenyHashRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM deny_hashes ORDER BY created_at, hash")
            .fetch_all(self.0)
            .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let bytes: Vec<u8> = r.get("hash");
                let hash: [u8; 32] = bytes.try_into().ok()?;
                Some(DenyHashRow {
                    hash,
                    reason: r.get("reason"),
                    added_by: r.get("added_by"),
                    created_at: r.get("created_at"),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn report_lifecycle_and_queue_paging() {
        let pool = open_in_memory().await.unwrap();
        let repo = ReportsRepo(&pool);

        let a = repo.create(1, 0, &[7u8; 32], "spam").await.unwrap();
        assert_eq!(a.state, REPORT_OPEN);
        assert_eq!(a.subject_ref, vec![7u8; 32]);
        assert!(a.resolved_at.is_none());
        let _b = repo
            .create(2, 3, &5i64.to_le_bytes(), "abuse")
            .await
            .unwrap();

        // Dedupe finds the reporter's still-open report on the same subject…
        let dup = repo.open_duplicate(1, 0, &[7u8; 32]).await.unwrap();
        assert_eq!(dup.as_ref().map(|r| r.id), Some(a.id));
        // …but not across reporters, kinds, or refs.
        assert!(repo
            .open_duplicate(2, 0, &[7u8; 32])
            .await
            .unwrap()
            .is_none());
        assert!(repo
            .open_duplicate(1, 2, &[7u8; 32])
            .await
            .unwrap()
            .is_none());
        assert!(repo
            .open_duplicate(1, 0, &[8u8; 32])
            .await
            .unwrap()
            .is_none());

        // Queue: oldest first, filterable by state, honest totals.
        let (open, total) = repo.list(Some(REPORT_OPEN), 0, 10).await.unwrap();
        assert_eq!((open.len(), total), (2, 2));
        assert_eq!(open[0].id, a.id);
        let (page, total) = repo.list(None, 1, 10).await.unwrap();
        assert_eq!((page.len(), total), (1, 2));

        // Claim guards the state machine: only an open report can be claimed.
        assert!(repo
            .set_state(a.id, &[REPORT_OPEN], REPORT_REVIEWING, "mo", "")
            .await
            .unwrap());
        assert!(!repo
            .set_state(a.id, &[REPORT_OPEN], REPORT_REVIEWING, "mo", "")
            .await
            .unwrap());
        let claimed = repo.by_id(a.id).await.unwrap().unwrap();
        assert_eq!(
            (claimed.state, claimed.resolver.as_str()),
            (REPORT_REVIEWING, "mo")
        );
        assert!(claimed.resolved_at.is_none(), "claim is not terminal");

        // A claimed report is no longer an open duplicate? It is — still live.
        assert!(repo
            .open_duplicate(1, 0, &[7u8; 32])
            .await
            .unwrap()
            .is_some());

        // Resolve stamps the terminal state.
        assert!(repo
            .set_state(
                a.id,
                &[REPORT_OPEN, REPORT_REVIEWING],
                REPORT_RESOLVED,
                "mo",
                "removed it",
            )
            .await
            .unwrap());
        let done = repo.by_id(a.id).await.unwrap().unwrap();
        assert_eq!(done.state, REPORT_RESOLVED);
        assert_eq!(done.resolution, "removed it");
        assert!(done.resolved_at.is_some());
        // Once terminal it stops deduping (the subject can be re-reported)…
        assert!(repo
            .open_duplicate(1, 0, &[7u8; 32])
            .await
            .unwrap()
            .is_none());
        // …and can't be moved again.
        assert!(!repo
            .set_state(
                a.id,
                &[REPORT_OPEN, REPORT_REVIEWING],
                REPORT_DISMISSED,
                "mo",
                "",
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn quarantine_set_clear_and_load() {
        let pool = open_in_memory().await.unwrap();
        let repo = QuarantineRepo(&pool);
        repo.set(0, &[1u8; 32], "under review", "mo").await.unwrap();
        repo.set(2, &[2u8; 32], "", "mo").await.unwrap();
        // Idempotent set refreshes rather than duplicating.
        repo.set(0, &[1u8; 32], "still under review", "mo2")
            .await
            .unwrap();
        let all = repo.all().await.unwrap();
        assert_eq!(all.len(), 2);
        let post = all.iter().find(|q| q.subject_kind == 0).unwrap();
        assert_eq!(post.reason, "still under review");
        assert_eq!(post.added_by, "mo2");

        assert!(repo.clear(0, &[1u8; 32]).await.unwrap());
        assert!(!repo.clear(0, &[1u8; 32]).await.unwrap(), "already gone");
        assert_eq!(repo.all().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deny_hashes_roundtrip() {
        let pool = open_in_memory().await.unwrap();
        let repo = DenyHashesRepo(&pool);
        repo.add(&[9u8; 32], "bad bits", "mo").await.unwrap();
        repo.add(&[9u8; 32], "worse bits", "mo").await.unwrap(); // idempotent
        repo.add(&[10u8; 32], "", "mo").await.unwrap();
        let all = repo.all().await.unwrap();
        assert_eq!(all.len(), 2);
        let first = all.iter().find(|d| d.hash == [9u8; 32]).unwrap();
        assert_eq!(first.reason, "worse bits");

        assert!(repo.remove(&[9u8; 32]).await.unwrap());
        assert!(!repo.remove(&[9u8; 32]).await.unwrap());
        assert_eq!(repo.all().await.unwrap().len(), 1);
    }
}
