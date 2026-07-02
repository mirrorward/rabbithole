//! Error type shared across the radio service layer.

use crate::track::TrackId;

/// Failures from station registry, playlist, and request-queue operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RadioError {
    /// A station with the given slug already exists in the registry.
    #[error("station {0:?} already exists")]
    StationExists(String),
    /// No station with the given slug (or mount) is registered.
    #[error("station {0:?} not found")]
    StationNotFound(String),
    /// The track is already present in the request queue (dedupe).
    #[error("track {0:?} is already queued")]
    TrackAlreadyQueued(TrackId),
    /// The track is not present in the request queue.
    #[error("track {0:?} is not queued")]
    TrackNotQueued(TrackId),
}
