//! # warren-stampede
//!
//! A load generator that drives many concurrent RHP sessions against a
//! burrow, reusing the real client core (`rabbithole-core`) — no protocol
//! re-implementation. The binary is a thin clap CLI over [`run`]; the CI
//! smoke test drives the same library entry point against an in-process
//! burrow.
//!
//! ## Scenarios
//!
//! - **idle** — connect, log in, then keepalive pings only.
//! - **chat** — send a lobby line on a jittered interval (5–15 s by
//!   default) and measure the round-trip until the sender's own echo push
//!   comes back.
//! - **mixed** — each tick is 80% idle (ping) / 20% chat.
//!
//! ## Real-hardware target (documented, not CI)
//!
//! The design target is 10 000 concurrent sessions against a dedicated
//! burrow. That run is *not* part of CI — it needs real hardware and a
//! raised file-descriptor limit (`ulimit -n 65536` or so on both ends):
//!
//! ```text
//! warren-stampede \
//!     --url ws://burrow.example.net:4654 \
//!     --sessions 10000 --ramp-per-sec 200 --duration 600 \
//!     --scenario mixed --guests --max-errors 500 --json > stampede.json
//! ```
//!
//! Exit codes: `0` clean run, `1` finished but with errors recorded,
//! `2` the `--max-errors` circuit breaker aborted the run.

#![forbid(unsafe_code)]

pub mod metrics;
pub mod runner;

pub use metrics::{LatencySummary, Metrics, Report};
pub use runner::{run, AuthMode, RunConfig, RunOutcome, Scenario};
