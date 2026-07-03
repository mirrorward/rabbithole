//! # QWK / QWKE offline-mail packet codec (`rabbithole-legacy-qwk`)
//!
//! A pure, dependency-light codec for the classic **QWK** offline-mail packet
//! format and its **QWKE** extensions, so vintage offline readers can exchange
//! mail with a RabbitHole server. This crate is *only* the byte-level codec:
//! there is **no networking, no ZIP bundling, and no board wiring** here. A
//! `.QWK`/`.REP` file is a ZIP of the members this crate encodes/decodes; the
//! bundling and delivery layers wire this codec in during later Wave 10 slices.
//! That ZIP boundary is the deliberate seam this slice leaves open.
//!
//! QWK is a **legacy** format with hand-rolled, fixed-width binary records and
//! its own oddities (128-byte blocks, a `0xE3` end-of-line marker, and
//! Microsoft-Binary-Format floats in the index). It is deliberately **not** the
//! RabbitHole-native protocol; nothing here uses postcard or serde.
//!
//! ## Members this codec handles
//!
//! - [`messages`] — `MESSAGES.DAT`: 128-byte records, a producer header block,
//!   then per-message `[header block][body blocks]` with `0xE3` line endings.
//! - [`control`] — `CONTROL.DAT`: the line-oriented ASCII manifest (BBS
//!   identity, target user, conference list, screen files).
//! - [`ndx`] — per-conference `.NDX`: 5-byte records (MBF pointer + conference
//!   byte).
//! - [`mbf`] — Microsoft Binary Format 4-byte float `<->` `u32`/`f32`.
//! - [`qwke`] — QWKE `DOOR.ID` advertisement and long To/From/Subject body
//!   kludges.
//! - [`model`] — the shared [`QwkMessage`] with `\n`-normalized body text.
//! - [`reply`] — `.REP` reply-packet ingest (the `<BBSID>.MSG` member), with
//!   per-record validation and blake3 dedupe of uploaded replies.
//! - [`packet`] — a pure high-level builder assembling the outbound QWK packet
//!   members (`MESSAGES.DAT` / `CONTROL.DAT` / `*.NDX` / `DOOR.ID`) from messages
//!   and conference metadata, for CLI/web export.
//!
//! ## The 128-byte message header (0-based offsets)
//!
//! ```text
//!  offset  len  field                        offset  len  field
//!  ------  ---  -----------------------      ------  ---  ------------------
//!    0      1   status flag                    96     12  password
//!    1      7   message number (ASCII)        108      8  reference number
//!    8      8   date "MM-DD-YY"               116      6  block count (incl. hdr)
//!   16      5   time "HH:MM"                  122      1  active flag E1/E2
//!   21     25   To                            123      2  conference # (LE)
//!   46     25   From                          125      2  logical/reader index
//!   71     25   Subject                       127      1  0x00
//!  ----------------------------------------------------------- total = 128
//! ```
//!
//! ## Safety & robustness
//!
//! `#![forbid(unsafe_code)]`. Every decoder is total: malformed, truncated, or
//! hostile input yields an [`error::QwkError`], never a panic. Text is
//! round-tripped losslessly through Latin-1 at the byte edge.

#![forbid(unsafe_code)]

pub mod control;
pub mod error;
pub mod mbf;
pub mod messages;
pub mod model;
pub mod ndx;
pub mod packet;
pub mod qwke;
pub mod reply;
pub mod zip;

mod text;

pub use control::ControlDat;
pub use error::QwkError;
pub use messages::MessagesDat;
pub use model::QwkMessage;
pub use ndx::NdxRecord;
pub use packet::{build_packet, NdxFile, QwkPacket};
pub use qwke::{DoorId, QwkeKludges};
pub use reply::{
    content_hash, dedupe, validate, ReplyDigest, ReplyMessage, ReplyPacket, ReplyProblem, Validated,
};
pub use zip::{crc32, zip_store};
