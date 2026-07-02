//! Frame mixer: sums N same-format sources with per-source gain, saturating.
//!
//! The station side of the radio needs to blend inputs — a DJ voice over a
//! music bed, or several live sources — into the single frame stream a
//! [`crate::Station`] broadcasts. [`Mixer`] does that one frame at a time:
//! each source sample is scaled by its gain, summed, and clamped to the
//! `i16` range so hot mixes clip audibly instead of wrapping into noise.

use crate::{AudioError, AudioFormat, Frame};

/// Maximum per-source gain accepted by [`Mixer::mix`] (+6 dB boost).
pub const MAX_GAIN: f32 = 2.0;

/// Mixes same-format frames into one output frame.
///
/// Stateless apart from its configured [`AudioFormat`]; call
/// [`Mixer::mix`] once per 20 ms tick with that tick's input frames.
#[derive(Clone, Copy, Debug)]
pub struct Mixer {
    format: AudioFormat,
}

impl Mixer {
    /// Creates a mixer for `format`; all inputs must match it.
    pub fn new(format: AudioFormat) -> Self {
        Self { format }
    }

    /// The format this mixer accepts and produces.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Sums `inputs` (frame, gain) pairs into one frame.
    ///
    /// Each sample is scaled by its source's gain (0.0–[`MAX_GAIN`]), the
    /// scaled sources are summed, and the total saturates to the `i16`
    /// range. No inputs yields silence. Fails on a format mismatch or an
    /// out-of-range/non-finite gain.
    pub fn mix(&self, inputs: &[(&Frame, f32)]) -> Result<Frame, AudioError> {
        for &(frame, gain) in inputs {
            if frame.format() != self.format {
                return Err(AudioError::FormatMismatch {
                    expected: self.format,
                    got: frame.format(),
                });
            }
            if !gain.is_finite() || !(0.0..=MAX_GAIN).contains(&gain) {
                return Err(AudioError::InvalidGain(gain));
            }
        }

        let mut acc = vec![0i64; self.format.samples_per_frame()];
        for &(frame, gain) in inputs {
            for (slot, &sample) in acc.iter_mut().zip(frame.samples()) {
                *slot += (f32::from(sample) * gain).round() as i64;
            }
        }
        let samples = acc
            .into_iter()
            .map(|total| total.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16)
            .collect();
        Frame::from_samples(self.format, samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono8k() -> AudioFormat {
        AudioFormat::new(8_000, 1).unwrap()
    }

    fn constant(format: AudioFormat, value: i16) -> Frame {
        Frame::from_samples(format, vec![value; format.samples_per_frame()]).unwrap()
    }

    #[test]
    fn sums_sources() {
        let f = mono8k();
        let a = constant(f, 1_000);
        let b = constant(f, -250);
        let out = Mixer::new(f).mix(&[(&a, 1.0), (&b, 1.0)]).unwrap();
        assert!(out.samples().iter().all(|&s| s == 750));
    }

    #[test]
    fn gain_scales_and_boosts() {
        let f = mono8k();
        let a = constant(f, 1_000);
        let mixer = Mixer::new(f);
        let half = mixer.mix(&[(&a, 0.5)]).unwrap();
        assert!(half.samples().iter().all(|&s| s == 500));
        let double = mixer.mix(&[(&a, 2.0)]).unwrap();
        assert!(double.samples().iter().all(|&s| s == 2_000));
        let muted = mixer.mix(&[(&a, 0.0)]).unwrap();
        assert!(muted.is_silent());
    }

    #[test]
    fn clipping_saturates_instead_of_wrapping() {
        let f = mono8k();
        let loud = constant(f, i16::MAX);
        let quiet = constant(f, i16::MIN);
        let mixer = Mixer::new(f);
        let hot = mixer.mix(&[(&loud, 1.0), (&loud, 1.0)]).unwrap();
        assert!(hot.samples().iter().all(|&s| s == i16::MAX));
        let cold = mixer.mix(&[(&quiet, 2.0), (&quiet, 2.0)]).unwrap();
        assert!(cold.samples().iter().all(|&s| s == i16::MIN));
    }

    #[test]
    fn empty_mix_is_silence() {
        let f = mono8k();
        assert!(Mixer::new(f).mix(&[]).unwrap().is_silent());
    }

    #[test]
    fn rejects_bad_gain_and_format() {
        let f = mono8k();
        let stereo = AudioFormat::new(8_000, 2).unwrap();
        let a = constant(f, 1);
        let wrong = Frame::silence(stereo);
        let mixer = Mixer::new(f);
        assert_eq!(mixer.mix(&[(&a, 2.5)]), Err(AudioError::InvalidGain(2.5)));
        assert_eq!(mixer.mix(&[(&a, -0.1)]), Err(AudioError::InvalidGain(-0.1)));
        assert!(matches!(
            mixer.mix(&[(&a, f32::NAN)]),
            Err(AudioError::InvalidGain(_))
        ));
        assert_eq!(
            mixer.mix(&[(&wrong, 1.0)]),
            Err(AudioError::FormatMismatch {
                expected: f,
                got: stereo
            })
        );
    }
}
