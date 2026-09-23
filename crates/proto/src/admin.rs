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

/// Ask the burrow to describe its own configuration: every key an operator can
/// see, with its value, its default and its shape. → [`ConfigDescription`].
/// Requires `CONFIG_ADMIN`.
///
/// `ConfigGet` answers one key and says nothing about it. A console built on
/// that alone has to guess which keys exist, which are switches, what a
/// choice may be set to and what "back to the default" means, and it guesses
/// for every burrow version at once. The burrow knows all four.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDescribeRequest;

impl Message for ConfigDescribeRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 15;
}

/// The shape of a config value ([`ConfigKeyInfo::kind`]).
pub mod config_kind {
    /// Free text: a name, a host, an address, a path.
    pub const TEXT: u8 = 0;
    /// `true` or `false`.
    pub const BOOL: u8 = 1;
    /// A whole number.
    pub const NUMBER: u8 = 2;
    /// One of [`super::ConfigKeyInfo::choices`].
    pub const CHOICE: u8 = 3;
}

/// Facts about a config key ([`ConfigKeyInfo::flags`]), as a bit set.
pub mod config_flag {
    /// A change takes effect at once. Clear: it is saved, and a restart applies it.
    pub const LIVE: u8 = 1;
    /// A credential. Its value is never sent; see [`SET`].
    pub const SECRET: u8 = 1 << 1;
    /// For a [`SECRET`]: one is stored.
    pub const SET: u8 = 1 << 2;
    /// Shown for reference. The burrow refuses a `ConfigSet` for it.
    pub const READ_ONLY: u8 = 1 << 3;
}

/// One key of a [`ConfigDescription`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigKeyInfo {
    /// The key, as `ConfigGet` and `ConfigSet` spell it.
    pub key: String,
    /// The current value (empty for a secret).
    pub value: String,
    /// What a burrow of this version ships with.
    pub default: String,
    /// One of [`config_kind`].
    pub kind: u8,
    /// A set of [`config_flag`] bits.
    pub flags: u8,
    /// The values a [`config_kind::CHOICE`] accepts; empty otherwise.
    pub choices: Vec<String>,
}

impl ConfigKeyInfo {
    pub fn new(
        key: impl Into<String>,
        value: impl Into<String>,
        default: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            default: default.into(),
            kind: config_kind::TEXT,
            flags: 0,
            choices: Vec::new(),
        }
    }

    pub fn kind(mut self, kind: u8) -> Self {
        self.kind = kind;
        self
    }

    pub fn flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    pub fn choices<I, S>(mut self, choices: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

/// The burrow's configuration, described. Reply to [`ConfigDescribeRequest`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDescription {
    pub entries: Vec<ConfigKeyInfo>,
}

impl ConfigDescription {
    pub fn new(entries: Vec<ConfigKeyInfo>) -> Self {
        Self { entries }
    }
}

impl Message for ConfigDescription {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 16;
}

/// Ask what the burrow's optional surfaces are actually doing.
/// → [`SurfaceStatus`]. Requires `CONFIG_ADMIN`.
///
/// A config key says what was asked for (`nntp_enabled = true`). This says
/// what happened: listening, and where; or not, and why. The two differ
/// exactly when an operator most needs to know (a port already taken).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceStatusRequest;

impl Message for SurfaceStatusRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 17;
}

/// What a surface is doing ([`SurfaceInfo::state`]).
pub mod surface_state {
    /// Not asked for, or stopped.
    pub const OFF: u8 = 0;
    /// Accepting connections on [`super::SurfaceInfo::addr`].
    pub const LISTENING: u8 = 1;
    /// Running, with no address of its own (the feed poller).
    pub const RUNNING: u8 = 2;
    /// Asked for and not running. [`super::SurfaceInfo::detail`] says why.
    pub const FAILED: u8 = 3;
    /// Switched on with nothing to do. `detail` says what is missing.
    pub const IDLE: u8 = 4;
}

