//! # Hotline wire codec (`rabbithole-legacy-hotline`)
//!
//! A pure, dependency-light codec for the classic **Hotline** binary protocol,
//! so real vintage Hotline clients can speak to a RabbitHole server. This crate
//! is *only* the wire format — no sockets, no async, no server logic. Those
//! layers wire this codec into the server in later Wave 7 slices.
//!
//! Hotline is a **legacy** protocol with its own hand-rolled big-endian binary
//! framing. It is deliberately **not** the RabbitHole-native protocol (which is
//! postcard over QUIC — see `rabbithole-proto`); nothing here uses postcard or
//! serde.
//!
//! ## Wire layers (outermost to innermost)
//!
//! ```text
//! 1. Handshake        client -> [ TRTP HOTL ver subver ]  (12 bytes)
//!                     server -> [ TRTP error ]            (8 bytes)
//!
//! 2. Transaction      [ 20-byte header ][ data_size body bytes ]  per frame
//!    header:          flags(1) is_reply(1) type(2) id(4)
//!                     error(4) total_size(4) data_size(4)   (all big-endian)
//!
//! 3. Body / TLV       [ count(2) ][ field* ]
//!    field:           [ id(2) ][ size(2) ][ value(size) ]
//!
//! 4. Fragmentation    data_size < total_size -> body continues in later
//!                     frames with the same id (see `reassembly`).
//! ```
//!
//! ## Module map
//!
//! - [`handshake`] — the 12-byte `TRTP`/`HOTL` handshake and 8-byte reply.
//! - [`transaction`] — the 20-byte transaction header and single-frame body.
//! - [`field`] — TLV parameter fields, the parameter list, and size-dependent
//!   integer helpers.
//! - [`reassembly`] — accumulates fragmented bodies by transaction id.
//! - [`access`] — the 64-bit account access bitmap and 16-bit user flags.
//! - [`flatten`] — flattened file objects (the HTXF payload) and the `RFLT`
//!   fork-offset resume structure.
//! - [`constants`] — well-known field ids and transaction type numbers.
//! - [`error`] — the total, panic-free [`HotlineError`].
//!
//! ## Safety & robustness
//!
//! `#![forbid(unsafe_code)]`; every decoder is total (malformed or truncated
//! input yields `Err`, never a panic).

#![forbid(unsafe_code)]

pub mod access;
pub mod constants;
pub mod error;
pub mod field;
pub mod flatten;
pub mod handshake;
pub mod reassembly;
pub mod transaction;

pub use access::{AccessMask, Privilege, UserFlags};
pub use error::HotlineError;
pub use field::{
    decode_params, deobfuscate, encode_params, min_int_bytes, obfuscate, read_int, Field,
    MAX_FIELD_SIZE,
};
pub use flatten::{FileResumeData, FlatHeader, ForkHeader, ForkOffset, InfoFork};
pub use handshake::{Handshake, HandshakeReply, HANDSHAKE_VERSION, PROTOCOL_ID, SUB_PROTOCOL_HOTL};
pub use reassembly::Reassembler;
pub use transaction::{Transaction, TransactionHeader};
