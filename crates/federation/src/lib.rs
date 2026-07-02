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
//! - [`catalog`]: signed, incremental server file-catalogs — a server's
//!   advertised file listing, canonicalized and Ed25519-signed.
//! - [`search`]: cross-server file search — a query model and a pure matcher
//!   over one catalog, tagged with provenance.
//! - [`dedupe`]: blake3 dedupe that collapses the same file offered by many
//!   servers into one result carrying all its sources.
//! - [`fanout`]: pull fan-out planning — turning a deduped match's sources
//!   into an ordered [`fanout::FetchPlan`] for the transfer layer.
//! - [`attestation`]: cross-server identity — `persona@server` addressing,
//!   home-server-signed [`attestation::PersonaAttestation`]s, and
//!   key-continuity chains where every rotation is cross-signed by the
//!   previous persona key so a server can't silently swap a user's key.
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

pub mod attestation;
pub mod bloom;
pub mod catalog;
pub mod dedupe;
pub mod fanout;
pub mod floodfill;
pub mod handshake;
pub mod policy;
pub mod redaction;
pub mod search;

pub use attestation::{
    is_valid_persona_name, is_valid_server_name, sign_challenge, verify_visitor, AddressError,
    AttestationBody, AttestationError, ContinuityChain, ContinuityError, FedAddress, KeyRotation,
    PersonaAttestation, ATTESTATION_CONTEXT, CHALLENGE_CONTEXT, MAX_PERSONA_LEN, MAX_SERVER_LEN,
    MIN_CHALLENGE_LEN, ROTATION_CONTEXT,
};
pub use bloom::BloomFilter;
pub use catalog::{Catalog, CatalogEntry, CatalogError, SignedCatalog, CATALOG_CONTEXT};
pub use dedupe::{dedupe_by_hash, DedupedMatch, ServerRef};
pub use fanout::{plan_fetch, plan_fetch_batch, FetchPlan, FetchPolicy, FetchStrategy};
pub use floodfill::{FedEvent, IHave, PullRequest, PushEvents, Subscription};
pub use handshake::{DescriptorBody, PeerDescriptor, PeerHello, PeerHelloAck};
pub use policy::{PeerPolicy, PolicyMode, RateLimiter};
pub use redaction::Redaction;
pub use search::{search_catalog, Match, SearchQuery, SearchResult};
