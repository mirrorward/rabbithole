//! Server-to-server federation: the protocol data model and the core,
//! I/O-free algorithms that a Burrow's federation service (Wave 9) is wired
//! on top of.
//!
//! This crate is the *foundation only* — it carries no networking, no store
//! access, and no Burrow wiring. It defines the wire types two servers speak
//! and the pure logic they run locally:
//!
//! - [`handshake`]: the peering hello/ack and the signed
//!   `.well-known/rabbithole/server` [`PeerDescriptor`].
//! - [`floodfill`]: the board flood-fill model — subscriptions and the
//!   `ihave`/`pull`/`push` exchange that moves signed board events between
//!   peers unchanged.
//! - [`bloom`]: a space-efficient Bloom filter for the "have I seen this
//!   event id?" seen-set that gates re-ingest and rebroadcast loops.
//! - [`redaction`]: server-sovereign tombstone/redact propagation.
//! - [`policy`]: ingest-defense primitives — a per-peer token-bucket rate
//!   limiter and an allow/deny [`policy::PeerPolicy`].
//!
//! Everything here is deliberately dependency-light and wasm-friendly (no
//! tokio, no filesystem, no sockets) so a browser client could verify a
//! server descriptor or a redaction offline. All wire decoders go through
//! `postcard`, which returns `Result` rather than panicking on arbitrary
//! bytes.
//!
//! Signatures follow the established RabbitHole pattern (see the swarm
//! capability tokens and board events): Ed25519 over **domain-separated**
//! canonical postcard bytes, so a signature minted for one surface can never
//! be replayed against another.

#![forbid(unsafe_code)]

pub mod bloom;
pub mod floodfill;
pub mod handshake;
pub mod policy;
pub mod redaction;

pub use bloom::BloomFilter;
pub use floodfill::{FedEvent, IHave, PullRequest, PushEvents, Subscription};
pub use handshake::{DescriptorBody, PeerDescriptor, PeerHello, PeerHelloAck};
pub use policy::{PeerPolicy, PolicyMode, RateLimiter};
pub use redaction::Redaction;
