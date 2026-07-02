//! The unit of programming: a [`Track`] and its opaque media handle.
//!
//! A track is *metadata plus a pointer*, never bytes. The scheduler, playlist,
//! and vote queue reason about tracks purely by their [`TrackId`] and
//! [`duration_ms`](Track::duration_ms); the actual audio lives behind a
//! [`BlobId`] — an opaque content handle this crate never dereferences.
//! Turning a [`BlobId`] into PCM (fetching the blob, decoding MP3/Opus, slicing
//! into [`Frame`](rabbithole_audio::Frame)s) is the encoder/transport slice's
//! job, downstream of everything here. Keeping tracks I/O-free is what makes
//! the whole service layer deterministic and unit-testable without a runtime.

/// Stable identity of a track within a station's library.
///
/// A plain `u64` keeps rotation, dedupe, and vote-ordering logic cheap and
/// `Copy`. Callers map their own catalog keys (row ids, hashes) onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

/// Opaque handle to a track's audio payload.
///
/// This is a 32-byte content address (e.g. a BLAKE3 blob id from
/// `rabbithole-blobs`). This crate treats it as **completely opaque**: it is
/// copied around, compared, and handed to the transport layer, but it is never
/// dereferenced, and holding a [`Track`] never reads a file or touches the
/// network. Resolving a `BlobId` to bytes is the encoder/transport slice's
/// responsibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlobId(pub [u8; 32]);

impl BlobId {
    /// The all-zero blob id, handy for placeholders and tests.
    pub const ZERO: BlobId = BlobId([0u8; 32]);
}

/// A single schedulable item: display metadata plus an opaque media handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    /// Stable identity used for rotation, dedupe, and vote ordering.
    pub id: TrackId,
    /// Human-readable title, surfaced in now-playing metadata.
    pub title: String,
    /// Human-readable artist, surfaced in now-playing metadata.
    pub artist: String,
    /// Playout duration in milliseconds; drives finish detection.
    pub duration_ms: u64,
    /// Opaque handle to the audio payload (see [`BlobId`]).
    pub source: BlobId,
}

impl Track {
    /// Builds a track from its parts.
    pub fn new(
        id: TrackId,
        title: impl Into<String>,
        artist: impl Into<String>,
        duration_ms: u64,
        source: BlobId,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            artist: artist.into(),
            duration_ms,
            source,
        }
    }
}
