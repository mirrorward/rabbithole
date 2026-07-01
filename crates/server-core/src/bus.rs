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
    /// A session ended.
    SessionClosed { session_id: u64 },
    /// A chat line was said in a room.
    Chat {
        room: String,
        from: String,
        text: String,
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
