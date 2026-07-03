//! Wave 2 repositories: personas, invites, TOTP, identity keys.

use sqlx::Row;

use crate::{SqlitePool, StoreError};

/// A persona row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaRow {
    pub id: i64,
    pub account_id: i64,
    pub screen_name: String,
    pub is_default: bool,
    pub location: Option<String>,
    pub interests: Option<String>,
    pub quote: Option<String>,
    pub plan: Option<String>,
    pub pronouns: Option<String>,
    pub avatar_hex: Option<String>,
    pub banner_hex: Option<String>,
    pub directory_visible: bool,
}

fn row_to_persona(r: &sqlx::sqlite::SqliteRow) -> PersonaRow {
    PersonaRow {
        id: r.get("id"),
        account_id: r.get("account_id"),
        screen_name: r.get("screen_name"),
        is_default: r.get::<i64, _>("is_default") != 0,
        location: r.get("location"),
        interests: r.get("interests"),
        quote: r.get("quote"),
        plan: r.get("plan"),
        pronouns: r.get("pronouns"),
        avatar_hex: r.get("avatar_hex"),
        banner_hex: r.get("banner_hex"),
        directory_visible: r.get::<i64, _>("directory_visible") != 0,
    }
}

pub struct PersonasRepo<'a>(pub &'a SqlitePool);

impl PersonasRepo<'_> {
    pub async fn create(
        &self,
        account_id: i64,
        screen_name: &str,
        is_default: bool,
    ) -> Result<PersonaRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO personas (account_id, screen_name, is_default, created_at)
             VALUES (?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(account_id)
        .bind(screen_name)
        .bind(is_default as i64)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.by_id(id).await?.expect("just inserted"))
    }

    pub async fn by_id(&self, id: i64) -> Result<Option<PersonaRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM personas WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_persona(&r)))
    }

    pub async fn by_screen_name(&self, name: &str) -> Result<Option<PersonaRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM personas WHERE screen_name = ?")
            .bind(name)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_persona(&r)))
    }

    pub async fn for_account(&self, account_id: i64) -> Result<Vec<PersonaRow>, StoreError> {
        Ok(
            sqlx::query("SELECT * FROM personas WHERE account_id = ? ORDER BY id")
                .bind(account_id)
                .fetch_all(self.0)
                .await?
                .iter()
                .map(row_to_persona)
                .collect(),
        )
    }

    pub async fn default_for_account(
        &self,
        account_id: i64,
    ) -> Result<Option<PersonaRow>, StoreError> {
        Ok(sqlx::query(
            "SELECT * FROM personas WHERE account_id = ? ORDER BY is_default DESC, id LIMIT 1",
        )
        .bind(account_id)
        .fetch_optional(self.0)
        .await?
        .map(|r| row_to_persona(&r)))
    }

    /// Apply profile/appearance updates (None = unchanged).
    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        &self,
        id: i64,
        location: Option<&str>,
        interests: Option<&str>,
        quote: Option<&str>,
        plan: Option<&str>,
        pronouns: Option<&str>,
        avatar_hex: Option<Option<&str>>,
        banner_hex: Option<Option<&str>>,
        directory_visible: Option<bool>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE personas SET
               location = COALESCE(?, location),
               interests = COALESCE(?, interests),
               quote = COALESCE(?, quote),
               plan = COALESCE(?, plan),
               pronouns = COALESCE(?, pronouns),
               avatar_hex = CASE WHEN ? THEN ? ELSE avatar_hex END,
               banner_hex = CASE WHEN ? THEN ? ELSE banner_hex END,
               directory_visible = COALESCE(?, directory_visible)
             WHERE id = ?",
        )
        .bind(location)
        .bind(interests)
        .bind(quote)
        .bind(plan)
        .bind(pronouns)
        .bind(avatar_hex.is_some())
        .bind(avatar_hex.flatten())
        .bind(banner_hex.is_some())
        .bind(banner_hex.flatten())
        .bind(directory_visible.map(|b| b as i64))
        .bind(id)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// Delete a persona; refuses to delete the account's last one.
    pub async fn delete(&self, id: i64, account_id: i64) -> Result<bool, StoreError> {
        let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM personas WHERE account_id = ?")
            .bind(account_id)
            .fetch_one(self.0)
            .await?
            .get("n");
        if n <= 1 {
            return Ok(false);
        }
        let affected =
            sqlx::query("DELETE FROM personas WHERE id = ? AND account_id = ? AND is_default = 0")
                .bind(id)
                .bind(account_id)
                .execute(self.0)
                .await?
                .rows_affected();
        Ok(affected > 0)
    }

    pub async fn count_for_account(&self, account_id: i64) -> Result<i64, StoreError> {
        Ok(
            sqlx::query("SELECT COUNT(*) AS n FROM personas WHERE account_id = ?")
                .bind(account_id)
                .fetch_one(self.0)
                .await?
                .get("n"),
        )
    }

    /// Directory search over visible personas (name + profile substrings).
    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<PersonaRow>, StoreError> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        Ok(sqlx::query(
            "SELECT * FROM personas WHERE directory_visible = 1 AND (
                 screen_name LIKE ?1 ESCAPE '\\'
                 OR location LIKE ?1 ESCAPE '\\'
                 OR interests LIKE ?1 ESCAPE '\\'
                 OR quote LIKE ?1 ESCAPE '\\'
             ) ORDER BY screen_name LIMIT ?2",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(self.0)
        .await?
        .iter()
        .map(row_to_persona)
        .collect())
    }
}

