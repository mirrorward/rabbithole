//! # ZMODEM protocol codec (`rabbithole-legacy-zmodem`)
//!
//! A pure, dependency-light codec for the classic **ZMODEM** file-transfer
//! protocol (Chuck Forsberg, 1988), so real retro terminals — SyncTERM,
//! NetRunner, qodem — can transfer files against RabbitHole's telnet surface.
//! This crate is *only* the wire format plus a sans-IO session sketch — no
//! sockets, no telnet integration, no async. Those layers wire this codec
//! into the telnet surface in a later Wave 6 slice.
//!
//! ## Wire layers (outermost to innermost)
//!
//! ```text
//! 1. ZDLE escaping    every binary-sensitive byte on the wire is escaped as
//!                     [ ZDLE=0x18 ][ byte ^ 0x40 ]   (see `zdle`)
//!
//! 2. Headers          hex:    [ * * ZDLE B ][ 14 hex digits ][ CR LF (XON) ]
//!                     bin16:  [ * ZDLE A ][ type p0 p1 p2 p3 crc16 ]  escaped
//!                     bin32:  [ * ZDLE C ][ type p0 p1 p2 p3 crc32 ]  escaped
//!                     (see `header`)
//!
//! 3. Data subpackets  [ payload* ][ ZDLE end ][ crc ]   all ZDLE-escaped,
//!                     end in { ZCRCE ZCRCG ZCRCQ ZCRCW } (see `subpacket`)
//!
//! 4. ZFILE payload    [ name NUL ][ "length mtime mode serial" NUL ]
//!                     carried inside the subpacket after a ZFILE header
//!                     (see `zfile`)
//! ```
//!
//! ## Module map
//!
//! - [`crc`] — CRC-16/XMODEM and CRC-32, the two checksums ZMODEM uses.
//! - [`zdle`] — ZDLE escaping/unescaping of the raw byte stream.
//! - [`header`] — frame types and the hex / binary-16 / binary-32 header
//!   codecs.
//! - [`subpacket`] — data subpackets: payload + frame-end + CRC.
//! - [`zfile`] — the ZFILE file-information block (name, length, mtime, …).
//! - [`session`] — sans-IO `SendState` / `RecvState` sketch of the happy-path
//!   transfer flow for the telnet integration slice to drive.
//!
//! ## Safety & robustness
//!
//! Decoding arbitrary, hostile, or truncated bytes never panics: every
//! decoder returns a structured error (with `Incomplete` distinguished from
//! corruption so streaming callers can wait for more input). `unsafe` is
//! forbidden crate-wide, and the only dependency is `thiserror`.

#![forbid(unsafe_code)]

pub mod crc;
pub mod header;
pub mod session;
pub mod subpacket;
pub mod zdle;
pub mod zfile;

pub use crc::{crc16_xmodem, crc32};
pub use header::{
    decode_header, DecodedHeader, FrameType, Header, HeaderError, HeaderFormat, CANFC32, CANFDX,
    CANOVIO,
};
pub use session::{
    Receiver, RecvAction, RecvEvent, RecvState, SendAction, SendEvent, SendState, Sender,
    SessionError,
};
pub use subpacket::{decode_subpacket, encode_subpacket, DecodedSubpacket, SubpacketError};
pub use zdle::{decode_one, escape, unescape, Escaper, FrameEnd, WireItem, ZdleError};
pub use zfile::{FileInfo, FileInfoError};

/// `ZPAD` — the `*` pad character that introduces every header.
pub const ZPAD: u8 = b'*';
/// `ZDLE` — the ZMODEM data-link escape (CAN, 0x18).
pub const ZDLE: u8 = 0x18;
/// `ZDLEE` — an escaped `ZDLE` on the wire (`ZDLE ^ 0x40`).
pub const ZDLEE: u8 = ZDLE ^ 0x40;
/// `ZBIN` — header format byte: binary header with 16-bit CRC.
pub const ZBIN: u8 = b'A';
/// `ZHEX` — header format byte: hex header with 16-bit CRC.
pub const ZHEX: u8 = b'B';
/// `ZBIN32` — header format byte: binary header with 32-bit CRC.
pub const ZBIN32: u8 = b'C';
/// XON — appended after hex headers (except ZACK/ZFIN) and ZCRCW subpackets.
pub const XON: u8 = 0x11;
/// XOFF — software flow control; ignored (skipped) by the decoders.
pub const XOFF: u8 = 0x13;
