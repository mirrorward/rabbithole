//! # rabbithole-server-core
//!
//! Burrow's domain logic, independent of any listener: sessions, presence,
//! rooms, boards, files, permissions, federation. Protocol surfaces (RHP,
//! telnet, Hotline, NNTP, …) are projections over this crate.
//!
//! Wave 0 provides the event bus and the crate shape; Wave 1 brings the
//! session/auth/presence/chat services.

#![forbid(unsafe_code)]

pub mod bus;

pub use bus::{EventBus, ServerEvent};
