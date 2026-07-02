//! Syndication ingest for RabbitHole boards — feed *parsing* slice (Wave 10).
//!
//! Turns RSS 2.0 and Atom 1.0 documents into a normalized [`Feed`] /
//! [`FeedItem`] model that later waves feed into `BoardService` as signed
//! post events. This crate deliberately contains **no networking and no
//! board wiring** — it is a pure, deterministic transform so the fetch
//! scheduler and board mapping can land independently.
//!
//! Design notes:
//!
//! - **Dependency-light.** XML is handled by a small hand-rolled pull
//!   tokenizer ([`mod@xml`]) rather than a full XML crate: feeds in the wild
//!   are frequently malformed, and a lenient scanner that never fails is a
//!   better fit than a strict parser we would have to wrap in recovery
//!   logic anyway. Dates are parsed manually (no chrono). The only
//!   dependency is `blake3` (already a workspace dep) for dedup ids.
//! - **Lenient everywhere, panic never.** Truncated documents yield the
//!   items parsed so far; junk bytes yield [`FeedError::NotAFeed`]; unknown
//!   elements are skipped by depth counting.
//! - **Two decode layers, applied in the right order.** XML text nodes are
//!   entity-decoded once (the XML layer); item bodies are then run through
//!   [`html_to_text`] which strips tags and decodes entities again (the
//!   HTML layer) — so `&amp;lt;p&amp;gt;` in an RSS description correctly
//!   ends up as stripped markup, while CDATA bodies skip the XML layer.
//! - **Stable dedup ids.** [`dedup_id`] derives a blake3 hash from the
//!   most stable identity an item offers (guid/id, then link, then
//!   title+date), domain-separated so ids never collide with other
//!   RabbitHole hash uses.
//!
//! Beyond parsing, three pure ingest layers build on this model — still
//! sans-IO, still driven entirely by injected inputs, so a later server
//! slice can wire them to a real fetcher and clock:
//!
//! - **[`mod@mapping`].** [`to_post_drafts`] turns a [`Feed`] into board-post
//!   drafts ([`PostDraft`]), each carrying its stable [`dedup_id`].
//! - **[`mod@seen`].** [`partition_fresh`] / [`SeenSet`] split a re-parsed
//!   batch into genuinely-new items and duplicates, so re-ingesting a feed
//!   never double-posts.
//! - **[`mod@poll`].** [`PollState`] models conditional-GET fetch scheduling —
//!   `ETag`/`Last-Modified` validators plus a next-poll time with error
//!   backoff and feed-TTL honoring — as pure, clockless transitions.

#![forbid(unsafe_code)]

pub mod date;
pub mod dedup;
pub mod feed;
pub mod mapping;
pub mod poll;
pub mod seen;
pub mod text;
mod xml;

pub use dedup::dedup_id;
pub use feed::{parse, parse_with_options, Feed, FeedError, FeedItem, ParseOptions};
pub use mapping::{to_post_drafts, BoardMapping, PostDraft};
pub use poll::{
    next_poll_at, poll_interval_secs, sy_update_period_secs, ttl_minutes_to_secs, PollConfig,
    PollDecision, PollState,
};
pub use seen::{partition_fresh, ItemId, Partition, SeenSet};
pub use text::html_to_text;
