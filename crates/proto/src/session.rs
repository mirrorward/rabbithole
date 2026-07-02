//! Session family (0): authentication, keepalive, welcome, agreement.
//!
//! Wave 1. See `docs/protocol/session.md`. Passwords travel in-frame —
//! acceptable only because every transport is TLS; the server hashes with
//! Argon2id and never stores plaintext.
//!
//! **Push replay:** push frames use the frame `id` field as a per-account
//! monotonically increasing sequence number. Clients remember the highest
//! seen (`replay_cursor`) and present it in [`AuthResume`]; the server
//! replays newer pushes from its ring buffer so a reconnect misses nothing.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// Authenticate with login + password. → [`AuthOk`] or error
/// (`Unauthenticated`, `RateLimited`; accounts with TOTP enrolled answer
/// `TotpRequired` until a valid `totp` code — or a recovery code — is
/// supplied).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPassword {
    pub login: String,
    pub password: String,
    /// Current TOTP code or a recovery code, when 2FA is enrolled.
    pub totp: Option<String>,
}

impl AuthPassword {
    pub fn new(login: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            login: login.into(),
            password: password.into(),
            totp: None,
        }
    }

    pub fn with_totp(mut self, code: impl Into<String>) -> Self {
        self.totp = Some(code.into());
        self
    }
}

impl Message for AuthPassword {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 10;
}

/// Sign in as a guest (if the server allows it). → [`AuthOk`] or
/// `Forbidden` when guests are disabled.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthGuest {
    /// Requested display name; the server may adjust it (e.g. "guest-7").
    pub desired_name: Option<String>,
}

impl AuthGuest {
    pub fn new(desired_name: Option<String>) -> Self {
        Self { desired_name }
    }
}

impl Message for AuthGuest {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 11;
}

/// Resume a previous session with its token. → [`AuthOk`] (with `resumed:
/// true`) or `SessionExpired`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResume {
    /// The session token from a previous [`AuthOk`], base64url.
    pub token: String,
    /// Highest push sequence number the client has seen (0 = none).
    pub replay_cursor: u64,
}

impl AuthResume {
    pub fn new(token: impl Into<String>, replay_cursor: u64) -> Self {
        Self {
            token: token.into(),
            replay_cursor,
        }
    }
}

impl Message for AuthResume {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 12;
}

/// Successful authentication reply (for any auth method).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthOk {
    /// Bearer token for [`AuthResume`] (empty for guests — guest sessions
    /// are not resumable).
    pub token: String,
    pub account_id: i64,
    pub screen_name: String,
    /// Role ordinal: 0 guest, 1 user, 2 moderator, 3 admin, 4 superuser.
    pub role: u8,
    /// The session's effective capability bitmask.
    pub caps: u64,
    /// True when this reply answers an [`AuthResume`].
    pub resumed: bool,
}

impl AuthOk {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        token: impl Into<String>,
        account_id: i64,
        screen_name: impl Into<String>,
        role: u8,
        caps: u64,
        resumed: bool,
    ) -> Self {
        Self {
            token: token.into(),
            account_id,
            screen_name: screen_name.into(),
            role,
            caps,
            resumed,
        }
    }
}

impl Message for AuthOk {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 13;
}

/// Keepalive request; the server answers with [`Pong`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Ping;

impl Message for Ping {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 20;
}

/// Keepalive reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Pong;

impl Message for Pong {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 21;
}

/// Accept the server agreement shown in [`Welcome`]. → empty ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgreementAccept;

impl Message for AgreementAccept {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 30;
}

/// Pushed by the server right after successful authentication.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// Message of the day (may be empty).
    pub motd: String,
    /// Agreement text the user must accept before participating
    /// (None = no agreement gate on this server).
    pub agreement: Option<String>,
}

impl Welcome {
    pub fn new(motd: impl Into<String>, agreement: Option<String>) -> Self {
        Self {
            motd: motd.into(),
            agreement,
        }
    }
}

impl Message for Welcome {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 40;
}

/// Push: an operator notice (admin broadcast, shutdown warning, …).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerNotice {
    pub text: String,
    pub from: String,
}

impl ServerNotice {
    pub fn new(text: impl Into<String>, from: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            from: from.into(),
        }
    }
}

impl Message for ServerNotice {
    const FAMILY: Family = Family::SESSION;
    const MESSAGE_TYPE: u16 = 41;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, RequestId};

    #[test]
    fn auth_roundtrips() {
        let req = Frame::request(RequestId(9), &AuthPassword::new("alice", "hunter2")).unwrap();
        let decoded = req.decode::<AuthPassword>().unwrap().unwrap();
        assert_eq!(decoded.login, "alice");

        let ok = AuthOk::new("tok", 42, "Alice", 1, 0b1011, false);
        let reply = Frame::reply_to(&req, &ok).unwrap();
        assert_eq!(reply.decode::<AuthOk>().unwrap().unwrap(), ok);
    }

    #[test]
    fn welcome_push_roundtrips() {
        let push = Frame::push(&Welcome::new("hi", Some("be kind".into()))).unwrap();
        let w = push.decode::<Welcome>().unwrap().unwrap();
        assert_eq!(w.agreement.as_deref(), Some("be kind"));
    }
}
