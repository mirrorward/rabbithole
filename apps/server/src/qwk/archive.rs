//! Provenance-bound archive import; the raw member API retains its old mapping.
use super::*;
use rabbithole_store_server::qwk::exports::{ExportGuard, QwkExportsRepo};
use rabbithole_store_server::repo::AccountsRepo;

/// Check the current account, posting permission, and account-owned QWK export.
/// Missing/expired provenance is never replaced by today's conference map.
pub(crate) async fn preflight_rep_archive(
    shared: &Shared,
    account: &Account,
    export_id: &str,
) -> Result<(Account, String), QwkGateError> {
    if !shared.config.read().qwk_enabled {
        return Err(QwkGateError::Disabled);
    }
    let account = AccountsRepo(&shared.pool)
        .by_id(account.id)
        .await?
        .filter(|a| !a.disabled)
        .ok_or(QwkGateError::Forbidden)?;
    let subject = subject_for(shared, &account);
    if !shared.perms.allows(&subject, "board", Caps::BOARD_POST) {
        return Err(QwkGateError::Forbidden);
    }
    let exports = QwkExportsRepo(&shared.pool);
    let bbs = exports
        .bbs_id(account.id, export_id, chrono::Utc::now().timestamp_millis())
        .await?
        .ok_or_else(|| {
            QwkGateError::Internal(
                "missing, foreign, or expired QWK export id; build a new packet".into(),
            )
        })?;
    Ok((account, bbs))
}

/// Ingest an actual REP ZIP using the exact account-owned QWK export id.
/// Recheck admission here even when a transport preflighted before receiving.
pub async fn ingest_rep_archive_for(
    shared: &Shared,
    account: &Account,
    export_id: &str,
    bytes: &[u8],
) -> Result<RepReport, QwkGateError> {
    let (account, bbs) = preflight_rep_archive(shared, account, export_id).await?;
    let subject = subject_for(shared, &account);
    let exports = QwkExportsRepo(&shared.pool);
    let packet = rabbithole_legacy_qwk::rep_archive::parse(bytes, &bbs)
        .map_err(|e| QwkGateError::Internal(e.to_string()))?;
    let author = format!("{}@{}", account.screen_name, shared.origin_name());
    let seed = author_seed(shared, account.id);
    let mut report = RepReport::default();
    for reply in packet.replies {
        let now = chrono::Utc::now().timestamp_millis();
        let guard = ExportGuard {
            export_id: export_id.into(),
            conference: reply.conference,
            reference: reply.reference,
        };
        let Some(target) = exports.resolve(account.id, &guard, now).await? else {
            report.rejected.push((
                reply.subject,
                "conference or reply reference no longer matches this export".into(),
            ));
            continue;
        };
        let problems = check(&reply, &HashSet::from([reply.conference]));
        if !problems.is_empty() {
            report
                .rejected
                .push((reply.subject, problems_text(&problems)));
            continue;
        }
        if !shared.perms.allows(
            &subject,
            &format!("board/{}", target.slug),
            Caps::BOARD_POST,
        ) {
            report
                .rejected
                .push((reply.subject, "not permitted on that board".into()));
            continue;
        }
        if let Some(parent) = target.parent {
            let row = shared.boards.post_by_id(&parent).await?;
            if row.as_ref().is_none_or(|r| {
                r.tombstoned
                    || shared.moderation.post_quarantined(&parent)
                    || r.root_id
                        .is_some_and(|root| shared.moderation.post_quarantined(&root))
            }) {
                report
                    .rejected
                    .push((reply.subject, "reply reference is unavailable".into()));
                continue;
            }
        }
        let mut semantic = reply.clone();
        semantic.conference = 0;
        let digest = content_hash(&semantic);
        match shared
            .boards
            .post_qwk_reply_from_export(
                account.id,
                &digest,
                &target.slug,
                target.parent,
                &author,
                &seed,
                &reply.subject,
                &reply.body,
                now,
                &guard,
            )
            .await
        {
            Ok(Some(row)) => {
                shared.bus.publish(ServerEvent::BoardPost {
                    board: row.board_slug,
                    id: row.event_id,
                    root: row.root_id,
                });
                report.accepted += 1;
                shared.stats.incr("qwk", "replies_ingested");
            }
            Ok(None) => report.duplicates += 1,
            Err(e) => report.rejected.push((reply.subject, e.to_string())),
        }
    }
    Ok(report)
}