/// One surface of a [`SurfaceStatus`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceInfo {
    /// The config key that switches this surface on (`nntp_enabled`): what a
    /// console ties the report to.
    pub key: String,
    /// One of [`surface_state`].
    pub state: u8,
    /// The bound address when listening (`0.0.0.0:1119`); empty otherwise.
    pub addr: String,
    /// Why it failed or what it is waiting for; empty otherwise.
    pub detail: String,
}

impl SurfaceInfo {
    pub fn new(key: impl Into<String>, state: u8) -> Self {
        Self {
            key: key.into(),
            state,
            addr: String::new(),
            detail: String::new(),
        }
    }

    pub fn addr(mut self, addr: impl Into<String>) -> Self {
        self.addr = addr.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// Reply to [`SurfaceStatusRequest`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceStatus {
    pub surfaces: Vec<SurfaceInfo>,
}

impl SurfaceStatus {
    pub fn new(surfaces: Vec<SurfaceInfo>) -> Self {
        Self { surfaces }
    }
}

impl Message for SurfaceStatus {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 18;
}

// ---------------------------------------------------------------------------
// Accounts and invites an operator manages by hand: types 19..24.
//
// The rules every one of them shares (and that `AccountSet` now shares too):
// an operator acts on accounts *below* their own role, never on themselves,
// and never hands out a role above their own. A superuser is exempt from the
// ordering, not from "never on yourself".
// ---------------------------------------------------------------------------

/// Create an account. → empty ack. Requires `ACCOUNT_ADMIN`.
/// `AlreadyExists` when the login (or a persona of that name) is taken,
/// `BadRequest` for a login or password the burrow will not accept,
/// `Forbidden` for a role above the operator's own.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCreate {
    pub login: String,
    pub password: String,
    /// The `Role` ordinal (0 guest .. 4 superuser).
    pub role: u8,
}

impl AccountCreate {
    pub fn new(login: impl Into<String>, password: impl Into<String>, role: u8) -> Self {
        Self {
            login: login.into(),
            password: password.into(),
            role,
        }
    }
}

// A password never reaches a log by way of `{:?}`.
impl std::fmt::Debug for AccountCreate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCreate")
            .field("login", &self.login)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl Message for AccountCreate {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 19;
}

/// Give an account a new password. → empty ack. Requires `ACCOUNT_ADMIN`.
/// Every saved sign-in of that account stops working, so whoever had the old
/// password is out as well as the person who forgot it.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPasswordSet {
    pub login: String,
    pub password: String,
}

impl AccountPasswordSet {
    pub fn new(login: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            login: login.into(),
            password: password.into(),
        }
    }
}

impl std::fmt::Debug for AccountPasswordSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountPasswordSet")
            .field("login", &self.login)
            .finish_non_exhaustive()
    }
}

impl Message for AccountPasswordSet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 20;
}

/// Remove an account's two-factor enrolment, for someone who lost their
/// device and their recovery codes. → empty ack. Requires `ACCOUNT_ADMIN`.
/// `NotFound` when the account has none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountTotpReset {
    pub login: String,
}

impl AccountTotpReset {
    pub fn new(login: impl Into<String>) -> Self {
        Self {
            login: login.into(),
        }
    }
}

impl Message for AccountTotpReset {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 21;
}

/// List invitations. → [`InviteList`]. Requires `ACCOUNT_ADMIN`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteListRequest;

impl Message for InviteListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 22;
}

/// One invitation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteEntry {
    pub code: String,
    /// Login of whoever made it.
    pub created_by: String,
    /// Unix seconds.
    pub expires_at: i64,
    /// Login of whoever used it; `None` while it is still good (or expired).
    pub used_by: Option<String>,
}

impl InviteEntry {
    pub fn new(
        code: impl Into<String>,
        created_by: impl Into<String>,
        expires_at: i64,
        used_by: Option<String>,
    ) -> Self {
        Self {
            code: code.into(),
            created_by: created_by.into(),
            expires_at,
            used_by,
        }
    }
}

/// Reply to [`InviteListRequest`]: newest first.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteList {
    pub invites: Vec<InviteEntry>,
}

impl InviteList {
    pub fn new(invites: Vec<InviteEntry>) -> Self {
        Self { invites }
    }
}

