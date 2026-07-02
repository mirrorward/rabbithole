//! The in-memory server registry with heartbeat expiry.
//!
//! Servers register by heartbeat; a registration is only as fresh as its last
//! heartbeat. Entries are keyed by **(ip, port)**. Two registration paths
//! feed the same table:
//!
//! - **Classic unsigned** ([`Registry::register_unsigned`]) — the HTRK UDP
//!   heartbeat. The IP is the observed UDP source address (never trusted from
//!   the packet body), the port is the one the server declares.
//! - **Signed** ([`Registry::register_descriptor`]) — a verified
//!   [`SignedDescriptor`], arriving either directly from the server (an
//!   announce) or relayed by a peer tracker (gossip, marked with
//!   [`ServerEntry::via`]). Here the whole address is *declared* inside the
//!   signed document; the signature turns it into an authenticated claim.
//!
//! ## Key-conflict policy (rotation trade-off)
//!
//! The first *verified* key to claim an `(ip, port)` slot holds it: a signed
//! re-registration under a **different** key is rejected until the current
//! entry expires. This stops a drive-by signer from hijacking a live listing
//! (they would have to suppress the real server's heartbeats for a full TTL
//! first). The cost: an operator who rotates or loses their key must wait one
//! TTL (~6 minutes by default) before the new key can take the slot — cheap
//! for a directory whose entries are this short-lived, which is why we chose
//! wait-for-expiry over a signed-handover scheme.
//!
//! Two softer rules round the policy out: a signed entry *upgrades* an
//! unsigned one for the same slot (proof beats no proof), and an unsigned
//! heartbeat landing on a signed entry only refreshes its TTL and user count
//! — it cannot rename, re-point, or strip the verified identity (so a spoofed
//! UDP source can at worst keep a signed listing alive, never rewrite it).
//! Replays of a captured signed descriptor are bounded the same way: an
//! *older* timestamp for the same key is rejected, an equal one merely
//! refreshes the TTL (no worse than the classic unsigned heartbeat).
//!
//! Expiry is **lazy**: nothing runs on a timer. Every snapshot (and length
//! query) first drops entries whose last heartbeat is older than the TTL, so
//! a stale entry can linger in memory but is never *served*. Gossiped entries
//! follow the exact same discipline — they expire unless re-gossiped. The
//! default TTL matches classic Hotline trackers: about six minutes, roughly
//! two missed five-minute heartbeats.
//!
//! ## Health observation (verifiable, not authoritative)
//!
//! Alongside each entry the registry keeps a local [`HealthLog`]
//! ([`crate::health`]): every *accepted* registration records a heartbeat
//! observation, and the lazy expiry sweep marks dropped slots expired (which
//! is what counts flaps). Health logs outlive their entries — a slot that
//! expires keeps accumulating silence so its uptime honestly sags — but are
//! themselves dropped once silent for the full 24 h observation window.
//! Health is **never** gossiped or imported: an entry learned from a peer
//! starts a fresh local log, because this tracker can only vouch for what it
//! watched itself (see [`crate::health`] for the full doctrine).
//!
//! Public mutators and queries come in pairs: the plain method stamps
//! `Instant::now()`, and an `*_at(…, now)` variant takes an injected instant
//! so tests can drive a whole day of observations deterministically.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::descriptor::{DescriptorError, SignedDescriptor};
use crate::health::{HealthLog, HealthReport, OBSERVATION_WINDOW};

/// Default registration TTL (like classic trackers: 6 minutes).
pub const DEFAULT_TTL: Duration = Duration::from_secs(6 * 60);

