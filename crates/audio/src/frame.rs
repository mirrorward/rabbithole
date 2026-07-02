//! PCM frame model: validated formats and fixed 20 ms frames of `i16` samples.
//!
//! Every stage of the radio pipeline — mixer, station fan-out, jitter buffer,
//! meters — speaks in [`Frame`]s: exactly [`FRAME_DURATION_MS`] of interleaved
//! signed 16-bit PCM in a validated [`AudioFormat`]. Fixed-size frames keep
//! timing math trivial (one frame == one tick of the playout clock) and map
//! directly onto the Opus packet cadence that later slices will encode.

use crate::AudioError;

/// Duration of every PCM frame, in milliseconds.
///
/// 20 ms is the classic real-time audio frame: small enough for live DJ
/// latency, large enough to amortize per-packet overhead, and Opus's
/// preferred packet duration when encoding lands.
pub const FRAME_DURATION_MS: u32 = 20;

/// A validated PCM stream format: sample rate and channel count.
///
/// Construct via [`AudioFormat::new`], which enforces 8 kHz–192 kHz and
/// mono/stereo; a value of this type is therefore always valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    sample_rate: u32,
    channels: u8,
}

impl AudioFormat {
    /// Lowest accepted sample rate (telephone-band 8 kHz).
    pub const MIN_SAMPLE_RATE: u32 = 8_000;
    /// Highest accepted sample rate (studio 192 kHz).
    pub const MAX_SAMPLE_RATE: u32 = 192_000;

    /// Creates a format, validating rate (8 kHz–192 kHz) and channels (1–2).
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, AudioError> {
        if !(Self::MIN_SAMPLE_RATE..=Self::MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(AudioError::InvalidSampleRate(sample_rate));
        }
        if !(1..=2).contains(&channels) {
            return Err(AudioError::InvalidChannels(channels));
        }
        Ok(Self {
            sample_rate,
            channels,
        })
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count (1 = mono, 2 = stereo).
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Samples **per channel** in one [`FRAME_DURATION_MS`] frame.
    ///
    /// Rates that don't divide evenly into 20 ms (e.g. 11 025 Hz) truncate
    /// to the nearest whole sample.
    pub fn samples_per_channel(&self) -> usize {
        self.sample_rate as usize * FRAME_DURATION_MS as usize / 1_000
    }

    /// Total interleaved samples in one frame (per-channel count × channels).
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_channel() * self.channels as usize
    }
}

/// One fixed-duration frame of interleaved `i16` PCM in a known format.
///
/// A frame always holds exactly [`AudioFormat::samples_per_frame`] samples;
/// constructors enforce this, so consumers never need length checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    format: AudioFormat,
    samples: Vec<i16>,
}

impl Frame {
    /// A frame of pure silence (all-zero samples) in `format`.
    pub fn silence(format: AudioFormat) -> Self {
        Self {
            format,
            samples: vec![0; format.samples_per_frame()],
        }
    }

    /// Wraps exactly one frame's worth of interleaved samples.
    ///
    /// Fails with [`AudioError::BadFrameLength`] if `samples` is not exactly
    /// [`AudioFormat::samples_per_frame`] long.
    pub fn from_samples(format: AudioFormat, samples: Vec<i16>) -> Result<Self, AudioError> {
        let expected = format.samples_per_frame();
        if samples.len() != expected {
            return Err(AudioError::BadFrameLength {
                expected,
                got: samples.len(),
            });
        }
        Ok(Self { format, samples })
    }

    /// The frame's format.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// The interleaved samples (always exactly one frame long).
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    /// True if every sample is zero.
    pub fn is_silent(&self) -> bool {
        self.samples.iter().all(|&s| s == 0)
    }
}

/// Slices a raw interleaved `i16` buffer into whole frames.
///
/// The final partial frame, if any, is padded with silence so callers always
/// get full frames. An empty buffer yields no frames.
pub fn frames_from_pcm(format: AudioFormat, pcm: &[i16]) -> Vec<Frame> {
    let per_frame = format.samples_per_frame();
    pcm.chunks(per_frame)
        .map(|chunk| {
            let mut samples = chunk.to_vec();
            samples.resize(per_frame, 0);
            Frame { format, samples }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono8k() -> AudioFormat {
        AudioFormat::new(8_000, 1).unwrap()
    }

    #[test]
    fn format_validation() {
        assert!(AudioFormat::new(8_000, 1).is_ok());
        assert!(AudioFormat::new(192_000, 2).is_ok());
        assert!(AudioFormat::new(48_000, 2).is_ok());
        assert_eq!(
            AudioFormat::new(7_999, 1),
            Err(AudioError::InvalidSampleRate(7_999))
        );
        assert_eq!(
            AudioFormat::new(192_001, 1),
            Err(AudioError::InvalidSampleRate(192_001))
        );
        assert_eq!(
            AudioFormat::new(48_000, 0),
            Err(AudioError::InvalidChannels(0))
        );
        assert_eq!(
            AudioFormat::new(48_000, 3),
            Err(AudioError::InvalidChannels(3))
        );
    }

    #[test]
    fn samples_per_frame_math() {
        assert_eq!(mono8k().samples_per_frame(), 160);
        let stereo48k = AudioFormat::new(48_000, 2).unwrap();
        assert_eq!(stereo48k.samples_per_channel(), 960);
        assert_eq!(stereo48k.samples_per_frame(), 1_920);
        let cd = AudioFormat::new(44_100, 2).unwrap();
        assert_eq!(cd.samples_per_frame(), 1_764);
    }

    #[test]
    fn frame_length_enforced() {
        let f = mono8k();
        assert!(Frame::from_samples(f, vec![0; 160]).is_ok());
        assert_eq!(
            Frame::from_samples(f, vec![0; 159]),
            Err(AudioError::BadFrameLength {
                expected: 160,
                got: 159
            })
        );
        assert_eq!(Frame::silence(f).samples().len(), 160);
        assert!(Frame::silence(f).is_silent());
    }

    #[test]
    fn slicing_pads_the_tail() {
        let f = mono8k();
        let pcm: Vec<i16> = (0..400).map(|i| i as i16).collect(); // 2.5 frames
        let frames = frames_from_pcm(f, &pcm);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].samples()[0], 0);
        assert_eq!(frames[1].samples()[0], 160);
        // Tail: 80 real samples then 80 of silence padding.
        assert_eq!(frames[2].samples()[79], 399);
        assert!(frames[2].samples()[80..].iter().all(|&s| s == 0));
        assert!(frames_from_pcm(f, &[]).is_empty());
    }
}
