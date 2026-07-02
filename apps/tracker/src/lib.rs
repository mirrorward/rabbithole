//! # Looking Glass — the RabbitHole tracker/directory service
//!
//! A tracker is a directory of live servers: servers *register* themselves
//! periodically (a heartbeat) and clients *list* what is currently alive.
//! Entries that stop refreshing quietly fall out of the directory.
//!
//! This slice speaks the classic **Hotline tracker protocol (HTRK)** so
//! vintage Hotline clients and servers can use a Looking Glass today, plus a
//! tiny plain-text status query as the native placeholder until the RHP
//! tracker family lands (see `PLAN.md`). Note that HTRK is a *different*
//! protocol from the Hotline server protocol (`TRTP`/`HOTL`, implemented in
//! `rabbithole-legacy-hotline`) — the codecs here are self-contained.
//!
//! On top of the classic flow (which keeps working unchanged), servers can
//! register with an Ed25519-**signed descriptor** ([`descriptor`]) carrying
//! category tags, and trackers with static peer lists exchange those signed
//! entries via **gossip** ([`gossip`]) so an announce to one tracker
//! propagates to all of them.
//!
//! ## Listeners
//!
//! | Listener             | Transport | Classic port | Module      |
//! |----------------------|-----------|--------------|-------------|
//! | Server registration  | UDP       | 5499         | [`service`] |
//! | Client listing       | TCP       | 5498         | [`service`] |
//! | Native status (stub) | TCP       | 4655         | [`service`] |
//! | Gossip + announces   | UDP       | 4656         | [`service`] |
//!
//! ## Module map
//!
//! - [`registry`] — the in-memory TTL'd server registry (and the signed-key
//!   conflict policy).
//! - [`htrk`] — pure HTRK wire codecs (registration packet, listing stream).
//! - [`descriptor`] — signed server descriptors (canonical postcard,
//!   context-prefixed Ed25519, like federation catalogs).
//! - [`gossip`] — sans-IO anti-entropy model (digest/diff/batch) + the UDP
//!   gossip codec.
//! - [`service`] — the async listeners that glue sockets to the registry.
//!
//! Every decoder is total: malformed or truncated input yields `Err`, never a
//! panic — the tracker sits on the open internet and must shrug off garbage.

#![forbid(unsafe_code)]

pub mod descriptor;
pub mod gossip;
pub mod htrk;
pub mod registry;
pub mod service;

pub use descriptor::{Descriptor, DescriptorError, SignedDescriptor, DESCRIPTOR_CONTEXT};
pub use gossip::{GossipBatch, GossipDigest, GossipMessage, Want};
pub use registry::{RegisterError, Registry, ServerEntry, DEFAULT_TTL};
