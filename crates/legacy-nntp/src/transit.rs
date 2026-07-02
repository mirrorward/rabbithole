//! Article transit / peering model — `IHAVE` (RFC 3977 §6.3.2) and streaming
//! `CHECK`/`TAKETHIS` (RFC 4644).
//!
//! When one server offers an article to another there are two protocols:
//!
//! * **`IHAVE <message-id>`** — the offering peer asks first. The receiver
//!   answers `335` (send it), `435` (not wanted), or `436` (try again later);
//!   after the article arrives it answers `235` (transferred) or `437`
//!   (rejected). `IHAVE` replies never echo the message-id.
//! * **Streaming (`MODE STREAM`)** — the offering peer pipelines `CHECK
//!   <message-id>` probes (answered `238` want / `438` don't / `431` defer) and
//!   then sends wanted articles with `TAKETHIS <message-id>` + body (answered
//!   `239` accepted / `439` rejected). Streaming replies **do** echo the
//!   message-id as their first token so the pipelined peer can correlate them.
//!
//! This module models the receiver's side of a single offered article as a tiny
//! pure state machine, [`Exchange`], that both tracks the legal progression
//! (offered → wanted/refused/deferred → transferred/rejected) and renders the
//! correct [`Response`] — with or without the echoed message-id — for whichever
//! verb opened the exchange. It performs no I/O and never panics; illegal
//! transitions are reported as [`TransitError`] rather than asserted away.

use crate::message_id::MessageId;
use crate::response::{Response, Status};

use thiserror::Error;

/// The verb that opened a transit exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferVerb {
    /// `IHAVE <message-id>` — offer, decision, then body (RFC 3977 §6.3.2).
    IHave,
    /// `CHECK <message-id>` — streaming probe only (RFC 4644).
    Check,
    /// `TAKETHIS <message-id>` — streaming unconditional send (RFC 4644).
    TakeThis,
}

/// The receiver-side state of one offered article.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitState {
    /// A decision is owed: after `IHAVE` or `CHECK`, before any reply.
    Offered,
    /// The receiver asked for the article: after `335`/`238`, before the body.
    ///
    /// A [`OfferVerb::TakeThis`] exchange starts here, since `TAKETHIS` carries
    /// the article unconditionally.
    Wanted,
    /// The receiver declined the offer (`435`/`438`). Terminal.
    Refused,
    /// The receiver deferred the offer (`436`/`431`). Terminal for this attempt.
    Deferred,
    /// The article was received and accepted (`235`/`239`). Terminal.
    Transferred,
    /// The article was received and rejected (`437`/`439`). Terminal.
    Rejected,
}

/// Reasons a transit transition could not be taken.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransitError {
    /// A decision (`want`/`refuse`/`defer`) was requested when the exchange was
    /// not awaiting one, or an outcome (`accept`/`reject`) when no article was
    /// expected.
    #[error("transit action not valid in state {state:?} for verb {verb:?}")]
    InvalidTransition {
        /// The verb that opened the exchange.
        verb: OfferVerb,
        /// The state the exchange was in when the action was attempted.
        state: TransitState,
    },
}

/// The receiver's side of a single offered article.
///
/// Construct with [`Exchange::open`] and drive it with [`Exchange::want`],
/// [`Exchange::refuse`], [`Exchange::defer`], [`Exchange::accept`], and
/// [`Exchange::reject`]. Each successful call advances [`Exchange::state`] and
/// returns the [`Response`] to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    id: MessageId,
    verb: OfferVerb,
    state: TransitState,
}

impl Exchange {
    /// Open an exchange for `id` offered via `verb`.
    ///
    /// `IHAVE`/`CHECK` start [`TransitState::Offered`] (a decision is owed);
    /// `TAKETHIS` starts [`TransitState::Wanted`] because the body follows the
    /// command unconditionally.
    #[must_use]
    pub fn open(verb: OfferVerb, id: MessageId) -> Self {
        let state = match verb {
            OfferVerb::IHave | OfferVerb::Check => TransitState::Offered,
            OfferVerb::TakeThis => TransitState::Wanted,
        };
        Exchange { id, verb, state }
    }

