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
//!
//! Requests (types 7..12): what is waiting on a station and what it can be
//! asked for — only what it can play, a page at a time, nothing a moderator
//! is holding back — open to anybody who can see the listing; asking for a
//! track and voting for one need an account (not a guest) that may talk on
//! `radio`, spend from the `post` budget, and are held to
//! [`REQUESTS_EACH`](crate::radio::REQUESTS_EACH) waiting per person and
//! [`REQUESTS_WAITING`](crate::radio::REQUESTS_WAITING) per station. Each
//! refusal has its own code, so a person is told the reason that applies:
//! `NotFound` (not something this station can be asked for, or a vote for
//! what is no longer waiting), `AlreadyExists` (it is playing now),
//! `TooLarge` (this person's share is waiting), `Unavailable` (the station's
//! queue is full), `RateLimited` (the posting budget).

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::radio as pradio;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::ratelimit::{class as rl, Scope};

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
    // The operator's view: every station, including the silent ones, and
    // what they could not play. Only for whoever runs the burrow — it says
    // what is in a file area and how the rotation is faring.
    if frame.decode::<pradio::RadioStatusRequest>().is_some() {
        if !_ctx.allows(shared, "admin", rabbithole_server_core::Caps::CONFIG_ADMIN) {
            conn.send(Frame::error_reply(
                frame,
                rabbithole_proto::ErrorCode::Forbidden,
            ))
            .await?;
            return Ok(true);
        }
        let status = pradio::RadioStatus::new(crate::radio::station_status(shared));
        conn.send(Frame::reply_to(frame, &status)?).await?;
        return Ok(true);
    }
    // Requests: what is waiting, what a station can be asked for, asking,
    // and joining in. What a moderator is holding back is not there.
    let held = |t: &rabbithole_radio::Track| crate::radio::is_held(shared, t);
    if let Some(Ok(req)) = frame.decode::<pradio::RadioRequestsRequest>() {
        match shared.radio.requests(&req.station, &_ctx.login, held) {
            Some(view) => conn.send(Frame::reply_to(frame, &view)?).await?,
            None => {
                conn.send(Frame::error_reply(frame, ErrorCode::NotFound))
                    .await?
            }
        }
        return Ok(true);
    }
    if let Some(Ok(req)) = frame.decode::<pradio::RadioOfferRequest>() {
        match shared.radio.offer(&req.station, &req.search, held) {
            Some(offer) => conn.send(Frame::reply_to(frame, &offer)?).await?,
            None => {
                conn.send(Frame::error_reply(frame, ErrorCode::NotFound))
                    .await?
            }
        }
        return Ok(true);
    }
    let asked = frame
        .decode::<pradio::RadioRequest>()
        .and_then(Result::ok)
        .map(|r| (r.station, r.track, true));
    let voted = frame
        .decode::<pradio::RadioRequestVote>()
        .and_then(Result::ok)
        .map(|r| (r.station, r.track, false));
    if let Some((station, track, is_request)) = asked.or(voted) {
        // An account, not a guest: one vote each only means something when
        // "each" is somebody who cannot come back under another name. And
        // somebody who may talk here: asking for a song is saying something
        // to the room, so it follows each class's word on talking, passes
        // through the agreement like anything else that is not only
        // looking, and an operator can stop it on its own with an ACL on
        // the `radio` resource.
        if _ctx.is_guest || !_ctx.allows(shared, "radio", rabbithole_server_core::Caps::CHAT_SEND) {
            conn.send(Frame::error_reply(frame, ErrorCode::Forbidden))
                .await?;
            return Ok(true);
        }
        if !shared.rate_allow(Scope::Account(_ctx.account_id), rl::POST) {
            conn.send(Frame::error_reply(frame, ErrorCode::RateLimited))
                .await?;
            return Ok(true);
        }
        let done = if is_request {
            shared.radio.request(&station, track, &_ctx.login, held)
        } else {
            shared
                .radio
                .vote_request(&station, track, &_ctx.login, held)
        };
        if let Err(why) = done {
            use crate::radio::RequestRefused as R;
            let code = match why {
                R::NoSuchStation | R::NotInRotation | R::NotWaiting => ErrorCode::NotFound,
                R::PlayingNow => ErrorCode::AlreadyExists,
                R::TooManyOfYours => ErrorCode::TooLarge,
                R::Full => ErrorCode::Unavailable,
            };
            conn.send(Frame::error_reply(frame, code)).await?;
            return Ok(true);
        }
        match shared.radio.requests(&station, &_ctx.login, held) {
            Some(view) => conn.send(Frame::reply_to(frame, &view)?).await?,
            None => {
                conn.send(Frame::error_reply(frame, ErrorCode::NotFound))
                    .await?
            }
        }
        return Ok(true);
    }
    Ok(false)
}
