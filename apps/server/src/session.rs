//! The per-connection session state machine.
//!
//! ```text
//! AwaitHello ──Hello/HelloAck──▶ AwaitAuth ──AuthOk──▶ Active
//! ```
//!
//! Before authentication only Hello, Register, auth requests, and Ping are
//! honored. Once active, the session joins presence, receives the Welcome
//! push, and pumps bus events out as pushes while serving requests.
//! Requests may be pipelined; pushes are routed by type, never by request
//! id. Wave 2 handlers (personas, directory, blobs, admin) live in
//! [`crate::handlers2`].

use std::sync::Arc;
use std::time::Instant;

use rabbithole_net::Connection;
use rabbithole_proto::{chat as pchat, presence as ppres, radio as pradio, session as psess};
use rabbithole_proto::{directory as pdir, persona as ppers};
use rabbithole_proto::{ErrorCode, Frame, FrameKind, ProtocolVersion, PROTOCOL_VERSION};
use rabbithole_proto::{Hello, HelloAck};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{
    AuthError, AuthedUser, Caps, PresenceEntry, Role, ServerEvent, Subject,
};

use crate::Shared;

/// Mutable per-session state shared by all request handlers.
pub struct SessionCtx {
    pub session_id: u64,
    pub account_id: i64,
    pub login: String,
    pub role: Role,
    pub class_id: Option<i64>,
    pub grant_mask: u64,
    pub revoke_mask: u64,
    pub persona_id: i64,
    pub screen_name: String,
    pub agreed: bool,
    pub is_guest: bool,
    /// The client's portable identity public key from the handshake, if any.
    pub pubkey: Option<[u8; 32]>,
}

impl SessionCtx {
    /// Build the subject with the **current** class mask — the live-
    /// inheritance mechanism: a `ClassSet` applies on the next check.
    pub fn subject(&self, shared: &Shared) -> Subject {
        Subject {
            account_id: self.account_id,
            role: self.role,
            class_id: self.class_id,
            class_mask: shared.classes.mask(self.class_id),
            grant_mask: self.grant_mask,
            revoke_mask: self.revoke_mask,
        }
    }

    pub fn allows(&self, shared: &Shared, resource: &str, needed: Caps) -> bool {
        shared.perms.allows(&self.subject(shared), resource, needed)
    }
}

