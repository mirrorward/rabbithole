//! Wave 10 handlers: live gateway/feed statistics (ADMIN family, types
//! 45..46).
//!
//! `GatewayStatsRequest` returns a point-in-time [`GatewayStatsReply`] built
//! from the in-memory [`crate::stats::GatewayStats`] counters plus each
//! surface's current enabled flag. Read-only and `CONFIG_ADMIN`-gated (the
//! same gate the syndication/gateways admin panel uses); not audited, per the
//! `ReportList` read-only precedent.

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::admin as padm;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::Caps;

use crate::session::SessionCtx;
use crate::Shared;

/// Snapshot the gateway/feed counters, tagging each gateway with whether its
/// surface is enabled in the live config right now.
pub fn snapshot(shared: &Shared) -> padm::GatewayStatsReply {
    let enabled: Vec<(&str, bool)> = {
        let c = shared.config.read();
        vec![
            ("telnet", c.telnet_enabled),
            ("nntp", c.nntp_enabled),
            ("nntp_feed", c.nntp_feed_enabled),
            ("ftn", c.ftn_enabled),
            ("qwk", c.qwk_enabled),
            ("hotline", c.hotline_enabled),
            ("radio", c.radio_enabled || c.radio_source_enabled),
            ("syndication", c.syndication_enabled),
        ]
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    shared.stats.snapshot(now_ms, &enabled)
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    if frame.decode::<padm::GatewayStatsRequest>().is_some() {
        if !ctx.allows(shared, "admin", Caps::CONFIG_ADMIN) {
            conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                .await?;
            return Ok(true);
        }
        conn.send(Frame::reply_to(frame, &snapshot(shared))?)
            .await?;
        return Ok(true);
    }
    Ok(false)
}
