//! QWK offline mail (Wave 10): outbound packet build + `.REP` reply ingest,
//! wiring the pure `rabbithole-legacy-qwk` codec into burrow's boards, read
//! pointers, RBAC, and the shared dedup subsystem.
//!
//! Opt-in via config (`qwk_enabled`, default **off**), gating both surfaces:
//! the telnet `qwk` command (see [`crate::telnet`]) and the ctl admin
//! commands `qwk-build <login>` / `qwk-ingest <login> <path>` (see
//! [`crate::ctl`]).
//!
//! # Conference numbering
//!
//! QWK conferences are small integers; boards are slugs. The mapping is
//! **stable and derivable**: every postable board (`kind == 2`), sorted by
//! slug, numbered `1..=N` (capped at [`MAX_CONFERENCES`] because the `.NDX`
//! conference byte is a `u8`). The full `number → slug` mapping is written
//! into `CONTROL.DAT`'s conference list (the name *is* the slug), so a reader
//! — and the `.REP` ingest below — can always translate back. Creating or
//! deleting boards renumbers later conferences; the packet and the reply are
//! interpreted against the *current* board set, which is the classic QWK-door
//! behavior.
//!
//! # What goes in a packet
//!
//! Per conference: every post **newer than the caller's read pointer** (the
//! same per-account/per-board `read_marks` high-water mark Wave 3's native
//! offline mode uses), oldest first, article numbers matching the NNTP
//! gateway's `(created_at, event_id)` numbering. Caps keep packets bounded:
//! at most [`PER_CONFERENCE_CAP`] messages per conference and [`TOTAL_CAP`]
//! per packet; because messages are taken oldest-first and the pointer only
//! advances over what was actually packed, a capped build simply resumes on
//! the next build — nothing is skipped. Tombstoned posts are treated as read
//! but never packed. After a successful build the read pointers advance to
//! each conference's packed high-water mark, so the next packet contains only
//! newer mail (shared with the native unread counters).
//!
//! # Delivery: raw members, no ZIP
//!
//! The codec crate documents ZIP bundling as out of scope, so the build
//! writes the **raw packet members** (`MESSAGES.DAT`, `CONTROL.DAT`,
//! `DOOR.ID`, per-conference `NNN.NDX`) into a per-user spool directory
//! (`<qwk_spool_dir>/<login>/`, wiped and rebuilt each time). The telnet
//! surface mints one `files_http_base` handoff link per member (link-minting
//! only — the same Wave 6 discipline the file browser used before Wave 8
//! served its links); real `.QWK` ZIP bundling, an HTTP route serving the
//! spool, and a zmodem transfer path are documented follow-ups.
//!
//! # `.REP` ingest
//!
//! `qwk-ingest` parses the already-unzipped `<BBSID>.MSG` member with
//! [`ReplyPacket::parse`], validates each reply against the known conference
//! numbers ([`rabbithole_legacy_qwk::reply::check`]), and posts the accepted
//! ones through [`BoardService`](rabbithole_server_core::BoardService) **as
//! the uploading user**: their own author seed (the identical
//! `rabbithole-author-seed-v1` derivation the native, NNTP, and Hotline
//! surfaces use — the user authored the reply, unlike the FTN/syndication
//! gateway seeds) and `BOARD_POST` checked per target board
//! (`board/<slug>`). A reply's `reference` field is resolved against the
//! board's article numbering to thread under its parent when possible.
//! Dedupe uses the reply's blake3 [`content_hash`] in the shared
//! [`DedupStore`](rabbithole_server_core::DedupStore) under the new
//! [`SeenKey::QwkReply`] namespace — content-addressed, so the QWK
//! `{conference, number}` identity key (which readers fill unreliably) is not
//! trusted. Like the other gateways the seen set is in-memory and
//! time-windowed; durable cross-restart dedupe is a follow-up.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rabbithole_legacy_qwk::reply::{check, content_hash, ReplyProblem};
use rabbithole_legacy_qwk::{build_packet, ControlDat, QwkMessage, ReplyPacket};
use rabbithole_server_core::{Caps, Role, SeenKey, ServerEvent, Subject};
use rabbithole_store_server::repo::Account;
use rabbithole_store_server::repo4::{BoardRow, ReadMarksRepo};

use crate::nntp::group_articles;
use crate::Shared;

/// Most messages packed per conference per build (classic mail doors bound
/// packets the same way). Oldest-first + pointer advance means a capped
/// build resumes where it stopped.
pub const PER_CONFERENCE_CAP: usize = 200;

/// Most messages packed per packet across all conferences.
pub const TOTAL_CAP: usize = 1000;

/// Most boards exposed as conferences: the `.NDX` conference byte is a `u8`,
/// so boards beyond the first 255 (by slug order) are not packed.
pub const MAX_CONFERENCES: usize = 255;

