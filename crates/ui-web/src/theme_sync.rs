//! Socket-generation and request correlation for live server themes.
//!
//! A change received during a fetch invalidates that reply and schedules one
//! replacement fetch. This keeps a delayed theme from undoing a newer clear.

use rabbithole_proto::RequestId;

#[derive(Debug, Default)]
pub(crate) struct ThemeSync {
    generation: u64,
    authenticated: bool,
    request: Request,
}

#[derive(Debug, Default)]
enum Request {
    #[default]
    Idle,
    Queued,
    Pending {
        id: RequestId,
        invalidated: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ThemeReplyAction {
    Apply,
    Refetch,
    Discard,
}

impl ThemeSync {
    pub(crate) fn reset(&mut self, generation: u64) {
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    /// Keep an outstanding id claimable when a second handshake arrives on
    /// the same socket, but never accept its reply under the replacement key.
    pub(crate) fn handshake(&mut self, generation: u64) {
        if self.generation != generation {
            return;
        }
        self.authenticated = false;
        match &mut self.request {
            Request::Pending { invalidated, .. } => *invalidated = true,
            request => *request = Request::Idle,
        }
    }

    pub(crate) fn authenticated(&mut self, generation: u64) -> bool {
        if self.generation != generation {
            return false;
        }
        self.authenticated = true;
        self.invalidate(generation)
    }

    /// Returns true only for the first scheduled fetch. Further notifications
    /// coalesce, including those arriving before its microtask starts.
    pub(crate) fn invalidate(&mut self, generation: u64) -> bool {
        if self.generation != generation || !self.authenticated {
            return false;
        }
        match &mut self.request {
            Request::Idle => {
                self.request = Request::Queued;
                true
            }
            Request::Queued => false,
            Request::Pending { invalidated, .. } => {
                *invalidated = true;
                false
            }
        }
    }

    pub(crate) fn begin(&mut self, generation: u64, id: RequestId) -> bool {
        if self.generation != generation
            || !self.authenticated
            || !matches!(self.request, Request::Queued)
        {
            return false;
        }
        self.request = Request::Pending {
            id,
            invalidated: false,
        };
        true
    }

    /// Only the current request id on the current socket can finish a fetch.
    /// Call for reply frames only: push sequence numbers share the id space.
    pub(crate) fn reply(&mut self, generation: u64, id: RequestId) -> Option<ThemeReplyAction> {
        if self.generation != generation {
            return None;
        }
        let Request::Pending {
            id: expected,
            invalidated,
        } = self.request
        else {
            return None;
        };
        if expected != id {
            return None;
        }
        self.request = Request::Idle;
        if !self.authenticated {
            Some(ThemeReplyAction::Discard)
        } else if invalidated {
            self.request = Request::Queued;
            Some(ThemeReplyAction::Refetch)
        } else {
            Some(ThemeReplyAction::Apply)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> ThemeSync {
        let mut sync = ThemeSync::default();
        sync.reset(1);
        assert!(!sync.invalidate(1));
        assert!(sync.authenticated(1));
        sync
    }

    #[test]
    fn authentication_fetches_once_and_only_matching_reply_finishes_it() {
        let mut sync = ready();
        assert!(!sync.invalidate(1));
        assert!(sync.begin(1, RequestId(10)));
        assert!(!sync.begin(1, RequestId(11)));
        assert_eq!(sync.reply(1, RequestId(11)), None);
        assert_eq!(sync.reply(1, RequestId(10)), Some(ThemeReplyAction::Apply));
        assert_eq!(sync.reply(1, RequestId(10)), None);
    }

    #[test]
    fn repeated_changes_discard_stale_reply_and_coalesce_one_refetch() {
        let mut sync = ready();
        assert!(sync.begin(1, RequestId(10)));
        for _ in 0..20 {
            assert!(!sync.invalidate(1));
        }
        assert_eq!(
            sync.reply(1, RequestId(10)),
            Some(ThemeReplyAction::Refetch)
        );
        assert!(!sync.invalidate(1));
        assert!(sync.begin(1, RequestId(11)));
        assert_eq!(sync.reply(1, RequestId(11)), Some(ThemeReplyAction::Apply));
    }

    #[test]
    fn reconnect_rejects_old_replies_and_scheduled_requests() {
        let mut sync = ready();
        assert!(sync.begin(1, RequestId(10)));
        sync.reset(2);
        assert_eq!(sync.reply(1, RequestId(10)), None);
        assert!(!sync.authenticated(1));
        assert!(sync.authenticated(2));
        assert!(!sync.begin(1, RequestId(11)));
        assert!(sync.begin(2, RequestId(11)));
        assert_eq!(sync.reply(1, RequestId(11)), None);
        assert_eq!(sync.reply(2, RequestId(11)), Some(ThemeReplyAction::Apply));
    }

    #[test]
    fn replacement_handshake_never_applies_old_key_reply() {
        let mut sync = ready();
        assert!(sync.begin(1, RequestId(10)));
        sync.handshake(1);
        assert_eq!(
            sync.reply(1, RequestId(10)),
            Some(ThemeReplyAction::Discard)
        );
        assert!(sync.authenticated(1));
        assert!(sync.begin(1, RequestId(11)));
        sync.handshake(1);
        assert!(!sync.authenticated(1));
        assert_eq!(
            sync.reply(1, RequestId(11)),
            Some(ThemeReplyAction::Refetch)
        );
    }

    #[test]
    fn independent_burrows_do_not_claim_each_others_requests() {
        let mut first = ready();
        let mut second = ready();
        assert!(first.begin(1, RequestId(10)));
        assert!(second.begin(1, RequestId(10)));
        assert!(!first.invalidate(1));
        assert_eq!(
            second.reply(1, RequestId(10)),
            Some(ThemeReplyAction::Apply)
        );
        assert_eq!(
            first.reply(1, RequestId(10)),
            Some(ThemeReplyAction::Refetch)
        );
    }
}
