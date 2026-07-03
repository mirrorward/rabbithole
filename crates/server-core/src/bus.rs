//! The server-internal event bus.
//!
//! Every domain service publishes [`ServerEvent`]s; every protocol surface
//! (RHP sessions, telnet screens, the Hotline compat layer, finger, the
//! admin monitor) subscribes and projects the events it cares about. This
//! is the single mechanism that keeps "one community, many doors" honest.
//!
//! Built on `tokio::sync::broadcast`: slow subscribers observe
//! [`Lagged`](tokio::sync::broadcast::error::RecvError::Lagged) rather than
//! back-pressuring the publisher — a surface that falls behind re-syncs
//! from state instead of stalling chat for everyone.

use tokio::sync::broadcast;

/// Events published on the bus.
///
/// `#[non_exhaustive]` — waves add variants; subscribers ignore unknowns.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// A session authenticated and joined (Wave 1 fills the fields out).
    SessionOpened {
        session_id: u64,
        screen_name: String,
    },
    /// A session ended. `was_invisible` lets push projection skip the
    /// synthetic leave for viewers who never saw the user arrive.
    SessionClosed {
        session_id: u64,
        screen_name: String,
        was_invisible: bool,
    },
    /// A chat line was said in a room.
    Chat {
        room: String,
        from: String,
        text: String,
    },
    /// A session's visible identity changed (persona switch, avatar…).
    SessionChanged {
        session_id: u64,
        screen_name: String,
    },
    /// A session's presence state changed (away/idle/Cheshire…).
    PresenceChanged {
        session_id: u64,
        screen_name: String,
        /// 0 online, 1 away, 2 idle, 3 invisible.
        state: u8,
        status: Option<String>,
        was_invisible: bool,
    },
    /// A direct message for `to_account` (live delivery; offline delivery
    /// rides the durable DM store, so the replay recorder skips these).
    Dm {
        to_account: i64,
        message: rabbithole_proto::dm::DmMessage,
    },
    /// Read receipt for `to_account`'s sent messages.
    DmRead {
        to_account: i64,
        by: String,
        up_to_id: i64,
    },
    /// A room invitation for `to_account`.
    RoomInvited {
        to_account: i64,
        room: String,
        from: String,
    },
    /// `account` was kicked from a room.
    RoomKicked {
        account: i64,
        room: String,
        banned: bool,
    },
    /// `account` was muted (`muted = true`, with the mute's duration) or
    /// unmuted in a room; fanned out to the room's members.
    RoomMuted {
        account: i64,
        screen_name: String,
        room: String,
        muted: bool,
        /// `None` = permanent (meaningful only when `muted`).
        duration_secs: Option<u32>,
    },
    /// A room's slow-mode interval changed (`0` = off); fanned out to the
    /// room's members.
    RoomSlowModeChanged {
        room: String,
        seconds: u32,
        by: String,
    },
    /// An operator notice for every session.
    Notice { text: String, from: String },
    /// A notice for moderators only (e.g. "a new report was filed").
    /// Surfaces deliver it solely to sessions holding moderator rank.
    ModNotice { text: String },
    /// An operator disconnected a session; the session task closes itself.
    Kick { session_id: u64, reason: String },
    /// A new board post landed (broadcast so unread counts stay live).
    BoardPost {
        board: String,
        id: [u8; 32],
        root: Option<[u8; 32]>,
    },
    /// A board **follow-up** (Edit/Tombstone) landed. Distinct from
    /// [`ServerEvent::BoardPost`] so it does **not** bump unread counts or
    /// notify as a new post; its consumer is the federation flood, which
    /// offers `id` under `board` to subscribed peers.
    BoardEvent { board: String, id: [u8; 32] },
    /// A wish changed status; pushed to the requester's account. Carries the
    /// full view so push projection stays synchronous (no DB round-trip).
    WishUpdated {
        to_account: i64,
        wish: rabbithole_proto::wish::WishView,
    },
    /// A file landed in a library (broadcast so listings/search stay live).
    FileAdded { area: String, id: i64 },
    /// A radio station's now-playing changed — a DJ took over the mount, or the
    /// playlist engine rotated to the next track. Surfaced in presence/status
    /// lines the way away/idle status is.
    RadioNowPlaying {
        /// Station mount slug (e.g. "live").
        station: String,
        /// Current track title (or station name before the first track).
        title: String,
        /// Current track artist (may be empty).
        artist: String,
        /// The source name: a live DJ, or the automation label.
        dj: String,
        /// Listeners currently tuned in.
        listeners: usize,
    },
    /// The server is shutting down; surfaces should drain gracefully.
    Shutdown,
}

/// Handle for publishing and subscribing to [`ServerEvent`]s.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
}

impl EventBus {
    /// `capacity` is the per-subscriber ring buffer; laggards skip, never block.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, event: ServerEvent) {
        // Zero subscribers is fine (e.g. during startup).
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fan_out_to_multiple_subscribers() {
        let bus = EventBus::default();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(ServerEvent::Chat {
            room: "lobby".into(),
            from: "alice".into(),
            text: "hi".into(),
        });
        for rx in [&mut a, &mut b] {
            match rx.recv().await.unwrap() {
                ServerEvent::Chat { room, from, text } => {
                    assert_eq!(
                        (room.as_str(), from.as_str(), text.as_str()),
                        ("lobby", "alice", "hi")
                    );
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_ok() {
        let bus = EventBus::default();
        bus.publish(ServerEvent::Shutdown);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
