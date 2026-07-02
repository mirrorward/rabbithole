//! Wave 5 handlers: the swarm coordinator surface (family 6).
//!
//! Peers advertise files they hold ([`ps::AdvertiseFiles`], gated by
//! `SWARM_ADVERTISE`) into the TTL'd soft-state
//! [`rabbithole_server_core::SwarmCatalog`]; anyone who can browse files
//! (`FILE_LIST`) can ask [`ps::FindSources`] who has a root. The reply also
//! says whether this server's own blob store holds the file, so a fetcher
//! can fall back to the origin when no peer is around. Adverts vanish on
//! session close (see `session.rs` teardown) or TTL lapse.

use std::sync::Arc;
use std::time::Duration;

use rabbithole_blobs::BlobId;
use rabbithole_net::Connection;
use rabbithole_proto::swarm as ps;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::swarm::NewAdvert;
use rabbithole_server_core::{Caps, Role};

use crate::session::SessionCtx;
use crate::Shared;

/// The permission resource for the whole swarm surface (operators can ACL
/// `swarm` like any other path).
const RESOURCE: &str = "swarm";

/// Most entries one `AdvertiseFiles` may carry — bounds the work done under
/// the catalog's write lock per request (re-announce in batches).
const ADVERT_BATCH_MAX: usize = 256;
/// Metadata length caps, so the count cap is also a memory bound and a
/// `SourceList` reply can never outgrow the 1 MiB control-frame limit.
const ADVERT_NAME_MAX: usize = 255;
const ADVERT_MIME_MAX: usize = 127;
/// Most sources one `SourceList` returns (more than any fetcher dials).
const SOURCES_MAX: usize = 200;
/// Granted TTL when the client asks for the default (`ttl_secs == 0`) and
/// the server has no configured maximum to fall back on.
const ADVERT_TTL_FALLBACK: u32 = 3600;

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

    // ---- Advertise (or re-announce) ---------------------------------------
    if let Some(Ok(req)) = frame.decode::<ps::AdvertiseFiles>() {
        if !ctx.allows(shared, RESOURCE, Caps::SWARM_ADVERTISE) {
            fail!(ErrorCode::Forbidden);
        }
        if req.entries.is_empty() {
            fail!(ErrorCode::BadRequest);
        }
        if req.entries.len() > ADVERT_BATCH_MAX {
            fail!(ErrorCode::TooLarge);
        }
        if req
            .entries
            .iter()
            .any(|e| e.name.len() > ADVERT_NAME_MAX || e.mime.len() > ADVERT_MIME_MAX)
        {
            fail!(ErrorCode::TooLarge);
        }
        let (max_ttl, max_adverts) = {
            let cfg = shared.config.read();
            (cfg.swarm_advert_ttl_secs, cfg.swarm_adverts_max)
        };
        // `ttl_secs == 0` asks for the server default; a configured max of 0
        // means "no maximum" (grant what was asked). Plain min/max arithmetic
        // — `clamp` would panic on a 0 max.
        let ttl_secs = match (req.ttl_secs, max_ttl) {
            (0, 0) => ADVERT_TTL_FALLBACK,
            (0, max) => max,
            (req_ttl, 0) => req_ttl,
            (req_ttl, max) => req_ttl.min(max),
        };
        let entries: Vec<NewAdvert> = req
            .entries
            .iter()
            .map(|e| NewAdvert {
                root: e.root,
                size: e.size,
                name: e.name.clone(),
                mime: e.mime.clone(),
            })
            .collect();
        let outcome = shared.swarm.advertise(
            &entries,
            ctx.account_id,
            ctx.session_id,
            &ctx.screen_name,
            Duration::from_secs(ttl_secs as u64),
            max_adverts as usize,
        );
        reply!(&ps::AdvertiseAck::new(
            outcome.accepted,
            ttl_secs,
            outcome.total
        ));
        return Ok(true);
    }

    // ---- Withdraw ----------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<ps::AdvertWithdraw>() {
        shared.swarm.withdraw(ctx.session_id, &req.roots);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Find sources ------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<ps::FindSources>() {
        if !ctx.allows(shared, RESOURCE, Caps::FILE_LIST) {
            fail!(ErrorCode::Forbidden);
        }
        // Cheshire mode is a real visibility boundary here as everywhere:
        // an advert is session-scoped, so naming its holder also confirms
        // they're online right now. Sub-moderators don't see invisible
        // sources (mirroring the who-list); a source whose presence entry is
        // already gone (teardown race) is dropped too.
        let viewer_is_mod = ctx.role >= Role::Moderator;
        let sources: Vec<ps::SourceInfo> = shared
            .swarm
            .find(&req.root)
            .into_iter()
            .filter(|a| {
                viewer_is_mod
                    || shared
                        .presence
                        .get(a.session_id)
                        .is_some_and(|e| !e.is_invisible())
            })
            .take(SOURCES_MAX)
            .map(|a| ps::SourceInfo::new(a.screen_name, a.size, a.name, a.mime))
            .collect();
        // Does the origin itself hold the blob? (Fetcher's fallback source.)
        let blobs = shared.blobs.clone();
        let root = req.root;
        let server_size = tokio::task::spawn_blocking(move || {
            let id = BlobId(root);
            blobs.contains(&id).then(|| blobs.size(&id).ok()).flatten()
        })
        .await?;
        reply!(&ps::SourceList::new(
            req.root,
            server_size.is_some(),
            server_size.unwrap_or(0),
            sources
        ));
        return Ok(true);
    }

    Ok(false)
}
