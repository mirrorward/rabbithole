//! FidoNet (FTN) packet codec + mail-processing services.
//!
//! This crate is the **host-testable, sans-IO** half of RabbitHole's FidoNet
//! interop. It encodes and decodes `.PKT` files and the messages inside them
//! ([`packet`], [`message`], [`kludge`]), and layers the mail-processing
//! services on top as pure transforms over in-memory structures:
//!
//! - [`tosser`] — split an inbound packet into echomail vs netmail, dedupe by
//!   MSGID, expand SEEN-BY / PATH loop control.
//! - [`scanner`] — group outbound messages by destination into packets and
//!   compute Binkley-Style Outbound (BSO) file names.
//! - [`arcmail`] — day-coded compressed-mail bundle names with collision
//!   handling.
//! - [`areafix`] — parse and process netmail-driven echo subscription commands.
//! - [`nodelist`] — parse St. Louis-format nodelists, apply a NODEDIFF, and
//!   verify the CRC-16 header checksum.
//!
//! It still contains **no networking** (binkp lands in `legacy-binkp`) and **no
//! board wiring**: nothing here touches a socket, a clock, or the filesystem —
//! every function is a deterministic transform that later waves build on.
//!
//! # Wire overview
//!
//! ```text
//!   .PKT file
//!   ┌────────────────────────────────────────────────────────────┐
//!   │ 58-byte packet header (FTS-0001 type-2 / FSC-0039 type-2+)   │  crate::packet
//!   ├────────────────────────────────────────────────────────────┤
//!   │ packed message 1  (14-byte hdr + DateTime + To/From/Subject  │  crate::message
//!   │                    + body, all NUL-terminated)               │
//!   │ packed message 2  ...                                        │
//!   │ ...                                                          │
//!   │ 0x0000  (end-of-messages terminator)                        │
//!   └────────────────────────────────────────────────────────────┘
//!
//!   message body (inside a packed message)                           crate::kludge
//!   ┌────────────────────────────────────────────────────────────┐
//!   │ AREA:tag           kludges (\x01…)   visible text            │
//!   │ tearline (---)     origin (* Origin:) SEEN-BY / \x01PATH     │
//!   └────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Design principles
//!
//! - **Total decoding.** Every `decode` path uses a bounds-checked reader
//!   ([`mod@reader`]); arbitrary, random, or truncated bytes yield an `Err`,
//!   never a panic.
//! - **Lossless CP437.** Message bodies (and name/subject fields) are carried
//!   as raw bytes; Unicode decoding via the inline [`mod@cp437`] table happens
//!   only at explicit text accessors, so no information is lost in transit.
//! - **Deterministic serialization.** [`kludge::Message::serialize`] emits
//!   control lines in a fixed canonical order, so parse→serialize is stable.
//! - **Dependency-light.** `std` + `thiserror` only; no `unsafe`.
//!
//! # Example
//!
//! ```
//! use rabbithole_legacy_ftn::{Packet, PackedMessage};
//!
//! let mut msg = PackedMessage {
//!     from: "Kevin".into(),
//!     to: "Sysop".into(),
//!     subject: "Hi".into(),
//!     body: b"AREA:GENERAL\rHello, FidoNet!\r".to_vec(),
//!     ..Default::default()
//! };
//! let parsed = msg.parse_body();
//! assert_eq!(parsed.area.as_deref(), Some("GENERAL"));
//! assert_eq!(parsed.text_str(), "Hello, FidoNet!");
//!
//! let packet = Packet { messages: vec![msg], ..Default::default() };
//! let bytes = packet.encode();
//! assert_eq!(Packet::decode(&bytes).unwrap(), packet);
//! ```

#![forbid(unsafe_code)]

pub mod address;
pub mod arcmail;
pub mod areafix;
pub mod cp437;
pub mod error;
pub mod kludge;
pub mod message;
pub mod nodelist;
pub mod packet;
pub mod scanner;
pub mod tosser;

mod reader;

pub use address::FtnAddress;
pub use arcmail::{bundle_basename, bundle_name, next_bundle_name, Weekday};
pub use error::{AddressErrorKind, FtnError, NodediffErrorKind, NodelistErrorKind};
pub use kludge::Message;
pub use message::{decode_messages, encode_messages, PackedMessage};
pub use nodelist::{
    apply_nodediff, crc16, parse as parse_nodelist, resolve_addresses, verify_nodelist,
    NodeKeyword, NodelistEntry,
};
pub use packet::{DosDateTime, Packet, PacketHeader, Type2Plus, HEADER_LEN};
pub use scanner::{
    bso_basename, bso_file_name, bso_packet_name, bso_relative_path, group_by_area, scan, BsoKind,
    Flavor, ScannedBundle,
};
pub use tosser::{EchoMail, NetMail, TossedBatch, Tosser};
