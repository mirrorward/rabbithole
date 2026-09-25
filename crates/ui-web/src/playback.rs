//! Radio playback intent and observed outcome. Browser promises belong to
//! one attempt: changing stations or stopping invalidates their late answers.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackStatus {
    #[default]
    Idle,
    Starting,
    Playing,
    Blocked,
    Failed,
}

impl PlaybackStatus {
    pub fn needs_retry(self) -> bool {
        matches!(self, Self::Blocked | Self::Failed)
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::Starting => "Connecting to the station…",
            Self::Playing => "Listening",
            Self::Blocked => {
                "Your browser paused automatic playback. Choose Start listening to play."
            }
            Self::Failed => "The station couldn’t play. Try again, or choose another station.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackAction {
    None,
    Stop,
    Start(u64),
}

#[derive(Debug, Default)]
pub struct Playback {
    source: Option<String>,
    generation: u64,
    status: PlaybackStatus,
}

impl Playback {
    pub fn status(&self) -> PlaybackStatus {
        self.status
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// Repeated listings and volume/mute changes are not new permission to
    /// play. Only a new source or a fresh enable transition starts an attempt.
    pub fn reconcile(&mut self, enabled: bool, source: Option<String>) -> PlaybackAction {
        let source = source.filter(|_| enabled);
        if self.source == source {
            return PlaybackAction::None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.source = source;
        self.status = if self.source.is_some() {
            PlaybackStatus::Starting
        } else {
            PlaybackStatus::Idle
        };
        if self.source.is_some() {
            PlaybackAction::Start(self.generation)
        } else {
            PlaybackAction::Stop
        }
    }

    pub fn retry(&mut self) -> PlaybackAction {
        if self.source.is_none() || !self.status.needs_retry() {
            return PlaybackAction::None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.status = PlaybackStatus::Starting;
        PlaybackAction::Start(self.generation)
    }

    /// A promise cannot revive a stopped player or erase a newer media error.
    pub fn settled(&mut self, generation: u64, outcome: PlaybackStatus) -> bool {
        if self.generation != generation || self.status != PlaybackStatus::Starting {
            return false;
        }
        self.status = outcome;
        true
    }

    /// Media failure can also happen after play() has successfully resolved.
    pub fn failed(&mut self, generation: u64) -> bool {
        if self.generation != generation
            || !matches!(
                self.status,
                PlaybackStatus::Starting | PlaybackStatus::Playing
            )
        {
            return false;
        }
        self.status = PlaybackStatus::Failed;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(action: PlaybackAction) -> u64 {
        let PlaybackAction::Start(id) = action else {
            panic!("expected a play attempt")
        };
        id
    }

    #[test]
    fn rejection_waits_for_explicit_retry_not_preference_or_listing_updates() {
        let mut p = Playback::default();
        let id = start(p.reconcile(true, Some("https://radio/one".into())));
        assert!(p.settled(id, PlaybackStatus::Blocked));
        for _ in 0..20 {
            assert_eq!(
                p.reconcile(true, Some("https://radio/one".into())),
                PlaybackAction::None
            );
        }
        assert_eq!(p.status(), PlaybackStatus::Blocked);
        let retry = start(p.retry());
        assert_eq!(
            p.retry(),
            PlaybackAction::None,
            "no duplicate pending attempt"
        );
        assert!(p.settled(retry, PlaybackStatus::Playing));
        assert_eq!(p.retry(), PlaybackAction::None);
        assert_eq!(p.status(), PlaybackStatus::Playing);
    }

    #[test]
    fn late_promises_and_media_events_cannot_cross_stations_or_stop() {
        let mut p = Playback::default();
        let old = start(p.reconcile(true, Some("https://a/one".into())));
        let current = start(p.reconcile(true, Some("https://b/one".into())));
        assert!(!p.settled(old, PlaybackStatus::Blocked));
        assert!(!p.failed(old));
        assert!(p.settled(current, PlaybackStatus::Playing));
        assert_eq!(
            p.reconcile(false, Some("https://b/one".into())),
            PlaybackAction::Stop
        );
        assert!(!p.settled(current, PlaybackStatus::Playing));
        assert!(!p.failed(current));
        assert_eq!(p.status(), PlaybackStatus::Idle);
        assert_eq!(p.source(), None);
    }

    #[test]
    fn stream_failure_wins_over_late_promise_and_can_be_retried() {
        let mut p = Playback::default();
        let id = start(p.reconcile(true, Some("https://radio/one".into())));
        assert!(p.failed(id));
        assert!(!p.settled(id, PlaybackStatus::Playing));
        let retry = start(p.retry());
        assert!(!p.failed(id));
        assert!(p.settled(retry, PlaybackStatus::Playing));
        assert!(
            p.failed(retry),
            "errors after playback starts still surface"
        );
        assert_eq!(p.reconcile(true, None), PlaybackAction::Stop);
        assert_eq!(p.retry(), PlaybackAction::None);
    }
}
