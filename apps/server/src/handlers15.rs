//! The accounts an operator manages by hand (ADMIN 19..24): create one, give
//! one a new password, clear one's two-factor, and the invitations that let
//! people make their own.
//!
//! ## Who may do what to whom
//!
//! `ACCOUNT_ADMIN` says an operator may manage accounts. It does not say
//! *which*. Without an ordering, anyone holding it could make themselves a
//! superuser or lock the owner out, so every operation here (and `AccountSet`
//! and `ClassSet`, which had no such check) goes through [`Standing`]:
//!
//! - you act on accounts **below** your own role, never on a peer, never on
//!   someone above you;
//! - you hand out roles **up to** your own, never above it;
//! - you never act on **yourself** here. Your own password and two-factor have
//!   their own doors, and disabling or demoting yourself is a lockout;
//! - a superuser is exempt from the ordering, and from nothing else.

use std::sync::Arc;

use rabbithole_identity::hash_password;
use rabbithole_net::Connection;
use rabbithole_proto::{admin as padm, ErrorCode, Frame};
use rabbithole_server_core::{AuthError, Caps, Role, ServerEvent};
use rabbithole_store_server::repo::{Account, AccountsRepo, AuditRepo, SessionsRepo};
use rabbithole_store_server::repo2::{InvitesRepo, TotpRepo};

use crate::session::SessionCtx;
use crate::Shared;

/// The shortest password an operator may set for someone. (Registration has
/// no floor of its own yet; an account made by hand should not be the weak one.)
pub const MIN_PASSWORD_CHARS: usize = 8;
const MAX_PASSWORD_CHARS: usize = 256;
const MAX_LOGIN_CHARS: usize = 32;

/// An operator's standing relative to the accounts they manage.
pub struct Standing {
    pub account_id: i64,
    pub role: Role,
}

impl Standing {
    pub fn of(ctx: &SessionCtx) -> Self {
        Self {
            account_id: ctx.account_id,
            role: ctx.role,
        }
    }

    /// May this operator change `target` at all?
    pub fn may_manage(&self, target: &Account) -> bool {
        if target.id == self.account_id {
            return false;
        }
        self.role == Role::Superuser || Role::from_ordinal(target.role) < self.role
    }

    /// May this operator hand out `role`?
    pub fn may_assign(&self, role: Role) -> bool {
        self.role == Role::Superuser || role <= self.role
    }
}

/// A role ordinal off the wire. `Role::from_ordinal` is total (it clamps), and
/// a clamped typo must not become a superuser.
pub fn role_from_wire(n: u8) -> Option<Role> {
    (n <= Role::Superuser as u8).then(|| Role::from_ordinal(n))
}

fn login_is_acceptable(login: &str) -> bool {
    let n = login.chars().count();
    (1..=MAX_LOGIN_CHARS).contains(&n)
        && login == login.trim()
        && !login.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn password_is_acceptable(password: &str) -> bool {
    (MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&password.chars().count())
}

fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
}