/// Why a QWK operation was refused. Plain enum (burrow carries no derive
/// crate for errors); telnet matches on the variants to phrase refusals.
#[derive(Debug)]
pub enum QwkGateError {
    /// `qwk_enabled` is off (checked here so both surfaces share the gate).
    Disabled,
    /// The account lacks the needed board capability.
    Forbidden,
    /// `.REP` bytes that don't parse.
    Packet(rabbithole_legacy_qwk::QwkError),
    /// Store/board/IO trouble, stringly for display.
    Internal(String),
}

impl std::fmt::Display for QwkGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QwkGateError::Disabled => write!(f, "QWK offline mail is not enabled on this system"),
            QwkGateError::Forbidden => write!(f, "not permitted"),
            QwkGateError::Packet(e) => write!(f, "bad packet: {e}"),
            QwkGateError::Internal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for QwkGateError {}

impl From<rabbithole_server_core::BoardError> for QwkGateError {
    fn from(e: rabbithole_server_core::BoardError) -> Self {
        QwkGateError::Internal(format!("board: {e}"))
    }
}

impl From<rabbithole_store_server::StoreError> for QwkGateError {
    fn from(e: rabbithole_store_server::StoreError) -> Self {
        QwkGateError::Internal(format!("store: {e}"))
    }
}

impl From<std::io::Error> for QwkGateError {
    fn from(e: std::io::Error) -> Self {
        QwkGateError::Internal(format!("io: {e}"))
    }
}

/// One written packet member: canonical filename, size, spool path.
#[derive(Debug, Clone)]
pub struct QwkMember {
    pub name: String,
    pub size: u64,
    pub path: PathBuf,
}

/// The result of a packet build.
#[derive(Debug, Clone)]
pub struct QwkBuild {
    /// The per-user spool directory the members were written into.
    pub spool_dir: PathBuf,
    /// Every member, in the packet's stable order.
    pub members: Vec<QwkMember>,
    /// Messages packed (matches `CONTROL.DAT`'s total).
    pub total_messages: usize,
    /// The `conference number → board slug` mapping used.
    pub conferences: Vec<(u16, String)>,
}

/// The outcome of a `.REP` ingest.
#[derive(Debug, Clone, Default)]
pub struct RepReport {
    /// Replies posted to boards.
    pub accepted: usize,
    /// Replies dropped as already-seen content (re-upload / in-batch repeat).
    pub duplicates: usize,
    /// Replies refused, as `(subject, reason)` pairs.
    pub rejected: Vec<(String, String)>,
}

/// The stable `conference number → postable board` mapping: boards with
/// `kind == 2`, sorted by slug, numbered `1..=N`, capped at
/// [`MAX_CONFERENCES`].
pub async fn conferences(shared: &Shared) -> Result<Vec<(u16, BoardRow)>, QwkGateError> {
    let mut boards: Vec<BoardRow> = shared
        .boards
        .boards()
        .await?
        .into_iter()
        .filter(|b| b.kind == 2)
        .collect();
    boards.sort_by(|a, b| a.slug.cmp(&b.slug));
    boards.truncate(MAX_CONFERENCES);
    Ok(boards
        .into_iter()
        .enumerate()
        .map(|(i, b)| ((i + 1) as u16, b))
        .collect())
}

