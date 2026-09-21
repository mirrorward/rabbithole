//! Wave 13 handlers: the moderation suite (ADMIN family, types 30..49).
//!
//! `ReportCreate` is open to any authenticated session (report volume rides
//! the existing per-account `post` rate-limit class); everything else is
//! gated on [`Caps::MODERATE`] against the `moderation` resource and
//! audited inside [`rabbithole_server_core::ModerationService`].

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::admin as padm;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::moderation::ModerationError;
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, ServerEvent};
use rabbithole_store_server::repo7::ReportRow;

use crate::handlers15::audit;
use crate::session::SessionCtx;
use crate::Shared;

/// A report's or a quarantine's subject, short enough to read in a log
/// line: what kind of thing it is, and enough of its reference to find it.
fn subject(kind: u8, reference: &[u8]) -> String {
    let what = match kind {
        padm::subject_kind::POST => "post",
        padm::subject_kind::DM => "message",
        padm::subject_kind::FILE => "file",
        padm::subject_kind::USER => "account",
        _ => "something",
    };
    let hex = hex::encode(reference);
    let short = if hex.len() > 16 {
        format!("{}\u{2026}{}", &hex[..8], &hex[hex.len() - 8..])
    } else {
        hex
    };
    format!("{what} {short}")
}

/// What a moderator did with a report, for the record.
fn report_action(action: u8) -> &'static str {
    match action {
        padm::report_state::RESOLVED => "resolved",
        padm::report_state::DISMISSED => "dismissed",
        padm::report_state::OPEN => "reopened",
        _ => "looked at",
    }
}

/// The ACL resource moderation ops are checked against (so operators can
/// grant or revoke the suite independently of `admin`).
const RESOURCE: &str = "moderation";

fn map_err(e: ModerationError) -> ErrorCode {
    match e {
        ModerationError::BadSubject | ModerationError::BadText | ModerationError::BadAction => {
            ErrorCode::BadRequest
        }
        ModerationError::NoSuchReport => ErrorCode::NotFound,
        // Claiming/resolving out of order is a state clash, not a malformed
        // request — but AlreadyExists is the closest wire code we have.
        ModerationError::BadState => ErrorCode::AlreadyExists,
        ModerationError::Store(_) => ErrorCode::Internal,
    }
}

fn entry(row: &ReportRow) -> padm::ReportEntry {
    padm::ReportEntry::new(
        row.id,
        row.reporter_account,
        row.subject_kind,
        row.subject_ref.clone(),
        row.reason.clone(),
        row.created_at,
        row.state,
        row.resolver.clone(),
        row.resolved_at,
        row.resolution.clone(),
    )
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    macro_rules! reply {
        ($msg:expr) => {
            conn.send(Frame::reply_to(frame, $msg)?).await?
        };
    }
    macro_rules! fail {
        ($code:expr) => {{
            conn.send(Frame::error_reply(frame, $code)).await?;
            return Ok(true);
        }};
    }
    macro_rules! moderators_only {
        () => {
            if !ctx.allows(shared, RESOURCE, Caps::MODERATE) {
                fail!(ErrorCode::Forbidden)
            }
        };
    }

    // ---- File a report (any authenticated session) ------------------------
    if let Some(Ok(req)) = frame.decode::<padm::ReportCreate>() {
        // Reports share the per-account posting budget — a report flood is
        // shaped exactly like a post flood.
        if !shared.rate_allow(Scope::Account(ctx.account_id), rl::POST) {
            fail!(ErrorCode::RateLimited);
        }
        match shared
            .moderation
            .file_report(
                ctx.account_id,
                &ctx.login,
                req.subject_kind,
                &req.subject_ref,
                &req.reason,
            )
            .await
        {
            Ok((row, deduped)) => {
                if !deduped {
                    shared.bus.publish(ServerEvent::ModNotice {
                        text: format!("new report #{} filed (reason: {})", row.id, row.reason),
                    });
                }
                reply!(&padm::ReportAck::new(row.id, deduped));
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    // ---- Work the queue (moderators) ---------------------------------------
    if let Some(Ok(req)) = frame.decode::<padm::ReportListRequest>() {
        moderators_only!();
        match shared
            .moderation
            .reports(req.state, req.offset as i64, req.limit.clamp(1, 200) as i64)
            .await
        {
            Ok((rows, total)) => reply!(&padm::ReportList::new(
                rows.iter().map(entry).collect(),
                total.max(0) as u64
            )),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::ReportResolve>() {
        moderators_only!();
        match shared
            .moderation
            .work_report(req.id, req.action, &ctx.login, &req.note)
            .await
        {
            Ok(_) => {
                audit(
                    shared,
                    &ctx.login,
                    "report-resolve",
                    format!("#{} {}", req.id, report_action(req.action)),
                );
                conn.send(Frame::ack(frame)).await?
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    // ---- Quarantine (moderators) --------------------------------------------
    if let Some(Ok(req)) = frame.decode::<padm::QuarantineSet>() {
        moderators_only!();
        match shared
            .moderation
            .quarantine_set(req.subject_kind, &req.subject_ref, &req.reason, &ctx.login)
            .await
        {
            Ok(()) => {
                audit(
                    shared,
                    &ctx.login,
                    "quarantine-set",
                    format!(
                        "{} ({})",
                        subject(req.subject_kind, &req.subject_ref),
                        req.reason
                    ),
                );
                conn.send(Frame::ack(frame)).await?
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::QuarantineClear>() {
        moderators_only!();
        match shared
            .moderation
            .quarantine_clear(req.subject_kind, &req.subject_ref, &ctx.login)
            .await
        {
            Ok(true) => {
                audit(
                    shared,
                    &ctx.login,
                    "quarantine-clear",
                    subject(req.subject_kind, &req.subject_ref),
                );
                conn.send(Frame::ack(frame)).await?
            }
            Ok(false) => fail!(ErrorCode::NotFound),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    // ---- Hash-deny list (moderators) ----------------------------------------
    if let Some(Ok(req)) = frame.decode::<padm::DenyHashAdd>() {
        moderators_only!();
        match shared
            .moderation
            .deny_add(&req.hash, &req.reason, &ctx.login)
            .await
        {
            Ok(()) => {
                audit(
                    shared,
                    &ctx.login,
                    "deny-add",
                    format!("{} ({})", hex::encode(req.hash), req.reason),
                );
                conn.send(Frame::ack(frame)).await?
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::DenyHashRemove>() {
        moderators_only!();
        match shared.moderation.deny_remove(&req.hash, &ctx.login).await {
            Ok(true) => {
                audit(shared, &ctx.login, "deny-remove", hex::encode(req.hash));
                conn.send(Frame::ack(frame)).await?
            }
            Ok(false) => fail!(ErrorCode::NotFound),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if frame.decode::<padm::DenyHashListRequest>().is_some() {
        moderators_only!();
        match shared.moderation.deny_list().await {
            Ok(rows) => reply!(&padm::DenyHashList::new(
                rows.iter()
                    .map(|r| padm::DenyHashEntry::new(
                        r.hash,
                        r.reason.clone(),
                        r.added_by.clone(),
                        r.created_at
                    ))
                    .collect()
            )),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    Ok(false)
}
