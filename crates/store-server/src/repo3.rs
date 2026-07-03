//! Wave 2.2 repositories: buddies, blocks, direct messages.

use sqlx::Row;

use crate::{SqlitePool, StoreError};

pub struct BuddiesRepo<'a>(pub &'a SqlitePool);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuddyRow {
    pub screen_name: String,
    pub group: String,
}

impl BuddiesRepo<'_> {
    pub async fn add(
        &self,
        account_id: i64,
        screen_name: &str,
        group: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO buddies (account_id, screen_name, grp, added_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT (account_id, screen_name) DO UPDATE SET grp = excluded.grp",
        )
        .bind(account_id)
        .bind(screen_name)
        .bind(group)
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn remove(&self, account_id: i64, screen_name: &str) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("DELETE FROM buddies WHERE account_id = ? AND screen_name = ?")
                .bind(account_id)
                .bind(screen_name)
                .execute(self.0)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn list(&self, account_id: i64) -> Result<Vec<BuddyRow>, StoreError> {
        Ok(sqlx::query(
            "SELECT screen_name, grp FROM buddies WHERE account_id = ? ORDER BY grp, screen_name",
        )
        .bind(account_id)
        .fetch_all(self.0)
        .await?
        .iter()
        .map(|r| BuddyRow {
            screen_name: r.get("screen_name"),
            group: r.get("grp"),
        })
        .collect())
    }
}

pub struct BlocksRepo<'a>(pub &'a SqlitePool);

