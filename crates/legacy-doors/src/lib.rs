//! # Door-game drop files (`rabbithole-legacy-doors`)
//!
//! Classic BBS "door" games learn about the current caller by reading a small
//! text **drop file** the BBS writes just before launching them. This crate is
//! the pure data-model + codec layer for that exchange:
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
//! ## Scope
//!
//! This slice is **pure data model + codecs**. There is deliberately no process
//! spawning, no sockets, and no server wiring here — a later Wave 6 slice runs
//! the door and bridges its I/O. Dependencies are `std` + `thiserror` only.
//!
//! ## Robustness
//!
//! Readers are total: malformed or truncated input never panics. Fields that
//! are missing or unparseable keep their [`Default`] values; the only hard
//! error is [`Error::Empty`] for input with no content.
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

pub mod context;
pub mod door32;
pub mod door_sys;
pub mod dorinfo;
pub mod dropfile;
pub mod error;

pub use context::{DoorContext, DoorUser, Emulation};
pub use door32::{read_door32_sys, write_door32_sys};
pub use door_sys::{read_door_sys, write_door_sys};
pub use dorinfo::write_dorinfo1;
pub use dropfile::{detect, write, DropFile};
pub use error::Error;
