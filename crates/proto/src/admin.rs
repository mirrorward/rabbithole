//! Remote administration (family 7, Wave 2).
//!
//! Every operation is gated by a capability bit and audited server-side.
//! This family makes any authorized client an admin console — the KDX
//! remote-administration lesson, minus the RAT excesses.

use serde::{Deserialize, Serialize};

use crate::frame::{Family, Message};

/// List permission classes. → [`ClassList`]. Requires `ACCOUNT_ADMIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClassListRequest;

impl Message for ClassListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 1;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassEntry {
    pub name: String,
    pub base_mask: u64,
    pub members: u64,
}

impl ClassEntry {
    pub fn new(name: impl Into<String>, base_mask: u64, members: u64) -> Self {
        Self {
            name: name.into(),
            base_mask,
            members,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClassList {
    pub classes: Vec<ClassEntry>,
}

impl ClassList {
    pub fn new(classes: Vec<ClassEntry>) -> Self {
        Self { classes }
    }
}

impl Message for ClassList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 2;
}

/// Create or update a class's capability mask. Changes apply to every
/// member **immediately** (live inheritance). Requires `ACCOUNT_ADMIN`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassSet {
    pub name: String,
    pub base_mask: u64,
}

impl ClassSet {
    pub fn new(name: impl Into<String>, base_mask: u64) -> Self {
        Self {
            name: name.into(),
            base_mask,
        }
    }
}

impl Message for ClassSet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 3;
}

/// Page through accounts. → [`AccountList`]. Requires `ACCOUNT_ADMIN`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AccountListRequest {
    pub offset: u32,
    pub limit: u32,
}

impl AccountListRequest {
    pub fn new(offset: u32, limit: u32) -> Self {
        Self { offset, limit }
    }
}

impl Message for AccountListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 4;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountEntry {
    pub id: i64,
    pub login: String,
    pub role: u8,
    pub class: Option<String>,
    pub disabled: bool,
}

impl AccountEntry {
    pub fn new(
        id: i64,
        login: impl Into<String>,
        role: u8,
        class: Option<String>,
        disabled: bool,
    ) -> Self {
        Self {
            id,
            login: login.into(),
            role,
            class,
            disabled,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AccountList {
    pub accounts: Vec<AccountEntry>,
    pub total: u64,
}

impl AccountList {
    pub fn new(accounts: Vec<AccountEntry>, total: u64) -> Self {
        Self { accounts, total }
    }
}

impl Message for AccountList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 5;
}

/// Modify an account: any `Some` field is applied. Requires
/// `ACCOUNT_ADMIN`. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSet {
    pub login: String,
    pub role: Option<u8>,
    pub class: Option<String>,
    pub disabled: Option<bool>,
}

impl AccountSet {
    pub fn new(login: impl Into<String>) -> Self {
        Self {
            login: login.into(),
            role: None,
            class: None,
            disabled: None,
        }
    }
}

impl Message for AccountSet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 6;
}

/// Mint an invite code (for invite-mode registration). → [`InviteCode`].
/// Requires `ACCOUNT_ADMIN`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteCreate {
    pub ttl_secs: i64,
}

impl InviteCreate {
    pub fn new(ttl_secs: i64) -> Self {
        Self { ttl_secs }
    }
}

impl Message for InviteCreate {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 7;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteCode {
    pub code: String,
    pub expires_at_unix: i64,
}

impl InviteCode {
    pub fn new(code: impl Into<String>, expires_at_unix: i64) -> Self {
        Self {
            code: code.into(),
            expires_at_unix,
        }
    }
}

impl Message for InviteCode {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 8;
}

/// Broadcast a notice to every connected session. Requires `BROADCAST`.
/// → empty ack; sessions receive [`crate::session::ServerNotice`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Broadcast {
    pub text: String,
}

impl Broadcast {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl Message for Broadcast {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 9;
}

/// Disconnect a session. Requires `USER_KICK`; targets holding
/// `CANNOT_BE_KICKED` answer `Forbidden`. → empty ack.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kick {
    pub session_id: u64,
}

impl Kick {
    pub fn new(session_id: u64) -> Self {
        Self { session_id }
    }
}

impl Message for Kick {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 10;
}

/// Read a config key. → [`ConfigValue`]. Requires `CONFIG_ADMIN`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigGet {
    pub key: String,
}

impl ConfigGet {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl Message for ConfigGet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 11;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigValue {
    pub key: String,
    pub value: String,
}

impl ConfigValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

impl Message for ConfigValue {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 12;
}

/// Set a config key. → [`ConfigApplied`]. Requires `CONFIG_ADMIN`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSet {
    pub key: String,
    pub value: String,
}

impl ConfigSet {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

impl Message for ConfigSet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 13;
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigApplied {
    /// False = saved but needs a restart (listener addresses etc.).
    pub applied_live: bool,
}

impl ConfigApplied {
    pub fn new(applied_live: bool) -> Self {
        Self { applied_live }
    }
}

impl Message for ConfigApplied {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 14;
}
