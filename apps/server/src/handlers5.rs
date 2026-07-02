//! Wave 2.3 handlers: welcome screen, theme bundle, keyword teleport.

use std::sync::Arc;

use rabbithole_blobs::BlobId;
use rabbithole_identity::keys::IdentityKey;
use rabbithole_net::Connection;
use rabbithole_proto::welcome as pw;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_store_server::repo2::PersonasRepo;
use rabbithole_store_server::repo3::DmsRepo;

use crate::session::SessionCtx;
use crate::Shared;

/// Build the composed welcome screen for a session.
async fn build_welcome(shared: &Shared, ctx: &SessionCtx) -> pw::WelcomeScreen {
    let cfg = shared.config.read();
    let mut widgets = Vec::new();
    if !cfg.motd.is_empty() {
        widgets.push(pw::WelcomeWidget::Motd(cfg.motd.clone()));
    }
    // Unread DMs (accounts only).
    if !ctx.is_guest {
        if let Ok(unread) = DmsRepo(&shared.pool).unread_for(ctx.account_id).await {
            if !unread.is_empty() {
                widgets.push(pw::WelcomeWidget::UnreadDms(unread.len() as u64));
            }
        }
    }
    // Who's on now — a sample, honoring Cheshire mode for non-moderators.
    let viewer_is_mod = ctx.role >= rabbithole_server_core::Role::Moderator;
    let visible: Vec<String> = shared
        .presence
        .snapshot()
        .into_iter()
        .filter(|e| !e.is_invisible() || viewer_is_mod || e.session_id == ctx.session_id)
        .map(|e| e.screen_name)
        .collect();
    widgets.push(pw::WelcomeWidget::OnlineNow {
        count: visible.len() as u32,
        sample: visible.into_iter().take(8).collect(),
    });
    if !cfg.welcome_featured.is_empty() {
        let (title, body) = cfg
            .welcome_featured
            .split_once('\n')
            .unwrap_or((cfg.welcome_featured.as_str(), ""));
        widgets.push(pw::WelcomeWidget::Featured {
            title: title.to_string(),
            body: body.to_string(),
        });
    }
    if !cfg.welcome_ticker.is_empty() {
        widgets.push(pw::WelcomeWidget::Ticker(cfg.welcome_ticker.clone()));
    }
    pw::WelcomeScreen::new(widgets)
}

/// Build the signed theme bundle (None when the server has no theme set).
fn build_theme(shared: &Shared) -> Option<pw::ThemeReply> {
    let cfg = shared.config.read();
    let accent = (!cfg.theme_accent.is_empty())
        .then(|| hex::decode(&cfg.theme_accent).ok())
        .flatten()
        .and_then(|v| <[u8; 3]>::try_from(v).ok());
    let logo = (!cfg.theme_logo_ansi.is_empty()).then(|| cfg.theme_logo_ansi.clone());
    if accent.is_none() && logo.is_none() {
        return None;
    }
    let mut bundle = pw::ThemeBundle::new(cfg.name.clone());
    bundle.accent_rgb = accent;
    bundle.logo_ansi = logo;
    drop(cfg);
    let bytes = postcard::to_allocvec(&bundle).ok()?;
    let key = IdentityKey::from_seed(&shared.server_signing_seed);
    let sig = key.sign(&bytes);
    Some(pw::ThemeReply::new(bytes, sig.0.to_vec()))
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

    if frame.decode::<pw::WelcomeScreenRequest>().is_some() {
        let screen = build_welcome(shared, ctx).await;
        reply!(&screen);
        return Ok(true);
    }

    if frame.decode::<pw::ThemeGet>().is_some() {
        match build_theme(shared) {
            Some(theme) => reply!(&theme),
            None => {
                conn.send(Frame::error_reply(frame, ErrorCode::NotFound))
                    .await?
            }
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pw::KeywordGo>() {
        let target = resolve_keyword(shared, &req.word).await?;
        reply!(&target);
        return Ok(true);
    }

    Ok(false)
}

/// Resolve a keyword: operator map first, then a live room, then an online
/// or known persona, else Unknown.
async fn resolve_keyword(shared: &Shared, word: &str) -> anyhow::Result<pw::KeywordTarget> {
    let word = word.trim();
    let lower = word.to_lowercase();

    // 1. Operator-configured keyword map.
    if let Some(mapped) = shared.config.read().keywords.get(&lower).cloned() {
        if let Some(room) = mapped.strip_prefix("room:") {
            return Ok(pw::KeywordTarget::new(
                pw::KeywordKind::Room,
                room.to_string(),
            ));
        }
        if let Some(user) = mapped.strip_prefix("user:") {
            return Ok(pw::KeywordTarget::new(
                pw::KeywordKind::User,
                user.to_string(),
            ));
        }
        if let Some(url) = mapped.strip_prefix("url:") {
            return Ok(pw::KeywordTarget::new(
                pw::KeywordKind::Url,
                url.to_string(),
            ));
        }
    }

    // 2. A room by that name (any lister sees at least public rooms).
    if shared
        .chat
        .list(0, -1)
        .iter()
        .any(|r| r.name.eq_ignore_ascii_case(word))
    {
        return Ok(pw::KeywordTarget::new(
            pw::KeywordKind::Room,
            word.to_string(),
        ));
    }

    // 3. A persona (online or in the directory).
    if shared.presence.is_screen_name_online(word).is_some()
        || PersonasRepo(&shared.pool)
            .by_screen_name(word)
            .await?
            .is_some()
    {
        return Ok(pw::KeywordTarget::new(
            pw::KeywordKind::User,
            word.to_string(),
        ));
    }

    Ok(pw::KeywordTarget::new(
        pw::KeywordKind::Unknown,
        word.to_string(),
    ))
}

/// Verify a theme bundle's signature against the server key — the client
/// side of the trust check, exposed here for the e2e test.
pub fn verify_theme(reply: &pw::ThemeReply, server_key: &[u8; 32]) -> Option<pw::ThemeBundle> {
    let sig: [u8; 64] = reply.signature.as_slice().try_into().ok()?;
    let pk = rabbithole_identity::PublicKey(*server_key);
    if !pk.verify(&reply.bundle, &rabbithole_identity::Signature(sig)) {
        return None;
    }
    postcard::from_bytes(&reply.bundle).ok()
}

/// Suppress unused import warnings until BlobId is used by theme banners.
#[allow(dead_code)]
fn _blob_touch(id: [u8; 32]) -> BlobId {
    BlobId(id)
}
