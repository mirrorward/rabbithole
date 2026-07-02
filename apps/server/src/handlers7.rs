//! Wave 3.2 handlers: the Wishing Well (request system).

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::wish as pw;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::{Caps, ServerEvent};
use rabbithole_store_server::repo5::{WishRow, WishesRepo};

use crate::session::SessionCtx;
use crate::Shared;

// Status codes.
const OPEN: u8 = 0;
const CLAIMED: u8 = 1;
const FULFILLED: u8 = 2;
const DECLINED: u8 = 3;

fn view(row: &WishRow) -> pw::WishView {
    pw::WishView::new(
        row.id,
        row.kind,
        row.title.clone(),
        row.details.clone(),
        row.requester.clone(),
        row.status,
        row.claimed_by.clone(),
        row.fulfillment.clone(),
        row.votes.max(0) as u64,
        row.created_at,
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

    let repo = WishesRepo(&shared.pool);

    if let Some(Ok(req)) = frame.decode::<pw::WishListRequest>() {
        let limit = req.limit.clamp(1, 200) as i64;
        let rows = repo
            .list(req.status, limit)
            .await
            .map_err(anyhow::Error::msg)?;
        reply!(&pw::WishList::new(rows.iter().map(view).collect()));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pw::WishCreate>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let title = req.title.trim();
        if title.is_empty() || title.len() > 200 {
            fail!(ErrorCode::BadRequest);
        }
        let requester = format!("{}@{}", ctx.screen_name, shared.origin_name());
        let row = repo
            .create(
                req.kind.min(3),
                title,
                &req.details,
                &requester,
                ctx.account_id,
            )
            .await
            .map_err(anyhow::Error::msg)?;
        reply!(&pw::WishReply::new(view(&row)));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pw::WishVote>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        if repo
            .by_id(req.id)
            .await
            .map_err(anyhow::Error::msg)?
            .is_none()
        {
            fail!(ErrorCode::NotFound);
        }
        repo.toggle_vote(req.id, ctx.account_id)
            .await
            .map_err(anyhow::Error::msg)?;
        let row = repo
            .by_id(req.id)
            .await
            .map_err(anyhow::Error::msg)?
            .unwrap();
        reply!(&pw::WishReply::new(view(&row)));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pw::WishSetStatus>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let Some(existing) = repo.by_id(req.id).await.map_err(anyhow::Error::msg)? else {
            fail!(ErrorCode::NotFound)
        };
        let is_requester = existing.requester_id == ctx.account_id;
        let can_moderate = ctx.allows(shared, "wishingwell", Caps::BOARD_MODERATE);
        let can_claim = ctx.allows(shared, "wishingwell", Caps::FILE_UPLOAD);

        // Authorize the transition.
        let allowed = match req.status {
            // Withdraw your own wish, or a moderator declines any.
            DECLINED => is_requester || can_moderate,
            // Claim: fulfillers (upload-capable) or moderators.
            CLAIMED => can_claim || can_moderate,
            // Fulfill: the claimer or a moderator.
            FULFILLED => {
                can_moderate
                    || existing
                        .claimed_by
                        .as_deref()
                        .is_some_and(|c| c.starts_with(&format!("{}@", ctx.screen_name)))
                    || can_claim
            }
            // Reopen: requester or moderator.
            OPEN => is_requester || can_moderate,
            _ => false,
        };
        if !allowed {
            fail!(ErrorCode::Forbidden);
        }

        let claimed_by = (req.status == CLAIMED)
            .then(|| format!("{}@{}", ctx.screen_name, shared.origin_name()));
        repo.set_status(
            req.id,
            req.status,
            claimed_by.as_deref(),
            req.fulfillment.as_deref(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
        let row = repo
            .by_id(req.id)
            .await
            .map_err(anyhow::Error::msg)?
            .unwrap();
        let updated = view(&row);

        // Notify the requester (if they're not the one making the change).
        if existing.requester_id != ctx.account_id {
            shared.bus.publish(ServerEvent::WishUpdated {
                to_account: existing.requester_id,
                wish: updated.clone(),
            });
        }
        reply!(&pw::WishReply::new(updated));
        return Ok(true);
    }

    Ok(false)
}
