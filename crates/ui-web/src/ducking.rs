//! A short radio gain envelope. User volume and mute remain separate controls.

const QUIET_GAIN: f64 = 0.25;
const ATTACK_MS: f64 = 40.0;
const RELEASE_MS: f64 = 180.0;

#[derive(Debug, Clone)]
pub(crate) struct Ducking {
    from: f64,
    target: f64,
    started: f64,
    duration: f64,
    hold_until: Option<f64>,
    clock: f64,
}

impl Default for Ducking {
    fn default() -> Self {
        Self {
            from: 1.0,
            target: 1.0,
            started: 0.0,
            duration: RELEASE_MS,
            hold_until: None,
            clock: 0.0,
        }
    }
}

impl Ducking {
    fn now(&mut self, now: f64) -> f64 {
        if now.is_finite() {
            self.clock = self.clock.max(now);
        }
        self.clock
    }

    fn interpolate(&self, now: f64) -> f64 {
        let elapsed = ((now - self.started) / self.duration).clamp(0.0, 1.0);
        if elapsed == 1.0 {
            return self.target;
        }
        self.from + (self.target - self.from) * elapsed
    }

    pub(crate) fn gain(&mut self, now: f64) -> f64 {
        let now = self.now(now);
        if let Some(end) = self.hold_until.filter(|end| now >= *end) {
            self.from = self.interpolate(end);
            self.target = 1.0;
            self.started = end;
            self.duration = RELEASE_MS;
            self.hold_until = None;
        }
        self.interpolate(now)
    }

    pub(crate) fn chime(&mut self, now: f64, duration_ms: u32) {
        let gain = self.gain(now);
        let now = self.clock;
        self.from = gain;
        self.target = QUIET_GAIN;
        self.started = now;
        self.duration = ATTACK_MS;
        self.hold_until = Some(
            self.hold_until
                .unwrap_or(now)
                .max(now + f64::from(duration_ms).max(ATTACK_MS)),
        );
    }

    pub(crate) fn release(&mut self, now: f64) {
        let gain = self.gain(now);
        if self.target != 1.0 || self.hold_until.is_some() {
            self.from = gain;
            self.target = 1.0;
            self.started = self.clock;
            self.duration = RELEASE_MS;
            self.hold_until = None;
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.hold_until.is_some() || self.interpolate(self.clock) != 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_around_the_chime_and_restores_gain() {
        let mut duck = Ducking::default();
        duck.chime(100.0, 220);
        assert_eq!(duck.gain(100.0), 1.0);
        assert_eq!(duck.gain(120.0), 0.625);
        assert_eq!(duck.gain(140.0), QUIET_GAIN);
        assert_eq!(duck.gain(320.0), QUIET_GAIN);
        assert_eq!(duck.gain(410.0), 0.625);
        assert_eq!(duck.gain(500.0), 1.0);
        assert!(!duck.active());
    }

    #[test]
    fn overlapping_chimes_extend_the_hold_without_resetting_gain() {
        let mut duck = Ducking::default();
        duck.chime(100.0, 220);
        duck.chime(280.0, 220);
        assert_eq!(duck.gain(280.0), QUIET_GAIN);
        assert_eq!(duck.gain(450.0), QUIET_GAIN);
        assert_eq!(duck.gain(680.0), 1.0);
        duck.chime(700.0, 120);
        let releasing = duck.gain(900.0);
        duck.chime(900.0, 220);
        assert_eq!(duck.gain(900.0), releasing);
        assert_eq!(duck.gain(940.0), QUIET_GAIN);
    }

    #[test]
    fn opt_out_releases_smoothly_and_clock_regression_never_rewinds() {
        let mut duck = Ducking::default();
        duck.chime(100.0, 220);
        duck.release(120.0);
        assert_eq!(duck.gain(120.0), 0.625);
        assert_eq!(duck.gain(110.0), 0.625);
        duck.release(200.0); // repeated preference sync must not prolong recovery
        assert_eq!(duck.gain(300.0), 1.0);
        duck.chime(400.0, 220);
        assert_eq!(duck.gain(60_000.0), 1.0); // delayed background timer
        assert!(!duck.active());
    }
}
