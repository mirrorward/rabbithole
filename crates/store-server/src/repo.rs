//! Repositories over the Wave 1 schema.
//!
//! Plain-`sqlx::query` (runtime-checked) rather than the compile-time
//! macros so builds don't need a DATABASE_URL. Capability masks are u64
//! bit-cast into SQLite's i64.

use sqlx::Row;

use crate::{SqlitePool, StoreError};

/// An account row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub login: String,
    pub phc: Option<String>,
    pub screen_name: String,
    pub role: u8,
    pub class_id: Option<i64>,
    pub grant_mask: u64,
    pub revoke_mask: u64,
    pub disabled: bool,
    pub authz_epoch: i64,
}

fn row_to_account(row: &sqlx::sqlite::SqliteRow) -> Account {
    Account {
        id: row.get("id"),
        login: row.get("login"),
        phc: row.get("phc"),
        screen_name: row.get("screen_name"),
        role: row.get::<i64, _>("role") as u8,
        class_id: row.get("class_id"),
        grant_mask: row.get::<i64, _>("grant_mask") as u64,
        revoke_mask: row.get::<i64, _>("revoke_mask") as u64,
        disabled: row.get::<i64, _>("disabled") != 0,
        authz_epoch: row.get("authz_epoch"),
    }
}

pub struct AccountsRepo<'a>(pub &'a SqlitePool);

