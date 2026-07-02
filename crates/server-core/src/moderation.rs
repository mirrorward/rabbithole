//! The moderation suite (Wave 13): report queues, quarantine-for-review,
//! and the blake3 hash-deny list.
//!
//! - **Reports**: any authenticated principal may file one against a post,
//!   DM, file, or user; identical still-open reports by the same reporter on
//!   the same subject are deduplicated. Moderators (the [`Caps::MODERATE`]
//!   bit) list the queue and claim/resolve/dismiss entries.
//! - **Quarantine**: a post (by event id) or file content (by blob hash)
//!   placed under review is hidden from non-moderators on the read/list
//!   paths that consult [`ModerationService::is_quarantined`].
//! - **Hash-deny**: [`ModerationService::is_denied`] is consulted at
//!   file-upload finalize and attachment send; denied content is refused.
//!
//! Quarantine and deny sets are kept as in-memory mirrors of their tables
//! (warmed by [`ModerationService::load`]) so the hot read paths pay one
//! lock + hash lookup, never a database round trip. Every mutation is
//! written through to the store and recorded in the audit log.
//!
//! [`Caps::MODERATE`]: crate::permissions::Caps::MODERATE

use std::collections::HashSet;

use parking_lot::RwLock;
use rabbithole_proto::admin::{report_action, subject_kind};
use rabbithole_store_server::repo::AuditRepo;
use rabbithole_store_server::repo7::{
    DenyHashRow, DenyHashesRepo, QuarantineRepo, ReportRow, ReportsRepo, REPORT_DISMISSED,
    REPORT_OPEN, REPORT_RESOLVED, REPORT_REVIEWING,
};
use rabbithole_store_server::{SqlitePool, StoreError};

/// Longest accepted report reason / resolution note.
const MAX_NOTE_LEN: usize = 1024;

