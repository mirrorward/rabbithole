//! Authentication service: password, guest, and token-resume sign-in.
//!
//! Wraps the store repos with the actual security policy: Argon2id
//! verification with transparent rehash-on-login, hashed session tokens,
//! guest identity synthesis, and a uniform [`AuthError`] that deliberately
//! does not distinguish "no such user" from "wrong password".

use std::sync::atomic::{AtomicU64, Ordering};

use rabbithole_identity::{hash_password, needs_rehash, verify_password, SessionToken};
use rabbithole_store_server::repo::{Account, AccountsRepo, ClassesRepo, SessionsRepo};
use rabbithole_store_server::SqlitePool;

use crate::permissions::{Role, Subject};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    BadCredentials,
    #[error("account disabled")]
    Disabled,
    #[error("guest access is disabled")]
    GuestsDisabled,
    #[error("session expired or revoked")]
    SessionExpired,
    #[error("login already taken")]
    LoginTaken,
    #[error("store: {0}")]
    Store(#[from] rabbithole_store_server::StoreError),
    #[error("hash: {0}")]
    Hash(#[from] rabbithole_identity::PasswordError),
}

/// An authenticated principal, ready for permission evaluation.
#[derive(Debug, Clone)]
pub struct AuthedUser {
    pub account: Account,
    pub subject: Subject,
    /// Present for password/resume logins; `None` for guests.
    pub token: Option<SessionToken>,
}

pub struct AuthService {
    pool: SqlitePool,
    session_ttl_secs: i64,
    guest_counter: AtomicU64,
}

impl AuthService {
    pub fn new(pool: SqlitePool, session_ttl_secs: i64) -> Self {
        Self {
            pool,
            session_ttl_secs,
            guest_counter: AtomicU64::new(1),
        }
    }

    /// First-boot initialization: give the seeded classes their default
    /// masks (idempotent — only zero masks are filled in).
    pub async fn seed_class_masks(&self) -> Result<(), AuthError> {
        let classes = ClassesRepo(&self.pool);
        for role in [
            Role::Guest,
            Role::User,
            Role::Moderator,
            Role::Admin,
            Role::Superuser,
        ] {
            if let Some(class) = classes.by_name(role.class_name()).await? {
                if class.base_mask == 0 {
                    classes
                        .set_mask(role.class_name(), role.default_caps().0)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Create an account (admin/ctl path — registration gating is Wave 2).
    pub async fn create_account(
        &self,
        login: &str,
        password: &str,
        role: Role,
    ) -> Result<Account, AuthError> {
        let accounts = AccountsRepo(&self.pool);
        if accounts.by_login(login).await?.is_some() {
            return Err(AuthError::LoginTaken);
        }
        let phc = hash_password(password)?;
        let class = ClassesRepo(&self.pool)
            .by_name(role.class_name())
            .await?
            .map(|c| c.id);
        Ok(accounts
            .create(login, Some(&phc), login, role as u8, class)
            .await?)
    }

    /// Password sign-in. Issues a resumable session token.
    pub async fn login_password(
        &self,
        login: &str,
        password: &str,
    ) -> Result<AuthedUser, AuthError> {
        let accounts = AccountsRepo(&self.pool);
        let Some(account) = accounts.by_login(login).await? else {
            // Burn comparable time so absent/present logins are
            // indistinguishable by latency.
            let _ = verify_password(password, &dummy_phc());
            return Err(AuthError::BadCredentials);
        };
        let Some(phc) = account.phc.as_deref() else {
            return Err(AuthError::BadCredentials);
        };
        if !verify_password(password, phc)? {
            return Err(AuthError::BadCredentials);
        }
        if account.disabled {
            return Err(AuthError::Disabled);
        }
        if needs_rehash(phc)? {
            accounts
                .update_phc(account.id, &hash_password(password)?)
                .await?;
        }

        let token = SessionToken::generate();
        SessionsRepo(&self.pool)
            .insert(&token.storage_hash(), account.id, self.session_ttl_secs)
            .await?;

        let subject = self.subject_for(&account).await?;
        Ok(AuthedUser {
            account,
            subject,
            token: Some(token),
        })
    }

    /// Guest sign-in (caller checks the config toggle *and* passes it here
    /// for defense in depth).
    pub async fn login_guest(
        &self,
        guests_enabled: bool,
        desired_name: Option<&str>,
    ) -> Result<AuthedUser, AuthError> {
        if !guests_enabled {
            return Err(AuthError::GuestsDisabled);
        }
        let n = self.guest_counter.fetch_add(1, Ordering::Relaxed);
        let base = desired_name.map(sanitize_name).filter(|s| !s.is_empty());
        let screen_name = match base {
            Some(name) => format!("{name} (guest)"),
            None => format!("guest-{n}"),
        };
        // Guests are ephemeral: no account row, a synthetic negative id.
        let account = Account {
            id: -(n as i64),
            login: format!("guest:{n}"),
            phc: None,
            screen_name,
            role: Role::Guest as u8,
            class_id: None,
            grant_mask: 0,
            revoke_mask: 0,
            disabled: false,
        };
        let class_mask = ClassesRepo(&self.pool)
            .by_name(Role::Guest.class_name())
            .await?
            .map(|c| c.base_mask)
            .unwrap_or(0);
        let subject = Subject {
            account_id: account.id,
            role: Role::Guest,
            class_id: None,
            class_mask,
            grant_mask: 0,
            revoke_mask: 0,
        };
        Ok(AuthedUser {
            account,
            subject,
            token: None,
        })
    }

    /// Token resume.
    pub async fn login_resume(&self, token_str: &str) -> Result<AuthedUser, AuthError> {
        let Some(token) = SessionToken::decode(token_str) else {
            return Err(AuthError::SessionExpired);
        };
        let Some(account_id) = SessionsRepo(&self.pool)
            .resume(&token.storage_hash())
            .await?
        else {
            return Err(AuthError::SessionExpired);
        };
        let Some(account) = AccountsRepo(&self.pool).by_id(account_id).await? else {
            return Err(AuthError::SessionExpired);
        };
        if account.disabled {
            return Err(AuthError::Disabled);
        }
        let subject = self.subject_for(&account).await?;
        Ok(AuthedUser {
            account,
            subject,
            token: Some(token),
        })
    }

    async fn subject_for(&self, account: &Account) -> Result<Subject, AuthError> {
        let class_mask = match account.class_id {
            Some(id) => ClassesRepo(&self.pool)
                .by_id(id)
                .await?
                .map(|c| c.base_mask)
                .unwrap_or(0),
            None => 0,
        };
        Ok(Subject {
            account_id: account.id,
            role: Role::from_ordinal(account.role),
            class_id: account.class_id,
            class_mask,
            grant_mask: account.grant_mask,
            revoke_mask: account.revoke_mask,
        })
    }
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(24)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A fixed valid PHC used to equalize timing when the login doesn't exist.
fn dummy_phc() -> String {
    // Hash of an unguessable constant, generated once at startup and cached.
    use std::sync::OnceLock;
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY
        .get_or_init(|| hash_password("rabbithole-timing-dummy").expect("hashable"))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_store_server::open_in_memory;

    async fn service() -> AuthService {
        let pool = open_in_memory().await.unwrap();
        let svc = AuthService::new(pool, 3600);
        svc.seed_class_masks().await.unwrap();
        svc
    }

    #[tokio::test]
    async fn password_login_and_resume() {
        let svc = service().await;
        svc.create_account("alice", "correct horse", Role::User)
            .await
            .unwrap();

        // Duplicate login rejected cleanly.
        assert!(matches!(
            svc.create_account("ALICE", "x", Role::User).await,
            Err(AuthError::LoginTaken)
        ));

        let authed = svc.login_password("alice", "correct horse").await.unwrap();
        assert_eq!(authed.subject.role, Role::User);
        assert!(authed.subject.base_caps() & Role::User.default_caps().0 != 0);

        // Wrong password and unknown user look identical.
        assert!(matches!(
            svc.login_password("alice", "wrong").await,
            Err(AuthError::BadCredentials)
        ));
        assert!(matches!(
            svc.login_password("nobody", "wrong").await,
            Err(AuthError::BadCredentials)
        ));

        // Resume with the issued token.
        let token = authed.token.unwrap().encode();
        let resumed = svc.login_resume(&token).await.unwrap();
        assert_eq!(resumed.account.login, "alice");

        // Garbage token.
        assert!(matches!(
            svc.login_resume("garbage").await,
            Err(AuthError::SessionExpired)
        ));
    }

    #[tokio::test]
    async fn guest_policy_and_naming() {
        let svc = service().await;
        assert!(matches!(
            svc.login_guest(false, None).await,
            Err(AuthError::GuestsDisabled)
        ));

        let g1 = svc.login_guest(true, None).await.unwrap();
        assert!(g1.account.screen_name.starts_with("guest-"));
        assert!(g1.token.is_none());
        assert_eq!(g1.subject.role, Role::Guest);

        let g2 = svc
            .login_guest(true, Some("  White Rabbit\u{7} "))
            .await
            .unwrap();
        assert_eq!(g2.account.screen_name, "White Rabbit (guest)");
        assert_ne!(g1.account.id, g2.account.id);
    }
}