/// One live server known to the tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    /// Server display name.
    pub name: String,
    /// One-line server description.
    pub description: String,
    /// Where clients connect: observed IP + declared port (unsigned path),
    /// or the address declared inside the signed descriptor.
    pub addr: SocketAddr,
    /// Users online, as last reported by the server.
    pub users_online: u16,
    /// Category tags (always empty for unsigned registrations).
    pub categories: Vec<String>,
    /// The verified descriptor this entry came from, if it was signed. Kept
    /// verbatim so the tracker can relay it to gossip peers — a tracker can
    /// never re-sign on a server's behalf.
    pub signed: Option<SignedDescriptor>,
    /// The gossip peer this entry was learned from; `None` means the server
    /// registered directly. Loop safety: entries are never gossiped back to
    /// the peer named here.
    pub via: Option<SocketAddr>,
    /// When the last heartbeat/announce for this entry arrived.
    pub last_heartbeat: Instant,
}

impl ServerEntry {
    /// A classic unsigned entry (no key, no categories, registered directly).
    pub fn unsigned(
        name: impl Into<String>,
        description: impl Into<String>,
        addr: SocketAddr,
        users_online: u16,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            addr,
            users_online,
            categories: Vec::new(),
            signed: None,
            via: None,
            last_heartbeat: Instant::now(),
        }
    }

    /// An entry built from a **verified** signed descriptor. `via` names the
    /// gossip peer it was learned from (`None` for a direct announce).
    pub fn from_signed(signed: SignedDescriptor, via: Option<SocketAddr>) -> Self {
        let d = &signed.descriptor;
        Self {
            name: d.name.clone(),
            description: d.description.clone(),
            addr: d.addr,
            users_online: d.users_online,
            categories: d.categories.clone(),
            signed: Some(signed),
            via,
            last_heartbeat: Instant::now(),
        }
    }

    /// The verified server key, if this entry was registered signed.
    pub fn server_key(&self) -> Option<[u8; 32]> {
        self.signed.as_ref().map(|s| s.descriptor.server_key)
    }

    /// The signed descriptor's timestamp (gossip generation), if signed.
    pub fn timestamp(&self) -> Option<i64> {
        self.signed.as_ref().map(|s| s.descriptor.timestamp)
    }

    /// Whether the entry carries `category` (ASCII-case-insensitive).
    pub fn has_category(&self, category: &str) -> bool {
        self.categories
            .iter()
            .any(|c| c.eq_ignore_ascii_case(category))
    }
}

/// Why a signed registration was rejected (never a panic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// The descriptor failed verification (signature or field limits).
    #[error("descriptor rejected: {0}")]
    BadDescriptor(#[from] DescriptorError),
    /// A live entry holds this `(ip, port)` under a different verified key;
    /// the new key may claim the slot only after the old entry expires (see
    /// the module docs for the rotation trade-off).
    #[error("address is held by a different verified key until it expires")]
    KeyConflict,
    /// Same key, but an older timestamp than the entry we already hold — a
    /// replayed or out-of-order descriptor.
    #[error("descriptor is older than the one already registered")]
    Stale,
}

/// One row of the directory index (`INDEX` verb): a live entry married to
/// the tracker's **local** health observations. The uptime numbers are this
/// tracker's own bookkeeping, not signed claims — see [`crate::health`].
#[derive(Debug, Clone)]
pub struct IndexRow {
    /// The live registry entry (name, address, categories, signature…).
    pub entry: ServerEntry,
    /// Observed 24 h uptime, integer per-mille (0..=1000).
    pub uptime_permille: u32,
    /// Seconds since the last accepted registration for this slot.
    pub last_seen_secs: u64,
}

/// Interior of the registry mutex: the live entries plus their health logs,
/// both keyed by `(ip, port)`. One lock, so entries and health can never
/// disagree mid-update.
#[derive(Debug, Default)]
struct State {
    entries: HashMap<(IpAddr, u16), ServerEntry>,
    health: HashMap<(IpAddr, u16), HealthLog>,
}

