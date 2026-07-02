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

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::descriptor::{DescriptorError, SignedDescriptor};

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

    /// Raw insert: adds or replaces the entry for its `(ip, port)`, bypassing
    /// the key-conflict policy. Plumbing for tests (backdated entries) and
    /// callers that have already applied policy; services should use
    /// [`register_unsigned`](Self::register_unsigned) or
    /// [`register_descriptor`](Self::register_descriptor).
    pub fn register(&self, entry: ServerEntry) {
        let key = (entry.addr.ip(), entry.addr.port());
        self.entries
            .lock()
            .expect("registry lock poisoned")
            .insert(key, entry);
    }

    /// The classic unsigned heartbeat. For an unclaimed (or unsigned) slot
    /// this inserts/replaces and restarts the TTL clock, as ever. If the slot
    /// is held by a live **signed** entry, the heartbeat refreshes only the
    /// TTL and user count — an unauthenticated packet cannot rename,
    /// re-point, or strip a verified identity.
    pub fn register_unsigned(&self, entry: ServerEntry) {
        let key = (entry.addr.ip(), entry.addr.port());
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("registry lock poisoned");
        entries.retain(|_, e| now.duration_since(e.last_heartbeat) < self.ttl);
        if let Some(existing) = entries.get_mut(&key) {
            if existing.signed.is_some() {
                existing.users_online = entry.users_online;
                existing.last_heartbeat = now;
                return;
            }
        }
        entries.insert(key, entry);
    }

    /// The signed registration path (direct announce or gossip). Verifies the
    /// descriptor, then applies the key-conflict policy from the module docs:
    ///
    /// - unclaimed or unsigned slot → insert (signed upgrades unsigned);
    /// - live slot under a **different** key → [`RegisterError::KeyConflict`]
    ///   until the old entry expires;
    /// - same key with an older timestamp → [`RegisterError::Stale`]; equal
    ///   or newer replaces the entry and restarts the TTL clock.
    ///
    /// `via` names the gossip peer this arrived from (`None` = direct).
    pub fn register_descriptor(
        &self,
        signed: SignedDescriptor,
        via: Option<SocketAddr>,
    ) -> Result<(), RegisterError> {
        signed.verify()?;
        let addr = signed.descriptor.addr;
        let key = (addr.ip(), addr.port());
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("registry lock poisoned");
        // Prune first so an expired old key never blocks a rotation.
        entries.retain(|_, e| now.duration_since(e.last_heartbeat) < self.ttl);
        if let Some(existing) = entries.get(&key) {
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
        entries.insert(key, ServerEntry::from_signed(signed, via));
        Ok(())
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
