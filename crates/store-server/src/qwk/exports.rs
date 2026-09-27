//! Bounded, account-scoped QWK routing snapshots. A REP cannot identify its
//! export itself; only an explicit export id selects these frozen identities.
use crate::repo4::PostRow;
use crate::{SqlitePool, StoreError};
use sqlx::{Row, SqliteConnection};

pub const EXPORT_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub const MAX_EXPORTS: i64 = 8;
pub const MAX_CONFERENCES: usize = 255;
pub const MAX_REFERENCES: usize = 2000;
#[derive(Debug, Clone)]
pub struct ExportBoard {
    pub conference: u16,
    pub id: i64,
    pub slug: String,
}
#[derive(Debug, Clone)]
pub struct ExportReference {
    pub conference: u16,
    pub number: u32,
    pub event_id: [u8; 32],
}
#[derive(Debug, Clone)]
pub struct ExportGuard {
    pub export_id: String,
    pub conference: u16,
    pub reference: u32,
}
#[derive(Debug, Clone)]
pub struct ResolvedReply {
    pub slug: String,
    pub parent: Option<[u8; 32]>,
}
pub struct QwkExportsRepo<'a>(pub &'a SqlitePool);

impl QwkExportsRepo<'_> {
    pub async fn new_id(&self) -> Result<String, StoreError> {
        Ok(sqlx::query_scalar("SELECT lower(hex(randomblob(16)))")
            .fetch_one(self.0)
            .await?)
    }
    /// Capture identity and numbering together, before reading message bodies.
    pub async fn conferences(&self) -> Result<Vec<ExportBoard>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, slug FROM boards WHERE kind=2 ORDER BY slug COLLATE BINARY LIMIT 255",
        )
        .fetch_all(self.0)
        .await?;
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(i, row)| ExportBoard {
                conference: (i + 1) as u16,
                id: row.get("id"),
                slug: row.get("slug"),
            })
            .collect())
    }
    /// Commit a completed export and its read marks atomically. No marks move
    /// when a concurrently removed/recreated board invalidates the snapshot.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        id: &str,
        account: i64,
        bbs: &str,
        boards: &[ExportBoard],
        refs: &[ExportReference],
        marks: &[(String, i64)],
        now: i64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        if boards.len() > MAX_CONFERENCES
            || refs.len() > MAX_REFERENCES
            || marks.len() > MAX_CONFERENCES
        {
            return Ok(None);
        }
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        for b in boards {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM boards WHERE id=? AND slug=? COLLATE BINARY AND kind=2)").bind(b.id).bind(&b.slug).fetch_one(&mut *tx).await?;
            if !valid {
                return Ok(None);
            }
        }
        sqlx::query(
            "INSERT INTO qwk_exports(id,account_id,bbs_id,created_at,expires_at) VALUES(?,?,?,?,?)",
        )
        .bind(id)
        .bind(account)
        .bind(bbs)
        .bind(now)
        .bind(now.saturating_add(EXPORT_TTL_MS))
        .execute(&mut *tx)
        .await?;
        for b in boards {
            sqlx::query(
                "INSERT INTO qwk_export_boards(export_id,conference,board_id,slug) VALUES(?,?,?,?)",
            )
            .bind(id)
            .bind(b.conference as i64)
            .bind(b.id)
            .bind(&b.slug)
            .execute(&mut *tx)
            .await?;
        }
        for r in refs {
            sqlx::query("INSERT INTO qwk_export_refs(export_id,conference,number,event_id) VALUES(?,?,?,?) ON CONFLICT DO NOTHING").bind(id).bind(r.conference as i64).bind(r.number as i64).bind(r.event_id.as_slice()).execute(&mut *tx).await?;
        }
        for (slug, mark) in marks {
            if !boards.iter().any(|b| &b.slug == slug) {
                return Ok(None);
            }
            sqlx::query("INSERT INTO read_marks(account_id,board_slug,last_read_ms) VALUES(?,?,?) ON CONFLICT(account_id,board_slug) DO UPDATE SET last_read_ms=MAX(last_read_ms,excluded.last_read_ms)").bind(account).bind(slug).bind(mark).execute(&mut *tx).await?;
        }
        // Return only committed, evicted identities. In-flight export dirs
        // have no row yet and must never be pruned by a concurrent build.
        let evicted:Vec<String>=sqlx::query_scalar("SELECT id FROM qwk_exports WHERE account_id=? AND id!=? AND (expires_at<=? OR id NOT IN (SELECT id FROM qwk_exports WHERE account_id=? AND id!=? ORDER BY created_at DESC,id DESC LIMIT ?))")
            .bind(account).bind(id).bind(now).bind(account).bind(id).bind(MAX_EXPORTS-1).fetch_all(&mut *tx).await?;
        for old in &evicted {
            sqlx::query("DELETE FROM qwk_exports WHERE id=?")
                .bind(old)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Some(evicted))
    }

    pub async fn bbs_id(
        &self,
        account: i64,
        id: &str,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT bbs_id FROM qwk_exports WHERE id=? AND account_id=? AND expires_at>?",
        )
        .bind(id)
        .bind(account)
        .bind(now)
        .fetch_optional(self.0)
        .await?)
    }
    pub async fn resolve(
        &self,
        account: i64,
        guard: &ExportGuard,
        now: i64,
    ) -> Result<Option<ResolvedReply>, StoreError> {
        resolve_on(&mut *self.0.acquire().await?, account, guard, now).await
    }
}
async fn resolve_on(
    conn: &mut SqliteConnection,
    account: i64,
    g: &ExportGuard,
    now: i64,
) -> Result<Option<ResolvedReply>, StoreError> {
    let slug:Option<String>=sqlx::query_scalar("SELECT b.slug FROM qwk_exports e JOIN qwk_export_boards b ON b.export_id=e.id JOIN boards current ON current.id=b.board_id AND current.slug=b.slug COLLATE BINARY AND current.kind=2 JOIN accounts a ON a.id=e.account_id WHERE e.id=? AND e.account_id=? AND e.expires_at>? AND b.conference=? AND a.disabled=0")
        .bind(&g.export_id).bind(account).bind(now).bind(g.conference as i64).fetch_optional(&mut *conn).await?;
    let Some(slug) = slug else { return Ok(None) };
    let parent = if g.reference == 0 {
        None
    } else {
        let id:Option<Vec<u8>>=sqlx::query_scalar("SELECT r.event_id FROM qwk_export_refs r JOIN posts p ON p.event_id=r.event_id WHERE r.export_id=? AND r.conference=? AND r.number=? AND p.board_slug=? COLLATE BINARY AND p.tombstoned=0")
            .bind(&g.export_id).bind(g.conference as i64).bind(g.reference as i64).bind(&slug).fetch_optional(&mut *conn).await?;
        let Some(id) = id else { return Ok(None) };
        let Ok(id) = id.try_into() else {
            return Ok(None);
        };
        Some(id)
    };
    Ok(Some(ResolvedReply { slug, parent }))
}
/// Recheck identities while holding the same writer lock as the receipt/post.
pub(super) async fn validate_on(
    conn: &mut SqliteConnection,
    account: i64,
    g: &ExportGuard,
    post: &PostRow,
) -> Result<bool, StoreError> {
    // Sample only after BEGIN IMMEDIATE has acquired its write lock. A
    // stale signed-event timestamp cannot prolong a packet after lock waits.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(i64::MAX);
    Ok(resolve_on(conn, account, g, now)
        .await?
        .is_some_and(|r| r.slug == post.board_slug && r.parent == post.parent_id))
}
