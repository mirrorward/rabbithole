//! Authentication service: password, guest, and token-resume sign-in.
//!
//! Wraps the store repos with the actual security policy: Argon2id
//! verification with transparent rehash-on-login, hashed session tokens,
//! guest identity synthesis, and a uniform [`AuthError`] that deliberately
//! does not distinguish "no such user" from "wrong password".

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use rabbithole_identity::totp::{check_recovery_code, TotpEnrollment};
use rabbithole_identity::{hash_password, needs_rehash, verify_password, SessionToken};
use rabbithole_store_server::repo::{Account, AccountsRepo, ClassesRepo, SessionsRepo};
use rabbithole_store_server::repo2::{InvitesRepo, PersonaRow, PersonasRepo, TotpRepo};
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
    #[error("a TOTP code is required")]
    TotpRequired,
    #[error("registration is closed")]
    RegistrationClosed,
    #[error("invalid or expired invite code")]
    BadInvite,
    #[error("store: {0}")]
    Store(#[from] rabbithole_store_server::StoreError),
    #[error("hash: {0}")]
    Hash(#[from] rabbithole_identity::PasswordError),
}

/// How new accounts may be created (config `registration_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationMode {
    Open,
    Invite,
    Closed,
}

impl RegistrationMode {
    pub fn parse(s: &str) -> Option<RegistrationMode> {
        match s {
            "open" => Some(RegistrationMode::Open),
            "invite" => Some(RegistrationMode::Invite),
            "closed" => Some(RegistrationMode::Closed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RegistrationMode::Open => "open",
            RegistrationMode::Invite => "invite",
            RegistrationMode::Closed => "closed",
        }
    }
}

/// An authenticated principal, ready for permission evaluation.
#[derive(Debug, Clone)]
pub struct AuthedUser {
    pub account: Account,
    pub subject: Subject,
    /// The persona this session starts as (accounts always have one;
    /// guests get a synthetic, unsaved one).
    pub persona: PersonaRow,
    /// Present for password/resume logins; `None` for guests.
    pub token: Option<SessionToken>,
}

pub struct AuthService {
    pool: SqlitePool,
    session_ttl_secs: i64,
    guest_counter: AtomicU64,
}

/// One node of an invite tree: an account plus how many invite hops it sits
/// below the queried root. Ordered breadth-first by [`AuthService::invite_subtree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteNode {
    /// The account's id.
    pub account_id: i64,
    /// The account's login.
    pub login: String,
    /// Invite hops from the queried root (`0` = the root itself).
    pub depth: usize,
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

    /// Create an account with its default persona (admin/ctl/registration).
    pub async fn create_account(
        &self,
        login: &str,
        password: &str,
        role: Role,
    ) -> Result<Account, AuthError> {
        let accounts = AccountsRepo(&self.pool);
        if accounts.by_login(login).await?.is_some()
            || PersonasRepo(&self.pool)
                .by_screen_name(login)
                .await?
                .is_some()
        {
            return Err(AuthError::LoginTaken);
        }
        let phc = hash_password(password)?;
        let class = ClassesRepo(&self.pool)
            .by_name(role.class_name())
            .await?
            .map(|c| c.id);
        let account = accounts
            .create(login, Some(&phc), login, role as u8, class)
            .await?;
        PersonasRepo(&self.pool)
            .create(account.id, login, true)
            .await?;
        Ok(account)
    }

    /// Self-service registration, honoring the server's mode. On success
    /// the account is created and signed in (returns like a password login).
    pub async fn register(
        &self,
        mode: RegistrationMode,
        login: &str,
        password: &str,
        invite_code: Option<&str>,
    ) -> Result<AuthedUser, AuthError> {
        // On invite mode, atomically reserve the code before creating the
        // account, capturing the inviter so we can record the invite-tree edge
        // once the new account has an id.
        let mut pending_invite: Option<(String, i64)> = None;
        match mode {
            RegistrationMode::Closed => return Err(AuthError::RegistrationClosed),
            RegistrationMode::Open => {}
            RegistrationMode::Invite => {
                let Some(code) = invite_code else {
                    return Err(AuthError::BadInvite);
                };
                let Some(inviter) = InvitesRepo(&self.pool).reserve(code).await? else {
                    return Err(AuthError::BadInvite);
                };
                pending_invite = Some((code.to_string(), inviter));
            }
        }
        let account = self.create_account(login, password, Role::User).await?;
        if let Some((code, inviter)) = pending_invite {
            // Finalise the invite with the real redeemer + record who invited
            // this account (the tree edge). Best-effort: a stored-lineage
            // hiccup must not fail an otherwise-valid registration.
            let _ = InvitesRepo(&self.pool).finalize(&code, account.id).await;
            let _ = AccountsRepo(&self.pool)
                .set_invited_by(account.id, inviter)
                .await;
        }
        self.login_password(login, password, None).await
    }

    /// Walk the invite subtree rooted at `root_login`: the account itself plus
    /// every account it (transitively) invited, breadth-first and bounded to
    /// `max_nodes` (a corrupt-data / runaway backstop). Empty when the login is
    /// unknown. Lets an operator trace — and then act on — a whole downline.
    pub async fn invite_subtree(
        &self,
        root_login: &str,
        max_nodes: usize,
    ) -> Result<Vec<InviteNode>, AuthError> {
        let accounts = AccountsRepo(&self.pool);
        let Some(root) = accounts.by_login(root_login).await? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        let mut queue: VecDeque<(i64, String, usize)> = VecDeque::new();
        queue.push_back((root.id, root.login, 0));
        while let Some((id, login, depth)) = queue.pop_front() {
            if out.len() >= max_nodes || !seen.insert(id) {
                continue;
            }
            out.push(InviteNode {
                account_id: id,
                login,
                depth,
            });
            for (child_id, child_login) in accounts.invitees(id).await? {
                queue.push_back((child_id, child_login, depth + 1));
            }
        }
        Ok(out)
    }

