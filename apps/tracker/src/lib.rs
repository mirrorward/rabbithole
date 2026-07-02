//! # Looking Glass — the RabbitHole tracker/directory service
//!
//! A tracker is a directory of live servers: servers *register* themselves
//! periodically (a heartbeat) and clients *list* what is currently alive.
//! Entries that stop refreshing quietly fall out of the directory.
//!
//! This first slice speaks the classic **Hotline tracker protocol (HTRK)** so
//! vintage Hotline clients and servers can use a Looking Glass today, plus a
//! tiny plain-text status query as the native placeholder until the RHP
//! tracker family lands (see `PLAN.md`). Note that HTRK is a *different*
//! protocol from the Hotline server protocol (`TRTP`/`HOTL`, implemented in
//! `rabbithole-legacy-hotline`) — the codecs here are self-contained.
//!
//! ## Listeners
//!
//! | Listener             | Transport | Classic port | Module      |
//! |----------------------|-----------|--------------|-------------|
//! | Server registration  | UDP       | 5499         | [`service`] |
//! | Client listing       | TCP       | 5498         | [`service`] |
//! | Native status (stub) | TCP       | 4655         | [`service`] |
//!
//! ## Module map
//!
//! - [`registry`] — the in-memory TTL'd server registry.
//! - [`htrk`] — pure HTRK wire codecs (registration packet, listing stream).
//! - [`service`] — the async listeners that glue sockets to the registry.
//!
//! Every decoder is total: malformed or truncated input yields `Err`, never a
//! panic — the tracker sits on the open internet and must shrug off garbage.

#![forbid(unsafe_code)]

pub mod htrk;
pub mod registry;
pub mod service;

pub use registry::{Registry, ServerEntry, DEFAULT_TTL};