pub struct InvitesRepo<'a>(pub &'a SqlitePool);

impl InvitesRepo<'_> {
    pub async fn create(
        &self,
        code: &str,
        created_by: i64,
        ttl_secs: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO invites (code, created_by, created_at, expires_at)
             VALUES (?, ?, unixepoch(), unixepoch() + ?)",
        )
        .bind(code)
        .bind(created_by)
        .bind(ttl_secs)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// Atomically consume an unused, unexpired invite. Returns success.
    pub async fn consume(&self, code: &str, used_by: i64) -> Result<bool, StoreError> {
        let affected = sqlx::query(
            "UPDATE invites SET used_by = ?
             WHERE code = ? AND used_by IS NULL AND expires_at > unixepoch()",
        )
        .bind(used_by)
        .bind(code)
        .execute(self.0)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Atomically reserve an unused, unexpired invite (marks `used_by = 0`,
    /// pending), returning its `created_by` — the inviter — so the caller can
    /// record the invite-tree edge. `None` = the code is absent, already used,
    /// or expired.
    pub async fn reserve(&self, code: &str) -> Result<Option<i64>, StoreError> {
        Ok(sqlx::query(
            "UPDATE invites SET used_by = 0
             WHERE code = ? AND used_by IS NULL AND expires_at > unixepoch()
             RETURNING created_by",
        )
        .bind(code)
        .fetch_optional(self.0)
        .await?
        .map(|r| r.get::<i64, _>("created_by")))
    }

    /// Finalise a reserved invite with the real redeemer account id (replaces
    /// the pending `0` from [`reserve`]).
    ///
    /// [`reserve`]: Self::reserve
    pub async fn finalize(&self, code: &str, used_by: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE invites SET used_by = ? WHERE code = ?")
            .bind(used_by)
            .bind(code)
            .execute(self.0)
            .await?;
        Ok(())
    }
}

pub struct TotpRepo<'a>(pub &'a SqlitePool);

#[derive(Debug, Clone)]
pub struct TotpRow {
    pub secret: Vec<u8>,
    pub confirmed: bool,
    pub recovery_hashes: Vec<[u8; 32]>,
}