impl Message for InviteList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 23;
}

/// Withdraw an invitation nobody has used. → empty ack. Requires
/// `ACCOUNT_ADMIN`. `NotFound` for an unknown code or one already used (the
/// account it made is a separate matter).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRevoke {
    pub code: String,
}

impl InviteRevoke {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

impl Message for InviteRevoke {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 24;
}

/// Read the audit log: what operators and moderators did here, newest last.
/// → [`AuditList`]. Requires `AUDIT_READ`. `limit` is clamped to 500.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditListRequest {
    pub limit: u32,
}

impl AuditListRequest {
    pub fn new(limit: u32) -> Self {
        Self { limit }
    }
}

impl Message for AuditListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 25;
}

/// One line of the audit log.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unix seconds.
    pub at: i64,
    /// Who did it: a login, or `ctl` for the burrow's command line.
    pub actor: String,
    /// What was done (`config-set`, `account-create`, `kick`, …).
    pub action: String,
    /// The particulars, in the action's own terms. Never a credential.
    pub detail: String,
}

impl AuditEntry {
    pub fn new(
        at: i64,
        actor: impl Into<String>,
        action: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            at,
            actor: actor.into(),
            action: action.into(),
            detail: detail.into(),
        }
    }
}

/// Reply to [`AuditListRequest`]: oldest first, the newest `limit` lines.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditList {
    pub entries: Vec<AuditEntry>,
}

impl AuditList {
    pub fn new(entries: Vec<AuditEntry>) -> Self {
        Self { entries }
    }
}

impl Message for AuditList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 26;
}

// ---------------------------------------------------------------------------
// Federation peers and backups (admin console, slice 7): types 47..60.
// ---------------------------------------------------------------------------

/// A federation peer's lifecycle, as [`PeerEntry::state`] carries it.
pub mod peer_state {
    /// It authenticated once and waits for an operator; no session.
    pub const PENDING: u8 = 0;
    /// Approved, with no live session right now.
    pub const DISCONNECTED: u8 = 1;
    /// Approved and talking.
    pub const CONNECTED: u8 = 2;
}

/// Why an origin's signing key is believed, as [`OriginEntry::trust`]
/// carries it.
pub mod origin_trust {
    /// Proven on a direct, approved peering session.
    pub const DIRECT_PEER: u8 = 0;
    /// An operator pinned it by hand.
    pub const OPERATOR: u8 = 1;
}

/// The burrows this one has met over federation, approved or waiting.
/// → [`PeerList`]. Requires `CONFIG_ADMIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerListRequest;

impl Message for PeerListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 47;
}

/// One federation peer.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerEntry {
    /// Its Ed25519 server identity: what an operator approves or revokes.
    pub key: [u8; 32],
    /// The name it announced, if any.
    pub name: String,
    /// The federation origin bound to the key: announced while pending,
    /// immutable once approved.
    pub origin: Option<String>,
    /// Where it last connected from.
    pub addr: Option<String>,
    /// One of [`peer_state`].
    pub state: u8,
    /// Whether an operator has approved peering with this key.
    pub approved: bool,
    /// Listed under `federation_peers` in the configuration: dialled at
    /// start, approved by that listing, and not revocable from here.
    pub configured: bool,
}

impl PeerEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: [u8; 32],
        name: impl Into<String>,
        origin: Option<String>,
        addr: Option<String>,
        state: u8,
        approved: bool,
        configured: bool,
    ) -> Self {
        Self {
            key,
            name: name.into(),
            origin,
            addr,
            state,
            approved,
            configured,
        }
    }
}

/// Reply to [`PeerListRequest`], sorted by key.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerList {
    pub peers: Vec<PeerEntry>,
}

impl PeerList {
    pub fn new(peers: Vec<PeerEntry>) -> Self {
        Self { peers }
    }
}

impl Message for PeerList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 48;
}

/// Approve a peer: bind its key to an origin, durably. `origin` may be left
/// out when the peer announced one while pending. → empty ack. Requires
/// `CONFIG_ADMIN`. `BadRequest` when no origin is known, when it is not a
/// valid server name, or when the key is already bound to another origin.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerApprove {
    pub key: [u8; 32],
    pub origin: Option<String>,
}