    /// The message-id under offer.
    #[must_use]
    pub fn message_id(&self) -> &MessageId {
        &self.id
    }

    /// The verb that opened this exchange.
    #[must_use]
    pub fn verb(&self) -> OfferVerb {
        self.verb
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> TransitState {
        self.state
    }

    /// Ask the offering peer to send the article (`335` for `IHAVE`, `238` for
    /// `CHECK`). Valid only from [`TransitState::Offered`].
    ///
    /// # Errors
    ///
    /// [`TransitError::InvalidTransition`] if not currently awaiting a decision
    /// (e.g. on a `TAKETHIS` exchange, whose body is unconditional).
    pub fn want(&mut self) -> Result<Response, TransitError> {
        self.decide(
            TransitState::Wanted,
            Status::IhaveSendArticle,
            Status::CheckWanted,
        )
    }

    /// Decline the article (`435` for `IHAVE`, `438` for `CHECK`). Valid only
    /// from [`TransitState::Offered`].
    ///
    /// # Errors
    ///
    /// [`TransitError::InvalidTransition`] if not currently awaiting a decision.
    pub fn refuse(&mut self) -> Result<Response, TransitError> {
        self.decide(
            TransitState::Refused,
            Status::IhaveNotWanted,
            Status::CheckNotWanted,
        )
    }

    /// Defer the article (`436` for `IHAVE`, `431` for `CHECK`). Valid only from
    /// [`TransitState::Offered`].
    ///
    /// # Errors
    ///
    /// [`TransitError::InvalidTransition`] if not currently awaiting a decision.
    pub fn defer(&mut self) -> Result<Response, TransitError> {
        self.decide(
            TransitState::Deferred,
            Status::IhaveDeferred,
            Status::CheckDeferred,
        )
    }

    /// Accept a received article (`235` for `IHAVE`, `239` for `TAKETHIS`).
    /// Valid only from [`TransitState::Wanted`], and never for a bare `CHECK`
    /// (which carries no body).
    ///
    /// # Errors
    ///
    /// [`TransitError::InvalidTransition`] if no article was expected.
    pub fn accept(&mut self) -> Result<Response, TransitError> {
        self.settle(
            TransitState::Transferred,
            Status::IhaveTransferOk,
            Status::TakethisAccepted,
        )
    }

    /// Reject a received article (`437` for `IHAVE`, `439` for `TAKETHIS`).
    /// Valid only from [`TransitState::Wanted`].
    ///
    /// # Errors
    ///
    /// [`TransitError::InvalidTransition`] if no article was expected.
    pub fn reject(&mut self) -> Result<Response, TransitError> {
        self.settle(
            TransitState::Rejected,
            Status::IhaveRejected,
            Status::TakethisRejected,
        )
    }

    /// Apply a decision from [`TransitState::Offered`], choosing the status by
    /// verb (`IHAVE` uses `ihave_status`; `CHECK` uses `check_status`).
    fn decide(
        &mut self,
        next: TransitState,
        ihave_status: Status,
        check_status: Status,
    ) -> Result<Response, TransitError> {
        if self.state != TransitState::Offered {
            return Err(self.invalid());
        }
        let status = match self.verb {
            OfferVerb::IHave => ihave_status,
            OfferVerb::Check => check_status,
            // TAKETHIS never sits in `Offered`, so this arm is unreachable in
            // practice; guard it anyway to stay total.
            OfferVerb::TakeThis => return Err(self.invalid()),
        };
        self.state = next;
        Ok(self.reply(status))
    }

    /// Apply an outcome from [`TransitState::Wanted`], choosing the status by
    /// verb (`IHAVE` uses `ihave_status`; `TAKETHIS` uses `takethis_status`).
    fn settle(
        &mut self,
        next: TransitState,
        ihave_status: Status,
        takethis_status: Status,
    ) -> Result<Response, TransitError> {
        if self.state != TransitState::Wanted {
            return Err(self.invalid());
        }
        let status = match self.verb {
            OfferVerb::IHave => ihave_status,
            OfferVerb::TakeThis => takethis_status,
            // A bare CHECK carries no article to accept or reject.
            OfferVerb::Check => return Err(self.invalid()),
        };
        self.state = next;
        Ok(self.reply(status))
    }

    /// Render the reply for `status`, echoing the message-id for streaming
    /// (`CHECK`/`TAKETHIS`) verbs and using the plain default text for `IHAVE`.
    fn reply(&self, status: Status) -> Response {
        match self.verb {
            OfferVerb::IHave => status.response(),
            OfferVerb::Check | OfferVerb::TakeThis => {
                Response::new(status.code(), self.id.as_str())
            }
        }
    }

    fn invalid(&self) -> TransitError {
        TransitError::InvalidTransition {
            verb: self.verb,
            state: self.state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mid() -> MessageId {
        MessageId::new("<a@b>").unwrap()
    }

    #[test]
    fn ihave_want_then_accept() {
        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        assert_eq!(ex.state(), TransitState::Offered);
        let r = ex.want().unwrap();
        assert_eq!(r.code(), 335);
        assert_eq!(r.render(), "335 Send it\r\n");
        assert_eq!(ex.state(), TransitState::Wanted);
        let r = ex.accept().unwrap();
        assert_eq!(r.code(), 235);
        assert_eq!(ex.state(), TransitState::Transferred);
    }

    #[test]
    fn ihave_refuse_and_defer_and_reject() {
        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        assert_eq!(ex.refuse().unwrap().code(), 435);
        assert_eq!(ex.state(), TransitState::Refused);

        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        assert_eq!(ex.defer().unwrap().code(), 436);

        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        ex.want().unwrap();
        assert_eq!(ex.reject().unwrap().code(), 437);
        assert_eq!(ex.state(), TransitState::Rejected);
    }

    #[test]
    fn ihave_replies_do_not_echo_message_id() {
        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        let r = ex.want().unwrap();
        assert!(!r.text().contains("<a@b>"));
    }

    #[test]
    fn check_echoes_message_id() {
        let mut ex = Exchange::open(OfferVerb::Check, mid());
        let r = ex.want().unwrap();
        assert_eq!(r.code(), 238);
        assert_eq!(r.render(), "238 <a@b>\r\n");

        let mut ex = Exchange::open(OfferVerb::Check, mid());
        assert_eq!(ex.refuse().unwrap().render(), "438 <a@b>\r\n");

        let mut ex = Exchange::open(OfferVerb::Check, mid());
        assert_eq!(ex.defer().unwrap().render(), "431 <a@b>\r\n");
    }

    #[test]
    fn check_cannot_accept_or_reject_a_body() {
        let mut ex = Exchange::open(OfferVerb::Check, mid());
        ex.want().unwrap();
        // After 238 the streaming peer sends a *separate* TAKETHIS; the CHECK
        // exchange itself never carries a body.
        assert!(matches!(
            ex.accept(),
            Err(TransitError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn takethis_starts_wanted_and_echoes() {
        let mut ex = Exchange::open(OfferVerb::TakeThis, mid());
        assert_eq!(ex.state(), TransitState::Wanted);
        // TAKETHIS carries the article; there is nothing to decide first.
        assert!(matches!(
            ex.want(),
            Err(TransitError::InvalidTransition { .. })
        ));
        let r = ex.accept().unwrap();
        assert_eq!(r.code(), 239);
        assert_eq!(r.render(), "239 <a@b>\r\n");

        let mut ex = Exchange::open(OfferVerb::TakeThis, mid());
        assert_eq!(ex.reject().unwrap().render(), "439 <a@b>\r\n");
    }

    #[test]
    fn cannot_decide_twice_or_accept_before_wanted() {
        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        ex.want().unwrap();
        assert!(matches!(
            ex.want(),
            Err(TransitError::InvalidTransition {
                verb: OfferVerb::IHave,
                state: TransitState::Wanted
            })
        ));

        let mut ex = Exchange::open(OfferVerb::IHave, mid());
        assert!(matches!(
            ex.accept(),
            Err(TransitError::InvalidTransition {
                state: TransitState::Offered,
                ..
            })
        ));
    }

    #[test]
    fn accessors_reflect_construction() {
        let ex = Exchange::open(OfferVerb::Check, mid());
        assert_eq!(ex.verb(), OfferVerb::Check);
        assert_eq!(ex.message_id().as_str(), "<a@b>");
    }
}
