//! Wave 3.2 repository: the Wishing Well (request system).

use sqlx::Row;

use crate::{SqlitePool, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WishRow {
    pub id: i64,
    pub kind: u8,
    pub title: String,
    pub details: String,
    pub requester: String,
    pub requester_id: i64,
    pub status: u8,
    pub claimed_by: Option<String>,
    pub fulfillment: Option<String>,
    pub votes: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

fn row_to_wish(r: &sqlx::sqlite::SqliteRow) -> WishRow {
    WishRow {
        id: r.get("id"),
        kind: r.get::<i64, _>("kind") as u8,
        title: r.get("title"),
        details: r.get("details"),
        requester: r.get("requester"),
        requester_id: r.get("requester_id"),
        status: r.get::<i64, _>("status") as u8,
        claimed_by: r.get("claimed_by"),
        fulfillment: r.get("fulfillment"),
        votes: r.try_get("votes").unwrap_or(0),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub struct WishesRepo<'a>(pub &'a SqlitePool);

impl WishesRepo<'_> {
    pub async fn create(
        &self,
        kind: u8,
        title: &str,
        details: &str,
        requester: &str,
        requester_id: i64,
    ) -> Result<WishRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO wishes (kind, title, details, requester, requester_id,
                                 created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, unixepoch(), unixepoch()) RETURNING id",
        )
        .bind(kind as i64)
        .bind(title)
        .bind(details)
        .bind(requester)
        .bind(requester_id)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.by_id(id).await?.expect("just inserted"))
    }

    pub async fn by_id(&self, id: i64) -> Result<Option<WishRow>, StoreError> {
        Ok(sqlx::query(
            "SELECT w.*, (SELECT COUNT(*) FROM wish_votes v WHERE v.wish_id = w.id) AS votes
             FROM wishes w WHERE w.id = ?",
        )
        .bind(id)
        .fetch_optional(self.0)
        .await?
        .map(|r| row_to_wish(&r)))
    }

    /// List wishes, optionally filtered by status, most-voted then newest.
    pub async fn list(&self, status: Option<u8>, limit: i64) -> Result<Vec<WishRow>, StoreError> {
        let rows = match status {
            Some(s) => sqlx::query(
                "SELECT w.*, (SELECT COUNT(*) FROM wish_votes v WHERE v.wish_id = w.id) AS votes
                     FROM wishes w WHERE w.status = ?
                     ORDER BY votes DESC, w.updated_at DESC LIMIT ?",
            )
            .bind(s as i64)
            .bind(limit)
            .fetch_all(self.0)
            .await?,
            None => sqlx::query(
                "SELECT w.*, (SELECT COUNT(*) FROM wish_votes v WHERE v.wish_id = w.id) AS votes
                     FROM wishes w ORDER BY votes DESC, w.updated_at DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(self.0)
            .await?,
        };
        Ok(rows.iter().map(row_to_wish).collect())
    }

    /// Toggle a vote; returns the new vote count.
    pub async fn toggle_vote(&self, wish_id: i64, account_id: i64) -> Result<i64, StoreError> {
        let existed = sqlx::query("DELETE FROM wish_votes WHERE wish_id = ? AND account_id = ?")
            .bind(wish_id)
            .bind(account_id)
            .execute(self.0)
            .await?
            .rows_affected()
            > 0;
        if !existed {
            sqlx::query("INSERT INTO wish_votes (wish_id, account_id) VALUES (?, ?)")
                .bind(wish_id)
                .bind(account_id)
                .execute(self.0)
                .await?;
        }
        Ok(
            sqlx::query("SELECT COUNT(*) AS n FROM wish_votes WHERE wish_id = ?")
                .bind(wish_id)
                .fetch_one(self.0)
                .await?
                .get("n"),
        )
    }

    pub async fn set_status(
        &self,
        wish_id: i64,
        status: u8,
        claimed_by: Option<&str>,
        fulfillment: Option<&str>,
    ) -> Result<bool, StoreError> {
        Ok(sqlx::query(
            "UPDATE wishes SET status = ?,
                 claimed_by = COALESCE(?, claimed_by),
                 fulfillment = COALESCE(?, fulfillment),
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status as i64)
        .bind(claimed_by)
        .bind(fulfillment)
        .bind(wish_id)
        .execute(self.0)
        .await?
        .rows_affected()
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn wish_lifecycle_votes_and_status() {
        let pool = open_in_memory().await.unwrap();
        let repo = WishesRepo(&pool);
        let w = repo
            .create(0, "Want the 1997 shareware CD", "please", "alice@home", 1)
            .await
            .unwrap();
        assert_eq!(w.status, 0);
        assert_eq!(w.votes, 0);

        // Voting toggles.
        assert_eq!(repo.toggle_vote(w.id, 2).await.unwrap(), 1);
        assert_eq!(repo.toggle_vote(w.id, 3).await.unwrap(), 2);
        assert_eq!(repo.toggle_vote(w.id, 2).await.unwrap(), 1); // un-vote

        // Claim → fulfill.
        assert!(repo
            .set_status(w.id, 1, Some("bob@home"), None)
            .await
            .unwrap());
        let claimed = repo.by_id(w.id).await.unwrap().unwrap();
        assert_eq!(claimed.status, 1);
        assert_eq!(claimed.claimed_by.as_deref(), Some("bob@home"));

        repo.set_status(w.id, 2, None, Some("rabbit://host/abc"))
            .await
            .unwrap();
        let done = repo.by_id(w.id).await.unwrap().unwrap();
        assert_eq!(done.status, 2);
        assert_eq!(done.fulfillment.as_deref(), Some("rabbit://host/abc"));
        assert_eq!(
            done.claimed_by.as_deref(),
            Some("bob@home"),
            "claim preserved"
        );
    }

    #[tokio::test]
    async fn list_orders_by_votes_then_filters_status() {
        let pool = open_in_memory().await.unwrap();
        let repo = WishesRepo(&pool);
        let a = repo.create(2, "feature A", "", "u@h", 1).await.unwrap();
        let b = repo.create(2, "feature B", "", "u@h", 1).await.unwrap();
        repo.toggle_vote(b.id, 10).await.unwrap();
        repo.toggle_vote(b.id, 11).await.unwrap();
        repo.toggle_vote(a.id, 10).await.unwrap();

        let all = repo.list(None, 10).await.unwrap();
        assert_eq!(all[0].id, b.id, "most-voted first");

        repo.set_status(a.id, 3, None, None).await.unwrap(); // declined
        let open = repo.list(Some(0), 10).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, b.id);
    }
}