impl PeerApprove {
    pub fn new(key: [u8; 32], origin: Option<String>) -> Self {
        Self { key, origin }
    }
}

impl Message for PeerApprove {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 49;
}

/// Withdraw approval; a live session with the peer is closed and it drops
/// back to pending. → empty ack. Requires `CONFIG_ADMIN`. `BadRequest` for a
/// peer listed under `federation_peers`: take it out of the configuration
/// instead.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRevoke {
    pub key: [u8; 32],
}

impl PeerRevoke {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }
}

impl Message for PeerRevoke {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 50;
}

/// The origins whose signing keys this burrow believes, for posts that
/// arrive relayed. → [`OriginList`]. Requires `CONFIG_ADMIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginListRequest;

impl Message for OriginListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 51;
}

/// One trusted origin: a federation server name and the key it signs with.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginEntry {
    pub origin: String,
    pub key: [u8; 32],
    /// One of [`origin_trust`].
    pub trust: u8,
}

impl OriginEntry {
    pub fn new(origin: impl Into<String>, key: [u8; 32], trust: u8) -> Self {
        Self {
            origin: origin.into(),
            key,
            trust,
        }
    }
}

/// Reply to [`OriginListRequest`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginList {
    pub origins: Vec<OriginEntry>,
}

impl OriginList {
    pub fn new(origins: Vec<OriginEntry>) -> Self {
        Self { origins }
    }
}

impl Message for OriginList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 52;
}

/// Pin an origin's signing key by hand, from a key obtained elsewhere.
/// → empty ack. Requires `CONFIG_ADMIN`. `BadRequest` for an origin that is
/// not a valid server name, or one already bound to a different key.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginPin {
    pub origin: String,
    pub key: [u8; 32],
}

impl OriginPin {
    pub fn new(origin: impl Into<String>, key: [u8; 32]) -> Self {
        Self {
            origin: origin.into(),
            key,
        }
    }
}

impl Message for OriginPin {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 53;
}

/// The snapshots in the burrow's backup folder. → [`BackupList`]. Requires
/// `CONFIG_ADMIN` and the Admin role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupListRequest;

impl Message for BackupListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 54;
}

/// One snapshot, as its manifest describes it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    /// The snapshot's directory name (`snapshot-20260918-224103`).
    pub name: String,
    /// When it was made, RFC 3339 UTC.
    pub created_at: String,
    /// The burrow version that wrote it.
    pub version: String,
    /// How many files it holds, the database included.
    pub files: u64,
    pub total_bytes: u64,
}

impl BackupEntry {
    pub fn new(
        name: impl Into<String>,
        created_at: impl Into<String>,
        version: impl Into<String>,
        files: u64,
        total_bytes: u64,
    ) -> Self {
        Self {
            name: name.into(),
            created_at: created_at.into(),
            version: version.into(),
            files,
            total_bytes,
        }
    }
}

/// Reply to [`BackupListRequest`]: where snapshots go, and the ones there,
/// oldest first.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupList {
    /// The folder, as the burrow resolves it.
    pub dir: String,
    pub snapshots: Vec<BackupEntry>,
}

impl BackupList {
    pub fn new(dir: impl Into<String>, snapshots: Vec<BackupEntry>) -> Self {
        Self {
            dir: dir.into(),
            snapshots,
        }
    }
}

impl Message for BackupList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 55;
}

/// Make a snapshot now, into the burrow's backup folder. → [`BackupMade`].
/// Requires `CONFIG_ADMIN` and the Admin role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupCreate;

impl Message for BackupCreate {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 56;
}

/// Reply to [`BackupCreate`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupMade {
    pub snapshot: BackupEntry,
}

impl BackupMade {
    pub fn new(snapshot: BackupEntry) -> Self {
        Self { snapshot }
    }
}

impl Message for BackupMade {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 57;
}

