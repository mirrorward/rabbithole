//! Error type for drop-file parsing and the door-session runner model.
//!
//! Drop-file readers are *total*: they never panic on malformed or truncated
//! input. The only hard error they raise is [`Error::Empty`] (nothing to
//! parse); everything else is handled best-effort, leaving unparseable fields
//! at their [`Default`](crate::DoorContext) values.
//!
//! The runner-model errors are equally tame: registry and node-pool
//! operations report structured failures ([`Error::DuplicateDoor`],
//! [`Error::NodesExhausted`], …) and the session state machine reports
//! illegal transitions as [`Error::BadTransition`]. Nothing here panics.

/// Errors that can arise while decoding a drop file back into a
/// [`DoorContext`](crate::DoorContext), or while working with the
/// door-session runner model ([`DoorRegistry`](crate::DoorRegistry),
/// [`NodePool`](crate::NodePool), [`DoorSession`](crate::DoorSession)).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The supplied buffer had no content lines at all.
    #[error("drop file is empty")]
    Empty,

    /// A [`DoorDef`](crate::DoorDef) failed validation; `reason` says why.
    #[error("invalid door definition `{id}`: {reason}")]
    InvalidDoor {
        /// The offending door's id (possibly empty — that is one failure mode).
        id: String,
        /// Human-readable reason the definition was rejected.
        reason: &'static str,
    },

    /// A door with this id is already present in the registry.
    #[error("door `{0}` is already registered")]
    DuplicateDoor(String),

    /// No free node number exists in the requested (pool-clamped) range.
    #[error("no free node in {first}..={last}")]
    NodesExhausted {
        /// First node number of the requested range (inclusive).
        first: u16,
        /// Last node number of the requested range (inclusive).
        last: u16,
    },

    /// A [`DoorSession`](crate::DoorSession) event was applied in a state
    /// that does not permit it.
    #[error("invalid door-session transition: {event} while {state}")]
    BadTransition {
        /// Name of the state the session was in.
        state: &'static str,
        /// Name of the rejected event.
        event: &'static str,
    },
}