/// Longest accepted subject reference (the defined kinds use 8 or 32 bytes).
const MAX_REF_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ModerationError {
    #[error("bad subject (unknown kind or malformed reference)")]
    BadSubject,
    #[error("empty or oversized text")]
    BadText,
    #[error("no such report")]
    NoSuchReport,
    #[error("report is not in a state that allows this action")]
    BadState,
    #[error("unknown action")]
    BadAction,
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// The moderation domain service. One instance lives in the server's shared
/// state; construction is cheap, [`load`](Self::load) warms the in-memory
/// quarantine/deny mirrors from the store.
pub struct ModerationService {
    pool: SqlitePool,
    /// Mirror of the `quarantine` table: `(subject_kind, subject_ref)`.
    quarantine: RwLock<HashSet<(u8, Vec<u8>)>>,
    /// Mirror of the `deny_hashes` table.
    deny: RwLock<HashSet<[u8; 32]>>,
}

/// Validate a `(kind, ref)` pair: known kind, byte shape matching the kind.
fn check_subject(kind: u8, subject_ref: &[u8]) -> Result<(), ModerationError> {
    let ok = match kind {
        subject_kind::POST | subject_kind::FILE => subject_ref.len() == 32,
        subject_kind::DM | subject_kind::USER => subject_ref.len() == 8,
        _ => false,
    };
    (ok && subject_ref.len() <= MAX_REF_LEN)
        .then_some(())
        .ok_or(ModerationError::BadSubject)
}

fn check_note(text: &str) -> Result<&str, ModerationError> {
    let text = text.trim();
    if text.len() > MAX_NOTE_LEN {
        return Err(ModerationError::BadText);
    }
    Ok(text)
}

impl ModerationService {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            quarantine: RwLock::new(HashSet::new()),
            deny: RwLock::new(HashSet::new()),
        }
    }

    /// Warm the in-memory quarantine/deny mirrors from the store (boot).
    pub async fn load(&self) -> Result<(), ModerationError> {
        let q: HashSet<(u8, Vec<u8>)> = QuarantineRepo(&self.pool)
            .all()
            .await?
            .into_iter()
            .map(|r| (r.subject_kind, r.subject_ref))
            .collect();
        let d: HashSet<[u8; 32]> = DenyHashesRepo(&self.pool)
            .all()
            .await?
            .into_iter()
            .map(|r| r.hash)
            .collect();
        *self.quarantine.write() = q;
        *self.deny.write() = d;
        Ok(())
    }

    async fn audit(&self, actor: &str, action: &str, detail: &str) -> Result<(), ModerationError> {
        AuditRepo(&self.pool).record(actor, action, detail).await?;
        Ok(())
    }

    // ---- Reports ----------------------------------------------------------

    /// File a report. Returns `(row, deduped)` — when the reporter already
    /// has a still-open report on the same subject, that row comes back with
    /// `deduped = true` and nothing new is written (or audited).
    pub async fn file_report(
        &self,
        reporter_account: i64,
        reporter: &str,
        kind: u8,
        subject_ref: &[u8],
        reason: &str,
    ) -> Result<(ReportRow, bool), ModerationError> {
        check_subject(kind, subject_ref)?;
        let reason = check_note(reason)?;
        if reason.is_empty() {
            return Err(ModerationError::BadText);
        }
        let repo = ReportsRepo(&self.pool);
        if let Some(existing) = repo
            .open_duplicate(reporter_account, kind, subject_ref)
            .await?
        {
            return Ok((existing, true));
        }
        let row = repo
            .create(reporter_account, kind, subject_ref, reason)
            .await?;
        self.audit(
            reporter,
            "report-create",
            &format!("#{} kind={kind} ref={}", row.id, hex::encode(subject_ref)),
        )
        .await?;
        Ok((row, false))
    }

    /// Page the queue (moderator-gated by the caller), oldest first.
    pub async fn reports(
        &self,
        state: Option<u8>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<ReportRow>, i64), ModerationError> {
        Ok(ReportsRepo(&self.pool)
            .list(state, offset.max(0), limit.clamp(1, 200))
            .await?)
    }

    /// Apply a [`report_action`] to a report: claim (open → reviewing),
    /// resolve, or dismiss (open/reviewing → terminal). Returns the updated
    /// row. Authorization is the caller's job.
    pub async fn work_report(
        &self,
        id: i64,
        action: u8,
        moderator: &str,
        note: &str,
    ) -> Result<ReportRow, ModerationError> {
        let note = check_note(note)?;
        let (from, to, audit_action): (&[u8], u8, &str) = match action {
            report_action::CLAIM => (&[REPORT_OPEN], REPORT_REVIEWING, "report-claim"),
            report_action::RESOLVE => (
                &[REPORT_OPEN, REPORT_REVIEWING],
                REPORT_RESOLVED,
                "report-resolve",
            ),
            report_action::DISMISS => (
                &[REPORT_OPEN, REPORT_REVIEWING],
                REPORT_DISMISSED,
                "report-dismiss",
            ),
            _ => return Err(ModerationError::BadAction),
        };
        let repo = ReportsRepo(&self.pool);
        if !repo.set_state(id, from, to, moderator, note).await? {
            // Distinguish "gone" from "wrong state" for an honest error.
            return match repo.by_id(id).await? {
                Some(_) => Err(ModerationError::BadState),
                None => Err(ModerationError::NoSuchReport),
            };
        }
        self.audit(moderator, audit_action, &format!("#{id} {note}"))
            .await?;
        repo.by_id(id).await?.ok_or(ModerationError::NoSuchReport)
    }

    // ---- Quarantine ---------------------------------------------------------

    /// Place a post (event id) or file content (blob hash) under review.
    /// Idempotent. Authorization is the caller's job.
    pub async fn quarantine_set(
        &self,
        kind: u8,
        subject_ref: &[u8],
        reason: &str,
        moderator: &str,
    ) -> Result<(), ModerationError> {
        check_subject(kind, subject_ref)?;
        // Only content with a read path that consults the set can be
        // meaningfully quarantined.
        if kind != subject_kind::POST && kind != subject_kind::FILE {
            return Err(ModerationError::BadSubject);
        }
        let reason = check_note(reason)?;
        QuarantineRepo(&self.pool)
            .set(kind, subject_ref, reason, moderator)
            .await?;
        self.quarantine.write().insert((kind, subject_ref.to_vec()));
        self.audit(
            moderator,
            "quarantine-set",
            &format!("kind={kind} ref={}", hex::encode(subject_ref)),
        )
        .await?;
        Ok(())
    }

    /// Lift a quarantine; `false` when nothing was quarantined (not audited).
    pub async fn quarantine_clear(
        &self,
        kind: u8,
        subject_ref: &[u8],
        moderator: &str,
    ) -> Result<bool, ModerationError> {
        check_subject(kind, subject_ref)?;
        let removed = QuarantineRepo(&self.pool).clear(kind, subject_ref).await?;
        self.quarantine
            .write()
            .remove(&(kind, subject_ref.to_vec()));
        if removed {
            self.audit(
                moderator,
                "quarantine-clear",
                &format!("kind={kind} ref={}", hex::encode(subject_ref)),
            )
            .await?;
        }
        Ok(removed)
    }

    /// Is this subject under review? Cheap and synchronous — safe on hot
    /// read/list paths.
    pub fn is_quarantined(&self, kind: u8, subject_ref: &[u8]) -> bool {
        // Allocation-free would need a borrowed key type; the set is small
        // and this only runs when the set is non-empty.
        let q = self.quarantine.read();
        !q.is_empty() && q.contains(&(kind, subject_ref.to_vec()))
    }

    /// Convenience for post read paths.
    pub fn post_quarantined(&self, event_id: &[u8; 32]) -> bool {
        self.is_quarantined(subject_kind::POST, event_id)
    }

    /// Convenience for file read paths (`None` blob = never quarantined).
    pub fn file_quarantined(&self, blob_id: Option<&[u8; 32]>) -> bool {
        blob_id.is_some_and(|b| self.is_quarantined(subject_kind::FILE, b))
    }

    // ---- Hash-deny ----------------------------------------------------------

    /// Deny content by blake3 hash. Idempotent. Authorization is the
    /// caller's job.
    pub async fn deny_add(
        &self,
        hash: &[u8; 32],
        reason: &str,
        moderator: &str,
    ) -> Result<(), ModerationError> {
        let reason = check_note(reason)?;
        DenyHashesRepo(&self.pool)
            .add(hash, reason, moderator)
            .await?;
        self.deny.write().insert(*hash);
        self.audit(moderator, "deny-hash-add", &hex::encode(hash))
            .await?;
        Ok(())
    }

    /// Remove a denied hash; `false` when it wasn't listed (not audited).
    pub async fn deny_remove(
        &self,
        hash: &[u8; 32],
        moderator: &str,
    ) -> Result<bool, ModerationError> {
        let removed = DenyHashesRepo(&self.pool).remove(hash).await?;
        self.deny.write().remove(hash);
        if removed {
            self.audit(moderator, "deny-hash-remove", &hex::encode(hash))
                .await?;
        }
        Ok(removed)
    }

    /// Is this blake3 hash denied? Cheap and synchronous — consulted at
    /// upload finalize and attachment send.
    pub fn is_denied(&self, hash: &[u8; 32]) -> bool {
        self.deny.read().contains(hash)
    }

    pub async fn deny_list(&self) -> Result<Vec<DenyHashRow>, ModerationError> {
        Ok(DenyHashesRepo(&self.pool).all().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::admin::report_state;
    use rabbithole_store_server::open_in_memory;

    async fn service() -> ModerationService {
        let svc = ModerationService::new(open_in_memory().await.unwrap());
        svc.load().await.unwrap();
        svc
    }

    #[tokio::test]
    async fn report_dedupe_and_lifecycle() {
        let svc = service().await;
        let (a, deduped) = svc
            .file_report(1, "alice", subject_kind::POST, &[7u8; 32], "spam")
            .await
            .unwrap();
        assert!(!deduped);
        // Same reporter + subject while open: deduped to the same row.
        let (b, deduped) = svc
            .file_report(1, "alice", subject_kind::POST, &[7u8; 32], "spam again")
            .await
            .unwrap();
        assert!(deduped);
        assert_eq!(a.id, b.id);
        // Another reporter is a fresh report.
        let (c, deduped) = svc
            .file_report(2, "bob", subject_kind::POST, &[7u8; 32], "also spam")
            .await
            .unwrap();
        assert!(!deduped);
        assert_ne!(a.id, c.id);

        // Claim, then resolve; the terminal report stops deduping.
        let claimed = svc
            .work_report(a.id, report_action::CLAIM, "mo", "")
            .await
            .unwrap();
        assert_eq!(claimed.state, report_state::REVIEWING);
        assert_eq!(claimed.resolver, "mo");
        // A second claim is refused (wrong state)…
        assert!(matches!(
            svc.work_report(a.id, report_action::CLAIM, "mo", "").await,
            Err(ModerationError::BadState)
        ));
        let resolved = svc
            .work_report(a.id, report_action::RESOLVE, "mo", "handled")
            .await
            .unwrap();
        assert_eq!(resolved.state, report_state::RESOLVED);
        assert_eq!(resolved.resolution, "handled");
        assert!(resolved.resolved_at.is_some());
        let (_again, deduped) = svc
            .file_report(1, "alice", subject_kind::POST, &[7u8; 32], "back")
            .await
            .unwrap();
        assert!(!deduped, "terminal reports don't dedupe");

        // Dismiss straight from open.
        let dismissed = svc
            .work_report(c.id, report_action::DISMISS, "mo", "not spam")
            .await
            .unwrap();
        assert_eq!(dismissed.state, report_state::DISMISSED);

        // Queue paging + filters.
        let (open, total) = svc.reports(Some(report_state::OPEN), 0, 10).await.unwrap();
        assert_eq!(
            (open.len(), total),
            (1, 1),
            "only alice's re-report is open"
        );
        let (_all, total) = svc.reports(None, 0, 10).await.unwrap();
        assert_eq!(total, 3);

        // Bad inputs.
        assert!(matches!(
            svc.file_report(1, "alice", 9, &[0u8; 32], "x").await,
            Err(ModerationError::BadSubject)
        ));
        assert!(
            matches!(
                svc.file_report(1, "alice", subject_kind::USER, &[0u8; 32], "x")
                    .await,
                Err(ModerationError::BadSubject),
            ),
            "user refs are 8 bytes"
        );
        assert!(matches!(
            svc.file_report(1, "alice", subject_kind::POST, &[0u8; 32], "  ")
                .await,
            Err(ModerationError::BadText)
        ));
        assert!(matches!(
            svc.work_report(999, report_action::CLAIM, "mo", "").await,
            Err(ModerationError::NoSuchReport)
        ));
        assert!(matches!(
            svc.work_report(c.id, 9, "mo", "").await,
            Err(ModerationError::BadAction)
        ));
    }

    #[tokio::test]
    async fn quarantine_mirrors_store_and_survives_reload() {
        let svc = service().await;
        let post = [3u8; 32];
        assert!(!svc.post_quarantined(&post));
        svc.quarantine_set(subject_kind::POST, &post, "review", "mo")
            .await
            .unwrap();
        assert!(svc.post_quarantined(&post));
        assert!(!svc.file_quarantined(Some(&post)), "kinds are distinct");
        assert!(!svc.file_quarantined(None));

        // Only posts and files can be quarantined.
        assert!(matches!(
            svc.quarantine_set(subject_kind::USER, &1i64.to_le_bytes(), "", "mo")
                .await,
            Err(ModerationError::BadSubject)
        ));

        // A fresh service over the same pool reloads the set from the store.
        let again = ModerationService::new(svc.pool.clone());
        assert!(!again.post_quarantined(&post), "cold cache until load");
        again.load().await.unwrap();
        assert!(again.post_quarantined(&post));

        assert!(svc
            .quarantine_clear(subject_kind::POST, &post, "mo")
            .await
            .unwrap());
        assert!(!svc.post_quarantined(&post));
        assert!(!svc
            .quarantine_clear(subject_kind::POST, &post, "mo")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn deny_hashes_mirror_store() {
        let svc = service().await;
        let hash = *blake3::hash(b"forbidden bytes").as_bytes();
        assert!(!svc.is_denied(&hash));
        svc.deny_add(&hash, "known bad", "mo").await.unwrap();
        assert!(svc.is_denied(&hash));
        let listed = svc.deny_list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].hash, hash);
        assert_eq!(listed[0].added_by, "mo");

        // Reload sees it; removal clears both store and mirror.
        let again = ModerationService::new(svc.pool.clone());
        again.load().await.unwrap();
        assert!(again.is_denied(&hash));
        assert!(svc.deny_remove(&hash, "mo").await.unwrap());
        assert!(!svc.is_denied(&hash));
        assert!(!svc.deny_remove(&hash, "mo").await.unwrap());
    }

    #[tokio::test]
    async fn everything_lands_in_the_audit_log() {
        let svc = service().await;
        let (r, _) = svc
            .file_report(1, "alice", subject_kind::USER, &2i64.to_le_bytes(), "rude")
            .await
            .unwrap();
        svc.work_report(r.id, report_action::CLAIM, "mo", "")
            .await
            .unwrap();
        svc.work_report(r.id, report_action::DISMISS, "mo", "fine")
            .await
            .unwrap();
        svc.quarantine_set(subject_kind::FILE, &[4u8; 32], "", "mo")
            .await
            .unwrap();
        svc.quarantine_clear(subject_kind::FILE, &[4u8; 32], "mo")
            .await
            .unwrap();
        svc.deny_add(&[5u8; 32], "", "mo").await.unwrap();
        svc.deny_remove(&[5u8; 32], "mo").await.unwrap();

        let actions: Vec<String> = AuditRepo(&svc.pool)
            .recent(100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.action)
            .collect();
        assert_eq!(
            actions,
            vec![
                "report-create",
                "report-claim",
                "report-dismiss",
                "quarantine-set",
                "quarantine-clear",
                "deny-hash-add",
                "deny-hash-remove",
            ]
        );
    }
}
