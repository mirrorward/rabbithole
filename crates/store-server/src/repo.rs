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

    /// Which agreement this person has accepted here (the blake3 of the
    /// text they were shown), if any.
    pub async fn agreed_hash(&self, id: i64) -> Result<Option<[u8; 32]>, StoreError> {
        let row = sqlx::query("SELECT agreed_hash FROM accounts WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?;
        Ok(row
            .and_then(|r| r.get::<Option<Vec<u8>>, _>("agreed_hash"))
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok()))
    }

    /// Remember that they accepted this agreement, so they are not asked
    /// again until its wording changes.
    pub async fn set_agreed(&self, id: i64, hash: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET agreed_hash = ?, agreed_at = unixepoch() WHERE id = ?")
            .bind(&hash[..])
            .bind(id)
            .execute(self.0)
            .await?;
        Ok(())
    }

    pub async fn update_phc(&self, id: i64, phc: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET phc = ? WHERE id = ?")
            .bind(phc)
            .bind(id)
            .execute(self.0)
            .await?;
        Ok(())
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

    /// How many accounts may administer the burrow: at `role` or above and
    /// not disabled. A burrow with one of them is a burrow that cannot
    /// lose them, or nobody could make another.
    pub async fn keepers(&self, role: u8) -> Result<i64, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE role >= ? AND disabled = 0")
                .bind(role as i64)
                .fetch_one(self.0)
                .await?,
        )
    }

    /// The accounts whose login holds `find`, and how many do in all: a
    /// burrow with thousands of them is not read a page at a time looking
    /// for one person. Case is forgiven; `%` and `_` are literal.
    pub async fn search(
        &self,
        find: &str,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<Account>, i64), StoreError> {
        let pattern = format!(
            "%{}%",
            find.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = sqlx::query(
            "SELECT * FROM accounts WHERE login LIKE ? ESCAPE '\\' ORDER BY id LIMIT ? OFFSET ?",
        )
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.0)
        .await?;
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE login LIKE ? ESCAPE '\\'")
                .bind(&pattern)
                .fetch_one(self.0)
                .await?;
        Ok((rows.iter().map(row_to_account).collect(), total))
    }

    /// Remove an account for good, unless it is the last one that can keep
    /// the burrow: at `keeper_role` or above and not disabled. What hangs
    /// off the person goes with them (personas, sessions, two-factor, keys,
    /// buddies, read marks); what they wrote does not, because a post and a
    /// file keep the name they were written under. Their names are retired
    /// so nobody can take the byline, and the invitations they handed out
    /// that nobody used are withdrawn.
    ///
    /// All of it in one transaction, so two operators cannot each remove
    /// the other's last keeper at the same moment. `Ok(false)` means there
    /// was no such account, or that removing it would leave the burrow with
    /// nobody to keep it.
    pub async fn delete(&self, id: i64, keeper_role: u8) -> Result<bool, StoreError> {
        let mut tx = self.0.begin().await?;
        // The names to retire, while the rows are still there.
        let login: Option<String> = sqlx::query_scalar("SELECT login FROM accounts WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(login) = login else {
            return Ok(false);
        };
        let names: Vec<String> =
            sqlx::query_scalar("SELECT screen_name FROM personas WHERE account_id = ?")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        // Somebody else who can keep the burrow has to be left.
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM accounts WHERE role >= ? AND disabled = 0 AND id <> ?",
        )
        .bind(keeper_role as i64)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let is_keeper: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM accounts WHERE id = ? AND role >= ? AND disabled = 0",
        )
        .bind(id)
        .bind(keeper_role as i64)
        .fetch_one(&mut *tx)
        .await?;
        if is_keeper > 0 && others == 0 {
            return Ok(false);
        }
        for name in std::iter::once(login.clone()).chain(names) {
            sqlx::query(
                "INSERT OR IGNORE INTO retired_names (name, was, at) VALUES (?, ?, unixepoch())",
            )
            .bind(&name)
            .bind(&login)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM invites WHERE created_by = ? AND used_by IS NULL")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Whether `name` belonged to somebody who was removed: it is theirs on
    /// everything they wrote, so nobody else takes it.
    pub async fn name_is_retired(&self, name: &str) -> Result<bool, StoreError> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM retired_names WHERE name = ?")
                .bind(name)
                .fetch_one(self.0)
                .await?
                > 0,
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
        ttl_secs: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO sessions (token_hash, account_id, created_at, expires_at, last_seen)
             VALUES (?, ?, unixepoch(), unixepoch() + ?, unixepoch())",
        )
        .bind(token_hash.as_slice())
        .bind(account_id)
        .bind(ttl_secs)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// Look up a live (unexpired) session, touching last_seen.
    pub async fn resume(&self, token_hash: &[u8; 32]) -> Result<Option<i64>, StoreError> {
        let row = sqlx::query(
            "UPDATE sessions SET last_seen = unixepoch()
             WHERE token_hash = ? AND expires_at > unixepoch()
             RETURNING account_id",
        )
        .bind(token_hash.as_slice())
        .fetch_optional(self.0)
        .await?;
        Ok(row.map(|r| r.get("account_id")))
    }

    pub async fn revoke(&self, token_hash: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash.as_slice())
            .execute(self.0)
            .await?;
        Ok(())
    }

    /// Remove expired sessions; returns how many were reaped.
    /// Forget every saved sign-in of one account: after a password reset or
    /// a disable, a token minted under the old state must not resume.
    pub async fn revoke_account(&self, account_id: i64) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM sessions WHERE account_id = ?")
            .bind(account_id)
            .execute(self.0)
            .await?
            .rows_affected())
    }

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
    async fn an_account_can_be_found_by_part_of_its_login_and_removed_for_good() {
        let pool = open_in_memory().await.unwrap();
        let accounts = AccountsRepo(&pool);
        for login in ["alice", "alicia", "bob", "100%pure"] {
            accounts
                .create(login, Some("$argon2id$fake"), login, 1, None)
                .await
                .unwrap();
        }

        // Part of a login, case forgiven, and how many match in all.
        let (found, total) = accounts.search("ALI", 0, 10).await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(
            found.iter().map(|a| a.login.as_str()).collect::<Vec<_>>(),
            ["alice", "alicia"]
        );
        // A page of one, out of two.
        let (page, total) = accounts.search("ali", 1, 1).await.unwrap();
        assert_eq!((page.len(), total), (1, 2));
        // The wildcards a person can type are letters here, not patterns.
        let (any, total) = accounts.search("%", 0, 10).await.unwrap();
        assert_eq!(total, 1, "only the account with a % in its name: {any:?}");
        assert_eq!(accounts.search("_", 0, 10).await.unwrap().1, 0);

        // Removing one takes what hangs off it (a persona) with it.
        let bob = accounts.by_login("bob").await.unwrap().unwrap();
        let personas = crate::repo2::PersonasRepo(&pool);
        personas.create(bob.id, "bobcat", true).await.unwrap();
        assert!(personas.by_screen_name("bobcat").await.unwrap().is_some());
        assert!(accounts.delete(bob.id, 3).await.unwrap());
        assert!(accounts.by_login("bob").await.unwrap().is_none());
        assert!(
            personas.by_screen_name("bobcat").await.unwrap().is_none(),
            "what hung off the account goes with it"
        );
        assert!(!accounts.delete(bob.id, 3).await.unwrap(), "only once");
        // Their names stay theirs: nobody takes the byline.
        assert!(accounts.name_is_retired("bob").await.unwrap());
        assert!(accounts.name_is_retired("BOBCAT").await.unwrap());
        assert!(!accounts.name_is_retired("alice").await.unwrap());

        // How many can keep the burrow: nobody here is an admin yet.
        assert_eq!(accounts.keepers(3).await.unwrap(), 0);
        let root = accounts
            .create("root", Some("$argon2id$fake"), "root", 3, None)
            .await
            .unwrap();
        assert_eq!(accounts.keepers(3).await.unwrap(), 1);
        // The last one who can keep the burrow stays, whoever asks.
        assert!(!accounts.delete(root.id, 3).await.unwrap());
        assert!(accounts.by_login("root").await.unwrap().is_some());
        let second = accounts
            .create("second", Some("$argon2id$fake"), "second", 3, None)
            .await
            .unwrap();
        assert!(
            accounts.delete(root.id, 3).await.unwrap(),
            "now there are two"
        );
        // A disabled one keeps nothing, so it can go even as the last.
        accounts
            .admin_set("second", None, None, Some(true))
            .await
            .unwrap();
        assert_eq!(accounts.keepers(3).await.unwrap(), 0);
        assert!(accounts.delete(second.id, 3).await.unwrap());
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
        sessions.insert(&hash, a.id, 3600).await.unwrap();
        assert_eq!(sessions.resume(&hash).await.unwrap(), Some(a.id));

        // Expired token doesn't resume and gets reaped.
        let stale = [9u8; 32];
        sessions.insert(&stale, a.id, -10).await.unwrap();
        assert_eq!(sessions.resume(&stale).await.unwrap(), None);
        assert_eq!(sessions.reap_expired().await.unwrap(), 1);

        sessions.revoke(&hash).await.unwrap();
        assert_eq!(sessions.resume(&hash).await.unwrap(), None);
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
