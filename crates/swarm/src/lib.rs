//! The Warren — swarm file distribution (Wave 5).
//!
//! Files move as content-addressed sets: a [`Manifest`] catalogs each file's
//! path, size, and blake3 root (the Bao verification anchor), and a
//! [`RabbitLink`] (`rabbit://…`) is the shareable, verifiable reference into
//! it. This first slice is the data layer — manifests and links — with no
//! network yet; peer discovery, advertise/announce, and multi-source
//! Bao-verified transfer build on top in the following slices. Since Wave 14,
//! links can also be homed on the Reticulum mesh or carry RNS destination
//! hashes as alternate routes (see [`link`]).

#![forbid(unsafe_code)]

pub mod cap;
pub mod link;
pub mod manifest;
pub mod peer;
pub mod scheduler;

pub use cap::{
    token_allows, CapClaim, CapError, CapToken, S2sCapClaim, S2sCapToken, CAP_CONTEXT,
    S2S_CAP_CONTEXT,
};
pub use link::{
    DestinationHash, DestinationHashError, LinkAuthority, LinkError, LinkTarget, RabbitLink,
};
pub use manifest::{Manifest, ManifestError, ManifestFile, CHUNK_SIZE};
pub use peer::{
    decode_proved, encode_proved, fetch_file, fetch_have, fetch_proved, fetch_range,
    fetch_range_proved, proofs_path, stream_limit, write_outboard, BaoPiece, HaveMap, PeerAsk,
    PeerError, PeerServer, PeerSource, Proved, RangeSource, SeedStore, Sharing, HAVE_UNIT,
    STATUS_NOT_HELD,
};
pub use scheduler::{
    fetch_swarm, fetch_swarm_from, fetch_swarm_resumable, fetch_swarm_resumable_with_progress,
    fetch_swarm_sharing, FetchReport, ProgressSink, SourcePeer, UnitDone, UNIT_SIZE,
};