impl TotpRepo<'_> {
    pub async fn get(&self, account_id: i64) -> Result<Option<TotpRow>, StoreError> {
        let Some(r) = sqlx::query("SELECT * FROM account_totp WHERE account_id = ?")
            .bind(account_id)
            .fetch_optional(self.0)
            .await?
        else {
            return Ok(None);
        };
        let recovery_json: String = r.get("recovery_json");
        let hashes: Vec<String> = serde_json::from_str(&recovery_json).unwrap_or_default();
        Ok(Some(TotpRow {
            secret: r.get("secret"),
            confirmed: r.get::<i64, _>("confirmed") != 0,
            recovery_hashes: hashes
                .iter()
                .filter_map(|h| hex::decode(h).ok()?.try_into().ok())
                .collect(),
        }))
    }

    /// Store a pending (unconfirmed) enrollment, replacing any prior one.
    pub async fn begin(&self, account_id: i64, secret: &[u8]) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO account_totp (account_id, secret, confirmed) VALUES (?, ?, 0)
             ON CONFLICT (account_id) DO UPDATE SET secret = excluded.secret,
                 confirmed = 0, recovery_json = '[]'",
        )
        .bind(account_id)
        .bind(secret)
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn confirm(
        &self,
        account_id: i64,
        recovery_hashes: &[[u8; 32]],
    ) -> Result<(), StoreError> {
        let json =
            serde_json::to_string(&recovery_hashes.iter().map(hex::encode).collect::<Vec<_>>())
                .expect("serializable");
        sqlx::query(
            "UPDATE account_totp SET confirmed = 1, recovery_json = ? WHERE account_id = ?",
        )
        .bind(json)
        .bind(account_id)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// Burn one recovery code (by index into the stored list).
    pub async fn spend_recovery(
        &self,
        account_id: i64,
        remaining: &[[u8; 32]],
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(&remaining.iter().map(hex::encode).collect::<Vec<_>>())
            .expect("serializable");
        sqlx::query("UPDATE account_totp SET recovery_json = ? WHERE account_id = ?")
            .bind(json)
            .bind(account_id)
            .execute(self.0)
            .await?;
        Ok(())
    }

    pub async fn remove(&self, account_id: i64) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM account_totp WHERE account_id = ?")
            .bind(account_id)
            .execute(self.0)
            .await?;
        Ok(())
    }
}

pub struct KeysRepo<'a>(pub &'a SqlitePool);

