//! Wave 2.2 handlers: presence states, buddy lists, blocks, DMs.

use std::sync::Arc;

use rabbithole_blobs::BlobId;
use rabbithole_net::Connection;
use rabbithole_proto::presence::PresenceState;
use rabbithole_proto::{dm as pdm, presence as ppres};
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, ServerEvent};
use rabbithole_store_server::repo2::PersonasRepo;
use rabbithole_store_server::repo3::{dm_receipts_enabled, BlocksRepo, BuddiesRepo, DmsRepo};

use crate::session::SessionCtx;
use crate::Shared;

const MAX_STATUS_LEN: usize = 200;
const MAX_DM_ATTACHMENTS: usize = 8;

fn state_ordinal(state: PresenceState) -> u8 {
    match state {
        PresenceState::Online => 0,
        PresenceState::Away => 1,
        PresenceState::Idle => 2,
        PresenceState::Invisible => 3,
        _ => 0,
    }
}

pub(crate) fn dm_row_to_message(row: &rabbithole_store_server::repo3::DmRow) -> pdm::DmMessage {
    pdm::DmMessage::new(
        row.id,
        row.from_persona.clone(),
        row.to_persona.clone(),
        row.text.clone(),
        row.quote_of,
        row.attachments_hex
            .iter()
            .filter_map(|h| BlobId::from_hex(h).map(|b| b.0))
            .collect(),
        row.at_ms,
        row.is_auto,
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

    // ---- Presence state ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<ppres::PresenceSet>() {
        let status = req.status.filter(|s| !s.trim().is_empty());
        if status.as_deref().is_some_and(|s| s.len() > MAX_STATUS_LEN) {
            fail!(ErrorCode::TooLarge);
        }
        let state = state_ordinal(req.state);
        shared.presence.set_state(ctx.session_id, state, status);
        // Coming back online re-arms away auto-responses.
        if state == 0 {
            shared
                .auto_responded
                .lock()
                .expect("lock")
                .retain(|(_, to)| *to != ctx.account_id);
        }
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Buddy list ---------------------------------------------------------
    if frame.decode::<ppres::BuddyListRequest>().is_some() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let rows = BuddiesRepo(&shared.pool).list(ctx.account_id).await?;
        let viewer_is_mod = ctx.role >= rabbithole_server_core::Role::Moderator;
        let buddies = rows
            .into_iter()
            .map(|b| {
                let mut entry = ppres::BuddyEntry::new(b.screen_name.clone(), b.group);
                if let Some(p) = shared.presence.is_screen_name_online(&b.screen_name) {
                    // Cheshire mode: invisible buddies read as offline.
                    if !p.is_invisible() || viewer_is_mod {
                        entry.online = true;
                        entry.state = PresenceState::from_ordinal(p.state);
                        entry.status = p.status.clone();
                    }
                }
                entry
            })
            .collect();
        let blocked = BlocksRepo(&shared.pool).list(ctx.account_id).await?;
        reply!(&ppres::BuddyList::new(buddies, blocked));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppres::BuddyAdd>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        if PersonasRepo(&shared.pool)
            .by_screen_name(&req.screen_name)
            .await?
            .is_none()
        {
            fail!(ErrorCode::NotFound);
        }
        let group = if req.group.trim().is_empty() {
            "Buddies"
        } else {
            req.group.trim()
        };
        BuddiesRepo(&shared.pool)
            .add(ctx.account_id, &req.screen_name, group)
            .await?;
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppres::BuddyRemove>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        BuddiesRepo(&shared.pool)
            .remove(ctx.account_id, &req.screen_name)
            .await?;
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppres::BlockAdd>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let Some(persona) = PersonasRepo(&shared.pool)
            .by_screen_name(&req.screen_name)
            .await?
        else {
            fail!(ErrorCode::NotFound)
        };
        if persona.account_id == ctx.account_id {
            fail!(ErrorCode::BadRequest); // can't block yourself
        }
        BlocksRepo(&shared.pool)
            .add(ctx.account_id, persona.account_id, &persona.screen_name)
            .await?;
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppres::BlockRemove>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        if let Some(persona) = PersonasRepo(&shared.pool)
            .by_screen_name(&req.screen_name)
            .await?
        {
            BlocksRepo(&shared.pool)
                .remove(ctx.account_id, persona.account_id)
                .await?;
        }
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Direct messages -----------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pdm::DmSend>() {
        if !ctx.allows(shared, "dm", Caps::DM_SEND) {
            fail!(ErrorCode::Forbidden);
        }
        // DMs share the per-account message budget with chat sends.
        if !shared.rate_allow(Scope::Account(ctx.account_id), rl::MSG) {
            fail!(ErrorCode::RateLimited);
        }
        let text = req.text.trim_end();
        if text.is_empty() && req.attachments.is_empty() {
            fail!(ErrorCode::BadRequest);
        }
        if text.len() > shared.config.read().chat_max_len
            || req.attachments.len() > MAX_DM_ATTACHMENTS
        {
            fail!(ErrorCode::TooLarge);
        }
        let Some(recipient) = PersonasRepo(&shared.pool).by_screen_name(&req.to).await? else {
            fail!(ErrorCode::NotFound)
        };
        if recipient.account_id == ctx.account_id {
            fail!(ErrorCode::BadRequest); // no self-mail (yet)
        }
        // Blocked either way = Forbidden, without revealing which side.
        let blocks = BlocksRepo(&shared.pool);
        if blocks
            .is_blocked(ctx.account_id, recipient.account_id)
            .await?
            || blocks
                .is_blocked(recipient.account_id, ctx.account_id)
                .await?
        {
            fail!(ErrorCode::Forbidden);
        }
        for a in &req.attachments {
            if !shared.blobs.contains(&BlobId(*a)) {
                fail!(ErrorCode::NotFound);
            }
        }

        let at_ms = chrono::Utc::now().timestamp_millis();
        let attachments_hex: Vec<String> = req
            .attachments
            .iter()
            .map(|a| BlobId(*a).to_hex())
            .collect();
        let id = DmsRepo(&shared.pool)
            .insert(
                ctx.account_id,
                &ctx.screen_name,
                recipient.account_id,
                &recipient.screen_name,
                text,
                req.quote_of,
                &attachments_hex,
                at_ms,
                false,
            )
            .await?;

        let message = pdm::DmMessage::new(
            id,
            ctx.screen_name.clone(),
            recipient.screen_name.clone(),
            text,
            req.quote_of,
            req.attachments.clone(),
            at_ms,
            false,
        );
        shared.bus.publish(ServerEvent::Dm {
            to_account: recipient.account_id,
            message,
        });
        reply!(&pdm::DmSent::new(id, at_ms));

        // Away auto-response (once per sender→recipient away period).
        maybe_auto_respond(shared, ctx, &recipient.screen_name, recipient.account_id).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pdm::DmHistoryRequest>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let Some(partner) = PersonasRepo(&shared.pool).by_screen_name(&req.with).await? else {
            fail!(ErrorCode::NotFound)
        };
        let rows = DmsRepo(&shared.pool)
            .thread(
                ctx.account_id,
                partner.account_id,
                req.before_id,
                req.limit.clamp(1, 200) as i64,
            )
            .await?;
        reply!(&pdm::DmHistory::new(
            rows.iter().map(dm_row_to_message).collect()
        ));
        return Ok(true);
    }

    if frame.decode::<pdm::DmThreadsRequest>().is_some() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let threads = DmsRepo(&shared.pool).threads(ctx.account_id).await?;
        let summaries = threads
            .into_iter()
            .map(|(partner_account, last, unread)| {
                let with = if last.from_account == partner_account {
                    last.from_persona.clone()
                } else {
                    last.to_persona.clone()
                };
                let mut text = last.text.clone();
                text.truncate(120);
                pdm::DmThreadSummary::new(with, text, last.at_ms, unread)
            })
            .collect();
        reply!(&pdm::DmThreads::new(summaries));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pdm::DmMarkRead>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let Some(partner) = PersonasRepo(&shared.pool).by_screen_name(&req.with).await? else {
            fail!(ErrorCode::NotFound)
        };
        let newly = DmsRepo(&shared.pool)
            .mark_read(ctx.account_id, partner.account_id, req.up_to_id)
            .await?;
        conn.send(Frame::ack(frame)).await?;
        if newly > 0 && dm_receipts_enabled(&shared.pool, ctx.account_id).await? {
            shared.bus.publish(ServerEvent::DmRead {
                to_account: partner.account_id,
                by: ctx.screen_name.clone(),
                up_to_id: req.up_to_id,
            });
        }
        return Ok(true);
    }

    Ok(false)
}

