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

// ---------------------------------------------------------------------------
// Moderation suite (Wave 13): types 30..49 of the ADMIN family.
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
