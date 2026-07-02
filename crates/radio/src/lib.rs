//! Radio service layer for RabbitHole (Wave 11.3).
//!
//! This crate is the *station/playlist/queue logic* that sits on top of the
//! audio primitives in [`rabbithole_audio`]. It owns no sockets, no server
//! wiring, and no codecs: it decides **what plays, on which station, in what
//! order, and who's listening** — then hands the result to the transport slice
//! that actually moves bytes.
//!
//! Everything here is pure, deterministic, and unit-tested. Where scheduling
//! needs time or randomness, the caller injects it (a monotonic `now_ms`
//! timestamp, a shuffle `seed`), because `Instant::now`/`Date::now` may be
//! restricted and reproducibility matters.
//!
//! # The pieces
//!
//! - [`Track`] — display metadata plus [`BlobId`], an opaque media handle this
//!   crate never dereferences (no file/network I/O lives here).
//! - [`StationRegistry`] — thread-safe directory of named stations:
//!   create/remove/list, per-station enable toggle, lookup by slug or mount,
//!   and per-station listener accounting for presence/UX.
//! - [`Playlist`] — an ordered track list with [`RotationMode`]s
//!   ([`Sequential`](RotationMode::Sequential),
//!   deterministic [`Shuffle`](RotationMode::Shuffle),
//!   [`RepeatOne`](RotationMode::RepeatOne)) and a wrapping cursor.
//! - [`RequestQueue`] — listener requests with one-vote-per-listener upvoting
//!   and track dedupe; the scheduler drains the highest-voted request first.
//! - [`StationController`] — ties a playlist and queue to an audio
//!   [`Station`](rabbithole_audio::Station): derives
//!   [`NowPlaying`](rabbithole_audio::NowPlaying)/[`StationMeta`], and
//!   advances via [`on_track_finished`](StationController::on_track_finished).
//!   It publishes metadata but never audio frames — that seam is documented on
//!   the type.

#![forbid(unsafe_code)]

mod controller;
mod error;
mod playlist;
mod queue;
mod registry;
mod track;

pub use controller::{StationController, StationMeta};
pub use error::RadioError;
pub use playlist::{Playlist, RotationMode};
pub use queue::{QueuedRequest, RequestQueue};
pub use registry::{StationConfig, StationInfo, StationRegistry};
pub use track::{BlobId, Track, TrackId};