/// Check a snapshot: every file against its manifest hash, and the database
/// against SQLite's own integrity check. → [`BackupVerified`]. Requires
/// `CONFIG_ADMIN` and the Admin role. `NotFound` for a name that is not a
/// snapshot in the backup folder.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupVerify {
    pub name: String,
}

impl BackupVerify {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Message for BackupVerify {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 58;
}

/// Reply to [`BackupVerify`]. A snapshot that fails is a reply, not an
/// error: `ok` is false and `detail` says what was wrong.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupVerified {
    pub name: String,
    pub ok: bool,
    /// What was found: the integrity check's word on success, the first
    /// mismatch otherwise.
    pub detail: String,
    pub files: u64,
    pub total_bytes: u64,
}

impl BackupVerified {
    pub fn new(
        name: impl Into<String>,
        ok: bool,
        detail: impl Into<String>,
        files: u64,
        total_bytes: u64,
    ) -> Self {
        Self {
            name: name.into(),
            ok,
            detail: detail.into(),
            files,
            total_bytes,
        }
    }
}

impl Message for BackupVerified {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 59;
}

/// Remove a snapshot from the backup folder. → empty ack. Requires
/// `CONFIG_ADMIN` and the Admin role. `NotFound` for a name that is not a
/// snapshot there.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDelete {
    pub name: String,
}

impl BackupDelete {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Message for BackupDelete {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 60;
}

// ---------------------------------------------------------------------------
// Moderation suite (Wave 13): types 30..40 of the ADMIN family.
// ---------------------------------------------------------------------------

/// What a report/quarantine subject reference points at (and the shape of
/// its `subject_ref` bytes).
pub mod subject_kind {
    /// A board post: 32-byte event id.
    pub const POST: u8 = 0;
    /// A direct message: 8-byte little-endian message id.
    pub const DM: u8 = 1;
    /// File content: 32-byte blake3 blob hash.
    pub const FILE: u8 = 2;
    /// An account: 8-byte little-endian account id.
    pub const USER: u8 = 3;
}

/// Report queue states.
pub mod report_state {
    pub const OPEN: u8 = 0;
    pub const REVIEWING: u8 = 1;
    pub const RESOLVED: u8 = 2;
    pub const DISMISSED: u8 = 3;
}

/// Actions a moderator takes on a report via [`ReportResolve`].
pub mod report_action {
    /// Open → reviewing, stamping the claimant.
    pub const CLAIM: u8 = 0;
    /// Open/reviewing → resolved (terminal).
    pub const RESOLVE: u8 = 1;
    /// Open/reviewing → dismissed (terminal).
    pub const DISMISS: u8 = 2;
}

/// File a report about a post/DM/file/user. Any authenticated session
/// (guests included); identical still-open reports by the same reporter on
/// the same subject are deduplicated. → [`ReportAck`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCreate {
    /// One of [`subject_kind`].
    pub subject_kind: u8,
    /// Opaque reference bytes, shaped by `subject_kind`.
    pub subject_ref: Vec<u8>,
    pub reason: String,
}

impl ReportCreate {
    pub fn new(subject_kind: u8, subject_ref: Vec<u8>, reason: impl Into<String>) -> Self {
        Self {
            subject_kind,
            subject_ref,
            reason: reason.into(),
        }
    }
}

impl Message for ReportCreate {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 30;
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportAck {
    pub id: i64,
    /// True when an identical still-open report already existed (its id is
    /// returned instead of a new row's).
    pub deduped: bool,
}

impl ReportAck {
    pub fn new(id: i64, deduped: bool) -> Self {
        Self { id, deduped }
    }
}

impl Message for ReportAck {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 31;
}

/// Page the report queue, optionally filtered by state (oldest first).
/// → [`ReportList`]. Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReportListRequest {
    /// `None` = every state; otherwise one of [`report_state`].
    pub state: Option<u8>,
    pub offset: u32,
    pub limit: u32,
}

impl ReportListRequest {
    pub fn new(state: Option<u8>, offset: u32, limit: u32) -> Self {
        Self {
            state,
            offset,
            limit,
        }
    }
}

