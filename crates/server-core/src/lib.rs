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
pub mod bus;
pub mod chat;
pub mod config;
pub mod permissions;
pub mod presence;
pub mod pushlog;

pub use auth::{AuthError, AuthService, AuthedUser};
pub use bus::{EventBus, ServerEvent};
pub use chat::{ChatService, LOBBY};
pub use config::{LiveConfig, ServerConfig};
pub use permissions::{Caps, PermissionEvaluator, Role, Subject};
pub use presence::{PresenceEntry, PresenceRegistry};
pub use pushlog::PushLog;
