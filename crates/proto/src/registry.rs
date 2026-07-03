//! The curated wire registry: the single source of truth for every
//! `(family, message_type, name)` triple that exists on the wire today.
//!
//! # Why this exists
//!
//! `(family, message_type)` is the wire's *routing key*. Two messages that
//! accidentally share one are a silent collision: a peer decodes the wrong
//! struct out of a byte-identical frame and either errors or — worse —
//! misinterprets it. Nothing in the type system stops you from typing the
//! same `MESSAGE_TYPE` twice, so this module makes the full assignment table
//! explicit and the [`tests/registry.rs`](../../tests/registry.rs) guard
//! asserts it stays collision-free, complete, and unchanged-by-accident.
//!
//! # The tripwire
//!
//! Every entry below is built from the message type's *own* [`Message`]
//! consts (`<T as Message>::FAMILY` / `MESSAGE_TYPE`) and `stringify!`d
//! name, so renaming or renumbering a const breaks compilation or shifts the
//! golden snapshot. Adding an `impl Message` without adding it here — or
//! removing one — trips [`EXPECTED`] and the golden-file test. That is the
//! deliberate "did you mean to change the wire?" checkpoint: mechanical, not
//! a matter of remembering.
//!
//! See `docs/protocol/versioning.md` for the compatibility policy this
//! registry enforces.

use crate::frame::{Family, Message};

// Imports grouped by family module. An unqualified import here means a
// renamed type stops compiling until this list is updated too.
use crate::admin::{
    AccountList, AccountListRequest, AccountSet, Broadcast, ClassList, ClassListRequest, ClassSet,
    ConfigApplied, ConfigGet, ConfigSet, ConfigValue, DenyHashAdd, DenyHashList,
    DenyHashListRequest, DenyHashRemove, GatewayStatsReply, GatewayStatsRequest, InviteCode,
    InviteCreate, Kick, QuarantineClear, QuarantineSet, ReportAck, ReportCreate, ReportList,
    ReportListRequest, ReportResolve, ThemeBundleClear, ThemeBundleGet, ThemeBundleInfo,
    ThemeBundleSet,
};
use crate::blob::{BlobData, BlobGet, BlobPut, BlobRef};
use crate::board::{
    BoardCreate, BoardCreated, BoardList, BoardListRequest, MarkRead, PostCreate, PostDelete,
    PostEdit, PostPosted, PostReply, ThreadList, ThreadListRequest, ThreadPosts, ThreadRequest,
};
use crate::chat::{
    ChatHistory, ChatHistoryRequest, ChatMessage, ChatSend, RoomCreate, RoomInfoReply, RoomInvite,
    RoomInvited, RoomJoin, RoomKick, RoomKicked, RoomLeave, RoomList, RoomListRequest,
    RoomMemberList, RoomMembersRequest, RoomMute, RoomMuted, RoomSlowMode, RoomSlowModeChanged,
    RoomTopicSet, RoomUnmute,
};
use crate::directory::{DirectoryResults, DirectorySearch, ProfileCard, ProfileGet, UserChanged};
use crate::dm::{
    DmHistory, DmHistoryRequest, DmMarkRead, DmReadReceipt, DmReceived, DmSend, DmSent, DmThreads,
    DmThreadsRequest,
};
use crate::filelib::{
    AliasCreate, AreaCreate, AreaList, AreaListRequest, AreaReply, FileAdded, FileContent,
    FileDownloadRequest, FileUpload, FolderCreate, FolderListRequest, NodeDelete, NodeGet,
    NodeList, NodeReply, RateFile, SearchRequest, SearchResults, SetMetadata,
};
use crate::hello::{Hello, HelloAck};
use crate::persona::{
    KeyEnroll, PersonaCreate, PersonaDelete, PersonaList, PersonaListRequest, PersonaReply,
    PersonaSwitch, PersonaUpdate, RecoveryCodes, Register, TotpDisable, TotpEnrollBegin,
    TotpEnrollConfirm, TotpEnrollInfo,
};
use crate::presence::{
    BlockAdd, BlockRemove, BuddyAdd, BuddyList, BuddyListRequest, BuddyRemove, PresenceSet,
    UserJoined, UserLeft, Who, WhoList,
};
use crate::session::{
    AgreementAccept, AuthGuest, AuthOk, AuthPassword, AuthResume, Ping, Pong, ServerNotice, Welcome,
};
use crate::swarm::{
    AdvertWithdraw, AdvertiseAck, AdvertiseFiles, FindSources, PeerContact, SourceList,
    SourceTicket, SourceTicketRequest,
};
use crate::transfer::{
    FileChunk, FileChunkPut, FileChunkRequest, FolderManifest, FolderManifestRequest,
    TransferAbort, TransferOpen, TransferResume, TransferTicket, UploadFinish,
};
use crate::welcome::{
    KeywordGo, KeywordTarget, ThemeGet, ThemePrefGet, ThemePrefSet, ThemePrefState, ThemeReply,
    WelcomeScreen, WelcomeScreenRequest,
};
use crate::wish::{
    WishCreate, WishList, WishListRequest, WishReply, WishSetStatus, WishUpdated, WishVote,
};

