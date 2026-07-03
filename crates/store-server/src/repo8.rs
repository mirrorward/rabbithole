//! Wave 13 repository: E2EE prekey bundles for opt-in 1:1 DM encryption.
//!
//! Holds only **public** key material. Publishing upserts the account's bundle
//! and replaces its one-time-prekey pool; fetching returns the bundle and
//! **atomically consumes** (deletes) one one-time prekey, or `None` once the
//! pool is exhausted. All mutation for a publish runs in a single transaction.

use sqlx::Row;

use crate::{SqlitePool, StoreError};

/// A published prekey bundle (public keys only), with one one-time prekey
/// already consumed for the fetch that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleRow {
    /// X25519 identity public key.
    pub identity_key: Vec<u8>,
    /// Ed25519 verifying key authenticating `signed_prekey_sig`.
    pub signing_key: Vec<u8>,
    /// X25519 signed prekey public key.
    pub signed_prekey: Vec<u8>,
    /// Ed25519 signature over `signed_prekey`.
    pub signed_prekey_sig: Vec<u8>,
    /// The one-time prekey consumed for this fetch, or `None` when exhausted.
    pub one_time_prekey: Option<Vec<u8>>,
}

pub struct KeyBundlesRepo<'a>(pub &'a SqlitePool);

impl KeyBundlesRepo<'_> {
    /// Publish (upsert) `account`'s bundle and replace its one-time-prekey pool.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish(
        &self,
        account: i64,
        identity_key: &[u8],
        signing_key: &[u8],
        signed_prekey: &[u8],
        signed_prekey_sig: &[u8],
        one_time_prekeys: &[[u8; 32]],
        now: i64,
    ) -> Result<(), StoreError> {
        let mut tx = self.0.begin().await?;
        sqlx::query(
            "INSERT INTO e2ee_bundles
                 (account_id, identity_key, signing_key, signed_prekey, signed_prekey_sig, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id) DO UPDATE SET
                 identity_key      = excluded.identity_key,
                 signing_key       = excluded.signing_key,
                 signed_prekey     = excluded.signed_prekey,
                 signed_prekey_sig = excluded.signed_prekey_sig,
                 updated_at        = excluded.updated_at",
        )
        .bind(account)
        .bind(identity_key)
        .bind(signing_key)
        .bind(signed_prekey)
        .bind(signed_prekey_sig)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM e2ee_one_time_prekeys WHERE account_id = ?1")
            .bind(account)
            .execute(&mut *tx)
            .await?;
        for otp in one_time_prekeys {
            sqlx::query(
                "INSERT INTO e2ee_one_time_prekeys (account_id, prekey, created_at)
                 VALUES (?1, ?2, ?3)",
            )
            .bind(account)
            .bind(otp.as_slice())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Fetch `account`'s bundle, atomically consuming one one-time prekey.
    ///
    /// Returns `None` when the account has published no bundle. When the bundle
    /// exists but the one-time-prekey pool is empty, the returned row's
    /// `one_time_prekey` is `None` (the initiator falls back to 3-DH X3DH-lite).
    pub async fn fetch_consume(&self, account: i64) -> Result<Option<BundleRow>, StoreError> {
        let mut tx = self.0.begin().await?;
        let Some(row) = sqlx::query(
            "SELECT identity_key, signing_key, signed_prekey, signed_prekey_sig
             FROM e2ee_bundles WHERE account_id = ?1",
        )
        .bind(account)
        .fetch_optional(&mut *tx)
        .await?
        else {
            tx.rollback().await?;
            return Ok(None);
        };

        // Atomically claim the lowest-id one-time prekey (if any).
        let one_time_prekey: Option<Vec<u8>> = sqlx::query(
            "DELETE FROM e2ee_one_time_prekeys
             WHERE id = (SELECT id FROM e2ee_one_time_prekeys
                         WHERE account_id = ?1 ORDER BY id LIMIT 1)
             RETURNING prekey",
        )
        .bind(account)
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| r.get::<Vec<u8>, _>("prekey"));

        tx.commit().await?;
        Ok(Some(BundleRow {
            identity_key: row.get("identity_key"),
            signing_key: row.get("signing_key"),
            signed_prekey: row.get("signed_prekey"),
            signed_prekey_sig: row.get("signed_prekey_sig"),
            one_time_prekey,
        }))
    }

    /// Count of unconsumed one-time prekeys for `account` (tests/metrics).
    pub async fn one_time_prekey_count(&self, account: i64) -> Result<i64, StoreError> {
        Ok(
            sqlx::query("SELECT COUNT(*) AS n FROM e2ee_one_time_prekeys WHERE account_id = ?1")
                .bind(account)
                .fetch_one(self.0)
                .await?
                .get("n"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use crate::repo::AccountsRepo;

    async fn account() -> (SqlitePool, i64) {
        let pool = open_in_memory().await.unwrap();
        let a = AccountsRepo(&pool)
            .create("alice", None, "alice", 1, None)
            .await
            .unwrap();
        (pool, a.id)
    }

    #[tokio::test]
    async fn publish_then_fetch_consumes_one_otp() {
        let (pool, id) = account().await;
        let repo = KeyBundlesRepo(&pool);
        let otps = [[1u8; 32], [2u8; 32]];
        repo.publish(
            id, &[9u8; 32], &[8u8; 32], &[7u8; 32], &[6u8; 64], &otps, 100,
        )
        .await
        .unwrap();
        assert_eq!(repo.one_time_prekey_count(id).await.unwrap(), 2);

        let b = repo.fetch_consume(id).await.unwrap().unwrap();
        assert_eq!(b.identity_key, vec![9u8; 32]);
        assert_eq!(b.one_time_prekey, Some(vec![1u8; 32]));
        assert_eq!(repo.one_time_prekey_count(id).await.unwrap(), 1);

        let b2 = repo.fetch_consume(id).await.unwrap().unwrap();
        assert_eq!(b2.one_time_prekey, Some(vec![2u8; 32]));
        // Pool exhausted: bundle still fetchable, but no OTP.
        let b3 = repo.fetch_consume(id).await.unwrap().unwrap();
        assert_eq!(b3.one_time_prekey, None);
    }

    #[tokio::test]
    async fn republish_replaces_pool() {
        let (pool, id) = account().await;
        let repo = KeyBundlesRepo(&pool);
        repo.publish(
            id,
            &[1u8; 32],
            &[1u8; 32],
            &[1u8; 32],
            &[1u8; 64],
            &[[1u8; 32]],
            1,
        )
        .await
        .unwrap();
        repo.publish(
            id,
            &[2u8; 32],
            &[2u8; 32],
            &[2u8; 32],
            &[2u8; 64],
            &[[3u8; 32]],
            2,
        )
        .await
        .unwrap();
        assert_eq!(repo.one_time_prekey_count(id).await.unwrap(), 1);
        let b = repo.fetch_consume(id).await.unwrap().unwrap();
        assert_eq!(b.identity_key, vec![2u8; 32]);
        assert_eq!(b.one_time_prekey, Some(vec![3u8; 32]));
    }

    #[tokio::test]
    async fn fetch_unknown_account_is_none() {
        let (pool, _) = account().await;
        assert!(KeyBundlesRepo(&pool)
            .fetch_consume(999)
            .await
            .unwrap()
            .is_none());
    }
}
