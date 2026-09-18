//! Wave 11 handler: the radio station listing (RADIO family, types 3..4).
//!
//! `RadioStationsRequest` returns every station on the air with its
//! now-playing, its recent tracks, its cover, and **where its audio is
//! served**. That last part is the point: the client used to ask the person to
//! type "your server's Icecast delivery address", a fact the server has and
//! the person usually does not.
//!
//! Open to every session, guests included. It reveals nothing the pushes and
//! the public stream listener do not already, and tuning in needs no account.

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::radio as pradio;
use rabbithole_proto::Frame;

use crate::session::SessionCtx;
use crate::Shared;

/// The listing for right now.
pub fn listing(shared: &Arc<Shared>) -> pradio::RadioStations {
    let base = shared.config.read().radio_public_base.clone();
    pradio::RadioStations::new(
        base,
        shared.radio.listen_port(),
        crate::radio::station_listing(shared),
    )
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    _ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    if frame.decode::<pradio::RadioStationsRequest>().is_some() {
        conn.send(Frame::reply_to(frame, &listing(shared))?).await?;
        return Ok(true);
    }
    Ok(false)
}
