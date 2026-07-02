//! Telnet BBS surface (Wave 6): RFC 854/855 protocol layer, line IO, and a
//! minimal login shell.
//!
//! Layered bottom-up, each layer independently testable:
//!
//! - [`proto`] — the sans-IO byte-level codec: IAC command parsing
//!   (WILL/WONT/DO/DONT, SB…SE subnegotiation), `0xFF 0xFF` data escaping.
//! - [`negotiate`] — the loop-safe option negotiation state machine. We
//!   offer ECHO (1) and SGA (3) on our side, ask the peer for SGA, NAWS (31,
//!   window size), and TTYPE (24, terminal type), and refuse everything else.
//! - [`encoding`] — output/input encodings: UTF-8 passthrough and a lossy
//!   CP437 mode (the art crate's real CP437 tables slot in later).
//! - [`stream`] — [`TelnetStream`]: the protocol layer bound to any
//!   `AsyncRead + AsyncWrite`, with a line-mode reader (CR/LF, backspace,
//!   server-side echo) and an encoding-aware writer.
//! - [`auth`] / [`shell`] — a pluggable [`TelnetAuth`] trait (no server-core
//!   dependency; burrow adapts its `AuthService` in a later slice) and the
//!   banner → login → MAIN MENU session shell.
//!
//! No listener wiring lives here: burrow binds the TCP port and hands each
//! accepted socket to [`run_shell`].

#![forbid(unsafe_code)]

pub mod auth;
pub mod encoding;
pub mod negotiate;
pub mod proto;
pub mod shell;
pub mod stream;

pub use auth::TelnetAuth;
pub use encoding::Encoding;
pub use negotiate::{Negotiator, Notice};
pub use proto::{opt, Event, Parser};
pub use shell::{run_shell, ShellOptions};
pub use stream::{Echo, Input, TelnetStream};