/// One registered wire message type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The message family (namespace).
    pub family: Family,
    /// The type number, unique within `family`.
    pub message_type: u16,
    /// The Rust type name, from `stringify!` on the type itself.
    pub name: &'static str,
}

/// Build [`REGISTRY`] from each type's own [`Message`] consts.
///
/// Referencing `<$ty as Message>::FAMILY`/`MESSAGE_TYPE` (rather than
/// re-typing the numbers) means a renumbered const changes the golden
/// snapshot and a renamed const fails to compile — the registry cannot
/// silently drift from the code.
macro_rules! wire_registry {
    ($($ty:ty),* $(,)?) => {
        /// Every `(family, message_type, name)` triple on the wire today,
        /// one entry per `impl Message`. Kept in sync with the source by the
        /// guard tests in `tests/registry.rs`.
        pub const REGISTRY: &[RegistryEntry] = &[
            $(RegistryEntry {
                family: <$ty as Message>::FAMILY,
                message_type: <$ty as Message>::MESSAGE_TYPE,
                name: stringify!($ty),
            }),*
        ];
    };
}

wire_registry! {
    // ── Family 0: SESSION ────────────────────────────────────────────────
    Hello, HelloAck,
    AuthPassword, AuthGuest, AuthResume, AuthOk, Register,
    Ping, Pong, AgreementAccept, Welcome, ServerNotice,
    WelcomeScreenRequest, WelcomeScreen, ThemeGet, ThemeReply, KeywordGo, KeywordTarget,
    PersonaListRequest, PersonaCreate, PersonaUpdate, PersonaDelete, PersonaSwitch, PersonaList,
    PersonaReply,
    ThemePrefGet, ThemePrefSet, ThemePrefState,
    TotpEnrollBegin, TotpEnrollInfo, TotpEnrollConfirm, RecoveryCodes, TotpDisable, KeyEnroll,

    // ── Family 1: PRESENCE ───────────────────────────────────────────────
    Who, WhoList, UserJoined, UserLeft, UserChanged,
    ProfileGet, ProfileCard, DirectorySearch, DirectoryResults,
    PresenceSet, BuddyListRequest, BuddyList, BuddyAdd, BuddyRemove, BlockAdd, BlockRemove,

    // ── Family 2: CHAT ───────────────────────────────────────────────────
    ChatSend, ChatMessage, ChatHistoryRequest, ChatHistory,
    RoomListRequest, RoomList, RoomCreate, RoomJoin, RoomLeave, RoomInvite, RoomInvited,
    RoomTopicSet, RoomKick, RoomInfoReply, RoomKicked, RoomMembersRequest, RoomMemberList,
    RoomMute, RoomUnmute, RoomSlowMode, RoomMuted, RoomSlowModeChanged,

    // ── Family 3: DM ─────────────────────────────────────────────────────
    DmSend, DmSent, DmReceived, DmHistoryRequest, DmHistory, DmThreadsRequest, DmThreads,
    DmMarkRead, DmReadReceipt,

    // ── Family 4: BOARD ──────────────────────────────────────────────────
    BoardListRequest, BoardList, ThreadListRequest, ThreadList, ThreadRequest, ThreadPosts,
    PostCreate, PostReply, PostEdit, PostDelete, MarkRead, BoardCreate, BoardCreated, PostPosted,

    // ── Family 5: FILE ───────────────────────────────────────────────────
    AreaListRequest, AreaList, FolderListRequest, NodeList, NodeGet, NodeReply, AreaCreate,
    AreaReply, FolderCreate, FileUpload, FileDownloadRequest, FileContent, NodeDelete, SetMetadata,
    SearchRequest, SearchResults, RateFile, AliasCreate, FileAdded,
    TransferOpen, TransferTicket, TransferResume, UploadFinish, TransferAbort,
    FolderManifestRequest, FolderManifest, FileChunkRequest, FileChunk, FileChunkPut,
    BlobPut, BlobRef, BlobGet, BlobData,

    // ── Family 6: SWARM ──────────────────────────────────────────────────
    AdvertiseFiles, AdvertiseAck, AdvertWithdraw, FindSources, SourceList, PeerContact,
    SourceTicketRequest, SourceTicket,

    // ── Family 7: ADMIN ──────────────────────────────────────────────────
    ClassListRequest, ClassList, ClassSet, AccountListRequest, AccountList, AccountSet,
    InviteCreate, InviteCode, Broadcast, Kick, ConfigGet, ConfigValue, ConfigSet, ConfigApplied,
    ReportCreate, ReportAck, ReportListRequest, ReportList, ReportResolve, QuarantineSet,
    QuarantineClear, DenyHashAdd, DenyHashRemove, DenyHashListRequest, DenyHashList, ThemeBundleSet,
    ThemeBundleClear, ThemeBundleGet, ThemeBundleInfo, GatewayStatsRequest, GatewayStatsReply,

    // Family 8 (FEDERATION) and Family 9 (RADIO) are reserved: no native
    // message types exist yet. See docs/protocol/README.md.

    // ── Family 10: WISHING_WELL ──────────────────────────────────────────
    WishListRequest, WishList, WishCreate, WishVote, WishSetStatus, WishReply, WishUpdated,
}

