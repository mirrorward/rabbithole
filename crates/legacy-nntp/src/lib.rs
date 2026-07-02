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
//! # Modules
//!
//! * [`command`] — parse a client command line into a typed [`Command`].
//! * [`response`] — typed [`Status`] codes and renderable [`Response`] lines.
//! * [`datablock`] — dot-stuffed, CRLF-framed multi-line block encode/decode.
//! * [`overview`] — the `OVERVIEW.FMT` [`Overview`] line model.
//! * [`message_id`] — the validated [`MessageId`] newtype.
//!
//! # Robustness
//!
//! Every decoder accepts arbitrary, possibly truncated bytes without panicking;
//! malformed input is reported through the module's error type. All rendering
//! uses CRLF line endings and scrubs control characters that could split a
//! line on the wire.
//!
//! [RFC 3977]: https://www.rfc-editor.org/rfc/rfc3977

#![forbid(unsafe_code)]

pub mod command;
pub mod datablock;
pub mod message_id;
pub mod overview;
pub mod response;

pub use command::{ArticleRef, Command, CommandError, ListKeyword, OverRef, Range};
pub use datablock::{
    decode_block, decode_lines, encode_block, encode_lines, DataBlockError, TERMINATOR,
};
pub use message_id::{MessageId, MessageIdError};
pub use overview::{overview_fmt_block, Overview, OverviewError, OVERVIEW_FMT};
pub use response::{Response, ResponseError, Status};
