//! # binkp mailer protocol codec (`rabbithole-legacy-binkp`)
//!
//! A pure, dependency-light codec for the **binkp** FidoNet mailer protocol
//! (FTS-1026 / FTS-1027 / FTS-1028, binkp 1.0 plus the CRAM-MD5 auth
//! extension), so classic FidoNet mailers — BinkD, Mystic, Argus, Radius —
//! can poll and exchange mail with RabbitHole's FidoNet transport. This crate
//! is *only* the wire format, the command model, the CRAM-MD5 auth math, and a
//! sans-IO session state machine — no sockets, no file IO, no tosser wiring.
//! Those layers wire this codec into the transport in a later Wave 10 slice.
//!
//! ## Wire layers (outermost to innermost)
//!
//! ```text
//! 1. Block framing    2-byte big-endian header; top bit = kind, low 15 bits
//!                     = length (so blocks cap at 32767 bytes):
//!                        [ T | len(15) ][ body … ]
//!                     T=1 command block, T=0 data block   (see `frame`)
//!
//! 2. Command frames   command block body = [ id ][ ASCII args ]
//!                        id 0..=10 → M_NUL … M_SKIP        (see `command`)
//!
//! 3. Argument text    M_FILE  "name size unixtime offset"
//!                     M_ADR   "z:n/no.p@dom z:n/no.p@dom …"
//!                     M_GOT/M_GET/M_SKIP "name size unixtime"
//!                                                          (see `command`)
//!
//! 4. CRAM-MD5 auth    M_NUL "OPT CRAM-MD5-<challenge-hex>"  (answering)
//!                     M_PWD "CRAM-MD5-<HMAC-MD5-digest-hex>" (originating)
//!                                                          (see `cram`)
//! ```
//!
//! ## Module map
//!
//! - [`frame`] — the 2-byte block header codec; [`frame::RawBlock`] and
//!   [`frame::decode_block`].
//! - [`command`] — the `M_*` command set, typed [`command::Command`] model,
//!   and its `name size time offset` argument parsing.
//! - [`address`] — 5D FidoNet addresses (`zone:net/node.point@domain`).
//! - [`cram`] — CRAM-MD5: challenge parsing, HMAC-MD5 digest, and the
//!   `CRAM-MD5-<hex>` wrapper.
//! - [`session`] — sans-IO [`session::Session`] driving the handshake and
//!   batch phase for both the originating and answering roles.
//!
//! ## Safety & robustness
//!
//! Decoding arbitrary, hostile, or truncated bytes never panics: every
//! decoder returns a structured error ([`frame::FrameError::Incomplete`] is
//! distinguished from corruption so streaming callers can wait for more
//! input). `unsafe` is forbidden crate-wide; dependencies are `thiserror`,
//! `hmac`, and `md-5`.

#![forbid(unsafe_code)]

pub mod address;
pub mod command;
pub mod cram;
pub mod frame;
pub mod session;

pub use address::{format_address_list, parse_address_list, Address, AddressError};
pub use command::{Command, CommandError, CommandId, FileId, FileInfo};
pub use cram::{
    cram_md5_digest, cram_md5_option, cram_md5_response, from_hex, parse_challenge, to_hex,
    CRAM_MD5_PREFIX,
};
pub use frame::{decode_block, FrameError, RawBlock, BLOCK_MAX, COMMAND_BIT};
pub use session::{Action, Event, Phase, Role, Session, SessionConfig, SessionError};