/// The number of message types [`REGISTRY`] is expected to hold.
///
/// This is the completeness tripwire: `tests/registry.rs` asserts
/// `REGISTRY.len() == EXPECTED`. Adding an `impl Message` without registering
/// it, or removing/registering one without updating this count, fails the
/// test on purpose — forcing a conscious "did you mean to change the wire?"
/// acknowledgement rather than a silent drift.
pub const EXPECTED: usize = 174;

/// Human-readable name for a family number, for the golden snapshot.
fn family_label(family: Family) -> &'static str {
    match family {
        Family::SESSION => "SESSION",
        Family::PRESENCE => "PRESENCE",
        Family::CHAT => "CHAT",
        Family::DM => "DM",
        Family::BOARD => "BOARD",
        Family::FILE => "FILE",
        Family::SWARM => "SWARM",
        Family::ADMIN => "ADMIN",
        Family::FEDERATION => "FEDERATION",
        Family::RADIO => "RADIO",
        Family::WISHING_WELL => "WISHING_WELL",
        _ => "?",
    }
}

/// Render [`REGISTRY`] to the canonical sorted text compared against
/// `tests/wire-registry.golden`. Sorted by `(family, message_type)` so the
/// output is independent of declaration order; a diff is an intentional wire
/// change to be re-blessed.
pub fn golden_text() -> String {
    use core::fmt::Write as _;

    let mut rows: Vec<&RegistryEntry> = REGISTRY.iter().collect();
    rows.sort_by_key(|e| (e.family.0, e.message_type));

    let mut out = String::new();
    out.push_str(
        "# RabbitHole Protocol (RHP) wire registry — golden snapshot. DO NOT hand-edit.\n",
    );
    out.push_str(
        "# One line per (family, message_type): <family#> <FAMILY> <type#> <MessageName>\n",
    );
    out.push_str(
        "# Re-bless an *intentional* wire change: BLESS=1 cargo test -p rabbithole-proto --test registry\n",
    );
    let _ = writeln!(out, "# total: {}", rows.len());
    for e in rows {
        let _ = writeln!(
            out,
            "{:>3} {:<12} {:>4}  {}",
            e.family.0,
            family_label(e.family),
            e.message_type,
            e.name
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A quick in-crate smoke test; the authoritative guards live in
    // tests/registry.rs so they exercise the crate's public surface.
    #[test]
    fn registry_is_nonempty_and_counts_match() {
        assert_eq!(REGISTRY.len(), EXPECTED);
        assert!(golden_text().contains("Hello"));
    }
}
