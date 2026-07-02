//! Wave 2 request handlers: personas, directory, blobs, TOTP/keys, admin.
//!
//! Called from the session loop after the Wave 1 families; returns
//! `Ok(false)` when the frame wasn't for us.

use std::sync::Arc;

use rabbithole_blobs::BlobId;
use rabbithole_identity::totp::{generate_recovery_codes, TotpEnrollment};
use rabbithole_identity::verify_password;
use rabbithole_net::Connection;
use rabbithole_proto::{admin as padm, blob as pblob, directory as pdir, persona as ppers};
use rabbithole_proto::{session as psess, ErrorCode, Frame};
use rabbithole_server_core::{Caps, Role, ServerEvent};
use rabbithole_store_server::repo::{AccountsRepo, AuditRepo, ClassesRepo};
use rabbithole_store_server::repo2::{InvitesRepo, KeysRepo, PersonaRow, PersonasRepo, TotpRepo};

use crate::session::SessionCtx;
use crate::Shared;

fn persona_info(row: &PersonaRow) -> ppers::PersonaInfo {
    let mut info = ppers::PersonaInfo::new(row.id, row.screen_name.clone());
    info.is_default = row.is_default;
    info.profile = ppers::Profile::new(
        row.location.clone(),
        row.interests.clone(),
        row.quote.clone(),
        row.plan.clone(),
        row.pronouns.clone(),
    );
    info.avatar = row
        .avatar_hex
        .as_deref()
        .and_then(BlobId::from_hex)
        .map(|b| b.0);
    info.banner = row
        .banner_hex
        .as_deref()
        .and_then(BlobId::from_hex)
        .map(|b| b.0);
    info.directory_visible = row.directory_visible;
    info
}

fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
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
    macro_rules! guests_cannot {
        () => {
            if ctx.is_guest {
                fail!(ErrorCode::Forbidden)
            }
        };
    }

    // ---- Personas (session family) --------------------------------------
    if frame.decode::<ppers::PersonaListRequest>().is_some() {
        guests_cannot!();
        let rows = PersonasRepo(&shared.pool)
            .for_account(ctx.account_id)
            .await?;
        let personas = rows.iter().map(persona_info).collect();
        reply!(&ppers::PersonaList::new(personas, ctx.persona_id));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::PersonaCreate>() {
        guests_cannot!();
        let repo = PersonasRepo(&shared.pool);
        let max = shared.config.read().persona_max as i64;
        if repo.count_for_account(ctx.account_id).await? >= max {
            fail!(ErrorCode::Forbidden);
        }
        let name = req.screen_name.trim();
        if name.is_empty() || name.len() > 32 {
            fail!(ErrorCode::BadRequest);
        }
        if repo.by_screen_name(name).await?.is_some()
            || AccountsRepo(&shared.pool).by_login(name).await?.is_some()
        {
            fail!(ErrorCode::AlreadyExists);
        }
        let row = repo.create(ctx.account_id, name, false).await?;
        reply!(&ppers::PersonaReply::new(persona_info(&row)));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::PersonaUpdate>() {
        guests_cannot!();
        let repo = PersonasRepo(&shared.pool);
        let Some(row) = repo.by_id(req.id).await? else {
            fail!(ErrorCode::NotFound)
        };
        if row.account_id != ctx.account_id {
            fail!(ErrorCode::Forbidden);
        }
        // Referenced blobs must exist (uploaded via BlobPut first).
        for blob in [req.avatar.flatten(), req.banner.flatten()]
            .into_iter()
            .flatten()
        {
            if !shared.blobs.contains(&BlobId(blob)) {
                fail!(ErrorCode::NotFound);
            }
        }
        let p = req.profile.unwrap_or_default();
        let avatar_hex = req.avatar.map(|o| o.map(|b| BlobId(b).to_hex()));
        let banner_hex = req.banner.map(|o| o.map(|b| BlobId(b).to_hex()));
        repo.update(
            req.id,
            p.location.as_deref(),
            p.interests.as_deref(),
            p.quote.as_deref(),
            p.plan.as_deref(),
            p.pronouns.as_deref(),
            avatar_hex.as_ref().map(|o| o.as_deref()),
            banner_hex.as_ref().map(|o| o.as_deref()),
            req.directory_visible,
        )
        .await?;
        let row = repo.by_id(req.id).await?.expect("still there");
        if row.id == ctx.persona_id {
            shared.presence.rename(ctx.session_id, &row.screen_name);
            shared
                .swarm
                .rename_session(ctx.session_id, &row.screen_name);
        }
        reply!(&ppers::PersonaReply::new(persona_info(&row)));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::PersonaDelete>() {
        guests_cannot!();
        if req.id == ctx.persona_id {
            fail!(ErrorCode::BadRequest); // switch away first
        }
        if PersonasRepo(&shared.pool)
            .delete(req.id, ctx.account_id)
            .await?
        {
            conn.send(Frame::ack(frame)).await?;
        } else {
            fail!(ErrorCode::Forbidden); // last/default persona or not yours
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::PersonaSwitch>() {
        guests_cannot!();
        let Some(row) = PersonasRepo(&shared.pool).by_id(req.id).await? else {
            fail!(ErrorCode::NotFound)
        };
        if row.account_id != ctx.account_id {
            fail!(ErrorCode::Forbidden);
        }
        ctx.persona_id = row.id;
        ctx.screen_name = row.screen_name.clone();
        shared.presence.rename(ctx.session_id, &row.screen_name);
        shared
            .swarm
            .rename_session(ctx.session_id, &row.screen_name);
        reply!(&ppers::PersonaReply::new(persona_info(&row)));
        return Ok(true);
    }

    // ---- TOTP + keys (session family) ------------------------------------
    if frame.decode::<ppers::TotpEnrollBegin>().is_some() {
        guests_cannot!();
        let enrollment = TotpEnrollment::generate("RabbitHole", &ctx.login);
        TotpRepo(&shared.pool)
            .begin(ctx.account_id, enrollment.secret())
            .await?;
        reply!(&ppers::TotpEnrollInfo::new(
            enrollment.secret_base32(),
            enrollment.provisioning_url()
        ));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::TotpEnrollConfirm>() {
        guests_cannot!();
        let Some(row) = TotpRepo(&shared.pool).get(ctx.account_id).await? else {
            fail!(ErrorCode::BadRequest)
        };
        let enrollment = TotpEnrollment::from_secret(&row.secret, "RabbitHole", &ctx.login)
            .map_err(|e| anyhow::anyhow!("totp: {e}"))?;
        if !enrollment.verify(&req.code).unwrap_or(false) {
            fail!(ErrorCode::Unauthenticated);
        }
        let codes = generate_recovery_codes(8);
        let hashes: Vec<[u8; 32]> = codes.iter().map(|(_, h)| *h).collect();
        TotpRepo(&shared.pool)
            .confirm(ctx.account_id, &hashes)
            .await?;
        audit(shared, &ctx.login, "totp-enroll", String::new());
        reply!(&ppers::RecoveryCodes::new(
            codes.into_iter().map(|(c, _)| c).collect()
        ));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::TotpDisable>() {
        guests_cannot!();
        let account = AccountsRepo(&shared.pool).by_id(ctx.account_id).await?;
        let Some(phc) = account.as_ref().and_then(|a| a.phc.as_deref()) else {
            fail!(ErrorCode::Forbidden)
        };
        if !verify_password(&req.password, phc).unwrap_or(false) {
            fail!(ErrorCode::Unauthenticated);
        }
        TotpRepo(&shared.pool).remove(ctx.account_id).await?;
        audit(shared, &ctx.login, "totp-disable", String::new());
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<ppers::KeyEnroll>() {
        guests_cannot!();
        if KeysRepo(&shared.pool)
            .add(ctx.account_id, &req.pubkey)
            .await
            .is_err()
        {
            fail!(ErrorCode::AlreadyExists);
        }
        audit(shared, &ctx.login, "key-enroll", hex::encode(req.pubkey));
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Directory (presence family) --------------------------------------
    if let Some(Ok(req)) = frame.decode::<pdir::ProfileGet>() {
        let repo = PersonasRepo(&shared.pool);
        let Some(row) = repo.by_screen_name(&req.screen_name).await? else {
            fail!(ErrorCode::NotFound)
        };
        if !row.directory_visible {
            fail!(ErrorCode::NotFound); // hidden = indistinguishable from absent
        }
        let info = persona_info(&row);
        let mut card = pdir::ProfileCard::new(row.screen_name.clone(), info.profile);
        card.avatar = info.avatar;
        card.banner = info.banner;
        card.online_transport = shared
            .presence
            .is_screen_name_online(&row.screen_name)
            .map(|e| e.transport);
        reply!(&card);
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pdir::DirectorySearch>() {
        let limit = req.limit.clamp(1, 100) as i64;
        let rows = PersonasRepo(&shared.pool)
            .search(req.query.trim(), limit)
            .await?;
        reply!(&pdir::DirectoryResults::new(
            rows.iter().map(persona_info).collect()
        ));
        return Ok(true);
    }

    // ---- Blobs (file family) ----------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pblob::BlobPut>() {
        guests_cannot!();
        let cfg = shared.config.read();
        let max = match req.purpose {
            pblob::BlobPurpose::Avatar => cfg.avatar_max_bytes,
            pblob::BlobPurpose::Banner => cfg.banner_max_bytes,
            _ => cfg.banner_max_bytes,
        };
        drop(cfg);
        if req.bytes.len() > max {
            fail!(ErrorCode::TooLarge);
        }
        let blobs = shared.blobs.clone();
        let id = tokio::task::spawn_blocking(move || blobs.put(&req.bytes))
            .await?
            .map_err(|e| anyhow::anyhow!("blob: {e}"))?;
        reply!(&pblob::BlobRef::new(id.0));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pblob::BlobGet>() {
        let blobs = shared.blobs.clone();
        match tokio::task::spawn_blocking(move || blobs.get(&BlobId(req.id))).await? {
            Ok(bytes) => reply!(&pblob::BlobData::new(bytes)),
            Err(_) => fail!(ErrorCode::NotFound),
        }
        return Ok(true);
    }

    // ---- Admin family -------------------------------------------------------
    if frame.decode::<padm::ClassListRequest>().is_some() {
        if !ctx.allows(shared, "admin", Caps::ACCOUNT_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        let classes = ClassesRepo(&shared.pool).all().await?;
        let counts = ClassesRepo(&shared.pool).member_counts().await?;
        let entries = classes
            .into_iter()
            .map(|c| {
                let members = counts.get(&c.id).copied().unwrap_or(0);
                padm::ClassEntry::new(c.name, c.base_mask, members)
            })
            .collect();
        reply!(&padm::ClassList::new(entries));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::ClassSet>() {
        if !ctx.allows(shared, "admin", Caps::ACCOUNT_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        shared
            .classes
            .set(&shared.pool, &req.name, req.base_mask)
            .await?;
        audit(
            shared,
            &ctx.login,
            "class-set",
            format!("{}={:#x}", req.name, req.base_mask),
        );
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::AccountListRequest>() {
        if !ctx.allows(shared, "admin", Caps::ACCOUNT_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        let limit = req.limit.clamp(1, 200) as i64;
        let (rows, total) =
            crate::admin_store::list_accounts(&shared.pool, req.offset as i64, limit).await?;
        reply!(&padm::AccountList::new(rows, total));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::AccountSet>() {
        if !ctx.allows(shared, "admin", Caps::ACCOUNT_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        let changed = crate::admin_store::account_set(
            shared,
            &req.login,
            req.role,
            req.class.as_deref(),
            req.disabled,
        )
        .await?;
        if !changed {
            fail!(ErrorCode::NotFound);
        }
        audit(shared, &ctx.login, "account-set", format!("{req:?}"));
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::InviteCreate>() {
        if !ctx.allows(shared, "admin", Caps::ACCOUNT_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        let code = format!(
            "rh-{}",
            &rabbithole_identity::SessionToken::generate().encode()[..16]
        );
        let ttl = req.ttl_secs.clamp(60, 60 * 60 * 24 * 90);
        InvitesRepo(&shared.pool)
            .create(&code, ctx.account_id, ttl)
            .await?;
        audit(shared, &ctx.login, "invite-create", code.clone());
        reply!(&padm::InviteCode::new(
            code,
            chrono::Utc::now().timestamp() + ttl
        ));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::Broadcast>() {
        if !ctx.allows(shared, "admin", Caps::BROADCAST) {
            fail!(ErrorCode::Forbidden);
        }
        shared.bus.publish(ServerEvent::Notice {
            text: req.text.clone(),
            from: ctx.screen_name.clone(),
        });
        audit(shared, &ctx.login, "broadcast", req.text);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::Kick>() {
        if !ctx.allows(shared, "admin", Caps::USER_KICK) {
            fail!(ErrorCode::Forbidden);
        }
        let Some(target) = shared.presence.get(req.session_id) else {
            fail!(ErrorCode::NotFound)
        };
        // Respect CANNOT_BE_KICKED and role ordering (no kicking upwards).
        let target_role = target.role;
        if target_role >= ctx.role && ctx.role != Role::Superuser {
            fail!(ErrorCode::Forbidden);
        }
        shared.bus.publish(ServerEvent::Kick {
            session_id: req.session_id,
            reason: "kicked".into(),
        });
        audit(
            shared,
            &ctx.login,
            "kick",
            format!("session {}", req.session_id),
        );
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::ConfigGet>() {
        if !ctx.allows(shared, "admin", Caps::CONFIG_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        match shared.config.get_key(&req.key) {
            Ok(value) => reply!(&padm::ConfigValue::new(req.key, value)),
            Err(_) => fail!(ErrorCode::NotFound),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::ConfigSet>() {
        if !ctx.allows(shared, "admin", Caps::CONFIG_ADMIN) {
            fail!(ErrorCode::Forbidden);
        }
        match shared.config.set_key(&req.key, &req.value) {
            Ok(applied_live) => {
                audit(
                    shared,
                    &ctx.login,
                    "config-set",
                    format!("{}={}", req.key, req.value),
                );
                reply!(&padm::ConfigApplied::new(applied_live));
            }
            Err(_) => fail!(ErrorCode::BadRequest),
        }
        return Ok(true);
    }

    // Also let clients read the agreement state via a no-op: (none yet)

    let _ = psess::ServerNotice::new("", ""); // keep import used until more session pushes land
    Ok(false)
}