pub async fn run_session(
    mut conn: Box<dyn Connection>,
    session_id: u64,
    shared: Arc<Shared>,
) -> anyhow::Result<()> {
    let peer = conn.peer().clone();
    let peer_ip = peer.remote_addr.ip();
    tracing::info!(session_id, remote = %peer.remote_addr, transport = %peer.transport, "connection");

    // ---- AwaitHello / AwaitAuth ----------------------------------------
    let mut negotiated: Option<ProtocolVersion> = None;
    // The client's portable identity key, from the handshake — surfaced in
    // presence so peers can verify who's who across burrows.
    let mut client_pubkey: Option<[u8; 32]> = None;
    let authed: AuthedUser;
    let mut replay_cursor: u64 = 0;
    let mut resumed = false;

    loop {
        let Some(frame) = conn.recv().await? else {
            return Ok(()); // peer went away before authenticating
        };
        if frame.kind != FrameKind::Request {
            continue;
        }

        if let Some(hello) = frame.decode::<Hello>() {
            let Ok(hello) = hello else {
                conn.send(Frame::error_reply(&frame, ErrorCode::BadRequest))
                    .await?;
                continue;
            };
            match ProtocolVersion::negotiate(PROTOCOL_VERSION, hello.version) {
                Some(v) => {
                    negotiated = Some(v);
                    client_pubkey = hello.client_pubkey;
                    let cfg = shared.config.read();
                    let ack = HelloAck::new(
                        v,
                        rabbithole_proto::CapabilitySet(vec![
                            rabbithole_proto::Capability::new(
                                rabbithole_proto::hello::caps::SESSION_RESUME,
                            ),
                            rabbithole_proto::Capability::new(rabbithole_proto::hello::caps::GUEST),
                        ]),
                        cfg.name,
                        env!("CARGO_PKG_VERSION"),
                        shared.server_key,
                    );
                    conn.send(Frame::reply_to(&frame, &ack)?).await?;
                }
                None => {
                    conn.send(Frame::error_reply(&frame, ErrorCode::VersionMismatch))
                        .await?;
                }
            }
            continue;
        }

        if negotiated.is_none() {
            // Auth before hello is a protocol violation.
            conn.send(Frame::error_reply(&frame, ErrorCode::BadRequest))
                .await?;
            continue;
        }

        if frame.decode::<psess::Ping>().is_some() {
            conn.send(Frame::reply_to(&frame, &psess::Pong)?).await?;
            continue;
        }

        let attempt: Option<Result<AuthedUser, AuthError>> =
            if let Some(Ok(req)) = frame.decode::<psess::AuthPassword>() {
                Some(
                    shared
                        .auth
                        .login_password(&req.login, &req.password, req.totp.as_deref())
                        .await,
                )
            } else if let Some(Ok(req)) = frame.decode::<psess::AuthGuest>() {
                let guests = shared.config.read().guest_enabled;
                Some(
                    shared
                        .auth
                        .login_guest(guests, req.desired_name.as_deref())
                        .await,
                )
            } else if let Some(Ok(req)) = frame.decode::<psess::AuthResume>() {
                replay_cursor = req.replay_cursor;
                resumed = true;
                Some(shared.auth.login_resume(&req.token).await)
            } else if let Some(Ok(req)) = frame.decode::<ppers::Register>() {
                let mode = shared.registration_mode();
                Some(
                    shared
                        .auth
                        .register(mode, &req.login, &req.password, req.invite_code.as_deref())
                        .await,
                )
            } else {
                None
            };

        match attempt {
            Some(Ok(user)) => {
                let ok = psess::AuthOk::new(
                    user.token.as_ref().map(|t| t.encode()).unwrap_or_default(),
                    user.account.id,
                    user.persona.screen_name.clone(),
                    user.account.role,
                    shared.perms.effective(&user.subject, ""),
                    resumed,
                );
                conn.send(Frame::reply_to(&frame, &ok)?).await?;
                authed = user;
                break;
            }
            Some(Err(e)) => {
                let code = match &e {
                    AuthError::GuestsDisabled | AuthError::Disabled => ErrorCode::Forbidden,
                    AuthError::SessionExpired => ErrorCode::SessionExpired,
                    AuthError::TotpRequired => ErrorCode::TotpRequired,
                    AuthError::RegistrationClosed | AuthError::BadInvite => ErrorCode::Forbidden,
                    AuthError::LoginTaken => ErrorCode::AlreadyExists,
                    _ => ErrorCode::Unauthenticated,
                };
                tracing::debug!(session_id, "auth failed: {e}");
                conn.send(Frame::error_reply(&frame, code)).await?;
                resumed = false;
                // Failed attempts drain the per-IP auth budget; once it is
                // empty the connection is closed (fresh connections then get
                // one refused attempt each until the bucket refills).
                if !shared.rate_allow(Scope::Ip(peer_ip), rl::AUTH) {
                    tracing::debug!(session_id, "auth rate limited; closing");
                    conn.close().await;
                    return Ok(());
                }
            }
            None => {
                conn.send(Frame::error_reply(&frame, ErrorCode::Unauthenticated))
                    .await?;
            }
        }
    }

    // ---- Active ---------------------------------------------------------
    let cfg = shared.config.read();
    let agreement = (!cfg.agreement.is_empty()).then(|| cfg.agreement.clone());
    let mut ctx = SessionCtx {
        session_id,
        account_id: authed.account.id,
        login: authed.account.login.clone(),
        role: Role::from_ordinal(authed.account.role),
        class_id: authed.account.class_id,
        grant_mask: authed.account.grant_mask,
        revoke_mask: authed.account.revoke_mask,
        persona_id: authed.persona.id,
        screen_name: authed.persona.screen_name.clone(),
        agreed: agreement.is_none(),
        is_guest: authed.token.is_none(),
        pubkey: client_pubkey,
    };
    let motd = cfg.motd.clone();
    drop(cfg);

    shared.presence.join(PresenceEntry {
        session_id,
        account_id: ctx.account_id,
        screen_name: ctx.screen_name.clone(),
        role: ctx.role,
        transport: peer.transport.to_string(),
        connected_at: Instant::now(),
        state: 0,
        status: None,
        pubkey: ctx.pubkey,
    });
    shared.chat.join_lobby(session_id, &ctx.screen_name);
    // Subscribe BEFORE welcome/replay so no event falls in a gap.
    let mut bus_rx = shared.bus.subscribe();

    // Replay pushes missed while disconnected (token resume) BEFORE the
    // fresh Welcome — the new Welcome gets a higher sequence and must not
    // appear in its own replay.
    if resumed && replay_cursor > 0 {
        for missed in shared.pushlog.since(ctx.account_id, replay_cursor) {
            conn.send(missed).await?;
        }
    }

    let welcome = Frame::push(&psess::Welcome::new(motd, agreement))?;
    conn.send(shared.pushlog.stamp(ctx.account_id, welcome))
        .await?;

    // Offline mail call: deliver unread DMs (capped; the rest via DmThreads).
    if !ctx.is_guest {
        use rabbithole_store_server::repo3::DmsRepo;
        let unread = DmsRepo(&shared.pool).unread_for(ctx.account_id).await?;
        for row in unread.iter().take(100) {
            let push = Frame::push(&rabbithole_proto::dm::DmReceived::new(
                crate::handlers3::dm_row_to_message(row),
            ))?;
            conn.send(shared.pushlog.stamp(ctx.account_id, push))
                .await?;
        }
    }

    // Bulk-transfer accept loop (QUIC only): dedicated streams carry file
    // bytes off the control channel. Each accepted stream is served
    // concurrently; the task ends when the connection closes.
    let bulk_task = conn.bulk().map(|bulk| {
        let shared = shared.clone();
        let account_id = ctx.account_id;
        tokio::spawn(async move {
            while let Ok((send, recv)) = bulk.accept().await {
                tokio::spawn(crate::handlers9::serve_bulk_stream(
                    shared.clone(),
                    account_id,
                    send,
                    recv,
                ));
            }
        })
    });

    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                incoming = conn.recv() => {
                    let Some(frame) = incoming? else { break };
                    if frame.kind != FrameKind::Request {
                        continue;
                    }
                    handle_request(&mut conn, &frame, &shared, &mut ctx).await?;
                }
                event = bus_rx.recv() => {
                    use tokio::sync::broadcast::error::RecvError;
                    match event {
                        Ok(ServerEvent::Kick { session_id: target, reason }) if target == session_id => {
                            let notice = Frame::push(&psess::ServerNotice::new(
                                format!("disconnected by operator: {reason}"),
                                "server",
                            ))?;
                            let _ = conn.send(notice).await;
                            break;
                        }
                        Ok(ev) => {
                            if let Some(push) = push_for_event(&ev, &shared, ctx.role, ctx.account_id, ctx.session_id) {
                                conn.send(shared.pushlog.stamp(ctx.account_id, push)).await?;
                            }
                            if matches!(ev, ServerEvent::Shutdown) {
                                break;
                            }
                        }
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!(session_id, missed = n, "session lagged behind the bus");
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    if let Some(task) = bulk_task {
        task.abort();
    }
    // Retire any transfer tickets this session left open (abandoned downloads
    // or half-finished uploads) and delete their staging files.
    for path in shared.transfers.close_session(session_id) {
        let _ = tokio::fs::remove_file(path).await;
    }
    // A disconnected peer can't serve swarm bytes: drop its advertisements.
    shared.swarm.session_closed(session_id);
    shared.chat.session_closed(session_id);
    shared.presence.leave(session_id);
    conn.close().await;
    tracing::info!(session_id, "session ended");
    result
}

async fn handle_request(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<()> {
    // Session family -------------------------------------------------------
    if frame.decode::<psess::Ping>().is_some() {
        conn.send(Frame::reply_to(frame, &psess::Pong)?).await?;
        return Ok(());
    }
    if frame.decode::<psess::AgreementAccept>().is_some() {
        ctx.agreed = true;
        conn.send(Frame::ack(frame)).await?;
        return Ok(());
    }
    if frame.decode::<Hello>().is_some()
        || frame.decode::<psess::AuthPassword>().is_some()
        || frame.decode::<psess::AuthGuest>().is_some()
        || frame.decode::<psess::AuthResume>().is_some()
        || frame.decode::<ppers::Register>().is_some()
    {
        // Re-hello / re-auth on a live session is a protocol violation.
        conn.send(Frame::error_reply(frame, ErrorCode::BadRequest))
            .await?;
        return Ok(());
    }

    // Presence family --------------------------------------------------------
    if frame.decode::<ppres::Who>().is_some() {
        if !ctx.allows(shared, "", Caps::WHO) {
            conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                .await?;
            return Ok(());
        }
        let viewer_is_mod = ctx.role >= Role::Moderator;
        let users = shared
            .presence
            .snapshot()
            .into_iter()
            .filter(|e| !e.is_invisible() || viewer_is_mod || e.session_id == ctx.session_id)
            .map(|e| {
                let state = ppres::PresenceState::from_ordinal(e.state);
                let status = e.status.clone();
                ppres::UserSummary::new(
                    e.session_id,
                    e.screen_name,
                    e.role as u8,
                    e.transport,
                    e.connected_at.elapsed().as_secs(),
                )
                .with_state(state, status)
                .with_pubkey(e.pubkey)
            })
            .collect();
        conn.send(Frame::reply_to(frame, &ppres::WhoList::new(users))?)
            .await?;
        return Ok(());
    }

    // Chat family -----------------------------------------------------------
    if let Some(req) = frame.decode::<pchat::ChatSend>() {
        let Ok(req) = req else {
            conn.send(Frame::error_reply(frame, ErrorCode::BadRequest))
                .await?;
            return Ok(());
        };
        if !ctx.agreed {
            conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                .await?;
            return Ok(());
        }
        // Per-account send budget: refuse the line, keep the session.
        if !shared.rate_allow(Scope::Account(ctx.account_id), rl::MSG) {
            conn.send(Frame::error_reply(frame, ErrorCode::RateLimited))
                .await?;
            return Ok(());
        }
        let resource = format!("chat/{}", req.room);
        if !ctx.allows(shared, &resource, Caps::CHAT_SEND) {
            conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                .await?;
            return Ok(());
        }
        use rabbithole_server_core::chat::{ChatError, Sender};
        let sender = Sender {
            session_id: ctx.session_id,
            account_id: ctx.account_id,
            is_moderator: ctx.allows(shared, "chat", Caps::CHAT_MODERATE),
            screen_name: &ctx.screen_name,
        };
        match shared.chat.send(
            &req.room,
            sender,
            &req.text,
            rabbithole_server_core::ratelimit::now_ms(),
        ) {
            Ok(_) => conn.send(Frame::ack(frame)).await?,
            Err(ChatError::NoSuchRoom(_)) => {
                conn.send(Frame::error_reply(frame, ErrorCode::NotFound))
                    .await?
            }
            Err(ChatError::NotMember | ChatError::Forbidden) => {
                conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                    .await?
            }
            Err(ChatError::TooLong { .. }) => {
                conn.send(Frame::error_reply(frame, ErrorCode::TooLarge))
                    .await?
            }
            // Room moderation refusals (Wave 13) are distinct from the
            // global RateLimited budget above; slow-mode carries its
            // retry-after inside the error code.
            Err(ChatError::Muted) => {
                conn.send(Frame::error_reply(frame, ErrorCode::Muted))
                    .await?
            }
            Err(ChatError::SlowMode { retry_after_secs }) => {
                conn.send(Frame::error_reply(
                    frame,
                    ErrorCode::SlowMode { retry_after_secs },
                ))
                .await?
            }
            Err(_) => {
                conn.send(Frame::error_reply(frame, ErrorCode::BadRequest))
                    .await?
            }
        }
        return Ok(());
    }
    if let Some(Ok(req)) = frame.decode::<pchat::ChatHistoryRequest>() {
        if !ctx.allows(shared, &format!("chat/{}", req.room), Caps::CHAT_READ) {
            conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                .await?;
            return Ok(());
        }
        match shared
            .chat
            .history(&req.room, ctx.session_id, req.limit.min(500) as usize)
        {
            Ok(lines) => {
                let messages = lines
                    .into_iter()
                    .map(|l| pchat::ChatMessage::new(l.room, l.from, l.text, l.at_unix_ms))
                    .collect();
                conn.send(Frame::reply_to(frame, &pchat::ChatHistory::new(messages))?)
                    .await?;
            }
            Err(_) => {
                conn.send(Frame::error_reply(frame, ErrorCode::NotFound))
                    .await?
            }
        }
        return Ok(());
    }

    // Wave 2 families (personas, directory, blobs, admin) -------------------
    if crate::handlers2::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 2.2 families (presence states, buddies, DMs) ----------------------
    if crate::handlers3::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 2.2b: rooms --------------------------------------------------------
    if crate::handlers4::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 2.3: welcome screen, theme, keyword nav ----------------------------
    if crate::handlers5::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 3.1: message bases -------------------------------------------------
    if crate::handlers6::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 3.2: the Wishing Well ---------------------------------------------
    if crate::handlers7::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 4.1: file libraries ------------------------------------------------
    if crate::handlers8::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 4.2: bulk transfers ------------------------------------------------
    if crate::handlers9::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 5: the swarm coordinator ---------------------------------------------
    if crate::handlers10::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 13: the moderation suite ---------------------------------------------
    if crate::handlers11::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    // Wave 8: server theme-bundle application ------------------------------------
    if crate::handlers12::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }
    if crate::handlers13::handle(conn, frame, shared, ctx).await? {
        return Ok(());
    }

    // Anything else: tolerated, answered, never fatal.
    conn.send(Frame::error_reply(frame, ErrorCode::Unsupported))
        .await?;
    Ok(())
}

/// Project a bus event into a push frame for a specific viewer
/// (None = nothing to send). Viewer-aware because Cheshire mode hides
/// arrivals/changes from sub-moderators, and DMs are targeted.
pub(crate) fn push_for_event(
    event: &ServerEvent,
    shared: &Shared,
    viewer_role: Role,
    viewer_account: i64,
    viewer_session: u64,
) -> Option<Frame> {
    let viewer_is_mod = viewer_role >= Role::Moderator;
    match event {
        ServerEvent::Chat { room, from, text } => {
            // Lobby chat is for everyone (including offline-replay); other
            // rooms deliver to members only.
            if room != rabbithole_server_core::LOBBY && !shared.chat.is_member(room, viewer_session)
            {
                return None;
            }
            Frame::push(&pchat::ChatMessage::new(
                room.clone(),
                from.clone(),
                text.clone(),
                chrono::Utc::now().timestamp_millis(),
            ))
            .ok()
        }
        ServerEvent::SessionOpened { session_id, .. } => {
            let entry = shared.presence.get(*session_id)?;
            if entry.is_invisible() && !viewer_is_mod {
                return None;
            }
            Frame::push(&ppres::UserJoined::new(
                ppres::UserSummary::new(
                    entry.session_id,
                    entry.screen_name,
                    entry.role as u8,
                    entry.transport,
                    0,
                )
                .with_pubkey(entry.pubkey),
            ))
            .ok()
        }
        ServerEvent::SessionClosed {
            session_id,
            screen_name,
            was_invisible,
        } => {
            if *was_invisible && !viewer_is_mod {
                return None;
            }
            Frame::push(&ppres::UserLeft::new(*session_id, screen_name.clone())).ok()
        }
        ServerEvent::SessionChanged {
            session_id,
            screen_name,
        } => Frame::push(&pdir::UserChanged::new(*session_id, screen_name.clone())).ok(),
        ServerEvent::PresenceChanged {
            session_id,
            screen_name,
            state,
            status,
            was_invisible,
        } => {
            let now_invisible = *state == 3;
            if viewer_is_mod {
                let s = ppres::PresenceState::from_ordinal(*state);
                return Frame::push(
                    &pdir::UserChanged::new(*session_id, screen_name.clone())
                        .with_state(s, status.clone()),
                )
                .ok();
            }
            match (*was_invisible, now_invisible) {
                // Vanishing: sub-moderators see them leave.
                (false, true) => {
                    Frame::push(&ppres::UserLeft::new(*session_id, screen_name.clone())).ok()
                }
                // Reappearing: they "arrive".
                (true, false) => {
                    let entry = shared.presence.get(*session_id)?;
                    Frame::push(&ppres::UserJoined::new(
                        ppres::UserSummary::new(
                            entry.session_id,
                            entry.screen_name,
                            entry.role as u8,
                            entry.transport,
                            entry.connected_at.elapsed().as_secs(),
                        )
                        .with_state(ppres::PresenceState::from_ordinal(*state), status.clone())
                        .with_pubkey(entry.pubkey),
                    ))
                    .ok()
                }
                (true, true) => None,
                (false, false) => {
                    let s = ppres::PresenceState::from_ordinal(*state);
                    Frame::push(
                        &pdir::UserChanged::new(*session_id, screen_name.clone())
                            .with_state(s, status.clone()),
                    )
                    .ok()
                }
            }
        }
        ServerEvent::Dm {
            to_account,
            message,
        } => {
            if *to_account != viewer_account {
                return None;
            }
            Frame::push(&rabbithole_proto::dm::DmReceived::new(message.clone())).ok()
        }
        ServerEvent::DmRead {
            to_account,
            by,
            up_to_id,
        } => {
            if *to_account != viewer_account {
                return None;
            }
            Frame::push(&rabbithole_proto::dm::DmReadReceipt::new(
                by.clone(),
                *up_to_id,
            ))
            .ok()
        }
        ServerEvent::RoomInvited {
            to_account,
            room,
            from,
        } => {
            if *to_account != viewer_account {
                return None;
            }
            Frame::push(&pchat::RoomInvited::new(room.clone(), from.clone())).ok()
        }
        ServerEvent::RoomKicked {
            account,
            room,
            banned,
        } => {
            if *account != viewer_account {
                return None;
            }
            Frame::push(&pchat::RoomKicked::new(room.clone(), *banned)).ok()
        }
        // Mute / slow-mode changes go to room members, lobby to everyone —
        // the same scoping as chat lines (the room string is client-cased,
        // hence the case-insensitive lobby test).
        ServerEvent::RoomMuted {
            screen_name,
            room,
            muted,
            duration_secs,
            ..
        } => {
            if !room.eq_ignore_ascii_case(rabbithole_server_core::LOBBY)
                && !shared.chat.is_member(room, viewer_session)
            {
                return None;
            }
            Frame::push(&pchat::RoomMuted::new(
                room.clone(),
                screen_name.clone(),
                *muted,
                *duration_secs,
            ))
            .ok()
        }
        ServerEvent::RoomSlowModeChanged { room, seconds, by } => {
            if !room.eq_ignore_ascii_case(rabbithole_server_core::LOBBY)
                && !shared.chat.is_member(room, viewer_session)
            {
                return None;
            }
            Frame::push(&pchat::RoomSlowModeChanged::new(
                room.clone(),
                *seconds,
                by.clone(),
            ))
            .ok()
        }
        ServerEvent::BoardPost { .. } => crate::handlers6::board_push(event),
        ServerEvent::FileAdded { .. } => crate::handlers8::file_push(event),
        ServerEvent::WishUpdated { to_account, wish } => {
            if *to_account != viewer_account {
                return None;
            }
            Frame::push(&rabbithole_proto::wish::WishUpdated::new(wish.clone())).ok()
        }
        ServerEvent::Notice { text, from } => {
            Frame::push(&psess::ServerNotice::new(text.clone(), from.clone())).ok()
        }
        // Moderator-only notices (new report filed, …).
        ServerEvent::ModNotice { text } => {
            if !viewer_is_mod {
                return None;
            }
            Frame::push(&psess::ServerNotice::new(text.clone(), "moderation")).ok()
        }
        // Radio now-playing / sign-off ride the typed RADIO family
        // (`RadioNowPlaying` / `RadioOff`). Ephemeral status is pointless after
        // the fact, so the offline-replay recorder (viewer_session 0 — real
        // sessions start at 1) skips both.
        ServerEvent::RadioNowPlaying {
            station,
            title,
            artist,
            dj,
            listeners,
        } => {
            if viewer_session == 0 {
                return None;
            }
            let live = shared
                .presence
                .radio_status(station)
                .map(|s| s.live)
                .unwrap_or(false);
            Frame::push(&pradio::RadioNowPlaying::new(
                station.clone(),
                title.clone(),
                artist.clone(),
                dj.clone(),
                *listeners as u32,
                live,
            ))
            .ok()
        }
        ServerEvent::RadioOff { station } => {
            if viewer_session == 0 {
                return None;
            }
            Frame::push(&pradio::RadioOff::new(station.clone())).ok()
        }
        _ => None,
    }
}