impl BlocksRepo<'_> {
    pub async fn add(
        &self,
        account_id: i64,
        blocked_account: i64,
        screen_name: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO blocks (account_id, blocked_account, screen_name, added_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT (account_id, blocked_account) DO NOTHING",
        )
        .bind(account_id)
        .bind(blocked_account)
        .bind(screen_name)
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn remove(&self, account_id: i64, blocked_account: i64) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("DELETE FROM blocks WHERE account_id = ? AND blocked_account = ?")
                .bind(account_id)
                .bind(blocked_account)
                .execute(self.0)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn is_blocked(&self, account_id: i64, by_account: i64) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("SELECT 1 FROM blocks WHERE account_id = ? AND blocked_account = ?")
                .bind(by_account)
                .bind(account_id)
                .fetch_optional(self.0)
                .await?
                .is_some(),
        )
    }

    pub async fn list(&self, account_id: i64) -> Result<Vec<String>, StoreError> {
        Ok(
            sqlx::query("SELECT screen_name FROM blocks WHERE account_id = ? ORDER BY screen_name")
                .bind(account_id)
                .fetch_all(self.0)
                .await?
                .iter()
                .map(|r| r.get("screen_name"))
                .collect(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmRow {
    pub id: i64,
    pub from_account: i64,
    pub from_persona: String,
    pub to_account: i64,
    pub to_persona: String,
    pub text: String,
    pub quote_of: Option<i64>,
    pub attachments_hex: Vec<String>,
    pub at_ms: i64,
    pub is_auto: bool,
    pub read_at: Option<i64>,
}

fn row_to_dm(r: &sqlx::sqlite::SqliteRow) -> DmRow {
    let attachments: Vec<String> =
        serde_json::from_str(&r.get::<String, _>("attachments")).unwrap_or_default();
    DmRow {
        id: r.get("id"),
        from_account: r.get("from_account"),
        from_persona: r.get("from_persona"),
        to_account: r.get("to_account"),
        to_persona: r.get("to_persona"),
        text: r.get("text"),
        quote_of: r.get("quote_of"),
        attachments_hex: attachments,
        at_ms: r.get("at"),
        is_auto: r.get::<i64, _>("is_auto") != 0,
        read_at: r.get("read_at"),
    }
}

pub struct DmsRepo<'a>(pub &'a SqlitePool);

impl DmsRepo<'_> {
    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        &self,
        from_account: i64,
        from_persona: &str,
        to_account: i64,
        to_persona: &str,
        text: &str,
        quote_of: Option<i64>,
        attachments_hex: &[String],
        at_ms: i64,
        is_auto: bool,
    ) -> Result<i64, StoreError> {
        Ok(sqlx::query(
            "INSERT INTO dms (from_account, from_persona, to_account, to_persona,
                              text, quote_of, attachments, at, is_auto)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(from_account)
        .bind(from_persona)
        .bind(to_account)
        .bind(to_persona)
        .bind(text)
        .bind(quote_of)
        .bind(serde_json::to_string(attachments_hex).expect("serializable"))
        .bind(at_ms)
        .bind(is_auto as i64)
        .fetch_one(self.0)
        .await?
        .get("id"))
    }

    /// Thread between two accounts, newest last. `before_id = 0` = newest.
    pub async fn thread(
        &self,
        a: i64,
        b: i64,
        before_id: i64,
        limit: i64,
    ) -> Result<Vec<DmRow>, StoreError> {
        let before = if before_id <= 0 { i64::MAX } else { before_id };
        let mut rows: Vec<DmRow> = sqlx::query(
            "SELECT * FROM dms
             WHERE ((from_account = ?1 AND to_account = ?2)
                 OR (from_account = ?2 AND to_account = ?1))
               AND id < ?3
             ORDER BY id DESC LIMIT ?4",
        )
        .bind(a)
        .bind(b)
        .bind(before)
        .bind(limit)
        .fetch_all(self.0)
        .await?
        .iter()
        .map(row_to_dm)
        .collect();
        rows.reverse();
        Ok(rows)
    }

    /// Unread messages addressed to `account` (offline queue), oldest first.
    pub async fn unread_for(&self, account: i64) -> Result<Vec<DmRow>, StoreError> {
        Ok(
            sqlx::query("SELECT * FROM dms WHERE to_account = ? AND read_at IS NULL ORDER BY id")
                .bind(account)
                .fetch_all(self.0)
                .await?
                .iter()
                .map(row_to_dm)
                .collect(),
        )
    }

    /// Conversation summaries for `account`: partner account, last message,
    /// unread count.
    pub async fn threads(&self, account: i64) -> Result<Vec<(i64, DmRow, u64)>, StoreError> {
        // Partner = the other account in each conversation.
        let rows = sqlx::query(
            "WITH conv AS (
                 SELECT *, CASE WHEN from_account = ?1 THEN to_account ELSE from_account END AS partner
                 FROM dms WHERE from_account = ?1 OR to_account = ?1
             )
             SELECT partner,
                    MAX(id) AS last_id,
                    SUM(CASE WHEN to_account = ?1 AND read_at IS NULL THEN 1 ELSE 0 END) AS unread
             FROM conv GROUP BY partner ORDER BY last_id DESC",
        )
        .bind(account)
        .fetch_all(self.0)
        .await?;

        let mut out = Vec::new();
        for r in &rows {
            let partner: i64 = r.get("partner");
            let last_id: i64 = r.get("last_id");
            let unread: i64 = r.get("unread");
            if let Some(last) = sqlx::query("SELECT * FROM dms WHERE id = ?")
                .bind(last_id)
                .fetch_optional(self.0)
                .await?
                .map(|r| row_to_dm(&r))
            {
                out.push((partner, last, unread as u64));
            }
        }
        Ok(out)
    }

    /// Mark messages from `partner` to `account` read up to `up_to_id`.
    /// Returns how many were newly marked.
    pub async fn mark_read(
        &self,
        account: i64,
        partner: i64,
        up_to_id: i64,
    ) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE dms SET read_at = unixepoch()
             WHERE to_account = ? AND from_account = ? AND id <= ? AND read_at IS NULL",
        )
        .bind(account)
        .bind(partner)
        .bind(up_to_id)
        .execute(self.0)
        .await?
        .rows_affected())
    }
}

/// Read the account's receipts preference.
pub async fn dm_receipts_enabled(pool: &SqlitePool, account: i64) -> Result<bool, StoreError> {
    Ok(sqlx::query("SELECT dm_receipts FROM accounts WHERE id = ?")
        .bind(account)
        .fetch_optional(pool)
        .await?
        .map(|r| r.get::<i64, _>("dm_receipts") != 0)
        .unwrap_or(true))
}

/// Read the account's server-theme opt-out (Wave 8). Unknown accounts —
/// guests never have a row — read as `false` (server theme applies).
pub async fn theme_server_disabled(pool: &SqlitePool, account: i64) -> Result<bool, StoreError> {
    Ok(
        sqlx::query("SELECT theme_server_disabled FROM accounts WHERE id = ?")
            .bind(account)
            .fetch_optional(pool)
            .await?
            .map(|r| r.get::<i64, _>("theme_server_disabled") != 0)
            .unwrap_or(false),
    )
}

