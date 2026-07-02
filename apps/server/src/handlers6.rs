//! Wave 3.1 handlers: message bases (boards).

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::board as pb;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::boards::BoardError;
use rabbithole_server_core::events::{EventBody, SignedEvent};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, ServerEvent};
use rabbithole_store_server::repo4::PostRow;

use crate::session::SessionCtx;
use crate::Shared;

fn view(row: &PostRow) -> pb::PostView {
    pb::PostView::new(
        row.event_id,
        row.board_slug.clone(),
        row.root_id,
        row.parent_id,
        row.author.clone(),
        row.subject.clone(),
        row.body.clone(),
        row.mime.clone(),
        row.created_at,
        row.edited,
        row.tombstoned,
    )
}

fn map_err(e: BoardError) -> ErrorCode {
    match e {
        BoardError::NoSuchBoard | BoardError::NoSuchPost => ErrorCode::NotFound,
        BoardError::NotPostable | BoardError::Empty => ErrorCode::BadRequest,
        BoardError::Forbidden => ErrorCode::Forbidden,
        BoardError::SlugExists => ErrorCode::AlreadyExists,
        BoardError::Store(_) => ErrorCode::Internal,
    }
}

/// A stable per-account author signing seed. Wave 3 derives it from the
/// server key + account id (deterministic, server-held); Wave 9 replaces
/// this with the account's enrolled Ed25519 identity key.
fn author_seed(shared: &Shared, account_id: i64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(&account_id.to_le_bytes());
    *hasher.finalize().as_bytes()
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

    if frame.decode::<pb::BoardListRequest>().is_some() {
        if !ctx.allows(shared, "board", Caps::BOARD_READ) {
            fail!(ErrorCode::Forbidden);
        }
        let rows = shared.boards.boards().await.map_err(anyhow::Error::msg)?;
        let mut boards = Vec::new();
        for r in rows {
            let mut info = pb::BoardInfo::new(r.slug.clone(), r.title, r.kind);
            info.description = r.description;
            info.parent_slug = r.parent_slug;
            if r.kind == 2 && !ctx.is_guest {
                info.unread = shared
                    .boards
                    .unread(ctx.account_id, &r.slug)
                    .await
                    .unwrap_or(0) as u64;
            }
            boards.push(info);
        }
        reply!(&pb::BoardList::new(boards));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::ThreadListRequest>() {
        if !ctx.allows(shared, "board", Caps::BOARD_READ) {
            fail!(ErrorCode::Forbidden);
        }
        match shared
            .boards
            .threads(&req.board, req.limit.clamp(1, 200) as i64)
            .await
        {
            Ok(rows) => {
                let threads = rows
                    .into_iter()
                    .map(|(root, replies, last)| {
                        pb::ThreadSummary::new(view(&root), replies as u64, last)
                    })
                    .collect();
                reply!(&pb::ThreadList::new(threads));
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::ThreadRequest>() {
        if !ctx.allows(shared, "board", Caps::BOARD_READ) {
            fail!(ErrorCode::Forbidden);
        }
        match shared
            .boards
            .thread(&req.root, req.limit.clamp(1, 1000) as i64)
            .await
        {
            Ok(rows) => reply!(&pb::ThreadPosts::new(rows.iter().map(view).collect())),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::PostCreate>() {
        if ctx.is_guest || !ctx.allows(shared, "board", Caps::BOARD_POST) {
            fail!(ErrorCode::Forbidden);
        }
        // Per-account posting budget: refuse the post, keep the session.
        if !shared.rate_allow(Scope::Account(ctx.account_id), rl::POST) {
            fail!(ErrorCode::RateLimited);
        }
        let seed = author_seed(shared, ctx.account_id);
        let author = format!("{}@{}", ctx.screen_name, shared.origin_name());
        let now = chrono::Utc::now().timestamp_millis();
        match shared
            .boards
            .post(
                &req.board,
                req.parent,
                &author,
                &seed,
                &req.subject,
                &req.body,
                &req.mime,
                now,
            )
            .await
        {
            Ok(row) => {
                shared.bus.publish(ServerEvent::BoardPost {
                    board: row.board_slug.clone(),
                    id: row.event_id,
                    root: row.root_id,
                });
                reply!(&pb::PostReply::new(view(&row)));
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::PostEdit>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        // Author or moderator.
        let Some(existing) = shared
            .boards
            .post_by_id(&req.target)
            .await
            .map_err(anyhow::Error::msg)?
        else {
            fail!(ErrorCode::NotFound)
        };
        let is_author = existing
            .author
            .starts_with(&format!("{}@", ctx.screen_name));
        if !is_author && !ctx.allows(shared, "board", Caps::BOARD_MODERATE) {
            fail!(ErrorCode::Forbidden);
        }
        let seed = author_seed(shared, ctx.account_id);
        let editor = format!("{}@{}", ctx.screen_name, shared.origin_name());
        let now = chrono::Utc::now().timestamp_millis();
        match shared
            .boards
            .edit(
                req.target,
                &editor,
                &seed,
                &req.subject,
                &req.body,
                &req.mime,
                now,
            )
            .await
        {
            Ok(row) => reply!(&pb::PostReply::new(view(&row))),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::PostDelete>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let Some(existing) = shared
            .boards
            .post_by_id(&req.target)
            .await
            .map_err(anyhow::Error::msg)?
        else {
            fail!(ErrorCode::NotFound)
        };
        let is_author = existing
            .author
            .starts_with(&format!("{}@", ctx.screen_name));
        if !is_author && !ctx.allows(shared, "board", Caps::BOARD_MODERATE) {
            fail!(ErrorCode::Forbidden);
        }
        match shared.boards.tombstone(req.target).await {
            Ok(()) => conn.send(Frame::ack(frame)).await?,
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::MarkRead>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let up_to = if req.up_to_unix_ms <= 0 {
            chrono::Utc::now().timestamp_millis()
        } else {
            req.up_to_unix_ms
        };
        shared
            .boards
            .mark_read(ctx.account_id, &req.board, up_to)
            .await
            .map_err(anyhow::Error::msg)?;
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pb::BoardCreate>() {
        if !ctx.allows(shared, "board", Caps::BOARD_MODERATE) {
            fail!(ErrorCode::Forbidden);
        }
        match shared
            .boards
            .create_board(
                &req.slug,
                &req.title,
                &req.description,
                req.kind,
                req.parent_slug.as_deref(),
                req.max_threads as i64,
            )
            .await
        {
            Ok(r) => {
                let mut info = pb::BoardInfo::new(r.slug, r.title, r.kind);
                info.description = r.description;
                info.parent_slug = r.parent_slug;
                reply!(&pb::BoardCreated::new(info));
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    Ok(false)
}

/// Project a BoardPost bus event into a push (for the session pump).
pub(crate) fn board_push(event: &ServerEvent) -> Option<Frame> {
    match event {
        ServerEvent::BoardPost { board, id, root } => {
            Frame::push(&pb::PostPosted::new(board.clone(), *id, *root)).ok()
        }
        _ => None,
    }
}

/// Verify a stored blob (used by tests / future federation ingest).
#[allow(dead_code)]
pub fn verify_blob(blob: &[u8], origin_key: &[u8; 32]) -> bool {
    match postcard::from_bytes::<SignedEvent>(blob) {
        Ok(ev) => ev.verify(origin_key).is_ok() && matches!(ev.body, EventBody::Post { .. }),
        Err(_) => false,
    }
}