/// If every online session of the recipient is away/idle with a status
/// message, send it back once as an auto-response DM.
async fn maybe_auto_respond(
    shared: &Arc<Shared>,
    ctx: &SessionCtx,
    recipient_name: &str,
    recipient_account: i64,
) -> anyhow::Result<()> {
    let sessions: Vec<_> = shared
        .presence
        .snapshot()
        .into_iter()
        .filter(|e| e.account_id == recipient_account)
        .collect();
    let away_status = sessions
        .iter()
        .filter(|e| e.state == 1 || e.state == 2)
        .find_map(|e| e.status.clone());
    let all_away = !sessions.is_empty() && sessions.iter().all(|e| e.state == 1 || e.state == 2);
    let Some(status) = away_status.filter(|_| all_away) else {
        return Ok(());
    };

    {
        let mut seen = shared.auto_responded.lock().expect("lock");
        if !seen.insert((ctx.account_id, recipient_account)) {
            return Ok(()); // already auto-responded this away period
        }
    }

    let at_ms = chrono::Utc::now().timestamp_millis();
    let id = DmsRepo(&shared.pool)
        .insert(
            recipient_account,
            recipient_name,
            ctx.account_id,
            &ctx.screen_name,
            &status,
            None,
            &[],
            at_ms,
            true,
        )
        .await?;
    shared.bus.publish(ServerEvent::Dm {
        to_account: ctx.account_id,
        message: pdm::DmMessage::new(
            id,
            recipient_name,
            ctx.screen_name.clone(),
            status,
            None,
            Vec::new(),
            at_ms,
            true,
        ),
    });
    Ok(())
}
