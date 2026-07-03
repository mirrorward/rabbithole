//! Wave 2.2b handlers: rooms. Wave 13 adds the room moderation trio
//! (mute / unmute / slow-mode), audited with the shared conventions.

use std::sync::Arc;
use std::time::Duration;

use rabbithole_net::Connection;
use rabbithole_proto::chat as pchat;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::chat::{ChatError, RoomSummary};
use rabbithole_server_core::ratelimit::now_ms;
use rabbithole_server_core::{Caps, ServerEvent};
use rabbithole_store_server::repo2::PersonasRepo;

use crate::session::SessionCtx;
use crate::Shared;

/// Fire-and-forget audit record, same conventions as the admin family.
fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        use rabbithole_store_server::repo::AuditRepo;
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
}

fn room_info(s: &RoomSummary) -> pchat::RoomInfo {
    let mut info = pchat::RoomInfo::new(s.name.clone());
    info.category = s.category.clone();
    info.topic = s.topic.clone();
    info.private = s.private;
    info.member_count = s.member_count;
    info.created_by = s.created_by.clone();
    info
}

fn map_err(e: ChatError) -> ErrorCode {
    match e {
        ChatError::NoSuchRoom(_) => ErrorCode::NotFound,
        ChatError::AlreadyExists => ErrorCode::AlreadyExists,
        ChatError::NotMember | ChatError::Forbidden => ErrorCode::Forbidden,
        ChatError::BadName | ChatError::Empty => ErrorCode::BadRequest,
        ChatError::TooLong { .. } => ErrorCode::TooLarge,
        ChatError::Muted => ErrorCode::Muted,
        ChatError::SlowMode { retry_after_secs } => ErrorCode::SlowMode { retry_after_secs },
    }
}