impl Message for ReportListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 32;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEntry {
    pub id: i64,
    pub reporter_account: i64,
    pub subject_kind: u8,
    pub subject_ref: Vec<u8>,
    pub reason: String,
    pub created_at_unix: i64,
    pub state: u8,
    /// Moderator login that claimed/closed it; empty = none yet.
    pub resolver: String,
    pub resolved_at_unix: Option<i64>,
    pub resolution: String,
}

impl ReportEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: i64,
        reporter_account: i64,
        subject_kind: u8,
        subject_ref: Vec<u8>,
        reason: impl Into<String>,
        created_at_unix: i64,
        state: u8,
        resolver: impl Into<String>,
        resolved_at_unix: Option<i64>,
        resolution: impl Into<String>,
    ) -> Self {
        Self {
            id,
            reporter_account,
            subject_kind,
            subject_ref,
            reason: reason.into(),
            created_at_unix,
            state,
            resolver: resolver.into(),
            resolved_at_unix,
            resolution: resolution.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReportList {
    pub reports: Vec<ReportEntry>,
    /// Total under the same state filter (for paging).
    pub total: u64,
}

impl ReportList {
    pub fn new(reports: Vec<ReportEntry>, total: u64) -> Self {
        Self { reports, total }
    }
}

impl Message for ReportList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 33;
}

/// Work a report: claim it, resolve it, or dismiss it (one of
/// [`report_action`]). → empty ack. Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportResolve {
    pub id: i64,
    pub action: u8,
    /// Resolution/dismissal note (ignored on claim).
    pub note: String,
}

impl ReportResolve {
    pub fn new(id: i64, action: u8, note: impl Into<String>) -> Self {
        Self {
            id,
            action,
            note: note.into(),
        }
    }
}

impl Message for ReportResolve {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 34;
}

/// Quarantine content pending review: hidden from non-moderators on the
/// read/list paths that consult the quarantine set. Supported kinds:
/// [`subject_kind::POST`] (event id) and [`subject_kind::FILE`] (blob
/// hash). → empty ack. Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineSet {
    pub subject_kind: u8,
    pub subject_ref: Vec<u8>,
    pub reason: String,
}

impl QuarantineSet {
    pub fn new(subject_kind: u8, subject_ref: Vec<u8>, reason: impl Into<String>) -> Self {
        Self {
            subject_kind,
            subject_ref,
            reason: reason.into(),
        }
    }
}

impl Message for QuarantineSet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 35;
}

/// Lift a quarantine. → empty ack. Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineClear {
    pub subject_kind: u8,
    pub subject_ref: Vec<u8>,
}

impl QuarantineClear {
    pub fn new(subject_kind: u8, subject_ref: Vec<u8>) -> Self {
        Self {
            subject_kind,
            subject_ref,
        }
    }
}

impl Message for QuarantineClear {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 36;
}

/// Ask what is being held back for review, a page at a time (a burrow that
/// has held back a spam run holds thousands). → [`QuarantineList`].
/// Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineListRequest {
    pub offset: u32,
    /// Clamped by the burrow to 1..=200.
    pub limit: u32,
}

impl QuarantineListRequest {
    pub fn new(offset: u32, limit: u32) -> Self {
        Self { offset, limit }
    }
}

impl Message for QuarantineListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 61;
}

/// One thing held back for review: what it is, why, who held it, and when.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldItem {
    /// One of [`subject_kind`].
    pub subject_kind: u8,
    pub subject_ref: Vec<u8>,
    pub reason: String,
    /// The moderator who held it back; empty when it was not recorded.
    pub held_by: String,
    pub at_unix: i64,
}

impl HeldItem {
    pub fn new(
        subject_kind: u8,
        subject_ref: Vec<u8>,
        reason: impl Into<String>,
        held_by: impl Into<String>,
        at_unix: i64,
    ) -> Self {
        Self {
            subject_kind,
            subject_ref,
            reason: reason.into(),
            held_by: held_by.into(),
            at_unix,
        }
    }
}

/// A page of what is held back, oldest first, and how many there are in
/// all.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineList {
    pub held: Vec<HeldItem>,
    pub total: u64,
}

