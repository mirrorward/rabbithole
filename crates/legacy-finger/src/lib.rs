//! Classic finger (RFC 1288) surface for RabbitHole (Wave 6).
//!
//! Serves the venerable finger protocol on a plain TCP listener: an empty
//! query lists who's currently in the warren, `user` looks up a member's
//! profile card and renders their `.plan` verbatim, the `/W` verbose flag is
//! tolerated (and ignored), and query forwarding (`user@host`) is refused
//! outright per long-standing security practice.
//!
//! The crate deliberately knows nothing about RabbitHole's stores. Data
//! arrives through the [`FingerDirectory`] trait so the server (or any other
//! host) can adapt its persona/presence layers behind it later. Output is
//! defensively rendered: control characters that could smuggle terminal
//! escapes into the *requester's* terminal are stripped, line endings are
//! normalized to CRLF on the wire, and total response size is capped.

#![forbid(unsafe_code)]

pub mod directory;
pub mod query;
pub mod render;
pub mod server;

pub use directory::{FingerDirectory, Profile, WhoEntry};
pub use query::{parse_query, Query};
pub use render::{
    format_forward_refused, format_profile, format_unknown, format_who, sanitize, to_wire,
    MAX_RESPONSE_BYTES,
};
pub use server::{handle_query, FingerServer, MAX_QUERY_BYTES};
