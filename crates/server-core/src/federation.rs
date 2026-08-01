//! Server-to-server peering registry: the domain state tracking which remote
//! burrows this server knows, has approved, and is currently connected to.
//!
//! This is the I/O-free bookkeeping half of federation (Wave 9). The socket
//! glue that drives the peering handshake lives in the `burrow` server
//! (`apps/server/src/federation.rs`); it calls into this registry to record
//! inbound handshakes, gate on admin approval, and reflect the connection
//! lifecycle.
//!
//! Trust model: a peer is identified by its Ed25519 server key. A key is
//! *approved* only once an admin says so (or it was loaded as approved from
//! disk / configured as a dial target). An inbound handshake from an unknown
//! key is recorded as [`PeerState::Pending`] and refused until approved.

use std::collections::HashMap;

use parking_lot::RwLock;

/// Where a known peer stands in the connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// Seen (it completed an authenticated handshake) but not yet
    /// admin-approved; no session is established.
    Pending,
    /// Approved, but no live session right now.
    Disconnected,
    /// Approved and a live peering session is up.
    Connected,
}

impl PeerState {
    /// Lowercase wire/display token.
    pub fn as_str(self) -> &'static str {
        match self {
            PeerState::Pending => "pending",
            PeerState::Disconnected => "disconnected",
            PeerState::Connected => "connected",
        }
    }
}

/// One tracked peer.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    /// The peer's Ed25519 server identity key.
    pub server_key: [u8; 32],
    /// Human-readable name announced in the handshake (best-effort).
    pub name: String,
    /// Immutable operator-approved federation origin bound to this key.
    pub origin: Option<String>,
    /// Last observed remote socket address, if any.
    pub addr: Option<String>,
    /// Lifecycle state.
    pub state: PeerState,
    /// Whether an admin has approved peering with this key.
    pub approved: bool,
}

impl PeerRecord {
    /// Hex form of the server key (the id admins pass to approve/revoke).
    pub fn key_hex(&self) -> String {
        hex::encode(self.server_key)
    }
}

/// Thread-safe registry of known/approved/pending peers and their live state.
#[derive(Default)]
pub struct PeerRegistry {
    inner: RwLock<HashMap<[u8; 32], PeerRecord>>,
}