/// Put an account out: its saved sign-ins stop resuming, and every session it
/// has open right now is closed. Used when it is disabled and when its
/// password changes, because in both cases whoever is connected got there
/// under a state that no longer holds.
pub async fn sign_out_everywhere(shared: &Arc<Shared>, account_id: i64, reason: &str) {
    let _ = SessionsRepo(&shared.pool).revoke_account(account_id).await;
    for entry in shared.presence.snapshot() {
        if entry.account_id == account_id {
            shared.bus.publish(ServerEvent::Kick {
                session_id: entry.session_id,
                reason: reason.to_string(),
            });
        }
    }
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    macro_rules! fail {
        ($code:expr) => {{
            conn.send(Frame::error_reply(frame, $code)).await?;
            return Ok(true);
        }};
    }
    macro_rules! account_admins_only {
        () => {
            if !ctx.allows(shared, "admin", Caps::ACCOUNT_ADMIN) {
                fail!(ErrorCode::Forbidden)
            }
        };
    }
    /// The account behind a login, if the operator may change it.
    macro_rules! manageable {
        ($login:expr) => {{
            let Some(target) = AccountsRepo(&shared.pool).by_login($login).await? else {
                fail!(ErrorCode::NotFound)
            };
            if !Standing::of(ctx).may_manage(&target) {
                fail!(ErrorCode::Forbidden)
            }
            target
        }};
    }

    if let Some(Ok(req)) = frame.decode::<padm::AccountCreate>() {
        account_admins_only!();
        let Some(role) = role_from_wire(req.role) else {
            fail!(ErrorCode::BadRequest)
        };
        if !Standing::of(ctx).may_assign(role) {
            fail!(ErrorCode::Forbidden);
        }
        if !login_is_acceptable(&req.login) || !password_is_acceptable(&req.password) {
            fail!(ErrorCode::BadRequest);
        }
        match shared
            .auth
            .create_account(&req.login, &req.password, role)
            .await
        {
            Ok(_) => {}
            Err(AuthError::LoginTaken) => fail!(ErrorCode::AlreadyExists),
            Err(e) => return Err(e.into()),
        }
        audit(
            shared,
            &ctx.login,
            "account-create",
            format!("{} role={role:?}", req.login),
        );
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::AccountPasswordSet>() {
        account_admins_only!();
        let target = manageable!(&req.login);
        if !password_is_acceptable(&req.password) {
            fail!(ErrorCode::BadRequest);
        }
        let phc = hash_password(&req.password)?;
        AccountsRepo(&shared.pool)
            .update_phc(target.id, &phc)
            .await?;
        sign_out_everywhere(shared, target.id, "password changed").await;
        audit(shared, &ctx.login, "account-password-set", req.login);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::AccountTotpReset>() {
        account_admins_only!();
        let target = manageable!(&req.login);
        if TotpRepo(&shared.pool).get(target.id).await?.is_none() {
            fail!(ErrorCode::NotFound);
        }
        TotpRepo(&shared.pool).remove(target.id).await?;
        audit(shared, &ctx.login, "account-totp-reset", req.login);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if frame.decode::<padm::InviteListRequest>().is_some() {
        account_admins_only!();
        let invites = InvitesRepo(&shared.pool)
            .list(500)
            .await?
            .into_iter()
            .map(|(code, by, expires, used)| padm::InviteEntry::new(code, by, expires, used))
            .collect();
        conn.send(Frame::reply_to(frame, &padm::InviteList::new(invites))?)
            .await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::AuditListRequest>() {
        // The bit has been granted to admins since the permission wave and
        // checked by nothing: the log was reachable from the command line only.
        if !ctx.allows(shared, "admin", Caps::AUDIT_READ) {
            fail!(ErrorCode::Forbidden)
        }
        let entries = AuditRepo(&shared.pool)
            .recent(i64::from(req.limit.clamp(1, 500)))
            .await?
            .into_iter()
            .map(|r| padm::AuditEntry::new(r.at, r.actor, r.action, r.detail))
            .collect();
        conn.send(Frame::reply_to(frame, &padm::AuditList::new(entries))?)
            .await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::InviteRevoke>() {
        account_admins_only!();
        if !InvitesRepo(&shared.pool).revoke(&req.code).await? {
            fail!(ErrorCode::NotFound);
        }
        audit(shared, &ctx.login, "invite-revoke", req.code);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: i64, role: Role) -> Account {
        Account {
            id,
            login: format!("acct{id}"),
            phc: None,
            screen_name: String::new(),
            role: role as u8,
            class_id: None,
            grant_mask: 0,
            revoke_mask: 0,
            disabled: false,
        }
    }

    #[test]
    fn an_operator_acts_below_themselves_and_never_on_themselves() {
        let admin = Standing {
            account_id: 1,
            role: Role::Admin,
        };
        assert!(admin.may_manage(&account(2, Role::User)));
        assert!(admin.may_manage(&account(2, Role::Moderator)));
        assert!(!admin.may_manage(&account(2, Role::Admin)), "a peer");
        assert!(!admin.may_manage(&account(2, Role::Superuser)), "above");
        assert!(!admin.may_manage(&account(1, Role::Admin)), "themselves");

        let root = Standing {
            account_id: 9,
            role: Role::Superuser,
        };
        assert!(root.may_manage(&account(2, Role::Admin)));
        assert!(
            root.may_manage(&account(3, Role::Superuser)),
            "exempt from the ordering"
        );
        assert!(
            !root.may_manage(&account(9, Role::Superuser)),
            "and from nothing else"
        );
    }

    #[test]
    fn nobody_hands_out_more_than_they_have() {
        let admin = Standing {
            account_id: 1,
            role: Role::Admin,
        };
        assert!(admin.may_assign(Role::User));
        assert!(admin.may_assign(Role::Admin), "a co-admin is fine");
        assert!(!admin.may_assign(Role::Superuser));
        let moderator = Standing {
            account_id: 2,
            role: Role::Moderator,
        };
        assert!(!moderator.may_assign(Role::Admin));
    }

    #[test]
    fn a_role_off_the_wire_is_never_clamped_into_one() {
        assert_eq!(role_from_wire(0), Some(Role::Guest));
        assert_eq!(role_from_wire(4), Some(Role::Superuser));
        assert_eq!(role_from_wire(5), None);
        assert_eq!(role_from_wire(255), None);
    }

    #[test]
    fn logins_and_passwords_an_operator_may_set() {
        assert!(login_is_acceptable("alice"));
        assert!(login_is_acceptable("mad-hatter_42"));
        assert!(!login_is_acceptable(""));
        assert!(!login_is_acceptable(" alice"));
        assert!(!login_is_acceptable("al ice"));
        assert!(!login_is_acceptable("al\tice"));
        assert!(!login_is_acceptable(&"x".repeat(33)));
        assert!(password_is_acceptable("12345678"));
        assert!(!password_is_acceptable("1234567"));
        assert!(!password_is_acceptable(&"x".repeat(257)));
    }
}