impl AccountsRepo<'_> {
    pub async fn create(
        &self,
        login: &str,
        phc: Option<&str>,
        screen_name: &str,
        role: u8,
        class_id: Option<i64>,
    ) -> Result<Account, StoreError> {
        let id = sqlx::query(
            "INSERT INTO accounts (login, phc, screen_name, role, class_id, created_at)
             VALUES (?, ?, ?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(login)
        .bind(phc)
        .bind(screen_name)
        .bind(role as i64)
        .bind(class_id)
        .fetch_one(self.0)
        .await?
        .get::<i64, _>("id");
        Ok(self.by_id(id).await?.expect("just inserted"))
    }

    pub async fn by_login(&self, login: &str) -> Result<Option<Account>, StoreError> {
        Ok(sqlx::query("SELECT * FROM accounts WHERE login = ?")
            .bind(login)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_account(&r)))
    }

    pub async fn by_id(&self, id: i64) -> Result<Option<Account>, StoreError> {
        Ok(sqlx::query("SELECT * FROM accounts WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_account(&r)))
    }

    pub async fn update_phc(&self, id: i64, phc: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET phc = ? WHERE id = ?")
            .bind(phc)
            .bind(id)
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// Replace a password hash and revoke every bearer for the account in
    /// the same SQLite write transaction.
    pub async fn update_phc_and_revoke_sessions(
        &self,
        id: i64,
        phc: &str,
    ) -> Result<bool, StoreError> {
        let mut tx = self.0.begin().await?;
        let affected =
            sqlx::query("UPDATE accounts SET phc = ?, authz_epoch = authz_epoch + 1 WHERE id = ?")
                .bind(phc)
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if affected == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM sessions WHERE account_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn count(&self) -> Result<i64, StoreError> {
        Ok(sqlx::query("SELECT COUNT(*) AS n FROM accounts")
            .fetch_one(self.0)
            .await?
            .get("n"))
    }

    /// Record which account invited `id` (the invite-tree edge).
    pub async fn set_invited_by(&self, id: i64, inviter: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET invited_by = ? WHERE id = ?")
            .bind(inviter)
            .bind(id)
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// The `(id, login)` of every account `inviter` directly invited, oldest
    /// first — one level of the invite tree.
    pub async fn invitees(&self, inviter: i64) -> Result<Vec<(i64, String)>, StoreError> {
        Ok(
            sqlx::query("SELECT id, login FROM accounts WHERE invited_by = ? ORDER BY id")
                .bind(inviter)
                .fetch_all(self.0)
                .await?
                .iter()
                .map(|r| (r.get::<i64, _>("id"), r.get::<String, _>("login")))
                .collect(),
        )
    }

    pub async fn list(&self, offset: i64, limit: i64) -> Result<Vec<Account>, StoreError> {
        Ok(
            sqlx::query("SELECT * FROM accounts ORDER BY id LIMIT ? OFFSET ?")
                .bind(limit)
                .bind(offset)
                .fetch_all(self.0)
                .await?
                .iter()
                .map(row_to_account)
                .collect(),
        )
    }

    /// Apply admin edits; `Some` fields are changed. Returns whether the
    /// login existed.
    pub async fn admin_set(
        &self,
        login: &str,
        role: Option<u8>,
        class_id: Option<Option<i64>>,
        disabled: Option<bool>,
    ) -> Result<bool, StoreError> {
        let affected = sqlx::query(
            "UPDATE accounts SET
               role = COALESCE(?, role),
               class_id = CASE WHEN ? THEN ? ELSE class_id END,
               disabled = COALESCE(?, disabled)
             WHERE login = ?",
        )
        .bind(role.map(|r| r as i64))
        .bind(class_id.is_some())
        .bind(class_id.flatten())
        .bind(disabled.map(|d| d as i64))
        .bind(login)
        .execute(self.0)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Apply account authorization edits and revoke every bearer atomically.
    /// Returns the changed account id, or `None` when `login` did not exist.
    pub async fn admin_update_and_revoke_sessions(
        &self,
        login: &str,
        phc: Option<&str>,
        role: Option<u8>,
        class_id: Option<Option<i64>>,
        disabled: Option<bool>,
    ) -> Result<Option<i64>, StoreError> {
        let mut tx = self.0.begin().await?;
        let row = sqlx::query(
            "UPDATE accounts SET
               phc = COALESCE(?, phc),
               role = COALESCE(?, role),
               class_id = CASE WHEN ? THEN ? ELSE class_id END,
               disabled = COALESCE(?, disabled),
               authz_epoch = authz_epoch + 1
             WHERE login = ?
             RETURNING id",
        )
        .bind(phc)
        .bind(role.map(|r| r as i64))
        .bind(class_id.is_some())
        .bind(class_id.flatten())
        .bind(disabled.map(|d| d as i64))
        .bind(login)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let id: i64 = row.get("id");
        sqlx::query("DELETE FROM sessions WHERE account_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(id))
    }
}

/// A permission class row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub id: i64,
    pub name: String,
    pub base_mask: u64,
}

pub struct ClassesRepo<'a>(pub &'a SqlitePool);

impl ClassesRepo<'_> {
    pub async fn by_name(&self, name: &str) -> Result<Option<Class>, StoreError> {
        Ok(sqlx::query("SELECT * FROM classes WHERE name = ?")
            .bind(name)
            .fetch_optional(self.0)
            .await?
            .map(|r| Class {
                id: r.get("id"),
                name: r.get("name"),
                base_mask: r.get::<i64, _>("base_mask") as u64,
            }))
    }

    pub async fn by_id(&self, id: i64) -> Result<Option<Class>, StoreError> {
        Ok(sqlx::query("SELECT * FROM classes WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?
            .map(|r| Class {
                id: r.get("id"),
                name: r.get("name"),
                base_mask: r.get::<i64, _>("base_mask") as u64,
            }))
    }

    pub async fn set_mask(&self, name: &str, mask: u64) -> Result<(), StoreError> {
        sqlx::query("UPDATE classes SET base_mask = ? WHERE name = ?")
            .bind(mask as i64)
            .bind(name)
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// Create/update a class and revoke every bearer belonging to its current
    /// members in one SQLite write transaction. Returns those account ids so
    /// live connections can be invalidated after the durable commit.
    pub async fn set_mask_and_revoke_sessions(
        &self,
        name: &str,
        mask: u64,
    ) -> Result<(i64, Vec<i64>), StoreError> {
        let mut tx = self.0.begin().await?;
        let id: i64 = sqlx::query(
            "INSERT INTO classes (name, base_mask) VALUES (?, ?)
             ON CONFLICT (name) DO UPDATE SET base_mask = excluded.base_mask
             RETURNING id",
        )
        .bind(name)
        .bind(mask as i64)
        .fetch_one(&mut *tx)
        .await?
        .get("id");
        let account_ids: Vec<i64> =
            sqlx::query("SELECT id FROM accounts WHERE class_id = ? ORDER BY id")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?
                .iter()
                .map(|row| row.get("id"))
                .collect();
        sqlx::query("UPDATE accounts SET authz_epoch = authz_epoch + 1 WHERE class_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM sessions WHERE account_id IN (SELECT id FROM accounts WHERE class_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((id, account_ids))
    }

    pub async fn all(&self) -> Result<Vec<Class>, StoreError> {
        Ok(sqlx::query("SELECT * FROM classes ORDER BY id")
            .fetch_all(self.0)
            .await?
            .iter()
            .map(|r| Class {
                id: r.get("id"),
                name: r.get("name"),
                base_mask: r.get::<i64, _>("base_mask") as u64,
            })
            .collect())
    }

    /// Create-or-update by name; returns the class id.
    pub async fn upsert(&self, name: &str, mask: u64) -> Result<i64, StoreError> {
        Ok(sqlx::query(
            "INSERT INTO classes (name, base_mask) VALUES (?, ?)
             ON CONFLICT (name) DO UPDATE SET base_mask = excluded.base_mask
             RETURNING id",
        )
        .bind(name)
        .bind(mask as i64)
        .fetch_one(self.0)
        .await?
        .get("id"))
    }

    /// Number of accounts in each class, keyed by class id.
    pub async fn member_counts(&self) -> Result<std::collections::HashMap<i64, u64>, StoreError> {
        Ok(sqlx::query(
            "SELECT class_id, COUNT(*) AS n FROM accounts
             WHERE class_id IS NOT NULL GROUP BY class_id",
        )
        .fetch_all(self.0)
        .await?
        .iter()
        .map(|r| (r.get::<i64, _>("class_id"), r.get::<i64, _>("n") as u64))
        .collect())
    }
}

pub struct SessionsRepo<'a>(pub &'a SqlitePool);

impl SessionsRepo<'_> {
    /// Persist a session token hash. `ttl_secs` from now.
    pub async fn insert(
        &self,
        token_hash: &[u8; 32],
        account_id: i64,
        authz_epoch: i64,
        ttl_secs: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO sessions
                (token_hash, account_id, authz_epoch, created_at, expires_at, last_seen)
             VALUES (?, ?, ?, unixepoch(), unixepoch() + ?, unixepoch())",
        )
        .bind(token_hash.as_slice())
        .bind(account_id)
        .bind(authz_epoch)
        .bind(ttl_secs)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// Issue a token only while the account is enabled and still at the
    /// authorization epoch observed during credential verification.
    pub async fn insert_if_current(
        &self,
        token_hash: &[u8; 32],
        account_id: i64,
        authz_epoch: i64,
        ttl_secs: i64,
    ) -> Result<bool, StoreError> {
        Ok(sqlx::query(
            "INSERT INTO sessions
                (token_hash, account_id, authz_epoch, created_at, expires_at, last_seen)
             SELECT ?, id, authz_epoch, unixepoch(), unixepoch() + ?, unixepoch()
             FROM accounts
             WHERE id = ? AND disabled = 0 AND authz_epoch = ?",
        )
        .bind(token_hash.as_slice())
        .bind(ttl_secs)
        .bind(account_id)
        .bind(authz_epoch)
        .execute(self.0)
        .await?
        .rows_affected()
            == 1)
    }

    /// Atomically consume one live token and replace it with a fresh token.
    ///
    /// The delete and insert share one SQLite write transaction, so two
    /// concurrent resume attempts with the same bearer cannot both succeed.
    /// `ttl_secs` deliberately starts a fresh lifetime for the rotated token.
    pub async fn rotate(
        &self,
        old_hash: &[u8; 32],
        new_hash: &[u8; 32],
        ttl_secs: i64,
    ) -> Result<Option<(i64, i64)>, StoreError> {
        let mut tx = self.0.begin().await?;
        let row = sqlx::query(
            "DELETE FROM sessions
             WHERE token_hash = ? AND expires_at > unixepoch()
             RETURNING account_id, authz_epoch",
        )
        .bind(old_hash.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let account_id: i64 = row.get("account_id");
        let authz_epoch: i64 = row.get("authz_epoch");
        let current = sqlx::query(
            "SELECT 1 FROM accounts
             WHERE id = ? AND disabled = 0 AND authz_epoch = ?",
        )
        .bind(account_id)
        .bind(authz_epoch)
        .fetch_optional(&mut *tx)
        .await?;
        if current.is_none() {
            tx.rollback().await?;
            return Ok(None);
        }
        sqlx::query(
            "INSERT INTO sessions
                (token_hash, account_id, authz_epoch, created_at, expires_at, last_seen)
             VALUES (?, ?, ?, unixepoch(), unixepoch() + ?, unixepoch())",
        )
        .bind(new_hash.as_slice())
        .bind(account_id)
        .bind(authz_epoch)
        .bind(ttl_secs)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some((account_id, authz_epoch)))
    }

    pub async fn revoke(&self, token_hash: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash.as_slice())
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// Resolve an unexpired bearer without consuming or extending it.
    pub async fn active_account(
        &self,
        token_hash: &[u8; 32],
    ) -> Result<Option<(i64, i64)>, StoreError> {
        Ok(sqlx::query(
            "SELECT s.account_id, s.authz_epoch
             FROM sessions s
             JOIN accounts a ON a.id = s.account_id
             WHERE s.token_hash = ? AND s.expires_at > unixepoch()
               AND a.disabled = 0 AND a.authz_epoch = s.authz_epoch",
        )
        .bind(token_hash.as_slice())
        .fetch_optional(self.0)
        .await?
        .map(|row| (row.get("account_id"), row.get("authz_epoch"))))
    }

    /// Revoke every resumable bearer belonging to an account.
    pub async fn revoke_account(&self, account_id: i64) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM sessions WHERE account_id = ?")
            .bind(account_id)
            .execute(self.0)
            .await?
            .rows_affected())
    }

    /// Remove expired sessions; returns how many were reaped.
    pub async fn reap_expired(&self) -> Result<u64, StoreError> {
        Ok(
            sqlx::query("DELETE FROM sessions WHERE expires_at <= unixepoch()")
                .execute(self.0)
                .await?
                .rows_affected(),
        )
    }
}

/// One ACL rule attached to a resource path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclRow {
    pub resource: String,
    /// 0 everyone, 1 role, 2 class, 3 account.
    pub principal_kind: u8,
    pub principal_id: i64,
    pub allow_mask: u64,
    pub deny_mask: u64,
}

pub struct AclRepo<'a>(pub &'a SqlitePool);

impl AclRepo<'_> {
    pub async fn upsert(&self, row: &AclRow) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO acl_entries (resource, principal_kind, principal_id, allow_mask, deny_mask)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (resource, principal_kind, principal_id)
             DO UPDATE SET allow_mask = excluded.allow_mask, deny_mask = excluded.deny_mask",
        )
        .bind(&row.resource)
        .bind(row.principal_kind as i64)
        .bind(row.principal_id)
        .bind(row.allow_mask as i64)
        .bind(row.deny_mask as i64)
        .execute(self.0)
        .await?;
        Ok(())
    }

    pub async fn all(&self) -> Result<Vec<AclRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM acl_entries")
            .fetch_all(self.0)
            .await?
            .iter()
            .map(|r| AclRow {
                resource: r.get("resource"),
                principal_kind: r.get::<i64, _>("principal_kind") as u8,
                principal_id: r.get("principal_id"),
                allow_mask: r.get::<i64, _>("allow_mask") as u64,
                deny_mask: r.get::<i64, _>("deny_mask") as u64,
            })
            .collect())
    }
}

