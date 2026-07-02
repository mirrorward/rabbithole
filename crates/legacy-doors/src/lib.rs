//! # Door games: drop files + session-runner model (`rabbithole-legacy-doors`)
//!
//! Classic BBS "door" games learn about the current caller by reading a small
//! text **drop file** the BBS writes just before launching them. This crate is
//! the pure data-model layer for hosting doors: the drop-file codecs, plus the
//! sans-IO session-runner model a driving slice uses to actually launch one.
//!
//! ## Drop files
//!
//! * [`DoorContext`] — the neutral, in-memory description of a call (node, comm
//!   port, terminal size, user facts, session start).
//! * Writers that project a [`DoorContext`] onto the three canonical formats:
//!   [`write_door_sys`] (GAP DOOR.SYS, 52 lines), [`write_dorinfo1`]
//!   (RBBS/QuickBBS DORINFO1.DEF) and [`write_door32_sys`] (Mystic/EleBBS
//!   DOOR32.SYS, 11 lines).
//! * Readers that parse DOOR.SYS and DOOR32.SYS back into a (partial)
//!   [`DoorContext`], so a door's edits to the drop file can be recovered.
//! * A [`DropFile`] enum with [`write`]/[`detect`] for runtime dispatch.
//!
//! All output uses `CRLF` line endings, matching the DOS-era tools.
//!
//! ## Session-runner model
//!
//! * [`DoorDef`] + [`DoorRegistry`] ([`door`]) — the config-shaped catalogue
//!   of installed doors: argv, working dir, drop-file kind, [`IoMode`],
//!   [`NodeRange`], daily time limit. serde-derived and TOML-friendly.
//! * [`NodePool`] + [`NodeLease`] ([`node`]) — thread-safe lowest-free node
//!   allocation with RAII release; single-node doors lock naturally.
//! * [`DoorSession`] + [`SessionState`] ([`session`]) — the pure lifecycle
//!   FSM (`Preparing → Running → Ended/TimedOut/Aborted`) with injected
//!   timestamps, plus [`prepare_dropfile`] to render the launch drop file.
//! * [`BridgeBuffer`] + [`BridgeStats`] ([`bridge`]) — the sans-IO byte pump:
//!   CP437-safe passthrough, telnet-[`IAC`] doubling for socket mode, and
//!   per-session byte/rate accounting.
//!
//! ## Scope: the process seam
//!
//! Everything in this crate is **pure / sans-IO**: no process spawning, no
//! sockets, no filesystem writes, no ambient clock. A separate burrow slice
//! (tokio-based) drives the model — it allocates a [`NodeLease`], writes the
//! [`prepare_dropfile`] output into a drop directory, spawns the door's
//! `command`, pumps bytes through a [`BridgeBuffer`], and reports the outcome
//! into the [`DoorSession`] FSM. Timestamps always flow *into* this crate as
//! arguments. Dependencies are `std` + `thiserror` + `serde` (derives only).
//!
//! ## Robustness
//!
//! Nothing here panics. Readers are total: malformed or truncated input never
//! panics — fields that are missing or unparseable keep their [`Default`]
//! values, and the only hard parse error is [`Error::Empty`]. The runner
//! model reports misuse (duplicate doors, exhausted node pools, illegal FSM
//! transitions) as structured [`Error`] values.
//!
//! ## Example
//!
//! ```
//! use rabbithole_legacy_doors::{DoorContext, DropFile, detect};
//!
//! let ctx = DoorContext::default();
//! let bytes = DropFile::Door32Sys.write(&ctx);
//! assert_eq!(detect(bytes.as_bytes()), Some(DropFile::Door32Sys));
//! ```

#![forbid(unsafe_code)]

mod datetime;
mod util;

pub mod bridge;
pub mod context;
pub mod door;
pub mod door32;
pub mod door_sys;
pub mod dorinfo;
pub mod dropfile;
pub mod error;
pub mod node;
pub mod session;

pub use bridge::{BridgeBuffer, BridgeStats, IAC};
pub use context::{DoorContext, DoorUser, Emulation};
pub use door::{DoorDef, DoorRegistry, IoMode, NodeRange};
pub use door32::{read_door32_sys, write_door32_sys};
pub use door_sys::{read_door_sys, write_door_sys};
pub use dorinfo::write_dorinfo1;
pub use dropfile::{detect, write, DropFile};
pub use error::Error;
pub use node::{NodeLease, NodePool};
pub use session::{prepare_dropfile, DoorSession, SessionState};
