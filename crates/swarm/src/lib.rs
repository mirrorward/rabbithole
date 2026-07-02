//! The Warren — swarm file distribution (Wave 5).
//!
//! Files move as content-addressed sets: a [`Manifest`] catalogs each file's
//! path, size, and blake3 root (the Bao verification anchor), and a
//! [`RabbitLink`] (`rabbit://…`) is the shareable, verifiable reference into
//! it. This first slice is the data layer — manifests and links — with no
//! network yet; peer discovery, advertise/announce, and multi-source
//! Bao-verified transfer build on top in the following slices.

#![forbid(unsafe_code)]

pub mod cap;
pub mod link;
pub mod manifest;
pub mod peer;

pub use cap::{CapClaim, CapError, CapToken, CAP_CONTEXT};
pub use link::{LinkError, LinkTarget, RabbitLink};
pub use manifest::{Manifest, ManifestError, ManifestFile, CHUNK_SIZE};
pub use peer::{fetch_file, fetch_range, PeerError, PeerServer, SeedStore};