/// Resolve a screen name to an account id: live sessions first (covers
/// guests), then the persona store.
async fn resolve_account(shared: &Shared, screen_name: &str) -> anyhow::Result<Option<i64>> {
    if let Some(entry) = shared.presence.is_screen_name_online(screen_name) {
        return Ok(Some(entry.account_id));
    }
    Ok(PersonasRepo(&shared.pool)
        .by_screen_name(screen_name)
        .await?
        .map(|p| p.account_id))
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

    if frame.decode::<pchat::RoomListRequest>().is_some() {
        let rooms = shared.chat.list(ctx.session_id, ctx.account_id);
        reply!(&pchat::RoomList::new(rooms.iter().map(room_info).collect()));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomCreate>() {
        if !ctx.allows(shared, "chat", Caps::CHAT_CREATE_ROOM) {
            fail!(ErrorCode::Forbidden);
        }
        match shared.chat.create(
            &req.name,
            &req.category,
            &req.topic,
            req.private,
            ctx.account_id,
            &ctx.screen_name,
            ctx.session_id,
        ) {
            Ok(summary) => reply!(&pchat::RoomInfoReply::new(room_info(&summary))),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomJoin>() {
        match shared
            .chat
            .join(&req.room, ctx.session_id, ctx.account_id, &ctx.screen_name)
        {
            Ok(summary) => reply!(&pchat::RoomInfoReply::new(room_info(&summary))),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomLeave>() {
        match shared.chat.leave(&req.room, ctx.session_id) {
            Ok(()) => conn.send(Frame::ack(frame)).await?,
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomInvite>() {
        let Some(target_account) = resolve_account(shared, &req.screen_name).await? else {
            fail!(ErrorCode::NotFound)
        };
        match shared
            .chat
            .invite(&req.room, ctx.session_id, target_account)
        {
            Ok(()) => {
                shared.bus.publish(ServerEvent::RoomInvited {
                    to_account: target_account,
                    room: req.room.clone(),
                    from: ctx.screen_name.clone(),
                });
                conn.send(Frame::ack(frame)).await?;
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomTopicSet>() {
        let is_moderator = ctx.allows(shared, "chat", Caps::CHAT_MODERATE);
        match shared
            .chat
            .set_topic(&req.room, &req.topic, ctx.account_id, is_moderator)
        {
            Ok(()) => conn.send(Frame::ack(frame)).await?,
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomKick>() {
        let Some(target_account) = resolve_account(shared, &req.screen_name).await? else {
            fail!(ErrorCode::NotFound)
        };
        let target_sessions: Vec<u64> = shared
            .presence
            .snapshot()
            .into_iter()
            .filter(|e| e.account_id == target_account)
            .map(|e| e.session_id)
            .collect();
        let is_moderator = ctx.allows(shared, "chat", Caps::CHAT_MODERATE);
        match shared.chat.kick(
            &req.room,
            ctx.account_id,
            is_moderator,
            target_account,
            &target_sessions,
            req.ban,
        ) {
            Ok(_kicked) => {
                shared.bus.publish(ServerEvent::RoomKicked {
                    account: target_account,
                    room: req.room.clone(),
                    banned: req.ban,
                });
                conn.send(Frame::ack(frame)).await?;
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomMembersRequest>() {
        match shared.chat.members(&req.room, ctx.session_id) {
            Ok(members) => reply!(&pchat::RoomMemberList::new(members)),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    // ---- Room moderation: mute / unmute / slow-mode (Wave 13) --------------

    if let Some(Ok(req)) = frame.decode::<pchat::RoomMute>() {
        let Some(target_account) = resolve_account(shared, &req.screen_name).await? else {
            fail!(ErrorCode::NotFound)
        };
        let is_moderator = ctx.allows(shared, "chat", Caps::CHAT_MODERATE);
        let duration = req.duration_secs.map(|s| Duration::from_secs(s.into()));
        match shared.chat.mute(
            &req.room,
            ctx.account_id,
            is_moderator,
            target_account,
            duration,
            now_ms(),
        ) {
            Ok(()) => {
                shared.bus.publish(ServerEvent::RoomMuted {
                    account: target_account,
                    screen_name: req.screen_name.clone(),
                    room: req.room.clone(),
                    muted: true,
                    duration_secs: req.duration_secs,
                });
                audit(
                    shared,
                    &ctx.login,
                    "room-mute",
                    format!(
                        "room={} target={} duration={}",
                        req.room,
                        req.screen_name,
                        req.duration_secs
                            .map_or_else(|| "permanent".into(), |s| format!("{s}s")),
                    ),
                );
                conn.send(Frame::ack(frame)).await?;
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomUnmute>() {
        let Some(target_account) = resolve_account(shared, &req.screen_name).await? else {
            fail!(ErrorCode::NotFound)
        };
        let is_moderator = ctx.allows(shared, "chat", Caps::CHAT_MODERATE);
        match shared.chat.unmute(
            &req.room,
            ctx.account_id,
            is_moderator,
            target_account,
            now_ms(),
        ) {
            Ok(true) => {
                shared.bus.publish(ServerEvent::RoomMuted {
                    account: target_account,
                    screen_name: req.screen_name.clone(),
                    room: req.room.clone(),
                    muted: false,
                    duration_secs: None,
                });
                audit(
                    shared,
                    &ctx.login,
                    "room-unmute",
                    format!("room={} target={}", req.room, req.screen_name),
                );
                conn.send(Frame::ack(frame)).await?;
            }
            // Nothing was muted (or the mute had already expired): an
            // honest NotFound, not audited — the quarantine-clear pattern.
            Ok(false) => fail!(ErrorCode::NotFound),
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pchat::RoomSlowMode>() {
        let is_moderator = ctx.allows(shared, "chat", Caps::CHAT_MODERATE);
        match shared
            .chat
            .set_slow_mode(&req.room, req.seconds, ctx.account_id, is_moderator)
        {
            Ok(applied) => {
                shared.bus.publish(ServerEvent::RoomSlowModeChanged {
                    room: req.room.clone(),
                    seconds: applied,
                    by: ctx.screen_name.clone(),
                });
                audit(
                    shared,
                    &ctx.login,
                    "room-slow-mode",
                    format!("room={} seconds={applied}", req.room),
                );
                conn.send(Frame::ack(frame)).await?;
            }
            Err(e) => fail!(map_err(e)),
        }
        return Ok(true);
    }

    Ok(false)
}
