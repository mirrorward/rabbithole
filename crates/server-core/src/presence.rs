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
    /// 0 online, 1 away, 2 idle, 3 invisible (Cheshire mode).
    pub state: u8,
    pub status: Option<String>,
    /// The session's portable identity public key from the handshake, if any —
    /// surfaced in the who-list so peers can verify identity across burrows.
    pub pubkey: Option<[u8; 32]>,
}

impl PresenceEntry {
    pub fn is_invisible(&self) -> bool {
        self.state == 3
    }
}

/// A radio station's live status, as presence surfaces it.
///
/// This is server-wide (per station), not per session: it is the "now playing"
/// line a who-list or status bar shows beside the roster, updated whenever a DJ
/// takes over a mount or the playlist engine rotates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioStatus {
    /// Station mount slug (e.g. "live").
    pub station: String,
    /// Current track title (or station name before the first track).
    pub title: String,
    /// Current track artist (may be empty).
    pub artist: String,
    /// The source name: a live DJ, or the automation label.
    pub dj: String,
    /// Listeners currently tuned in.
    pub listeners: usize,
    /// Whether a live DJ is sourcing the mount (vs. playlist automation).
    pub live: bool,
}

#[derive(Default)]
pub struct PresenceRegistry {
    inner: RwLock<HashMap<u64, PresenceEntry>>,
    /// Radio now-playing, keyed by station slug. Server-wide, not per session.
    radio: RwLock<HashMap<String, RadioStatus>>,
    bus: Option<EventBus>,
}

impl PresenceRegistry {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: RwLock::default(),
            radio: RwLock::default(),
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
                    was_invisible: e.is_invisible(),
                });
            }
        }
        entry
    }

    /// Change a session's presence state; publishes the transition.
    pub fn set_state(&self, session_id: u64, state: u8, status: Option<String>) {
        let mut inner = self.inner.write();
        if let Some(entry) = inner.get_mut(&session_id) {
            let was_invisible = entry.is_invisible();
            entry.state = state;
            entry.status = status.clone();
            let event = ServerEvent::PresenceChanged {
                session_id,
                screen_name: entry.screen_name.clone(),
                state,
                status,
                was_invisible,
            };
            drop(inner);
            if let Some(bus) = &self.bus {
                bus.publish(event);
            }
        }
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

    /// Records (or replaces) a station's now-playing status and publishes a
    /// [`ServerEvent::RadioNowPlaying`] so every surface can update its status
    /// line. Idempotent-friendly: repeated identical updates still publish
    /// (listeners may have changed).
    pub fn set_radio_now_playing(&self, status: RadioStatus) {
        let event = ServerEvent::RadioNowPlaying {
            station: status.station.clone(),
            title: status.title.clone(),
            artist: status.artist.clone(),
            dj: status.dj.clone(),
            listeners: status.listeners,
        };
        self.radio.write().insert(status.station.clone(), status);
        if let Some(bus) = &self.bus {
            bus.publish(event);
        }
    }

    /// Drops a station's now-playing status (the mount went off the air).
    pub fn clear_radio_now_playing(&self, station: &str) {
        self.radio.write().remove(station);
    }

    /// Current now-playing for one station, if it is on the air.
    pub fn radio_status(&self, station: &str) -> Option<RadioStatus> {
        self.radio.read().get(station).cloned()
    }

    /// Snapshot of every station's now-playing, sorted by station slug.
    pub fn radio_now_playing(&self) -> Vec<RadioStatus> {
        let mut all: Vec<RadioStatus> = self.radio.read().values().cloned().collect();
        all.sort_by(|a, b| a.station.cmp(&b.station));
        all
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
            state: 0,
            status: None,
            pubkey: None,
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
