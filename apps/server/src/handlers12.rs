//! Wave 8 handlers: server theme-bundle application (ADMIN family, types
//! 41..44).
//!
//! `ThemeBundleSet` validates hard before anything lands — structured
//! tokens only, WCAG contrast rails (reject below 4.5:1, ratio reported),
//! art size caps, v1 signature verification — because a server-applied
//! theme reaches every client. All three ops are gated on
//! [`Caps::CONFIG_ADMIN`] (theming is server configuration) and audited;
//! refusals are audited too, so the operator log carries the computed
//! contrast ratio that sank a bundle.

use std::sync::Arc;

use rabbithole_blobs::BlobId;
use rabbithole_net::Connection;
use rabbithole_proto::admin as padm;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::theme::{self, ThemeError, ThemeLimits};
use rabbithole_server_core::Caps;
use rabbithole_store_server::repo::AuditRepo;

use crate::session::SessionCtx;
use crate::Shared;

fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
}

fn map_err(e: &ThemeError) -> ErrorCode {
    match e {
        ThemeError::LogoTooLarge { .. }
        | ThemeError::BannerTooLarge { .. }
        | ThemeError::IconTooLarge { .. }
        | ThemeError::TooManyIcons { .. } => ErrorCode::TooLarge,
        ThemeError::BannerMissing | ThemeError::IconMissing { .. } => ErrorCode::NotFound,
        _ => ErrorCode::BadRequest,
    }
}

/// Summarize the applied theme for `ThemeBundleSet`/`ThemeBundleGet`
/// replies (all-default when no theme is set).
fn bundle_info(shared: &Shared) -> padm::ThemeBundleInfo {
    let cfg = shared.config.read();
    let mut info = padm::ThemeBundleInfo::default();
    if let Some(applied) = theme::applied_from_config(&cfg) {
        info.present = true;
        info.id = applied.id;
        info.name = applied.bundle.name.clone();
        info.applied_at_unix = cfg.theme_applied_at_unix;
        info.applied_by = cfg.theme_applied_by.clone();
        info.accent_rgb = applied.bundle.accent_rgb;
        info.has_logo = applied.bundle.logo_ansi.is_some();
        info.has_banner = applied.bundle.banner.is_some();
        info.icons = applied.bundle.icons.len() as u32;
        info.tokens_light = applied.bundle.tokens_light.len() as u32;
        info.tokens_dark = applied.bundle.tokens_dark.len() as u32;
        info.tokens_shared = applied.bundle.tokens_shared.len() as u32;
    }
    info
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
    macro_rules! config_admins_only {
        () => {
            if !ctx.allows(shared, "admin", Caps::CONFIG_ADMIN) {
                fail!(ErrorCode::Forbidden)
            }
        };
    }

    if let Some(Ok(req)) = frame.decode::<padm::ThemeBundleSet>() {
        config_admins_only!();
        let limits = ThemeLimits::from_config(&shared.config.read());
        let applied = theme::apply_theme_bundle(
            &req.bundle,
            &req.signature,
            &shared.server_key,
            &limits,
            |id| shared.blobs.size(&BlobId(*id)).ok(),
        );
        match applied {
            Ok(applied) => {
                let now = chrono::Utc::now().timestamp();
                shared
                    .config
                    .update(|c| theme::write_to_config(&applied, c, now, &ctx.login));
                audit(
                    shared,
                    &ctx.login,
                    "theme-set",
                    format!("{} id={}", applied.bundle.name, hex::encode(applied.id)),
                );
                reply!(&bundle_info(shared));
            }
            Err(e) => {
                // The refusal reason (including any computed contrast
                // ratio) lands in the audit log; the wire carries the code.
                audit(shared, &ctx.login, "theme-set-refused", e.to_string());
                fail!(map_err(&e));
            }
        }
        return Ok(true);
    }

    if frame.decode::<padm::ThemeBundleClear>().is_some() {
        config_admins_only!();
        shared.config.update(theme::clear_config);
        audit(shared, &ctx.login, "theme-clear", String::new());
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if frame.decode::<padm::ThemeBundleGet>().is_some() {
        config_admins_only!();
        reply!(&bundle_info(shared));
        return Ok(true);
    }

    Ok(false)
}
