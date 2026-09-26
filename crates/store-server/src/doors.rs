//! Durable, prepaid per-account/per-door UTC-day budgets.
//!
//! Reserve before running a child; settle once it stops. A crash or failed
//! settlement retains the full reservation, conservatively charging that day.
//! Admission and refunds serialize in SQLite, including across separate pools.

use crate::{SqlitePool, StoreError};

#[derive(Debug)]
pub struct DoorReservation {
    pub id: i64,
    pub granted_ms: u64,
}

pub struct DoorUsageRepo<'a>(pub &'a SqlitePool);

impl DoorUsageRepo<'_> {
    /// Return a reservation up to `requested_ms`, after subtracting both
    /// completed usage and active reservations. Synthetic guest IDs cannot
    /// obtain a durable budget. The account foreign key also rejects deleted
    /// or otherwise nonexistent accounts.
    pub async fn reserve(
        &self,
        account_id: i64,
        door_id: &str,
        utc_day: i64,
        daily_limit_ms: u64,
        requested_ms: u64,
    ) -> Result<Option<DoorReservation>, StoreError> {
        if account_id <= 0 || requested_ms == 0 || daily_limit_ms == 0 {
            return Ok(None);
        }
        let daily_limit_ms = daily_limit_ms.min(i64::MAX as u64) as i64;
        let requested_ms = requested_ms.min(i64::MAX as u64) as i64;
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO door_daily_usage (account_id, door_id, utc_day, charged_ms)
             VALUES (?, ?, ?, 0) ON CONFLICT DO NOTHING",
        )
        .bind(account_id)
        .bind(door_id)
        .bind(utc_day)
        .execute(&mut *tx)
        .await?;
        let charged: i64 = sqlx::query_scalar(
            "SELECT charged_ms FROM door_daily_usage
             WHERE account_id = ? AND door_id = ? AND utc_day = ?",
        )
        .bind(account_id)
        .bind(door_id)
        .bind(utc_day)
        .fetch_one(&mut *tx)
        .await?;
        let granted = requested_ms.min(daily_limit_ms.saturating_sub(charged).max(0));
        if granted == 0 {
            tx.commit().await?;
            return Ok(None);
        }
        sqlx::query(
            "UPDATE door_daily_usage SET charged_ms = charged_ms + ?
             WHERE account_id = ? AND door_id = ? AND utc_day = ?",
        )
        .bind(granted)
        .bind(account_id)
        .bind(door_id)
        .bind(utc_day)
        .execute(&mut *tx)
        .await?;
        let id = sqlx::query(
            "INSERT INTO door_daily_reservations (account_id, door_id, utc_day, granted_ms)
             VALUES (?, ?, ?, ?)",
        )
        .bind(account_id)
        .bind(door_id)
        .bind(utc_day)
        .bind(granted)
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        tx.commit().await?;
        Ok(Some(DoorReservation {
            id,
            granted_ms: granted as u64,
        }))
    }

    /// Refund unused prepaid time exactly once. A repeated settlement cannot
    /// refund another run's usage; account deletion is an ordinary no-op.
    pub async fn settle(&self, id: i64, elapsed_ms: u64) -> Result<(), StoreError> {
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(i64, String, i64, i64)> = sqlx::query_as(
            "DELETE FROM door_daily_reservations WHERE id = ?
             RETURNING account_id, door_id, utc_day, granted_ms",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((account, door, day, granted)) = row {
            let refund = granted - elapsed_ms.min(granted as u64) as i64;
            sqlx::query(
                "UPDATE door_daily_usage SET charged_ms = charged_ms - ?
                 WHERE account_id = ? AND door_id = ? AND utc_day = ?",
            )
            .bind(refund)
            .bind(account)
            .bind(door)
            .bind(day)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Charged usage includes active (and abandoned) reservations.
    pub async fn charged_ms(
        &self,
        account_id: i64,
        door_id: &str,
        utc_day: i64,
    ) -> Result<u64, StoreError> {
        let charged: Option<i64> = sqlx::query_scalar(
            "SELECT charged_ms FROM door_daily_usage
             WHERE account_id = ? AND door_id = ? AND utc_day = ?",
        )
        .bind(account_id)
        .bind(door_id)
        .bind(utc_day)
        .fetch_optional(self.0)
        .await?;
        Ok(charged.unwrap_or(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::AccountsRepo;

    #[tokio::test]
    async fn repeated_usage_refunds_once_and_is_scoped_to_account_door_and_day() {
        let pool = crate::open_in_memory().await.unwrap();
        let accounts = AccountsRepo(&pool);
        let alice = accounts
            .create("alice", None, "Alice", 1, None)
            .await
            .unwrap()
            .id;
        let bob = accounts
            .create("bob", None, "Bob", 1, None)
            .await
            .unwrap()
            .id;
        let usage = DoorUsageRepo(&pool);
        let first = usage
            .reserve(alice, "lord", 10, 60_000, 40_000)
            .await
            .unwrap()
            .unwrap();
        usage.settle(first.id, 25_001).await.unwrap();
        usage.settle(first.id, 0).await.unwrap();
        assert_eq!(usage.charged_ms(alice, "lord", 10).await.unwrap(), 25_001);
        assert!(
            usage
                .reserve(alice, "lord", 10, 20_000, 10_000)
                .await
                .unwrap()
                .is_none(),
            "lowering the configured limit cannot refund spent time"
        );
        let remaining = usage
            .reserve(alice, "lord", 10, 60_000, 60_000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remaining.granted_ms, 34_999);
        assert!(usage
            .reserve(alice, "lord", 10, 60_000, 1)
            .await
            .unwrap()
            .is_none());
        for (account, door, day) in [(bob, "lord", 10), (alice, "other", 10), (alice, "lord", 11)] {
            assert_eq!(
                usage
                    .reserve(account, door, day, 60_000, 60_000)
                    .await
                    .unwrap()
                    .unwrap()
                    .granted_ms,
                60_000
            );
        }
        // A late old-day refund never credits the new day.
        usage.settle(remaining.id, 10_000).await.unwrap();
        assert_eq!(usage.charged_ms(alice, "lord", 10).await.unwrap(), 35_001);
        assert_eq!(usage.charged_ms(alice, "lord", 11).await.unwrap(), 60_000);
        for synthetic in [0, -1] {
            assert!(usage
                .reserve(synthetic, "lord", 10, 60_000, 1)
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn concurrent_pools_share_prepaid_allowance_and_restart_does_not_refund_crashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doors.db");
        let a = crate::open(&path).await.unwrap();
        let b = crate::open(&path).await.unwrap();
        let account = AccountsRepo(&a)
            .create("alice", None, "Alice", 1, None)
            .await
            .unwrap()
            .id;
        let one = DoorUsageRepo(&a);
        let two = DoorUsageRepo(&b);
        let (first, second) = tokio::join!(
            one.reserve(account, "lord", 10, 60_000, 40_000),
            two.reserve(account, "lord", 10, 60_000, 40_000),
        );
        let first = first.unwrap().unwrap();
        let second = second.unwrap().unwrap();
        assert_eq!(first.granted_ms + second.granted_ms, 60_000);
        assert!(one
            .reserve(account, "lord", 10, 60_000, 1)
            .await
            .unwrap()
            .is_none());
        // A failed spawn refunds its entire reservation. The other run is
        // deliberately abandoned to model abrupt server death.
        one.settle(first.id, 0).await.unwrap();
        a.close().await;
        b.close().await;
        let reopened = crate::open(&path).await.unwrap();
        let usage = DoorUsageRepo(&reopened);
        assert_eq!(
            usage.charged_ms(account, "lord", 10).await.unwrap(),
            second.granted_ms
        );
        assert_eq!(
            usage
                .reserve(account, "lord", 10, 60_000, 60_000)
                .await
                .unwrap()
                .unwrap()
                .granted_ms,
            first.granted_ms
        );
        assert_eq!(
            usage
                .reserve(account, "lord", 11, 60_000, 60_000)
                .await
                .unwrap()
                .unwrap()
                .granted_ms,
            60_000
        );
        reopened.close().await;
    }
}