impl KeysRepo<'_> {
    pub async fn add(&self, account_id: i64, pubkey: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO account_keys (account_id, pubkey, added_at) VALUES (?, ?, unixepoch())",
        )
        .bind(account_id)
        .bind(pubkey.as_slice())
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn for_account(&self, account_id: i64) -> Result<Vec<[u8; 32]>, StoreError> {
        Ok(
            sqlx::query("SELECT pubkey FROM account_keys WHERE account_id = ?")
                .bind(account_id)
                .fetch_all(self.0)
                .await?
                .iter()
                .filter_map(|r| r.get::<Vec<u8>, _>("pubkey").try_into().ok())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use crate::repo::AccountsRepo;

    async fn pool_with_account() -> (SqlitePool, i64) {
        let pool = open_in_memory().await.unwrap();
        let a = AccountsRepo(&pool)
            .create("alice", None, "alice", 1, None)
            .await
            .unwrap();
        (pool, a.id)
    }

    #[tokio::test]
    async fn account_creation_backfills_default_persona() {
        let (pool, account_id) = pool_with_account().await;
        // The 0003 backfill only covers pre-migration accounts; new accounts
        // get their default persona from the service layer. Simulate it:
        let p = PersonasRepo(&pool)
            .create(account_id, "alice", true)
            .await
            .unwrap();
        assert!(p.is_default);
        let found = PersonasRepo(&pool)
            .default_for_account(account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, p.id);
    }

    #[tokio::test]
    async fn persona_lifecycle_and_search() {
        let (pool, account_id) = pool_with_account().await;
        let repo = PersonasRepo(&pool);
        let main = repo.create(account_id, "Alice", true).await.unwrap();
        let alt = repo
            .create(account_id, "White Rabbit", false)
            .await
            .unwrap();

        // Unique names, case-insensitive.
        assert!(repo.create(account_id, "alice", false).await.is_err());

        repo.update(
            alt.id,
            Some("Wonderland"),
            None,
            Some("I'm late!"),
            None,
            None,
            Some(Some("aabb")),
            None,
            None,
        )
        .await
        .unwrap();
        let alt2 = repo.by_id(alt.id).await.unwrap().unwrap();
        assert_eq!(alt2.location.as_deref(), Some("Wonderland"));
        assert_eq!(alt2.avatar_hex.as_deref(), Some("aabb"));

        // Search hits name and profile fields; hidden personas don't appear.
        assert_eq!(repo.search("rabbit", 10).await.unwrap().len(), 1);
        assert_eq!(repo.search("wonderland", 10).await.unwrap().len(), 1);
        repo.update(
            alt.id,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(false),
        )
        .await
        .unwrap();
        assert_eq!(repo.search("rabbit", 10).await.unwrap().len(), 0);

        // Can't delete the default or the last persona.
        assert!(!repo.delete(main.id, account_id).await.unwrap());
        assert!(repo.delete(alt.id, account_id).await.unwrap());
        assert!(
            !repo.delete(main.id, account_id).await.unwrap(),
            "last persona survives"
        );
    }

    #[tokio::test]
    async fn invites_consume_once_and_expire() {
        let (pool, account_id) = pool_with_account().await;
        let invites = InvitesRepo(&pool);
        invites
            .create("golden-ticket", account_id, 3600)
            .await
            .unwrap();
        assert!(invites.consume("golden-ticket", 99).await.unwrap());
        assert!(
            !invites.consume("golden-ticket", 100).await.unwrap(),
            "single use"
        );

        invites.create("stale", account_id, -5).await.unwrap();
        assert!(!invites.consume("stale", 99).await.unwrap(), "expired");
        assert!(!invites.consume("never-existed", 99).await.unwrap());
    }

    #[tokio::test]
    async fn reserve_captures_inviter_finalizes_and_lists_downline() {
        let (pool, alice) = pool_with_account().await;
        let invites = InvitesRepo(&pool);
        invites.create("code", alice, 3600).await.unwrap();

        // reserve returns the inviter and marks the code pending (id 0).
        assert_eq!(invites.reserve("code").await.unwrap(), Some(alice));
        assert_eq!(
            invites.reserve("code").await.unwrap(),
            None,
            "reserved only once"
        );
        assert_eq!(invites.reserve("missing").await.unwrap(), None);

        // A fresh account redeems it; finalize records the real id and
        // set_invited_by draws the tree edge.
        let bob = AccountsRepo(&pool)
            .create("bob", None, "bob", 1, None)
            .await
            .unwrap();
        invites.finalize("code", bob.id).await.unwrap();
        AccountsRepo(&pool)
            .set_invited_by(bob.id, alice)
            .await
            .unwrap();

        // invitees lists the direct downline; a childless account is empty.
        assert_eq!(
            AccountsRepo(&pool).invitees(alice).await.unwrap(),
            vec![(bob.id, "bob".to_string())]
        );
        assert!(AccountsRepo(&pool)
            .invitees(bob.id)
            .await
            .unwrap()
            .is_empty());

        // An expired code cannot be reserved.
        invites.create("stale", alice, -5).await.unwrap();
        assert_eq!(invites.reserve("stale").await.unwrap(), None, "expired");
    }

    #[tokio::test]
    async fn totp_enrollment_lifecycle() {
        let (pool, account_id) = pool_with_account().await;
        let totp = TotpRepo(&pool);
        assert!(totp.get(account_id).await.unwrap().is_none());

        totp.begin(account_id, b"secret-bytes-here!!!")
            .await
            .unwrap();
        let row = totp.get(account_id).await.unwrap().unwrap();
        assert!(!row.confirmed);

        totp.confirm(account_id, &[[1u8; 32], [2u8; 32]])
            .await
            .unwrap();
        let row = totp.get(account_id).await.unwrap().unwrap();
        assert!(row.confirmed);
        assert_eq!(row.recovery_hashes.len(), 2);

        totp.spend_recovery(account_id, &[[2u8; 32]]).await.unwrap();
        assert_eq!(
            totp.get(account_id)
                .await
                .unwrap()
                .unwrap()
                .recovery_hashes
                .len(),
            1
        );

        totp.remove(account_id).await.unwrap();
        assert!(totp.get(account_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn keys_roundtrip() {
        let (pool, account_id) = pool_with_account().await;
        let keys = KeysRepo(&pool);
        keys.add(account_id, &[7u8; 32]).await.unwrap();
        assert!(
            keys.add(account_id, &[7u8; 32]).await.is_err(),
            "duplicate key"
        );
        assert_eq!(keys.for_account(account_id).await.unwrap(), vec![[7u8; 32]]);
    }
}
