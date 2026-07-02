//! The presence registry: who is connected, through which door.
//!
//! One shared registry feeds every surface (native who-list, and later the
//! telnet who screen, finger, Hotline user list, Icecast listener counts).
//! Join/leave publish [`ServerEvent`]s on the bus so sessions can push
//! roster deltas.

use std::collections::HashMap;
use std::time::Instant;

use parking_lot::RwLock;

use crate::bus::{EventBus, ServerEvent};
use crate::permissions::Role;

/// A live session, as presence sees it.
#[derive(Debug, Clone)]
pub struct PresenceEntry {
    pub session_id: u64,
    pub account_id: i64,
    pub screen_name: String,
    pub role: Role,
    /// "quic", "websocket", later "telnet", "hotline", …
    pub transport: String,
    pub connected_at: Instant,
}

#[derive(Default)]
pub struct PresenceRegistry {
    inner: RwLock<HashMap<u64, PresenceEntry>>,
    bus: Option<EventBus>,
}

impl PresenceRegistry {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: RwLock::default(),
            bus: Some(bus),
        }
    }

    pub fn join(&self, entry: PresenceEntry) {
        let event = ServerEvent::SessionOpened {
            session_id: entry.session_id,
            screen_name: entry.screen_name.clone(),
        };
        self.inner.write().insert(entry.session_id, entry);
        if let Some(bus) = &self.bus {
            bus.publish(event);
        }
    }

    pub fn leave(&self, session_id: u64) -> Option<PresenceEntry> {
        let entry = self.inner.write().remove(&session_id);
        if let Some(e) = &entry {
            if let Some(bus) = &self.bus {
                bus.publish(ServerEvent::SessionClosed {
                    session_id,
                    screen_name: e.screen_name.clone(),
                });
            }
        }
        entry
    }

    pub fn get(&self, session_id: u64) -> Option<PresenceEntry> {
        self.inner.read().get(&session_id).cloned()
    }

    /// Rename a session's visible identity (persona switch); publishes a
    /// change event.
    pub fn rename(&self, session_id: u64, screen_name: &str) {
        let mut inner = self.inner.write();
        if let Some(entry) = inner.get_mut(&session_id) {
            entry.screen_name = screen_name.to_string();
            drop(inner);
            if let Some(bus) = &self.bus {
                bus.publish(ServerEvent::SessionChanged {
                    session_id,
                    screen_name: screen_name.to_string(),
                });
            }
        }
    }

    /// Is any live session currently using this persona screen name?
    pub fn is_screen_name_online(&self, screen_name: &str) -> Option<PresenceEntry> {
        self.inner
            .read()
            .values()
            .find(|e| e.screen_name.eq_ignore_ascii_case(screen_name))
            .cloned()
    }

    /// Snapshot for who-lists, sorted by join time (regulars first).
    pub fn snapshot(&self) -> Vec<PresenceEntry> {
        let mut all: Vec<PresenceEntry> = self.inner.read().values().cloned().collect();
        all.sort_by_key(|e| e.connected_at);
        all
    }

    pub fn count(&self) -> usize {
        self.inner.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, name: &str) -> PresenceEntry {
        PresenceEntry {
            session_id: id,
            account_id: id as i64,
            screen_name: name.into(),
            role: Role::User,
            transport: "quic".into(),
            connected_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn join_leave_and_events() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        let reg = PresenceRegistry::new(bus);

        reg.join(entry(1, "alice"));
        reg.join(entry(2, "bob"));
        assert_eq!(reg.count(), 2);
        assert_eq!(reg.snapshot()[0].screen_name, "alice");

        assert!(matches!(
            rx.recv().await.unwrap(),
            ServerEvent::SessionOpened { session_id: 1, .. }
        ));

        let gone = reg.leave(1).unwrap();
        assert_eq!(gone.screen_name, "alice");
        assert_eq!(reg.count(), 1);
        // Double-leave publishes nothing and returns None.
        assert!(reg.leave(1).is_none());
    }
}
