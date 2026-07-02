//! The in-memory server registry with heartbeat expiry.
//!
//! Servers register by heartbeat; a registration is only as fresh as its last
//! heartbeat. Entries are keyed by **(ip, port)** — the IP is the observed
//! UDP source address (never trusted from the packet body), the port is the
//! one the server declares it accepts connections on.
//!
//! Expiry is **lazy**: nothing runs on a timer. Every snapshot (and length
//! query) first drops entries whose last heartbeat is older than the TTL, so
//! a stale entry can linger in memory but is never *served*. The default TTL
//! matches classic Hotline trackers: about six minutes, roughly two missed
//! five-minute heartbeats.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default registration TTL (like classic trackers: 6 minutes).
pub const DEFAULT_TTL: Duration = Duration::from_secs(6 * 60);

/// One live server known to the tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    /// Server display name.
    pub name: String,
    /// One-line server description.
    pub description: String,
    /// Where clients connect: observed IP + declared port.
    pub addr: SocketAddr,
    /// Users online, as last reported by the server.
    pub users_online: u16,
    /// When the last heartbeat for this entry arrived.
    pub last_heartbeat: Instant,
}

/// A thread-safe registry of live servers, keyed by `(ip, port)`.
#[derive(Debug)]
pub struct Registry {
    ttl: Duration,
    entries: Mutex<HashMap<(IpAddr, u16), ServerEntry>>,
}

impl Registry {
    /// Creates an empty registry whose entries expire `ttl` after their last
    /// heartbeat.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// The configured time-to-live for registrations.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Adds or refreshes a registration. The entry's `last_heartbeat` is the
    /// moment of the call; re-registering the same `(ip, port)` replaces the
    /// stored name/description/user count and restarts the TTL clock.
    pub fn register(&self, entry: ServerEntry) {
        let key = (entry.addr.ip(), entry.addr.port());
        self.entries
            .lock()
            .expect("registry lock poisoned")
            .insert(key, entry);
    }

    /// Returns the live (non-expired) entries, pruning expired ones as a side
    /// effect. Order: name, then address, so listings are stable.
    pub fn snapshot(&self) -> Vec<ServerEntry> {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("registry lock poisoned");
        entries.retain(|_, e| now.duration_since(e.last_heartbeat) < self.ttl);
        let mut live: Vec<ServerEntry> = entries.values().cloned().collect();
        live.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.addr.cmp(&b.addr)));
        live
    }

    /// The number of live entries (prunes expired ones first).
    pub fn len(&self) -> usize {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("registry lock poisoned");
        entries.retain(|_, e| now.duration_since(e.last_heartbeat) < self.ttl);
        entries.len()
    }

    /// True when no live entries remain.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(DEFAULT_TTL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, ip: [u8; 4], port: u16, users: u16) -> ServerEntry {
        ServerEntry {
            name: name.to_owned(),
            description: format!("{name} desc"),
            addr: SocketAddr::from((ip, port)),
            users_online: users,
            last_heartbeat: Instant::now(),
        }
    }

    /// Like [`entry`], but with `last_heartbeat` backdated by `age` — lets the
    /// TTL tests be deterministic instead of sleeping on the wall clock
    /// (which flakes on slow/loaded CI runners, notably macOS).
    fn aged_entry(name: &str, ip: [u8; 4], port: u16, users: u16, age: Duration) -> ServerEntry {
        let mut e = entry(name, ip, port, users);
        e.last_heartbeat = Instant::now().checked_sub(age).expect("recent enough");
        e
    }

    #[test]
    fn add_and_snapshot() {
        let reg = Registry::new(DEFAULT_TTL);
        assert!(reg.is_empty());
        reg.register(entry("Wonderland", [10, 0, 0, 1], 5500, 3));
        reg.register(entry("Tea Party", [10, 0, 0, 2], 5500, 7));
        let snap = reg.snapshot();
        assert_eq!(reg.len(), 2);
        // Sorted by name.
        assert_eq!(snap[0].name, "Tea Party");
        assert_eq!(snap[1].name, "Wonderland");
    }

    #[test]
    fn refresh_replaces_same_key() {
        let reg = Registry::new(DEFAULT_TTL);
        reg.register(entry("Old Name", [10, 0, 0, 1], 5500, 1));
        reg.register(entry("New Name", [10, 0, 0, 1], 5500, 9));
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "New Name");
        assert_eq!(snap[0].users_online, 9);
    }

    #[test]
    fn same_ip_different_port_are_distinct() {
        let reg = Registry::new(DEFAULT_TTL);
        reg.register(entry("A", [10, 0, 0, 1], 5500, 0));
        reg.register(entry("B", [10, 0, 0, 1], 5510, 0));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn entries_expire_after_ttl() {
        let reg = Registry::new(Duration::from_millis(50));
        // A heartbeat older than the TTL is pruned on the next query…
        reg.register(aged_entry(
            "Ephemeral",
            [10, 0, 0, 1],
            5500,
            0,
            Duration::from_millis(500),
        ));
        assert!(reg.snapshot().is_empty());
        assert!(reg.is_empty());
        // …while a fresh one survives.
        reg.register(entry("Live", [10, 0, 0, 2], 5500, 0));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn refresh_restarts_the_ttl_clock() {
        let reg = Registry::new(Duration::from_millis(50));
        // Aging but still within TTL.
        reg.register(aged_entry(
            "Alive",
            [10, 0, 0, 1],
            5500,
            0,
            Duration::from_millis(40),
        ));
        assert_eq!(reg.len(), 1);
        // A fresh heartbeat replaces the stored timestamp with "now" — the
        // TTL clock restarts, so the entry is young again, not near expiry.
        reg.register(entry("Alive", [10, 0, 0, 1], 5500, 1));
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].users_online, 1);
        assert!(
            snap[0].last_heartbeat.elapsed() < Duration::from_millis(30),
            "refresh restarted the heartbeat clock"
        );
    }
}