impl QuarantineList {
    pub fn new(held: Vec<HeldItem>, total: u64) -> Self {
        Self { held, total }
    }
}

impl Message for QuarantineList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 62;
}

/// Add a blake3 hash to the deny list: content with this hash is refused at
/// upload finalize and attachment send. → empty ack. Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyHashAdd {
    pub hash: [u8; 32],
    pub reason: String,
}

impl DenyHashAdd {
    pub fn new(hash: [u8; 32], reason: impl Into<String>) -> Self {
        Self {
            hash,
            reason: reason.into(),
        }
    }
}

impl Message for DenyHashAdd {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 37;
}

/// Remove a hash from the deny list. → empty ack. Requires `MODERATE`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyHashRemove {
    pub hash: [u8; 32],
}

impl DenyHashRemove {
    pub fn new(hash: [u8; 32]) -> Self {
        Self { hash }
    }
}

impl Message for DenyHashRemove {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 38;
}

/// List the deny list. → [`DenyHashList`]. Requires `MODERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DenyHashListRequest;

impl Message for DenyHashListRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 39;
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyHashEntry {
    pub hash: [u8; 32],
    pub reason: String,
    pub added_by: String,
    pub created_at_unix: i64,
}

impl DenyHashEntry {
    pub fn new(
        hash: [u8; 32],
        reason: impl Into<String>,
        added_by: impl Into<String>,
        created_at_unix: i64,
    ) -> Self {
        Self {
            hash,
            reason: reason.into(),
            added_by: added_by.into(),
            created_at_unix,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DenyHashList {
    pub entries: Vec<DenyHashEntry>,
}

impl DenyHashList {
    pub fn new(entries: Vec<DenyHashEntry>) -> Self {
        Self { entries }
    }
}

impl Message for DenyHashList {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 40;
}

// ---------------------------------------------------------------------------
// Server theme-bundle application (Wave 8): types 41..44 of the ADMIN
// family. Gated on `CONFIG_ADMIN` (theming is server configuration) and
// audited server-side.
// ---------------------------------------------------------------------------

/// Upload and activate a theme bundle. `bundle` is a postcard-encoded
/// [`crate::welcome::ThemeBundle`] — the exact bytes a
/// [`crate::welcome::ThemeReply`] would carry (art travels as blob refs
/// uploaded via `BlobPut` first, matching v1). `signature`, when
/// non-empty, must be a valid Ed25519 signature over `bundle` by the
/// server identity key (the re-import path for a previously served
/// bundle); empty means the server signs at serve time as usual.
///
/// The server validates before applying — structured tokens only, WCAG
/// contrast rails (≥ 4.5:1 text-on-bg and accent-on-bg per mode), blob
/// size caps — and **rejects** anything below the bar. →
/// [`ThemeBundleInfo`] on success. Requires `CONFIG_ADMIN`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeBundleSet {
    pub bundle: Vec<u8>,
    pub signature: Vec<u8>,
}

impl ThemeBundleSet {
    pub fn new(bundle: Vec<u8>, signature: Vec<u8>) -> Self {
        Self { bundle, signature }
    }
}

impl Message for ThemeBundleSet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 41;
}

/// Clear the applied theme bundle: every client falls back to default
/// tokens on its next fetch. → empty ack. Requires `CONFIG_ADMIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemeBundleClear;

impl Message for ThemeBundleClear {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 42;
}

/// Inspect the currently applied theme bundle. → [`ThemeBundleInfo`].
/// Requires `CONFIG_ADMIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemeBundleGet;

impl Message for ThemeBundleGet {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 43;
}

/// Summary of the applied theme bundle (all-default when `present` is
/// false).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThemeBundleInfo {
    pub present: bool,
    /// blake3 of the canonical bundle bytes (zeroes when absent) — the
    /// same id clients cache [`crate::welcome::ThemeReply`] payloads by.
    pub id: [u8; 32],
    pub name: String,
    pub applied_at_unix: i64,
    /// Login that applied it ("ctl" for the local socket; empty = none).
    pub applied_by: String,
    pub accent_rgb: Option<[u8; 3]>,
    pub has_logo: bool,
    pub has_banner: bool,
    /// Token summary: icon overrides and per-map token counts.
    pub icons: u32,
    pub tokens_light: u32,
    pub tokens_dark: u32,
    pub tokens_shared: u32,
}

