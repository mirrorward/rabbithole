//! # RabbitHole Protocol (RHP)
//!
//! Wire types, framing, and version negotiation for the RabbitHole native
//! protocol. This crate is intentionally small and dependency-light: it is
//! compiled by the server, every native client, and the wasm web client, so
//! it must build for `wasm32-unknown-unknown` (no tokio, no I/O — pure types
//! and codecs).
//!
//! Design DNA (see `PLAN.md` §5 and `docs/protocol/`):
//! - Hotline's uniform transaction model: every message is a request, a
//!   reply, or a server push, in one framing.
//! - OSCAR's family namespacing and TLV-style extensibility: messages are
//!   grouped by [`Family`], and payloads are `#[non_exhaustive]` serde enums
//!   so unknown variants and new optional fields degrade gracefully.
//! - Pushes are routed by `(kind, family, type)`, never by outstanding
//!   request id.
//!
//! The wire format is [postcard](https://docs.rs/postcard) with explicit
//! length-delimited framing ([`codec`]). The protocol version is negotiated
//! in [`Hello`]/[`HelloAck`] before anything else flows.

#![forbid(unsafe_code)]

pub mod admin;
pub mod blob;
pub mod board;
pub mod chat;
pub mod codec;
pub mod directory;
pub mod dm;
pub mod error;
pub mod filelib;
pub mod frame;
pub mod hello;
pub mod persona;
pub mod presence;
pub mod registry;
pub mod session;
pub mod swarm;
pub mod transfer;
pub mod version;
pub mod welcome;
pub mod wish;

pub use codec::{decode_frame, encode_frame, FrameCodec};
pub use error::{ErrorCode, ProtoError};
pub use frame::{Family, Frame, FrameKind, Message, Payload, RequestId};
pub use hello::{Capability, CapabilitySet, Hello, HelloAck};
pub use registry::{RegistryEntry, REGISTRY};
pub use version::{ProtocolVersion, PROTOCOL_VERSION};