pub struct AuditRepo<'a>(pub &'a SqlitePool);

/// One audit-log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub id: i64,
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

impl AuditRepo<'_> {
    pub async fn record(&self, actor: &str, action: &str, detail: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO audit_log (at, actor, action, detail) VALUES (unixepoch(), ?, ?, ?)",
        )
        .bind(actor)
        .bind(action)
        .bind(detail)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// The most recent `limit` entries, oldest first (for AUDIT_READ views
    /// and tests asserting the trail).
    pub async fn recent(&self, limit: i64) -> Result<Vec<AuditRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM (SELECT * FROM audit_log ORDER BY id DESC LIMIT ?) ORDER BY id",
        )
        .bind(limit)
        .fetch_all(self.0)
        .await?;
        Ok(rows
            .iter()
            .map(|r| AuditRow {
                id: r.get("id"),
                at: r.get("at"),
                actor: r.get("actor"),
                action: r.get("action"),
                detail: r.get("detail"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn account_lifecycle() {
        let pool = open_in_memory().await.unwrap();
        let accounts = AccountsRepo(&pool);
        let classes = ClassesRepo(&pool);

        let member = classes.by_name("member").await.unwrap().unwrap();
        let a = accounts
            .create("Alice", Some("$argon2id$fake"), "Alice", 1, Some(member.id))
            .await
            .unwrap();
        assert_eq!(a.login, "Alice");

        // Logins are case-insensitive.
        let found = accounts.by_login("alice").await.unwrap().unwrap();
        assert_eq!(found.id, a.id);

        // Duplicate login rejected.
        assert!(accounts
            .create("ALICE", None, "Impostor", 1, None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn class_masks_bitcast_roundtrip() {
        let pool = open_in_memory().await.unwrap();
        let classes = ClassesRepo(&pool);
        // High bit set — must survive the u64 ↔ i64 bit-cast.
        let mask = 0x8000_0000_0000_0001u64;
        classes.set_mask("admin", mask).await.unwrap();
        assert_eq!(
            classes.by_name("admin").await.unwrap().unwrap().base_mask,
            mask
        );
    }

    #[tokio::test]
    async fn session_resume_and_expiry() {
        let pool = open_in_memory().await.unwrap();
        let accounts = AccountsRepo(&pool);
        let sessions = SessionsRepo(&pool);
        let a = accounts.create("bob", None, "Bob", 1, None).await.unwrap();

        let hash = [7u8; 32];
        sessions
            .insert(&hash, a.id, a.authz_epoch, 3600)
            .await
            .unwrap();
        let rotated = [8u8; 32];
        assert_eq!(
            sessions.rotate(&hash, &rotated, 3600).await.unwrap(),
            Some((a.id, a.authz_epoch))
        );
        assert_eq!(
            sessions.rotate(&hash, &[6u8; 32], 3600).await.unwrap(),
            None,
            "a resumed bearer is single-use"
        );

        // Expired token doesn't resume and gets reaped.
        let stale = [9u8; 32];
        sessions
            .insert(&stale, a.id, a.authz_epoch, -10)
            .await
            .unwrap();
        assert_eq!(
            sessions.rotate(&stale, &[5u8; 32], 3600).await.unwrap(),
            None
        );
        assert_eq!(sessions.reap_expired().await.unwrap(), 1);

        sessions.revoke(&rotated).await.unwrap();
        assert_eq!(
            sessions.rotate(&rotated, &[4u8; 32], 3600).await.unwrap(),
            None
        );

        sessions
            .insert(&[1u8; 32], a.id, a.authz_epoch, 3600)
            .await
            .unwrap();
        sessions
            .insert(&[2u8; 32], a.id, a.authz_epoch, 3600)
            .await
            .unwrap();
        assert_eq!(sessions.revoke_account(a.id).await.unwrap(), 2);
        assert_eq!(
            sessions.rotate(&[1u8; 32], &[3u8; 32], 3600).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn authorization_mutations_bump_epoch_and_revoke_scoped_bearers() {
        let pool = open_in_memory().await.unwrap();
        let accounts = AccountsRepo(&pool);
        let classes = ClassesRepo(&pool);
        let sessions = SessionsRepo(&pool);
        let member = classes.by_name("member").await.unwrap().unwrap();
        let alice = accounts
            .create("alice", None, "Alice", 1, Some(member.id))
            .await
            .unwrap();
        let bob = accounts
            .create("bob", None, "Bob", 1, Some(member.id))
            .await
            .unwrap();
        sessions
            .insert(&[1u8; 32], alice.id, alice.authz_epoch, 3600)
            .await
            .unwrap();
        sessions
            .insert(&[2u8; 32], bob.id, bob.authz_epoch, 3600)
            .await
            .unwrap();

        assert_eq!(
            accounts
                .admin_update_and_revoke_sessions("alice", None, Some(2), None, None)
                .await
                .unwrap(),
            Some(alice.id)
        );
        let alice_now = accounts.by_id(alice.id).await.unwrap().unwrap();
        assert_eq!(alice_now.authz_epoch, alice.authz_epoch + 1);
        assert_eq!(sessions.active_account(&[1u8; 32]).await.unwrap(), None);
        assert!(
            !sessions
                .insert_if_current(&[9u8; 32], alice.id, alice.authz_epoch, 3600)
                .await
                .unwrap(),
            "a credential check from the prior epoch cannot issue after mutation"
        );
        assert_eq!(
            sessions.active_account(&[2u8; 32]).await.unwrap(),
            Some((bob.id, bob.authz_epoch)),
            "another account is unaffected"
        );

        sessions
            .insert(&[3u8; 32], alice.id, alice_now.authz_epoch, 3600)
            .await
            .unwrap();
        let (class_id, affected) = classes
            .set_mask_and_revoke_sessions("member", 0x55)
            .await
            .unwrap();
        assert_eq!(class_id, member.id);
        assert_eq!(affected, vec![alice.id, bob.id]);
        assert_eq!(sessions.active_account(&[2u8; 32]).await.unwrap(), None);
        assert_eq!(sessions.active_account(&[3u8; 32]).await.unwrap(), None);
        assert_eq!(
            accounts.by_id(alice.id).await.unwrap().unwrap().authz_epoch,
            alice_now.authz_epoch + 1
        );
    }

    #[tokio::test]
    async fn issue_and_resume_races_cannot_survive_an_authorization_change() {
        let work = tempfile::tempdir().unwrap();
        let pool = crate::open(&work.path().join("authz-races.sqlite"))
            .await
            .unwrap();
        let account = AccountsRepo(&pool)
            .create("alice", None, "Alice", 1, None)
            .await
            .unwrap();

        for round in 0..20u8 {
            let before = AccountsRepo(&pool)
                .by_id(account.id)
                .await
                .unwrap()
                .unwrap();
            let issued_hash = [round; 32];
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

            let issue_pool = pool.clone();
            let issue_barrier = barrier.clone();
            let issue = tokio::spawn(async move {
                issue_barrier.wait().await;
                SessionsRepo(&issue_pool)
                    .insert_if_current(&issued_hash, before.id, before.authz_epoch, 3600)
                    .await
                    .unwrap()
            });

            let mutation_pool = pool.clone();
            let mutation_barrier = barrier.clone();
            let mutate = tokio::spawn(async move {
                mutation_barrier.wait().await;
                AccountsRepo(&mutation_pool)
                    .admin_update_and_revoke_sessions("alice", None, Some(before.role), None, None)
                    .await
                    .unwrap()
            });

            barrier.wait().await;
            let _issued_before_mutation = issue.await.unwrap();
            assert_eq!(mutate.await.unwrap(), Some(account.id));
            assert_eq!(
                SessionsRepo(&pool)
                    .active_account(&issued_hash)
                    .await
                    .unwrap(),
                None,
                "round {round}: a password-login issuance race left a usable bearer"
            );

            let current = AccountsRepo(&pool)
                .by_id(account.id)
                .await
                .unwrap()
                .unwrap();
            let old_hash = [round.wrapping_add(64); 32];
            let successor_hash = [round.wrapping_add(128); 32];
            SessionsRepo(&pool)
                .insert(&old_hash, current.id, current.authz_epoch, 3600)
                .await
                .unwrap();
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

            let resume_pool = pool.clone();
            let resume_barrier = barrier.clone();
            let resume = tokio::spawn(async move {
                resume_barrier.wait().await;
                SessionsRepo(&resume_pool)
                    .rotate(&old_hash, &successor_hash, 3600)
                    .await
                    .unwrap()
            });

            let mutation_pool = pool.clone();
            let mutation_barrier = barrier.clone();
            let mutate = tokio::spawn(async move {
                mutation_barrier.wait().await;
                AccountsRepo(&mutation_pool)
                    .admin_update_and_revoke_sessions("alice", None, Some(current.role), None, None)
                    .await
                    .unwrap()
            });

            barrier.wait().await;
            let _resumed_before_mutation = resume.await.unwrap();
            assert_eq!(mutate.await.unwrap(), Some(account.id));
            let sessions = SessionsRepo(&pool);
            assert_eq!(
                sessions.active_account(&old_hash).await.unwrap(),
                None,
                "round {round}: the consumed bearer survived its mutation race"
            );
            assert_eq!(
                sessions.active_account(&successor_hash).await.unwrap(),
                None,
                "round {round}: the rotated successor survived its mutation race"
            );
        }
    }

    #[tokio::test]
    async fn acl_upsert_and_audit() {
        let pool = open_in_memory().await.unwrap();
        let acl = AclRepo(&pool);
        let row = AclRow {
            resource: "files/uploads".into(),
            principal_kind: 1,
            principal_id: 0,
            allow_mask: 0b11,
            deny_mask: 0b100,
        };
        acl.upsert(&row).await.unwrap();
        // Upsert replaces.
        acl.upsert(&AclRow {
            allow_mask: 0b1,
            ..row.clone()
        })
        .await
        .unwrap();
        let all = acl.all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].allow_mask, 0b1);

        AuditRepo(&pool)
            .record("system", "test", "detail")
            .await
            .unwrap();
    }
}
