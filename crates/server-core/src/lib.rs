//! # rabbithole-server-core
//!
//! Burrow's domain logic, independent of any listener: sessions, presence,
//! rooms, boards, files, permissions, federation. Protocol surfaces (RHP,
//! telnet, Hotline, NNTP, …) are projections over this crate.
//!
//! Wave 1: event bus, config, permissions (roles/classes/ACLs), auth
//! (password/guest/resume), presence registry, lobby chat, push replay log.

#![forbid(unsafe_code)]

pub mod auth;
pub mod boards;
pub mod bus;
pub mod chat;
pub mod classes;
pub mod config;
pub mod dedup;
pub mod events;
pub mod federation;
pub mod files;
pub mod permissions;
pub mod presence;
pub mod pushlog;
pub mod ratelimit;
pub mod swarm;

pub use auth::{AuthError, AuthService, AuthedUser, RegistrationMode};
pub use boards::{BoardError, BoardService};
pub use bus::{EventBus, ServerEvent};
pub use chat::{ChatService, LOBBY};
pub use classes::ClassCache;
pub use config::{LiveConfig, ServerConfig};
pub use dedup::{DedupStore, SeenKey};
pub use federation::{PeerRecord, PeerRegistry, PeerState};
pub use files::{FileError, FileService};
pub use permissions::{security_level, Caps, PermissionEvaluator, Role, Subject};
pub use presence::{PresenceEntry, PresenceRegistry, RadioStatus};
pub use pushlog::PushLog;
pub use ratelimit::RateLimiter;
pub use swarm::SwarmCatalog;
