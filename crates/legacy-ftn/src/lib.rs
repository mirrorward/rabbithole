//! FidoNet (FTN) packet codec — Wave 10.3 (pure codec slice).
//!
//! This crate is the **byte-level** half of RabbitHole's FidoNet interop: it
//! encodes and decodes `.PKT` files and the messages inside them, and it parses
//! the control-line ("kludge") grammar carried in message bodies. It contains
//! **no networking** (no binkp), **no tosser/scanner services**, and **no board
//! wiring** — those land in later slices. Everything here is a pure,
//! deterministic transform that later waves build on.
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
pub mod cp437;
pub mod error;
pub mod kludge;
pub mod message;
pub mod packet;

mod reader;

pub use address::FtnAddress;
pub use error::{AddressErrorKind, FtnError};
pub use kludge::Message;
pub use message::{decode_messages, encode_messages, PackedMessage};
pub use packet::{DosDateTime, Packet, PacketHeader, Type2Plus, HEADER_LEN};
