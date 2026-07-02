//! Transport-agnostic audio core for RabbitHole's radio (Wave 11).
//!
//! Server-hosted "pirate radio" needs one shared vocabulary long before any
//! socket or codec exists: a DJ source produces PCM, the server mixes and
//! fans it out to N listeners, and each listener smooths network jitter into
//! a steady 20 ms cadence. This crate is exactly that seam — pure data types
//! and async fan-out, no I/O, no Opus/MP3 (encode pipelines land in a later
//! slice on top of these frames).
//!
//! The pieces, in signal order:
//!
//! - [`AudioFormat`] + [`Frame`]: validated PCM formats (8 kHz–192 kHz,
//!   mono/stereo) and fixed [`FRAME_DURATION_MS`] frames of interleaved
//!   `i16` samples; [`frames_from_pcm`] slices raw buffers into frames,
//!   padding the tail with silence.
//! - [`Mixer`]: sums N same-format frames into one with per-source gain
//!   (0.0–2.0) and saturating `i16` output — hot signals clip, they never
//!   wrap.
//! - [`Station`]: a broadcast hub where one live source feeds
//!   [`StationEvent`]s (frames plus [`NowPlaying`] metadata) to many
//!   [`Listener`]s. Slow listeners are dropped-behind (they skip and count
//!   lag); they can never stall the source.
//! - [`JitterBuffer`]: ring-buffered listener-side smoothing with a target
//!   depth in frames — underruns yield silence, overruns drop the oldest
//!   frame, both are counted.
//! - [`Loudness`]: per-frame RMS + peak metering for VU displays.
//!
//! ```
//! use rabbithole_audio::{frames_from_pcm, AudioFormat, Mixer};
//!
//! let format = AudioFormat::new(8_000, 1).unwrap();
//! let frames = frames_from_pcm(format, &[1_000i16; 200]); // 160/frame, tail padded
//! assert_eq!(frames.len(), 2);
//! let mix = Mixer::new(format)
//!     .mix(&[(&frames[0], 1.0), (&frames[1], 0.5)])
//!     .unwrap();
//! assert_eq!(mix.samples()[0], 1_500);
//! ```

#![forbid(unsafe_code)]

mod frame;
mod jitter;
mod meter;
mod mixer;
mod station;

pub use frame::{frames_from_pcm, AudioFormat, Frame, FRAME_DURATION_MS};
pub use jitter::JitterBuffer;
pub use meter::Loudness;
pub use mixer::{Mixer, MAX_GAIN};
pub use station::{Listener, NowPlaying, Station, StationEvent};

/// Errors from the audio core: bad formats, mismatched frames, bad gain.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AudioError {
    /// Sample rate outside the supported 8 kHz–192 kHz range.
    #[error("sample rate {0} Hz out of range ({min}-{max} Hz)", min = AudioFormat::MIN_SAMPLE_RATE, max = AudioFormat::MAX_SAMPLE_RATE)]
    InvalidSampleRate(u32),
    /// Channel count other than 1 (mono) or 2 (stereo).
    #[error("unsupported channel count {0} (only mono/stereo)")]
    InvalidChannels(u8),
    /// A frame's format did not match the format an API expected.
    #[error("format mismatch: expected {expected:?}, got {got:?}")]
    FormatMismatch {
        /// Format the operation was configured for.
        expected: AudioFormat,
        /// Format of the offending frame.
        got: AudioFormat,
    },
    /// A raw sample buffer was not exactly one frame long.
    #[error("bad frame length: expected {expected} samples, got {got}")]
    BadFrameLength {
        /// Samples required for one frame in the given format.
        expected: usize,
        /// Samples actually supplied.
        got: usize,
    },
    /// A mixer gain outside 0.0–[`MAX_GAIN`] (or non-finite).
    #[error("gain {0} out of range (0.0-{MAX_GAIN})")]
    InvalidGain(f32),
}