impl State {
    /// The lazy sweep: drops entries past `ttl` — telling their health logs,
    /// which is what counts flaps — then drops health logs that have been
    /// silent for the full observation window (and are not shielded by a
    /// still-live entry, which matters only for TTLs longer than 24 h).
    fn prune(&mut self, ttl: Duration, now: Instant) {
        let health = &mut self.health;
        self.entries.retain(|key, e| {
            let live = now.saturating_duration_since(e.last_heartbeat) < ttl;
            if !live {
                if let Some(log) = health.get_mut(key) {
                    log.mark_expired(now);
                }
            }
            live
        });
        let entries = &self.entries;
        self.health.retain(|key, log| {
            entries.contains_key(key)
                || now.saturating_duration_since(log.last_seen()) < OBSERVATION_WINDOW
        });
    }

    /// Feeds one accepted registration into the slot's health log, creating
    /// a fresh log on first sight (gossiped entries start here too — their
    /// uptime is always this tracker's own observation, never imported).
    fn observe(&mut self, key: (IpAddr, u16), now: Instant) {
        self.health
            .entry(key)
            .or_insert_with(|| HealthLog::new(now))
            .record_heartbeat(now);
    }
}

/// A thread-safe registry of live servers, keyed by `(ip, port)`.
#[derive(Debug)]
pub struct Registry {
    ttl: Duration,
    state: Mutex<State>,
}