/// Set the account's server-theme opt-out (Wave 8).
pub async fn set_theme_server_disabled(
    pool: &SqlitePool,
    account: i64,
    disabled: bool,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE accounts SET theme_server_disabled = ? WHERE id = ?")
        .bind(disabled as i64)
        .bind(account)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use crate::repo::AccountsRepo;

    async fn two_accounts() -> (SqlitePool, i64, i64) {
        let pool = open_in_memory().await.unwrap();
        let a = AccountsRepo(&pool)
            .create("alice", None, "alice", 1, None)
            .await
            .unwrap();
        let b = AccountsRepo(&pool)
            .create("bob", None, "bob", 1, None)
            .await
            .unwrap();
        (pool, a.id, b.id)
    }

    #[tokio::test]
    async fn buddies_upsert_and_group_move() {
        let (pool, a, _) = two_accounts().await;
        let repo = BuddiesRepo(&pool);
        repo.add(a, "bob", "Buddies").await.unwrap();
        repo.add(a, "bob", "Co-Workers").await.unwrap(); // move via upsert
        let list = repo.list(a).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].group, "Co-Workers");
        assert!(repo.remove(a, "bob").await.unwrap());
        assert!(!repo.remove(a, "bob").await.unwrap());
    }

    #[tokio::test]
    async fn blocks_lifecycle() {
        let (pool, a, b) = two_accounts().await;
        let repo = BlocksRepo(&pool);
        repo.add(a, b, "bob").await.unwrap();
        // "is bob blocked by alice?" — from bob's viewpoint sending to alice.
        assert!(repo.is_blocked(b, a).await.unwrap());
        assert!(!repo.is_blocked(a, b).await.unwrap());
        assert_eq!(repo.list(a).await.unwrap(), vec!["bob".to_string()]);
        assert!(repo.remove(a, b).await.unwrap());
        assert!(!repo.is_blocked(b, a).await.unwrap());
    }

    #[tokio::test]
    async fn dm_thread_unread_and_receipts() {
        let (pool, a, b) = two_accounts().await;
        let dms = DmsRepo(&pool);
        let id1 = dms
            .insert(a, "alice", b, "bob", "hi bob", None, &[], 1000, false)
            .await
            .unwrap();
        let _id2 = dms
            .insert(b, "bob", a, "alice", "hi alice", None, &[], 2000, false)
            .await
            .unwrap();
        let id3 = dms
            .insert(
                a,
                "alice",
                b,
                "bob",
                "you there?",
                None,
                &["aabb".into()],
                3000,
                false,
            )
            .await
            .unwrap();

        // Thread is ordered oldest→newest and symmetric.
        let t = dms.thread(a, b, 0, 10).await.unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].id, id1);
        assert_eq!(t[2].attachments_hex, vec!["aabb".to_string()]);

        // Bob has 2 unread from alice.
        let unread = dms.unread_for(b).await.unwrap();
        assert_eq!(unread.len(), 2);

        // Threads summary for bob: one conversation, 2 unread, last = id3.
        let threads = dms.threads(b).await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].0, a);
        assert_eq!(threads[0].1.id, id3);
        assert_eq!(threads[0].2, 2);

        // Mark read up to id3; unread drops to zero; idempotent.
        assert_eq!(dms.mark_read(b, a, id3).await.unwrap(), 2);
        assert_eq!(dms.mark_read(b, a, id3).await.unwrap(), 0);
        assert!(dms.unread_for(b).await.unwrap().is_empty());

        // Receipts default on.
        assert!(dm_receipts_enabled(&pool, a).await.unwrap());
    }

    #[tokio::test]
    async fn dm_history_pagination() {
        let (pool, a, b) = two_accounts().await;
        let dms = DmsRepo(&pool);
        for i in 0..10 {
            dms.insert(a, "alice", b, "bob", &format!("m{i}"), None, &[], i, false)
                .await
                .unwrap();
        }
        let newest = dms.thread(a, b, 0, 4).await.unwrap();
        assert_eq!(newest.len(), 4);
        assert_eq!(newest.last().unwrap().text, "m9");
        let older = dms.thread(a, b, newest[0].id, 4).await.unwrap();
        assert_eq!(older.last().unwrap().text, "m5");
    }

    #[tokio::test]
    async fn theme_server_disabled_pref_roundtrips() {
        let (pool, a, b) = two_accounts().await;
        // Defaults off; unknown accounts (guests) read as off too.
        assert!(!theme_server_disabled(&pool, a).await.unwrap());
        assert!(!theme_server_disabled(&pool, -42).await.unwrap());
        set_theme_server_disabled(&pool, a, true).await.unwrap();
        assert!(theme_server_disabled(&pool, a).await.unwrap());
        assert!(
            !theme_server_disabled(&pool, b).await.unwrap(),
            "per-account"
        );
        set_theme_server_disabled(&pool, a, false).await.unwrap();
        assert!(!theme_server_disabled(&pool, a).await.unwrap());
    }
}
