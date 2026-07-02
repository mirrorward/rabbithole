//! Listener-side jitter buffer: ring of frames between network and playout.
//!
//! Frames arrive from a [`crate::Station`] in bursts (network jitter, task
//! scheduling); the sound card wants one frame exactly every 20 ms. A
//! [`JitterBuffer`] sits between: pushes go in as they arrive, pops come out
//! on the playout clock. When the buffer runs dry, [`JitterBuffer::pop`]
//! substitutes silence and counts an underrun; when arrivals outrun playout
//! past twice the target depth, the oldest frame is dropped and an overrun
//! is counted. Both counters feed the stats/status surfaces later slices
//! will build.

use std::collections::VecDeque;

use crate::{AudioError, AudioFormat, Frame};

/// Ring-buffered playout smoothing with a target depth in frames.
///
/// Capacity is `2 × target_depth`: steady state hovers around the target
/// (target × 20 ms of added latency), with equal headroom above it before
/// drop-oldest kicks in.
#[derive(Debug)]
pub struct JitterBuffer {
    format: AudioFormat,
    target_depth: usize,
    frames: VecDeque<Frame>,
    underruns: u64,
    overruns: u64,
}

impl JitterBuffer {
    /// Creates a buffer aiming to hold `target_depth` frames.
    ///
    /// `target_depth` is clamped to at least 1; a target of 5 adds ~100 ms
    /// of smoothing latency.
    pub fn new(format: AudioFormat, target_depth: usize) -> Self {
        let target_depth = target_depth.max(1);
        Self {
            format,
            target_depth,
            frames: VecDeque::with_capacity(target_depth * 2),
            underruns: 0,
            overruns: 0,
        }
    }

    /// The PCM format this buffer carries.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// The steady-state depth this buffer aims for, in frames.
    pub fn target_depth(&self) -> usize {
        self.target_depth
    }

    /// Maximum frames held before pushes drop the oldest (2 × target).
    pub fn max_depth(&self) -> usize {
        self.target_depth * 2
    }

    /// Frames currently buffered.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// True when nothing is buffered (the next pop will underrun).
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Queues an arriving frame.
    ///
    /// If the buffer is already at [`JitterBuffer::max_depth`], the oldest
    /// frame is dropped to make room and the overrun counter increments.
    /// Fails on a format mismatch.
    pub fn push(&mut self, frame: Frame) -> Result<(), AudioError> {
        if frame.format() != self.format {
            return Err(AudioError::FormatMismatch {
                expected: self.format,
                got: frame.format(),
            });
        }
        if self.frames.len() >= self.max_depth() {
            self.frames.pop_front();
            self.overruns += 1;
        }
        self.frames.push_back(frame);
        Ok(())
    }

    /// Takes the next frame for playout.
    ///
    /// On an empty buffer this returns a silence frame and increments the
    /// underrun counter — playout never stops, it just goes quiet.
    pub fn pop(&mut self) -> Frame {
        match self.frames.pop_front() {
            Some(frame) => frame,
            None => {
                self.underruns += 1;
                Frame::silence(self.format)
            }
        }
    }

    /// Total pops that found the buffer empty and substituted silence.
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// Total pushes that had to drop the oldest buffered frame.
    pub fn overruns(&self) -> u64 {
        self.overruns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono8k() -> AudioFormat {
        AudioFormat::new(8_000, 1).unwrap()
    }

    fn tone(value: i16) -> Frame {
        let f = mono8k();
        Frame::from_samples(f, vec![value; f.samples_per_frame()]).unwrap()
    }

    #[test]
    fn fifo_push_pop() {
        let mut jb = JitterBuffer::new(mono8k(), 3);
        jb.push(tone(1)).unwrap();
        jb.push(tone(2)).unwrap();
        assert_eq!(jb.depth(), 2);
        assert_eq!(jb.pop().samples()[0], 1);
        assert_eq!(jb.pop().samples()[0], 2);
        assert!(jb.is_empty());
        assert_eq!(jb.underruns(), 0);
        assert_eq!(jb.overruns(), 0);
    }

    #[test]
    fn underrun_yields_silence_and_counts() {
        let mut jb = JitterBuffer::new(mono8k(), 2);
        assert!(jb.pop().is_silent());
        assert!(jb.pop().is_silent());
        assert_eq!(jb.underruns(), 2);
        // Recovery: a real frame plays untouched afterwards.
        jb.push(tone(9)).unwrap();
        assert_eq!(jb.pop().samples()[0], 9);
        assert_eq!(jb.underruns(), 2);
    }

    #[test]
    fn overrun_drops_oldest_and_counts() {
        let mut jb = JitterBuffer::new(mono8k(), 2); // max_depth 4
        for i in 0..6 {
            jb.push(tone(i)).unwrap();
        }
        assert_eq!(jb.overruns(), 2); // frames 0 and 1 dropped
        assert_eq!(jb.depth(), 4);
        for expected in 2..6 {
            assert_eq!(jb.pop().samples()[0], expected);
        }
        assert_eq!(jb.underruns(), 0);
    }

    #[test]
    fn rejects_format_mismatch_and_clamps_target() {
        let stereo = AudioFormat::new(8_000, 2).unwrap();
        let mut jb = JitterBuffer::new(mono8k(), 0);
        assert_eq!(jb.target_depth(), 1);
        assert_eq!(jb.max_depth(), 2);
        assert_eq!(
            jb.push(Frame::silence(stereo)),
            Err(AudioError::FormatMismatch {
                expected: mono8k(),
                got: stereo
            })
        );
    }
}