impl Registry {
    /// Creates an empty registry whose entries expire `ttl` after their last
    /// heartbeat.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            state: Mutex::new(State::default()),
        }
    }

    /// The configured time-to-live for registrations.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("registry lock poisoned")
    }

    /// Raw insert: adds or replaces the entry for its `(ip, port)`, bypassing
    /// the key-conflict policy. Plumbing for tests (backdated entries) and
    /// callers that have already applied policy; services should use
    /// [`register_unsigned`](Self::register_unsigned) or
    /// [`register_descriptor`](Self::register_descriptor). The entry's own
    /// `last_heartbeat` is taken as the health observation instant.
    pub fn register(&self, entry: ServerEntry) {
        let key = (entry.addr.ip(), entry.addr.port());
        let seen = entry.last_heartbeat;
        let mut state = self.lock();
        state.entries.insert(key, entry);
        state.observe(key, seen);
    }

    /// The classic unsigned heartbeat, stamped `Instant::now()` — see
    /// [`register_unsigned_at`](Self::register_unsigned_at).
    pub fn register_unsigned(&self, entry: ServerEntry) {
        self.register_unsigned_at(entry, Instant::now());
    }

    /// The classic unsigned heartbeat, observed at `now`. For an unclaimed
    /// (or unsigned) slot this inserts/replaces and restarts the TTL clock,
    /// as ever. If the slot is held by a live **signed** entry, the heartbeat
    /// refreshes only the TTL and user count — an unauthenticated packet
    /// cannot rename, re-point, or strip a verified identity. Either way the
    /// slot's health log records one heartbeat.
    pub fn register_unsigned_at(&self, mut entry: ServerEntry, now: Instant) {
        let key = (entry.addr.ip(), entry.addr.port());
        entry.last_heartbeat = now;
        let mut state = self.lock();
        state.prune(self.ttl, now);
        if let Some(existing) = state.entries.get_mut(&key) {
            if existing.signed.is_some() {
                existing.users_online = entry.users_online;
                existing.last_heartbeat = now;
                state.observe(key, now);
                return;
            }
        }
        state.entries.insert(key, entry);
        state.observe(key, now);
    }

    /// The signed registration path, stamped `Instant::now()` — see
    /// [`register_descriptor_at`](Self::register_descriptor_at).
    pub fn register_descriptor(
        &self,
        signed: SignedDescriptor,
        via: Option<SocketAddr>,
    ) -> Result<(), RegisterError> {
        self.register_descriptor_at(signed, via, Instant::now())
    }

    /// The signed registration path (direct announce or gossip), observed at
    /// `now`. Verifies the descriptor, then applies the key-conflict policy
    /// from the module docs:
    ///
    /// - unclaimed or unsigned slot → insert (signed upgrades unsigned);
    /// - live slot under a **different** key → [`RegisterError::KeyConflict`]
    ///   until the old entry expires;
    /// - same key with an older timestamp → [`RegisterError::Stale`]; equal
    ///   or newer replaces the entry and restarts the TTL clock.
    ///
    /// `via` names the gossip peer this arrived from (`None` = direct). An
    /// accepted registration records one heartbeat in the slot's **local**
    /// health log; nothing about health arrives with the descriptor.
    pub fn register_descriptor_at(
        &self,
        signed: SignedDescriptor,
        via: Option<SocketAddr>,
        now: Instant,
    ) -> Result<(), RegisterError> {
        signed.verify()?;
        let addr = signed.descriptor.addr;
        let key = (addr.ip(), addr.port());
        let mut state = self.lock();
        // Prune first so an expired old key never blocks a rotation.
        state.prune(self.ttl, now);
        if let Some(existing) = state.entries.get(&key) {
            match existing.server_key() {
                Some(held) if held != signed.descriptor.server_key => {
                    return Err(RegisterError::KeyConflict);
                }
                Some(_) => {
                    let held_ts = existing.timestamp().unwrap_or(i64::MIN);
                    if signed.descriptor.timestamp < held_ts {
                        return Err(RegisterError::Stale);
                    }
                }
                None => {} // signed upgrades unsigned
            }
        }
        let mut entry = ServerEntry::from_signed(signed, via);
        entry.last_heartbeat = now;
        state.entries.insert(key, entry);
        state.observe(key, now);
        Ok(())
    }

    /// [`snapshot_at`](Self::snapshot_at) stamped `Instant::now()`.
    pub fn snapshot(&self) -> Vec<ServerEntry> {
        self.snapshot_at(Instant::now())
    }

    /// Returns the live (non-expired) entries as of `now`, pruning expired
    /// ones as a side effect. Order: name, then address, so listings are
    /// stable.
    pub fn snapshot_at(&self, now: Instant) -> Vec<ServerEntry> {
        let mut state = self.lock();
        state.prune(self.ttl, now);
        let mut live: Vec<ServerEntry> = state.entries.values().cloned().collect();
        live.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.addr.cmp(&b.addr)));
        live
    }

    /// Like [`snapshot`](Self::snapshot), but only entries carrying
    /// `category` (ASCII-case-insensitive).
    pub fn snapshot_category(&self, category: &str) -> Vec<ServerEntry> {
        let mut live = self.snapshot();
        live.retain(|e| e.has_category(category));
        live
    }

    /// Category summary: (lowercased category name → live server count),
    /// sorted by name. Tags differing only in ASCII case are merged.
    pub fn category_counts(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for entry in self.snapshot() {
            for category in &entry.categories {
                *counts.entry(category.to_ascii_lowercase()).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }

    /// [`index_at`](Self::index_at) stamped `Instant::now()`.
    pub fn index(&self) -> Vec<IndexRow> {
        self.index_at(Instant::now())
    }

    /// The directory index as of `now`: every live entry paired with this
    /// tracker's local health observations. Sort order: signed entries
    /// first, then observed uptime descending, then name, then address —
    /// proof outranks bookkeeping, bookkeeping outranks the alphabet.
    pub fn index_at(&self, now: Instant) -> Vec<IndexRow> {
        let mut state = self.lock();
        state.prune(self.ttl, now);
        let mut rows: Vec<IndexRow> = state
            .entries
            .iter()
            .map(|(key, entry)| {
                let (uptime_permille, last_seen) = match state.health.get(key) {
                    Some(log) => (log.uptime_24h_permille(now), log.last_seen()),
                    // Unreachable in practice (every insert observes), but
                    // never panic over bookkeeping.
                    None => (0, entry.last_heartbeat),
                };
                IndexRow {
                    entry: entry.clone(),
                    uptime_permille,
                    last_seen_secs: now.saturating_duration_since(last_seen).as_secs(),
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.entry
                .signed
                .is_some()
                .cmp(&a.entry.signed.is_some())
                .then_with(|| b.uptime_permille.cmp(&a.uptime_permille))
                .then_with(|| a.entry.name.cmp(&b.entry.name))
                .then_with(|| a.entry.addr.cmp(&b.entry.addr))
        });
        rows
    }

    /// [`index_category_at`](Self::index_category_at) stamped
    /// `Instant::now()`.
    pub fn index_category(&self, category: &str) -> Vec<IndexRow> {
        self.index_category_at(category, Instant::now())
    }

    /// Like [`index_at`](Self::index_at), but only entries carrying
    /// `category` (ASCII-case-insensitive).
    pub fn index_category_at(&self, category: &str, now: Instant) -> Vec<IndexRow> {
        let mut rows = self.index_at(now);
        rows.retain(|row| row.entry.has_category(category));
        rows
    }

    /// [`health_report_at`](Self::health_report_at) stamped `Instant::now()`.
    pub fn health_report(&self, addr: SocketAddr) -> Option<HealthReport> {
        self.health_report_at(addr, Instant::now())
    }

    /// This tracker's local health observations for one slot, as of `now`.
    /// Works for recently-expired slots too (the log outlives the entry by
    /// up to the 24 h observation window); `None` for servers this tracker
    /// has never seen or has long forgotten.
    pub fn health_report_at(&self, addr: SocketAddr, now: Instant) -> Option<HealthReport> {
        let key = (addr.ip(), addr.port());
        let mut state = self.lock();
        state.prune(self.ttl, now);
        state.health.get(&key).map(|log| log.report(addr, now))
    }

    /// The number of live entries (prunes expired ones first).
    pub fn len(&self) -> usize {
        let mut state = self.lock();
        state.prune(self.ttl, Instant::now());
        state.entries.len()
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
    use crate::descriptor::Descriptor;
    use rabbithole_identity::IdentityKey;

    fn entry(name: &str, ip: [u8; 4], port: u16, users: u16) -> ServerEntry {
        ServerEntry::unsigned(name, format!("{name} desc"), (ip, port).into(), users)
    }

    /// Like [`entry`], but with `last_heartbeat` backdated by `age` — lets the
    /// TTL tests be deterministic instead of sleeping on the wall clock
    /// (which flakes on slow/loaded CI runners, notably macOS).
    fn aged_entry(name: &str, ip: [u8; 4], port: u16, users: u16, age: Duration) -> ServerEntry {
        let mut e = entry(name, ip, port, users);
        e.last_heartbeat = Instant::now().checked_sub(age).expect("recent enough");
        e
    }

    fn signed(seed: u8, name: &str, ip: [u8; 4], port: u16, ts: i64) -> SignedDescriptor {
        let key = IdentityKey::from_seed(&[seed; 32]);
        Descriptor::new(name, (ip, port).into())
            .with_description(format!("{name} desc"))
            .with_category("chat")
            .with_users(5)
            .with_timestamp(ts)
            .sign(&key)
            .unwrap()
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
        reg.register_unsigned(entry("Old Name", [10, 0, 0, 1], 5500, 1));
        reg.register_unsigned(entry("New Name", [10, 0, 0, 1], 5500, 9));
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
        reg.register_unsigned(entry("Alive", [10, 0, 0, 1], 5500, 1));
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].users_online, 1);
        assert!(
            snap[0].last_heartbeat.elapsed() < Duration::from_millis(30),
            "refresh restarted the heartbeat clock"
        );
    }

    #[test]
    fn signed_registration_carries_key_and_categories() {
        let reg = Registry::new(DEFAULT_TTL);
        let sd = signed(1, "Wonderland", [10, 0, 0, 1], 5500, 100);
        reg.register_descriptor(sd.clone(), None).unwrap();
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].server_key(), Some(sd.descriptor.server_key));
        assert_eq!(snap[0].categories, vec!["chat".to_owned()]);
        assert_eq!(snap[0].via, None);
        assert_eq!(snap[0].timestamp(), Some(100));
    }

    #[test]
    fn tampered_descriptor_is_rejected() {
        let reg = Registry::new(DEFAULT_TTL);
        let mut sd = signed(1, "Wonderland", [10, 0, 0, 1], 5500, 100);
        sd.descriptor.name = "Evil Twin".into();
        assert_eq!(
            reg.register_descriptor(sd, None),
            Err(RegisterError::BadDescriptor(DescriptorError::BadSignature))
        );
        assert!(reg.is_empty());
    }

    #[test]
    fn different_key_is_rejected_while_the_old_entry_lives() {
        let reg = Registry::new(DEFAULT_TTL);
        reg.register_descriptor(signed(1, "Original", [10, 0, 0, 1], 5500, 100), None)
            .unwrap();
        // A different key claiming the same (ip, port) must wait for expiry.
        assert_eq!(
            reg.register_descriptor(signed(2, "Hijack", [10, 0, 0, 1], 5500, 999), None),
            Err(RegisterError::KeyConflict)
        );
        assert_eq!(reg.snapshot()[0].name, "Original");
    }

    #[test]
    fn different_key_claims_the_slot_after_expiry() {
        let reg = Registry::new(Duration::from_millis(50));
        // Backdate a signed entry beyond the TTL (aged_entry pattern).
        let mut old =
            ServerEntry::from_signed(signed(1, "Original", [10, 0, 0, 1], 5500, 100), None);
        old.last_heartbeat = Instant::now()
            .checked_sub(Duration::from_millis(500))
            .expect("recent enough");
        reg.register(old);
        // The old entry is expired, so the new key takes the slot.
        reg.register_descriptor(signed(2, "Rotated", [10, 0, 0, 1], 5500, 200), None)
            .unwrap();
        assert_eq!(reg.snapshot()[0].name, "Rotated");
    }

    #[test]
    fn same_key_rejects_stale_and_accepts_newer_timestamps() {
        let reg = Registry::new(DEFAULT_TTL);
        reg.register_descriptor(signed(1, "Gen2", [10, 0, 0, 1], 5500, 200), None)
            .unwrap();
        // A replayed older descriptor is rejected…
        assert_eq!(
            reg.register_descriptor(signed(1, "Gen1", [10, 0, 0, 1], 5500, 100), None),
            Err(RegisterError::Stale)
        );
        // …an equal timestamp refreshes (heartbeat semantics)…
        reg.register_descriptor(signed(1, "Gen2", [10, 0, 0, 1], 5500, 200), None)
            .unwrap();
        // …and a newer one replaces.
        reg.register_descriptor(signed(1, "Gen3", [10, 0, 0, 1], 5500, 300), None)
            .unwrap();
        assert_eq!(reg.snapshot()[0].name, "Gen3");
    }

    #[test]
    fn signed_upgrades_unsigned_but_not_the_reverse() {
        let reg = Registry::new(DEFAULT_TTL);
        reg.register_unsigned(entry("Plain", [10, 0, 0, 1], 5500, 2));
        reg.register_descriptor(signed(1, "Verified", [10, 0, 0, 1], 5500, 100), None)
            .unwrap();
        assert_eq!(reg.snapshot()[0].name, "Verified");

        // An unsigned heartbeat on a signed slot refreshes TTL + users only.
        reg.register_unsigned(entry("Spoof", [10, 0, 0, 1], 5500, 9));
        let snap = reg.snapshot();
        assert_eq!(snap[0].name, "Verified", "identity survives");
        assert_eq!(snap[0].users_online, 9, "user count refreshed");
        assert!(snap[0].signed.is_some(), "key survives");
    }

    #[test]
    fn gossiped_entries_expire_unless_re_gossiped() {
        let reg = Registry::new(Duration::from_millis(50));
        let peer: SocketAddr = ([192, 0, 2, 9], 4656).into();
        let mut e =
            ServerEntry::from_signed(signed(1, "Remote", [10, 0, 0, 1], 5500, 100), Some(peer));
        e.last_heartbeat = Instant::now()
            .checked_sub(Duration::from_millis(500))
            .expect("recent enough");
        reg.register(e);
        // Same TTL discipline as direct entries: expired, so never served.
        assert!(reg.snapshot().is_empty());
        // Re-gossiping (same key, same timestamp) revives it.
        reg.register_descriptor(signed(1, "Remote", [10, 0, 0, 1], 5500, 100), Some(peer))
            .unwrap();
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].via, Some(peer));
    }

    /// All health tests inject time **forward** from a base instant —
    /// simulating a day never sleeps and never underflows `Instant` (unlike
    /// backdating by 24 h, which can fail on freshly booted machines).
    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn index_sorts_signed_first_then_uptime_then_name() {
        let reg = Registry::new(DEFAULT_TTL);
        let base = Instant::now();

        // "Steady" (signed): announces every 5 minutes for an hour.
        for k in 0..12 {
            reg.register_descriptor_at(signed(1, "Steady", [10, 0, 0, 1], 5500, 100), None, {
                at(base, k * 300)
            })
            .unwrap();
        }
        // "Flaky" (signed): announces at the start, then only near the end.
        reg.register_descriptor_at(signed(2, "Flaky", [10, 0, 0, 2], 5500, 100), None, base)
            .unwrap();
        reg.register_descriptor_at(
            signed(2, "Flaky", [10, 0, 0, 2], 5500, 100),
            None,
            at(base, 3300),
        )
        .unwrap();
        // Two unsigned entries with identical (fresh) histories.
        reg.register_unsigned_at(entry("Beta", [10, 0, 0, 4], 5500, 0), at(base, 3300));
        reg.register_unsigned_at(entry("Alpha", [10, 0, 0, 3], 5500, 0), at(base, 3300));

        let rows = reg.index_at(at(base, 3600));
        let names: Vec<&str> = rows.iter().map(|r| r.entry.name.as_str()).collect();
        // Signed before unsigned (even when a signed entry's observed uptime
        // is lower than an unsigned one's); uptime breaks ties among signed,
        // name among equals.
        assert_eq!(names, ["Steady", "Flaky", "Alpha", "Beta"]);
        // Steady: 4 full buckets of 3/3, then a silent partial → 4000/5.
        assert_eq!(rows[0].uptime_permille, 800);
        // Flaky: 1/3, silence, 1/3, silent partial → (333+0+0+333+0)/5.
        assert_eq!(rows[1].uptime_permille, 133);
        assert!(rows[1].uptime_permille < rows[2].uptime_permille);
        assert_eq!(rows[2].uptime_permille, rows[3].uptime_permille);
        assert_eq!(rows[0].last_seen_secs, 300, "last announce was at +55 min");
        assert_eq!(rows[2].last_seen_secs, 300);
    }

    #[test]
    fn index_category_filters_but_keeps_the_ordering() {
        let reg = Registry::new(DEFAULT_TTL);
        let base = Instant::now();
        reg.register_descriptor_at(signed(1, "Chatty", [10, 0, 0, 1], 5500, 100), None, base)
            .unwrap();
        reg.register_unsigned_at(entry("Plain", [10, 0, 0, 2], 5500, 0), base);

        let rows = reg.index_category_at("CHAT", base);
        assert_eq!(rows.len(), 1, "unsigned entries carry no categories");
        assert_eq!(rows[0].entry.name, "Chatty");
        assert!(reg.index_category_at("nope", base).is_empty());
    }

    #[test]
    fn gossiped_entries_start_fresh_local_health_logs() {
        // The "not authoritative" line: a peer has watched this server for
        // (say) a day, but nothing about that history travels with the
        // gossiped descriptor — the local log starts at local first sight.
        let reg = Registry::new(DEFAULT_TTL);
        let base = Instant::now();
        let peer: SocketAddr = ([192, 0, 2, 9], 4656).into();
        let addr: SocketAddr = ([10, 0, 0, 1], 5500).into();
        reg.register_descriptor_at(
            signed(1, "Remote", [10, 0, 0, 1], 5500, 100),
            Some(peer),
            base,
        )
        .unwrap();

        let report = reg.health_report_at(addr, base).unwrap();
        assert!(report.live);
        assert_eq!(report.first_seen_secs, 0, "log starts at local first sight");
        assert_eq!(report.uptime_permille, 1000);
        assert_eq!(report.flap_count, 0);
        assert_eq!(report.sparkline, "#");
    }

    #[test]
    fn lazy_expiry_sweeps_count_flaps_and_health_outlives_the_entry() {
        let reg = Registry::new(DEFAULT_TTL);
        let base = Instant::now();
        let addr: SocketAddr = ([10, 0, 0, 1], 5500).into();
        reg.register_unsigned_at(entry("Blinky", [10, 0, 0, 1], 5500, 0), base);

        // Past the TTL the entry is swept…
        let after = at(base, DEFAULT_TTL.as_secs() + 1);
        assert!(reg.snapshot_at(after).is_empty());
        // …but its health log survives, now expired with one flap on record.
        let report = reg.health_report_at(addr, after).unwrap();
        assert!(!report.live);
        assert_eq!(report.flap_count, 1);
        assert_eq!(report.last_seen_secs, DEFAULT_TTL.as_secs() + 1);

        // Coming back revives the same log: first_seen persists, and a
        // second death is a second flap.
        reg.register_unsigned_at(entry("Blinky", [10, 0, 0, 1], 5500, 0), at(base, 400));
        let report = reg.health_report_at(addr, at(base, 400)).unwrap();
        assert!(report.live);
        assert_eq!(report.flap_count, 1);
        assert_eq!(report.first_seen_secs, 400, "first sight is remembered");
        let much_later = at(base, 400 + DEFAULT_TTL.as_secs() + 1);
        assert!(reg.snapshot_at(much_later).is_empty());
        assert_eq!(
            reg.health_report_at(addr, much_later).unwrap().flap_count,
            2
        );
    }

    #[test]
    fn health_logs_are_forgotten_after_a_silent_observation_window() {
        let reg = Registry::new(DEFAULT_TTL);
        let base = Instant::now();
        let addr: SocketAddr = ([10, 0, 0, 1], 5500).into();
        reg.register_unsigned_at(entry("Gone", [10, 0, 0, 1], 5500, 0), base);

        // Still remembered just inside the 24 h window…
        let window = crate::health::OBSERVATION_WINDOW.as_secs();
        assert!(reg.health_report_at(addr, at(base, window - 1)).is_some());
        // …forgotten once silent for the whole window.
        assert!(reg.health_report_at(addr, at(base, window)).is_none());
    }

    #[test]
    fn category_filter_and_counts() {
        let reg = Registry::new(DEFAULT_TTL);
        let key = IdentityKey::from_seed(&[1u8; 32]);
        let chatty = Descriptor::new("Chatty", ([10, 0, 0, 1], 5500).into())
            .with_category("Chat")
            .with_category("retro")
            .sign(&key)
            .unwrap();
        let key2 = IdentityKey::from_seed(&[2u8; 32]);
        let filesy = Descriptor::new("Filesy", ([10, 0, 0, 2], 5500).into())
            .with_category("warez")
            .with_category("chat")
            .sign(&key2)
            .unwrap();
        reg.register_descriptor(chatty, None).unwrap();
        reg.register_descriptor(filesy, None).unwrap();
        reg.register_unsigned(entry("Plain", [10, 0, 0, 3], 5500, 0));

        // Filtering is ASCII-case-insensitive; unsigned entries have no tags.
        let chat = reg.snapshot_category("CHAT");
        assert_eq!(
            chat.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["Chatty", "Filesy"]
        );
        assert_eq!(reg.snapshot_category("warez").len(), 1);
        assert!(reg.snapshot_category("nope").is_empty());

        // The summary merges case and sorts by name.
        assert_eq!(
            reg.category_counts(),
            vec![
                ("chat".to_owned(), 2),
                ("retro".to_owned(), 1),
                ("warez".to_owned(), 1),
            ]
        );
    }
}