/// Build a QWK packet for `account` and write its members into the spool.
///
/// Gated on `qwk_enabled` and `BOARD_READ`; boards the account can't read
/// (`board/<slug>` RBAC) stay in the conference list (numbering must be
/// stable for everyone) but contribute no messages. On success the account's
/// read pointers advance to each conference's packed high-water mark.
pub async fn build_for(shared: &Shared, account: &Account) -> Result<QwkBuild, QwkGateError> {
    let cfg = shared.config.read();
    if !cfg.qwk_enabled {
        return Err(QwkGateError::Disabled);
    }
    let subject = subject_for(shared, account);
    if !shared.perms.allows(&subject, "board", Caps::BOARD_READ) {
        return Err(QwkGateError::Forbidden);
    }

    let confs = conferences(shared).await?;
    let marks = ReadMarksRepo(&shared.pool);
    let mut messages: Vec<QwkMessage> = Vec::new();
    // Per-board packed high-water marks, applied only after the spool write
    // succeeds (a failed build must not eat mail).
    let mut high_water: Vec<(String, i64)> = Vec::new();
    for (num, board) in &confs {
        if messages.len() >= TOTAL_CAP {
            break;
        }
        if !shared
            .perms
            .allows(&subject, &format!("board/{}", board.slug), Caps::BOARD_READ)
        {
            continue; // unreadable boards contribute nothing
        }
        let mark = marks.get(account.id, &board.slug).await?;
        let arts = group_articles(shared, &board.slug)
            .await
            .map_err(|e| QwkGateError::Internal(format!("read {}: {e}", board.slug)))?;
        let mut taken = 0usize;
        let mut last_ms = mark;
        for (idx, post) in arts.iter().enumerate() {
            if post.created_at <= mark {
                continue;
            }
            if taken >= PER_CONFERENCE_CAP || messages.len() >= TOTAL_CAP {
                break;
            }
            if post.tombstoned {
                last_ms = post.created_at; // read past it, never pack it
                continue;
            }
            // Thread linkage: the parent's article number, when packed state
            // still knows it.
            let reference = post
                .parent_id
                .and_then(|pid| arts.iter().position(|p| p.event_id == pid))
                .map(|i| (i + 1) as u32)
                .unwrap_or(0);
            let (date, time) = qwk_stamp(post.created_at);
            messages.push(QwkMessage {
                status: b' ',
                number: (idx + 1) as u32,
                conference: *num,
                date,
                time,
                to: "ALL".into(),
                from: post.author.clone(),
                subject: post.subject.clone(),
                password: String::new(),
                reference,
                active: true,
                body: post.body.clone(),
            });
            taken += 1;
            last_ms = post.created_at;
        }
        if last_ms > mark {
            high_water.push((board.slug.clone(), last_ms));
        }
    }
    let total_messages = messages.len();

    let control = ControlDat {
        bbs_name: cfg.name.clone(),
        city_state: String::new(),
        phone: String::new(),
        sysop: String::new(),
        serial: "0".into(),
        bbs_id: bbs_id(&cfg.name),
        date: chrono::Utc::now().format("%m-%d-%Y,%H:%M:%S").to_string(),
        username: account.login.to_ascii_uppercase(),
        total_messages: 0, // recomputed by build_packet
        conferences: confs.iter().map(|(n, b)| (*n, b.slug.clone())).collect(),
        files: Vec::new(),
    };
    let packet = build_packet(control, messages, None);

    // Spool the members: <qwk_spool_dir>/<login>/, wiped per build so stale
    // members from a previous (larger) packet never linger.
    let spool_root = crate::resolve_dir(&cfg.data_dir, &cfg.qwk_spool_dir);
    let dir = spool_root.join(spool_component(&account.login));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut members = Vec::new();
    for (name, bytes) in packet.members() {
        let path = dir.join(name);
        std::fs::write(&path, bytes)?;
        members.push(QwkMember {
            name: name.to_string(),
            size: bytes.len() as u64,
            path,
        });
    }

    // The packet is on disk: advance the shared read pointers.
    for (slug, ms) in &high_water {
        marks.set(account.id, slug, *ms).await?;
    }

    Ok(QwkBuild {
        spool_dir: dir,
        members,
        total_messages,
        conferences: confs.into_iter().map(|(n, b)| (n, b.slug)).collect(),
    })
}

/// Ingest an already-unzipped `.REP` `<BBSID>.MSG` member uploaded by
/// `account`, posting accepted replies as that user. See the module docs for
/// the validation / RBAC / dedupe rules.
pub async fn ingest_rep_for(
    shared: &Shared,
    account: &Account,
    bytes: &[u8],
) -> Result<RepReport, QwkGateError> {
    if !shared.config.read().qwk_enabled {
        return Err(QwkGateError::Disabled);
    }
    let subject = subject_for(shared, account);
    if !shared.perms.allows(&subject, "board", Caps::BOARD_POST) {
        return Err(QwkGateError::Forbidden);
    }
    let packet = ReplyPacket::parse(bytes).map_err(QwkGateError::Packet)?;
    let confs = conferences(shared).await?;
    let by_num: HashMap<u16, &BoardRow> = confs.iter().map(|(n, b)| (*n, b)).collect();
    let valid: HashSet<u16> = by_num.keys().copied().collect();

    let origin = shared.origin_name();
    let author = format!("{}@{origin}", account.screen_name);
    let seed = author_seed(shared, account.id);
    let mut report = RepReport::default();
    for reply in packet.replies {
        let problems = check(&reply, &valid);
        if !problems.is_empty() {
            report
                .rejected
                .push((reply.subject.clone(), problems_text(&problems)));
            continue;
        }
        let digest = content_hash(&reply);
        let key = SeenKey::QwkReply(digest);
        if shared.dedup.seen(&key) {
            report.duplicates += 1;
            continue;
        }
        let board = by_num[&reply.conference];
        if !shared
            .perms
            .allows(&subject, &format!("board/{}", board.slug), Caps::BOARD_POST)
        {
            report
                .rejected
                .push((reply.subject.clone(), "not permitted on that board".into()));
            continue;
        }
        // `reference` is the parent's article number in this board's stable
        // numbering; unresolvable references post as top-level threads.
        let parent = if reply.reference > 0 {
            group_articles(shared, &board.slug)
                .await
                .ok()
                .and_then(|arts| arts.get(reply.reference as usize - 1).map(|p| p.event_id))
        } else {
            None
        };
        let now = chrono::Utc::now().timestamp_millis();
        match shared
            .boards
            .post(
                &board.slug,
                parent,
                &author,
                &seed,
                &reply.subject,
                &reply.body,
                "text/plain",
                now,
            )
            .await
        {
            Ok(row) => {
                // Record only what actually posted, so an RBAC/board refusal
                // today doesn't shadow a legitimate retry tomorrow. This also
                // catches an in-batch repeat: the second copy sees the key.
                shared.dedup.check_and_record(key, now);
                shared.bus.publish(ServerEvent::BoardPost {
                    board: row.board_slug.clone(),
                    id: row.event_id,
                    root: row.root_id,
                });
                report.accepted += 1;
            }
            Err(e) => report.rejected.push((reply.subject.clone(), e.to_string())),
        }
    }
    Ok(report)
}