impl Message for ThemeBundleInfo {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 44;
}

/// Ask for a live snapshot of the syndication + legacy-gateway counters
/// (Wave 10). → [`GatewayStatsReply`]. Requires `CONFIG_ADMIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GatewayStatsRequest;

impl Message for GatewayStatsRequest {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 45;
}

/// Per-feed syndication statistics: the transient outcome the fetcher
/// already computes, surfaced so the web/CLI admin can render the feed
/// monitor.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeedStat {
    /// Feed URL (the row key).
    pub url: String,
    /// Last poll time, unix millis (0 = never polled this run).
    pub last_poll_ms: i64,
    /// `"ok"` | `"not_modified"` | `"error"` | `""` (never polled).
    pub last_status: String,
    /// Items encountered across polls (fresh, pre-dedupe).
    pub items_seen: u64,
    /// Items actually posted to the mapped board.
    pub items_posted: u64,
    /// Items dropped because the shared dedupe gate had already seen them.
    pub dupes_dropped: u64,
}

/// One legacy-gateway's counters. `counters` is string-keyed so the set
/// can grow without a protocol bump — clients render whatever keys arrive.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GatewayStat {
    /// Gateway name, e.g. `"nntp"`, `"hotline"`, `"radio"`.
    pub name: String,
    /// Whether the surface is enabled in config right now.
    pub enabled: bool,
    /// `(counter-name, value)` pairs, sorted by name.
    pub counters: Vec<(String, u64)>,
}

/// A point-in-time snapshot of all gateway/feed activity counters.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GatewayStatsReply {
    /// When the snapshot was taken, unix millis.
    pub generated_at_ms: i64,
    pub feeds: Vec<FeedStat>,
    pub gateways: Vec<GatewayStat>,
}

impl Message for GatewayStatsReply {
    const FAMILY: Family = Family::ADMIN;
    const MESSAGE_TYPE: u16 = 46;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_admin_messages_roundtrip() {
        let set = ThemeBundleSet::new(vec![1, 2, 3], vec![]);
        let bytes = postcard::to_allocvec(&set).unwrap();
        assert_eq!(postcard::from_bytes::<ThemeBundleSet>(&bytes).unwrap(), set);

        let info = ThemeBundleInfo {
            present: true,
            id: [5; 32],
            name: "Wonderland".into(),
            applied_at_unix: 12345,
            applied_by: "root".into(),
            accent_rgb: Some([0x2b, 0x63, 0xd8]),
            has_logo: false,
            has_banner: true,
            icons: 2,
            tokens_light: 3,
            tokens_dark: 3,
            tokens_shared: 1,
        };
        let bytes = postcard::to_allocvec(&info).unwrap();
        assert_eq!(
            postcard::from_bytes::<ThemeBundleInfo>(&bytes).unwrap(),
            info
        );
    }

    #[test]
    fn gateway_stats_reply_roundtrips() {
        let reply = GatewayStatsReply {
            generated_at_ms: 1_700_000_000_000,
            feeds: vec![FeedStat {
                url: "https://example.org/feed.xml".into(),
                last_poll_ms: 1_700_000_000_000,
                last_status: "ok".into(),
                items_seen: 12,
                items_posted: 9,
                dupes_dropped: 3,
            }],
            gateways: vec![GatewayStat {
                name: "nntp".into(),
                enabled: true,
                counters: vec![("posts".into(), 4), ("sessions".into(), 7)],
            }],
        };
        let bytes = postcard::to_allocvec(&reply).unwrap();
        assert_eq!(
            postcard::from_bytes::<GatewayStatsReply>(&bytes).unwrap(),
            reply
        );
        assert_eq!(GatewayStatsRequest::MESSAGE_TYPE, 45);
        assert_eq!(GatewayStatsReply::MESSAGE_TYPE, 46);
    }
}
