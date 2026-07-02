//! Loudness metering: per-frame RMS and peak for VU displays.
//!
//! Every UI that shows the radio — GUI player, TUI status bar, web admin —
//! wants a VU meter. [`Loudness::measure`] reduces one [`Frame`] to two
//! normalized numbers: RMS (perceived level, the needle) and peak (clip
//! headroom, the red LED). Both are in `0.0..=1.0` of full scale, with
//! dBFS conversions for meters that draw in decibels.

use crate::Frame;

/// Full-scale reference for normalizing `i16` samples (`|i16::MIN|`).
const FULL_SCALE: f32 = 32_768.0;

/// One frame's loudness: RMS level and absolute peak, normalized to full scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Root-mean-square level in `0.0..=1.0` (perceived loudness).
    pub rms: f32,
    /// Largest absolute sample in `0.0..=1.0` (1.0 = digital full scale).
    pub peak: f32,
}

impl Loudness {
    /// Measures one frame (all channels pooled).
    pub fn measure(frame: &Frame) -> Self {
        let samples = frame.samples();
        let mut sum_squares = 0.0f64;
        let mut peak = 0i32;
        for &s in samples {
            let s = i32::from(s);
            sum_squares += f64::from(s) * f64::from(s);
            peak = peak.max(s.abs());
        }
        let mean = sum_squares / samples.len() as f64;
        Self {
            rms: (mean.sqrt() / f64::from(FULL_SCALE)) as f32,
            peak: peak as f32 / FULL_SCALE,
        }
    }

    /// RMS in dBFS (`0.0` = full scale; silence is `-inf`).
    pub fn rms_dbfs(&self) -> f32 {
        20.0 * self.rms.log10()
    }

    /// Peak in dBFS (`0.0` = full scale; silence is `-inf`).
    pub fn peak_dbfs(&self) -> f32 {
        20.0 * self.peak.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioFormat;

    fn mono8k() -> AudioFormat {
        AudioFormat::new(8_000, 1).unwrap()
    }

    #[test]
    fn silence_is_zero() {
        let l = Loudness::measure(&Frame::silence(mono8k()));
        assert_eq!(l.rms, 0.0);
        assert_eq!(l.peak, 0.0);
        assert_eq!(l.rms_dbfs(), f32::NEG_INFINITY);
        assert_eq!(l.peak_dbfs(), f32::NEG_INFINITY);
    }

    #[test]
    fn dc_full_scale_reads_hot() {
        let f = mono8k();
        let frame = Frame::from_samples(f, vec![i16::MIN; f.samples_per_frame()]).unwrap();
        let l = Loudness::measure(&frame);
        assert_eq!(l.peak, 1.0);
        assert_eq!(l.rms, 1.0);
        assert!(l.peak_dbfs().abs() < 1e-6);
    }

    #[test]
    fn square_wave_rms_equals_peak() {
        let f = mono8k();
        let samples: Vec<i16> = (0..f.samples_per_frame())
            .map(|i| if i % 2 == 0 { 16_384 } else { -16_384 })
            .collect();
        let l = Loudness::measure(&Frame::from_samples(f, samples).unwrap());
        assert!((l.peak - 0.5).abs() < 1e-6);
        assert!((l.rms - 0.5).abs() < 1e-6);
        assert!((l.rms_dbfs() - (-6.020_6)).abs() < 1e-3);
    }

    #[test]
    fn peak_tracks_single_spike() {
        let f = mono8k();
        let mut samples = vec![0i16; f.samples_per_frame()];
        samples[42] = -8_192; // negative spikes count too
        let l = Loudness::measure(&Frame::from_samples(f, samples).unwrap());
        assert!((l.peak - 0.25).abs() < 1e-6);
        assert!(l.rms > 0.0 && l.rms < l.peak);
    }
}
