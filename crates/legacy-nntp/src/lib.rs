//! NNTP wire codec for RabbitHole's news-reader surface (Wave 10).
//!
//! This crate is the pure, self-contained *codec and command model* for a
//! reader-facing NNTP server per [RFC 3977]. It knows how to turn client
//! command lines into typed values, how to format status responses, how to
//! frame multi-line data blocks with dot-stuffing in both directions, and how
//! to model `OVER`/`XOVER` overview records. It deliberately contains **no**
//! networking, session state, or message-base wiring — those land in later
//! Wave 10 slices so they can evolve without churning this codec.
//!
//! It also covers the transit/peering and authentication surface: the
//! streaming feed ([RFC 4644]) `MODE STREAM`/`CHECK`/`TAKETHIS` verbs and the
//! classic `IHAVE` offer, `NEWNEWS`/`NEWGROUPS` with wildmat group selection,
//! and `AUTHINFO USER`/`PASS` ([RFC 4643]).
//!
//! # Modules
//!
//! * [`command`] — parse a client command line into a typed [`Command`].
//! * [`response`] — typed [`Status`] codes and renderable [`Response`] lines.
//! * [`datablock`] — dot-stuffed, CRLF-framed multi-line block encode/decode.
//! * [`overview`] — the `OVERVIEW.FMT` [`Overview`] line model.
//! * [`message_id`] — the validated [`MessageId`] newtype.
//! * [`wildmat`] — RFC 3977 §4 newsgroup-pattern matching.
//! * [`datetime`] — `NEWNEWS`/`NEWGROUPS` date/time argument parsing.
//! * [`transit`] — `IHAVE`/streaming article-transfer state and responses.
//! * [`listing`] — active/newsgroups/message-id listing lines and blocks.
//!
//! # Robustness
//!
//! Every decoder accepts arbitrary, possibly truncated bytes without panicking;
//! malformed input is reported through the module's error type. All rendering
//! uses CRLF line endings and scrubs control characters that could split a
//! line on the wire.
//!
//! [RFC 3977]: https://www.rfc-editor.org/rfc/rfc3977
//! [RFC 4643]: https://www.rfc-editor.org/rfc/rfc4643
//! [RFC 4644]: https://www.rfc-editor.org/rfc/rfc4644

#![forbid(unsafe_code)]

pub mod command;
pub mod datablock;
pub mod datetime;
pub mod listing;
pub mod message_id;
pub mod overview;
pub mod response;
pub mod transit;
pub mod wildmat;

pub use command::{ArticleRef, Command, CommandError, ListKeyword, OverRef, Range};
pub use datablock::{
    decode_block, decode_lines, encode_block, encode_lines, DataBlockError, TERMINATOR,
};
pub use datetime::{days_in_month, expand_two_digit_year, DateTimeError, DateTimeSpec};
pub use listing::{
    active_block, active_times_block, new_articles_block, new_groups_block, newsgroups_block,
    ActiveGroup, ActiveTimesEntry, ListingError, NewsgroupDescription,
};
pub use message_id::{MessageId, MessageIdError};
pub use overview::{overview_fmt_block, Overview, OverviewError, OVERVIEW_FMT};
pub use response::{Response, ResponseError, Status};
pub use transit::{Exchange, OfferVerb, TransitError, TransitState};