    /// Password sign-in. Issues a resumable session token. Accounts with
    /// confirmed TOTP must supply a current code or a recovery code.
    pub async fn login_password(
        &self,
        login: &str,
        password: &str,
        totp_code: Option<&str>,
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

        // 2FA gate — only after the password checked out, so the error
        // doesn't leak whether the password was right.
        if let Some(totp) = TotpRepo(&self.pool).get(account.id).await? {
            if totp.confirmed {
                let Some(code) = totp_code else {
                    return Err(AuthError::TotpRequired);
                };
                let enrollment =
                    TotpEnrollment::from_secret(&totp.secret, "RabbitHole", &account.login)
                        .map_err(|_| AuthError::BadCredentials)?;
                let ok_totp = enrollment.verify(code).unwrap_or(false);
                if !ok_totp {
                    // Try it as a recovery code; burn it on success.
                    match check_recovery_code(code, &totp.recovery_hashes) {
                        Some(idx) => {
                            let mut remaining = totp.recovery_hashes.clone();
                            remaining.remove(idx);
                            TotpRepo(&self.pool)
                                .spend_recovery(account.id, &remaining)
                                .await?;
                        }
                        None => return Err(AuthError::BadCredentials),
                    }
                }
            }
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
        let persona = self.default_persona(&account).await?;
        Ok(AuthedUser {
            account,
            subject,
            persona,
            token: Some(token),
        })
    }

    /// The account's default persona (creating one if somehow missing).
    async fn default_persona(&self, account: &Account) -> Result<PersonaRow, AuthError> {
        let personas = PersonasRepo(&self.pool);
        match personas.default_for_account(account.id).await? {
            Some(p) => Ok(p),
            None => Ok(personas.create(account.id, &account.login, true).await?),
        }
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
        // Synthetic, unsaved persona for the guest session.
        let persona = PersonaRow {
            id: account.id,
            account_id: account.id,
            screen_name: account.screen_name.clone(),
            is_default: true,
            location: None,
            interests: None,
            quote: None,
            plan: None,
            pronouns: None,
            avatar_hex: None,
            banner_hex: None,
            directory_visible: false,
        };
        Ok(AuthedUser {
            account,
            subject,
            persona,
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
        let persona = self.default_persona(&account).await?;
        Ok(AuthedUser {
            account,
            subject,
            persona,
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

        let authed = svc
            .login_password("alice", "correct horse", None)
            .await
            .unwrap();
        assert_eq!(authed.subject.role, Role::User);
        assert!(authed.subject.base_caps() & Role::User.default_caps().0 != 0);

        // Wrong password and unknown user look identical.
        assert!(matches!(
            svc.login_password("alice", "wrong", None).await,
            Err(AuthError::BadCredentials)
        ));
        assert!(matches!(
            svc.login_password("nobody", "wrong", None).await,
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
    async fn invite_registration_records_the_lineage_tree() {
        let svc = service().await;
        let pw = "correct-horse-battery-staple";
        let alice = svc.create_account("alice", pw, Role::User).await.unwrap();

        // Alice mints invites; Bob and Carol redeem them.
        let invites = InvitesRepo(&svc.pool);
        invites.create("code-bob", alice.id, 3600).await.unwrap();
        invites.create("code-carol", alice.id, 3600).await.unwrap();
        svc.register(RegistrationMode::Invite, "bob", pw, Some("code-bob"))
            .await
            .unwrap();
        svc.register(RegistrationMode::Invite, "carol", pw, Some("code-carol"))
            .await
            .unwrap();

        // Bob invites Dan — a second level of the tree.
        let bob = AccountsRepo(&svc.pool)
            .by_login("bob")
            .await
            .unwrap()
            .unwrap();
        invites.create("code-dan", bob.id, 3600).await.unwrap();
        svc.register(RegistrationMode::Invite, "dan", pw, Some("code-dan"))
            .await
            .unwrap();

        // The invite is finalised to the real redeemer (not the 0 placeholder),
        // so it can't be consumed again.
        assert!(
            !invites.consume("code-bob", 42).await.unwrap(),
            "a redeemed invite is spent"
        );

        // Alice's subtree: alice(0) → {bob, carol}(1) → dan(2).
        let tree = svc.invite_subtree("alice", 100).await.unwrap();
        let depth: std::collections::HashMap<&str, usize> =
            tree.iter().map(|n| (n.login.as_str(), n.depth)).collect();
        assert_eq!(tree.len(), 4);
        assert_eq!(depth.get("alice"), Some(&0));
        assert_eq!(depth.get("bob"), Some(&1));
        assert_eq!(depth.get("carol"), Some(&1));
        assert_eq!(depth.get("dan"), Some(&2));
        // Breadth-first: the root is first, level 1 before level 2.
        assert_eq!(tree[0].login, "alice");
        assert!(tree.last().unwrap().login == "dan");

        // A leaf's subtree is just itself; an unknown login is empty.
        assert_eq!(svc.invite_subtree("dan", 100).await.unwrap().len(), 1);
        assert!(svc.invite_subtree("nobody", 100).await.unwrap().is_empty());

        // The node cap bounds the walk.
        assert_eq!(svc.invite_subtree("alice", 2).await.unwrap().len(), 2);

        // A bad invite code is refused and creates no account.
        assert!(matches!(
            svc.register(RegistrationMode::Invite, "mallory", pw, Some("no-such"))
                .await,
            Err(AuthError::BadInvite)
        ));
        assert!(AccountsRepo(&svc.pool)
            .by_login("mallory")
            .await
            .unwrap()
            .is_none());
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