/// The account's permission subject with the **current** class mask — the
/// same layering a live session uses (see `SessionCtx::subject`).
fn subject_for(shared: &Shared, account: &Account) -> Subject {
    Subject {
        account_id: account.id,
        role: Role::from_ordinal(account.role),
        class_id: account.class_id,
        class_mask: shared.classes.mask(account.class_id),
        grant_mask: account.grant_mask,
        revoke_mask: account.revoke_mask,
    }
}

/// A stable per-account author signing seed — identical derivation to the
/// native/NNTP/Hotline board handlers, so a QWK-authored reply is
/// indistinguishable from a natively-authored post.
fn author_seed(shared: &Shared, account_id: i64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(&account_id.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// QWK `MM-DD-YY` / `HH:MM` stamps (UTC) from unix milliseconds.
fn qwk_stamp(unix_ms: i64) -> (String, String) {
    let dt = chrono::DateTime::from_timestamp_millis(unix_ms).unwrap_or_default();
    (
        dt.format("%m-%d-%y").to_string(),
        dt.format("%H:%M").to_string(),
    )
}

/// The BBS id written to `CONTROL.DAT`: the server name's alphanumerics,
/// uppercased, at most 8 (the classic length) — `"BURROW"` when nothing
/// survives.
fn bbs_id(name: &str) -> String {
    let id: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if id.is_empty() {
        "BURROW".into()
    } else {
        id
    }
}

/// A login as a safe single spool path component: anything outside
/// `[A-Za-z0-9._-]` becomes `_`, and a component that is all dots (or empty)
/// is replaced outright — no traversal, no hidden aliasing.
fn spool_component(login: &str) -> String {
    let s: String = login
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "_".into()
    } else {
        s
    }
}

/// Human-readable reasons for a rejected reply (ctl JSON / logs).
fn problems_text(problems: &[ReplyProblem]) -> String {
    problems
        .iter()
        .map(|p| match p {
            ReplyProblem::ConferenceOutOfRange { conference } => {
                format!("unknown conference {conference}")
            }
            ReplyProblem::EmptyBody => "empty body".to_string(),
            ReplyProblem::MalformedHeader { reason } => format!("malformed header: {reason}"),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbs_id_uppercases_and_bounds() {
        assert_eq!(bbs_id("RabbitHole BBS"), "RABBITHO");
        assert_eq!(bbs_id("The Warren"), "THEWARRE");
        assert_eq!(bbs_id("w9"), "W9");
        assert_eq!(bbs_id("!!! ***"), "BURROW");
        assert_eq!(bbs_id(""), "BURROW");
    }

    #[test]
    fn spool_component_never_traverses() {
        assert_eq!(spool_component("alice"), "alice");
        assert_eq!(spool_component("a/b\\c"), "a_b_c");
        assert_eq!(spool_component(".."), "_");
        assert_eq!(spool_component("."), "_");
        assert_eq!(spool_component(""), "_");
        assert_eq!(spool_component("mad.hatter-42_x"), "mad.hatter-42_x");
    }

    #[test]
    fn qwk_stamp_shape() {
        // 2026-07-02 13:45 UTC.
        let (date, time) = qwk_stamp(1_782_999_900_000);
        assert_eq!(date.len(), 8, "{date}");
        assert_eq!(&date[2..3], "-");
        assert_eq!(time.len(), 5, "{time}");
        assert_eq!(&time[2..3], ":");
    }

    #[test]
    fn problems_text_reads_well() {
        let text = problems_text(&[
            ReplyProblem::ConferenceOutOfRange { conference: 99 },
            ReplyProblem::EmptyBody,
            ReplyProblem::MalformedHeader {
                reason: "empty recipient",
            },
        ]);
        assert_eq!(
            text,
            "unknown conference 99; empty body; malformed header: empty recipient"
        );
    }
}