impl PeerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an admin-approved peer key (loaded from disk on boot, or a
    /// configured dial target we implicitly trust). Idempotent; never
    /// downgrades a live session.
    pub fn seed_approved(&self, key: [u8; 32], name: impl Into<String>, origin: Option<String>) {
        let mut g = self.inner.write();
        let rec = g.entry(key).or_insert_with(|| PeerRecord {
            server_key: key,
            name: String::new(),
            origin: origin.clone(),
            addr: None,
            state: PeerState::Disconnected,
            approved: true,
        });
        rec.approved = true;
        if rec.origin.is_none() {
            rec.origin = origin;
        }
        let name = name.into();
        if !name.is_empty() {
            rec.name = name;
        }
        if rec.state == PeerState::Pending {
            rec.state = PeerState::Disconnected;
        }
    }

    /// Record an authenticated-but-unapproved inbound handshake as pending.
    /// Never overwrites an already-approved record.
    pub fn note_pending(
        &self,
        key: [u8; 32],
        name: impl Into<String>,
        origin: String,
        addr: Option<String>,
    ) {
        let mut g = self.inner.write();
        let rec = g.entry(key).or_insert_with(|| PeerRecord {
            server_key: key,
            name: String::new(),
            origin: Some(origin.clone()),
            addr: None,
            state: PeerState::Pending,
            approved: false,
        });
        if !rec.approved {
            rec.state = PeerState::Pending;
            rec.origin = Some(origin);
        }
        let name = name.into();
        if !name.is_empty() {
            rec.name = name;
        }
        if addr.is_some() {
            rec.addr = addr;
        }
    }

    /// Whether peering with this key is admin-approved.
    pub fn is_approved(&self, key: &[u8; 32]) -> bool {
        self.inner
            .read()
            .get(key)
            .map(|r| r.approved)
            .unwrap_or(false)
    }

    /// Whether both the key and immutable origin are approved as one tuple.
    pub fn is_approved_origin(&self, key: &[u8; 32], origin: &str) -> bool {
        self.inner
            .read()
            .get(key)
            .map(|record| record.approved && record.origin.as_deref() == Some(origin))
            .unwrap_or(false)
    }

    /// Run `action` only while this exact origin-key tuple remains approved.
    /// The read guard prevents an operator revoke from interleaving with a
    /// trust decision made after the peer has proved key possession.
    pub fn while_approved_origin<T>(
        &self,
        key: &[u8; 32],
        origin: &str,
        action: impl FnOnce() -> T,
    ) -> Option<T> {
        let guard = self.inner.read();
        let approved = guard
            .get(key)
            .map(|record| record.approved && record.origin.as_deref() == Some(origin))
            .unwrap_or(false);
        approved.then(action)
    }

    /// Admin approval that durably binds the peer key to an immutable origin.
    pub fn approve_origin(&self, key: &[u8; 32], origin: String) -> bool {
        self.approve_with_origin(key, Some(origin))
    }

    fn approve_with_origin(&self, key: &[u8; 32], origin: Option<String>) -> bool {
        let mut g = self.inner.write();
        match g.get_mut(key) {
            Some(rec) => {
                rec.approved = true;
                if origin.is_some() {
                    rec.origin = origin;
                }
                if rec.state == PeerState::Pending {
                    rec.state = PeerState::Disconnected;
                }
                true
            }
            None => {
                g.insert(
                    *key,
                    PeerRecord {
                        server_key: *key,
                        name: String::new(),
                        origin,
                        addr: None,
                        state: PeerState::Disconnected,
                        approved: true,
                    },
                );
                false
            }
        }
    }

    /// Revoke approval; the peer drops back to pending. Returns whether it
    /// was known.
    pub fn revoke(&self, key: &[u8; 32]) -> bool {
        let mut g = self.inner.write();
        match g.get_mut(key) {
            Some(rec) => {
                rec.approved = false;
                rec.state = PeerState::Pending;
                true
            }
            None => false,
        }
    }

    /// Mark a session live only if the exact approved tuple still exists.
    /// Returns false after revocation or an origin mismatch.
    pub fn set_connected_if_approved_origin(
        &self,
        key: [u8; 32],
        origin: &str,
        name: impl Into<String>,
        addr: Option<String>,
    ) -> bool {
        let mut guard = self.inner.write();
        let Some(record) = guard.get_mut(&key) else {
            return false;
        };
        if !record.approved || record.origin.as_deref() != Some(origin) {
            return false;
        }
        record.state = PeerState::Connected;
        let name = name.into();
        if !name.is_empty() {
            record.name = name;
        }
        if addr.is_some() {
            record.addr = addr;
        }
        true
    }

    /// Mark a peer's session down (keeps approval). No-op for pending peers.
    pub fn set_disconnected(&self, key: &[u8; 32]) {
        let mut g = self.inner.write();
        if let Some(rec) = g.get_mut(key) {
            if rec.state == PeerState::Connected {
                rec.state = PeerState::Disconnected;
            }
        }
    }

    /// Current lifecycle state of a key, if known.
    pub fn state(&self, key: &[u8; 32]) -> Option<PeerState> {
        self.inner.read().get(key).map(|r| r.state)
    }

    /// A clone of one record, if known.
    pub fn get(&self, key: &[u8; 32]) -> Option<PeerRecord> {
        self.inner.read().get(key).cloned()
    }

    /// All records, sorted by key for stable output.
    pub fn snapshot(&self) -> Vec<PeerRecord> {
        let mut v: Vec<PeerRecord> = self.inner.read().values().cloned().collect();
        v.sort_by_key(|r| r.server_key);
        v
    }

    /// The approved peer keys — what a caller persists to disk.
    pub fn approved_keys(&self) -> Vec<[u8; 32]> {
        let mut v: Vec<[u8; 32]> = self
            .inner
            .read()
            .values()
            .filter(|r| r.approved)
            .map(|r| r.server_key)
            .collect();
        v.sort();
        v
    }

    /// Approved peer tuples, sorted by key for deterministic persistence.
    pub fn approved_origins(&self) -> Vec<([u8; 32], String)> {
        let mut entries: Vec<_> = self
            .inner
            .read()
            .values()
            .filter_map(|record| {
                (record.approved)
                    .then(|| {
                        record
                            .origin
                            .clone()
                            .map(|origin| (record.server_key, origin))
                    })
                    .flatten()
            })
            .collect();
        entries.sort_by_key(|(key, _)| *key);
        entries
    }

    /// Pending (authenticated but unapproved) peers awaiting an admin.
    pub fn pending(&self) -> Vec<PeerRecord> {
        self.inner
            .read()
            .values()
            .filter(|r| !r.approved)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_key_is_not_approved() {
        let reg = PeerRegistry::new();
        assert!(!reg.is_approved(&[1u8; 32]));
        assert_eq!(reg.state(&[1u8; 32]), None);
    }

    #[test]
    fn pending_then_approve_then_connect() {
        let reg = PeerRegistry::new();
        let key = [7u8; 32];
        reg.note_pending(
            key,
            "peer.example",
            "peer.example".into(),
            Some("1.2.3.4:5".into()),
        );
        assert_eq!(reg.state(&key), Some(PeerState::Pending));
        assert!(!reg.is_approved(&key));
        assert_eq!(reg.pending().len(), 1);

        // Admin approves the pending key.
        assert!(reg.approve_origin(&key, "peer.example".into()));
        assert!(reg.is_approved(&key));
        assert!(reg.is_approved_origin(&key, "peer.example"));
        assert!(!reg.is_approved_origin(&key, "victim.example"));
        assert_eq!(reg.state(&key), Some(PeerState::Disconnected));
        assert!(reg.pending().is_empty());

        // Session comes up, then drops.
        assert!(reg.set_connected_if_approved_origin(key, "peer.example", "peer.example", None));
        assert_eq!(reg.state(&key), Some(PeerState::Connected));
        reg.set_disconnected(&key);
        assert_eq!(reg.state(&key), Some(PeerState::Disconnected));
        assert_eq!(reg.approved_keys(), vec![key]);
    }

    #[test]
    fn approving_unknown_origin_key_tuple_seeds_it() {
        let reg = PeerRegistry::new();
        let key = [9u8; 32];
        assert!(!reg.approve_origin(&key, "peer.example".into()));
        assert!(reg.is_approved(&key));
        assert!(reg.is_approved_origin(&key, "peer.example"));
        assert_eq!(reg.state(&key), Some(PeerState::Disconnected));
    }

    #[test]
    fn revoke_returns_to_pending() {
        let reg = PeerRegistry::new();
        let key = [3u8; 32];
        reg.seed_approved(key, "p", Some("p.example".into()));
        assert!(reg.is_approved(&key));
        assert!(reg.revoke(&key));
        assert!(!reg.is_approved(&key));
        assert_eq!(reg.state(&key), Some(PeerState::Pending));
    }

    #[test]
    fn note_pending_never_downgrades_approved() {
        let reg = PeerRegistry::new();
        let key = [5u8; 32];
        reg.seed_approved(key, "p", Some("p.example".into()));
        assert!(reg.set_connected_if_approved_origin(key, "p.example", "p", None));
        // A fresh inbound handshake must not knock an approved peer to pending.
        reg.note_pending(key, "p", "p.example".into(), None);
        assert!(reg.is_approved(&key));
        assert_eq!(reg.state(&key), Some(PeerState::Connected));
        assert_eq!(reg.get(&key).unwrap().origin.as_deref(), Some("p.example"));
    }

    #[test]
    fn conditional_connect_fails_after_revoke_or_origin_mismatch() {
        let reg = PeerRegistry::new();
        let key = [6u8; 32];
        reg.seed_approved(key, "p", Some("p.example".into()));
        assert!(!reg.set_connected_if_approved_origin(key, "victim.example", "p", None));
        assert_eq!(reg.state(&key), Some(PeerState::Disconnected));

        reg.revoke(&key);
        assert!(!reg.set_connected_if_approved_origin(key, "p.example", "p", None));
        assert_eq!(reg.state(&key), Some(PeerState::Pending));
    }

    #[test]
    fn revoke_cannot_interleave_with_post_proof_trust_decision() {
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration;

        let reg = Arc::new(PeerRegistry::new());
        let key = [8u8; 32];
        reg.seed_approved(key, "p", Some("p.example".into()));

        let inside = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let auth = {
            let reg = reg.clone();
            let inside = inside.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                reg.while_approved_origin(&key, "p.example", || {
                    inside.wait();
                    release.wait();
                })
            })
        };
        inside.wait();

        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoke_started = Arc::new(Barrier::new(2));
        let revoke = {
            let reg = reg.clone();
            let revoke_started = revoke_started.clone();
            std::thread::spawn(move || {
                revoke_started.wait();
                revoked_tx.send(reg.revoke(&key)).unwrap();
            })
        };
        revoke_started.wait();
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "revoke must wait until the guarded trust decision completes"
        );
        release.wait();
        assert_eq!(auth.join().unwrap(), Some(()));
        assert!(revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        revoke.join().unwrap();
        assert!(!reg.set_connected_if_approved_origin(key, "p.example", "p", None));
    }
}
