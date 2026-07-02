//! The swarm catalog (Wave 5): TTL'd soft state of who-has-what.
//!
//! Peers advertise files they hold (by blake3 root) without uploading;
//! this catalog answers "who has root X?" for the coordinator. State is
//! deliberately *soft*: every advert expires unless re-announced, and all
//! of a session's adverts vanish the moment it closes — the catalog never
//! claims a source that can't currently serve. Nothing here persists;
//! a restart simply waits for peers to re-announce.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

/// One live advertisement: `session_id` scopes its lifetime, `expires_at`
/// its freshness.
#[derive(Debug, Clone)]
pub struct Advert {
    pub account_id: i64,
    pub session_id: u64,
    pub screen_name: String,
    pub size: u64,
    pub name: String,
    pub mime: String,
    pub expires_at: Instant,
}

/// What `advertise` did, for the ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvertiseOutcome {
    /// Entries stored (the rest hit the per-account cap).
    pub accepted: u32,
    /// The account's live advert count after the call.
    pub total: u32,
}

/// An entry a peer wants to advertise.
#[derive(Debug, Clone)]
pub struct NewAdvert {
    pub root: [u8; 32],
    pub size: u64,
    pub name: String,
    pub mime: String,
}

/// A session's peer-wire contact card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEndpoint {
    /// `ip:port` — observed IP paired with the client-declared port.
    pub endpoint: String,
    /// Self-signed TLS cert fingerprint peers pin when dialing.
    pub cert_fp: [u8; 32],
}

#[derive(Default)]
pub struct SwarmCatalog {
    /// root → live adverts for it (at most one per session per root).
    inner: RwLock<HashMap<[u8; 32], Vec<Advert>>>,
    /// session → its peer-wire contact card (None until registered).
    contacts: RwLock<HashMap<u64, PeerEndpoint>>,
}

impl SwarmCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store (or refresh) adverts for a session. Re-advertising a root this
    /// session already holds refreshes its TTL/metadata without counting
    /// against the cap again; `max_per_account` (0 = unlimited) bounds the
    /// account's total live adverts.
    pub fn advertise(
        &self,
        entries: &[NewAdvert],
        account_id: i64,
        session_id: u64,
        screen_name: &str,
        ttl: Duration,
        max_per_account: usize,
    ) -> AdvertiseOutcome {
        self.advertise_at(
            entries,
            account_id,
            session_id,
            screen_name,
            ttl,
            max_per_account,
            Instant::now(),
        )
    }

    /// Clock-injectable core of [`advertise`] (tests fast-forward `now`).
    #[allow(clippy::too_many_arguments)]
    fn advertise_at(
        &self,
        entries: &[NewAdvert],
        account_id: i64,
        session_id: u64,
        screen_name: &str,
        ttl: Duration,
        max_per_account: usize,
        now: Instant,
    ) -> AdvertiseOutcome {
        let mut map = self.inner.write();
        // Opportunistic global prune keeps the map from accumulating stale
        // entries between finds.
        prune(&mut map, now);

        let mut total = live_count_for_account(&map, account_id);
        let mut accepted = 0u32;
        for e in entries {
            let adverts = map.entry(e.root).or_default();
            if let Some(existing) = adverts.iter_mut().find(|a| a.session_id == session_id) {
                // Refresh: same session re-announcing this root.
                existing.expires_at = now + ttl;
                existing.size = e.size;
                existing.name = e.name.clone();
                existing.mime = e.mime.clone();
                existing.screen_name = screen_name.to_string();
                accepted += 1;
                continue;
            }
            if max_per_account > 0 && total >= max_per_account {
                // Cap hit: drop the empty vec we may have just created.
                if adverts.is_empty() {
                    map.remove(&e.root);
                }
                continue;
            }
            adverts.push(Advert {
                account_id,
                session_id,
                screen_name: screen_name.to_string(),
                size: e.size,
                name: e.name.clone(),
                mime: e.mime.clone(),
                expires_at: now + ttl,
            });
            accepted += 1;
            total += 1;
        }
        AdvertiseOutcome {
            accepted,
            total: total as u32,
        }
    }

    /// Withdraw a session's adverts for `roots` (empty = all of them).
    pub fn withdraw(&self, session_id: u64, roots: &[[u8; 32]]) {
        let mut map = self.inner.write();
        if roots.is_empty() {
            map.retain(|_, adverts| {
                adverts.retain(|a| a.session_id != session_id);
                !adverts.is_empty()
            });
        } else {
            for root in roots {
                if let Some(adverts) = map.get_mut(root) {
                    adverts.retain(|a| a.session_id != session_id);
                    if adverts.is_empty() {
                        map.remove(root);
                    }
                }
            }
        }
    }

    /// Session teardown: every advert it held vanishes (a disconnected peer
    /// can't serve bytes), and so does its contact card.
    pub fn session_closed(&self, session_id: u64) {
        self.withdraw(session_id, &[]);
        self.contacts.write().remove(&session_id);
    }

    /// Register (or replace) a session's peer-wire contact card.
    pub fn set_contact(&self, session_id: u64, endpoint: PeerEndpoint) {
        self.contacts.write().insert(session_id, endpoint);
    }

    /// The session's contact card, if it registered one.
    pub fn contact(&self, session_id: u64) -> Option<PeerEndpoint> {
        self.contacts.read().get(&session_id).cloned()
    }

    /// Persona switch/rename: keep the catalog's names in step with
    /// presence so `FindSources` never reports an identity the session no
    /// longer presents.
    pub fn rename_session(&self, session_id: u64, screen_name: &str) {
        let mut map = self.inner.write();
        for adverts in map.values_mut() {
            for a in adverts.iter_mut().filter(|a| a.session_id == session_id) {
                a.screen_name = screen_name.to_string();
            }
        }
    }

    /// Live sources for a root (expired entries are pruned on the way).
    pub fn find(&self, root: &[u8; 32]) -> Vec<Advert> {
        self.find_at(root, Instant::now())
    }

    fn find_at(&self, root: &[u8; 32], now: Instant) -> Vec<Advert> {
        let mut map = self.inner.write();
        match map.get_mut(root) {
            Some(adverts) => {
                adverts.retain(|a| a.expires_at > now);
                if adverts.is_empty() {
                    map.remove(root);
                    Vec::new()
                } else {
                    adverts.clone()
                }
            }
            None => Vec::new(),
        }
    }

    /// Live advert count for an account (diagnostics / cap checks).
    pub fn count_for_account(&self, account_id: i64) -> usize {
        live_count_for_account(&self.inner.read(), account_id)
    }
}

fn live_count_for_account(map: &HashMap<[u8; 32], Vec<Advert>>, account_id: i64) -> usize {
    map.values()
        .flat_map(|v| v.iter())
        .filter(|a| a.account_id == account_id)
        .count()
}

fn prune(map: &mut HashMap<[u8; 32], Vec<Advert>>, now: Instant) {
    map.retain(|_, adverts| {
        adverts.retain(|a| a.expires_at > now);
        !adverts.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adv(root: u8, name: &str) -> NewAdvert {
        NewAdvert {
            root: [root; 32],
            size: 100,
            name: name.into(),
            mime: "application/octet-stream".into(),
        }
    }

    const TTL: Duration = Duration::from_secs(60);

    #[test]
    fn advertise_find_withdraw() {
        let cat = SwarmCatalog::new();
        let out = cat.advertise(&[adv(1, "a"), adv(2, "b")], 10, 100, "alice", TTL, 0);
        assert_eq!(
            out,
            AdvertiseOutcome {
                accepted: 2,
                total: 2
            }
        );

        let sources = cat.find(&[1; 32]);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].screen_name, "alice");
        assert_eq!(sources[0].name, "a");

        cat.withdraw(100, &[[1; 32]]);
        assert!(cat.find(&[1; 32]).is_empty());
        assert_eq!(cat.find(&[2; 32]).len(), 1, "other root untouched");
    }

    #[test]
    fn readvertise_refreshes_without_double_count() {
        let cat = SwarmCatalog::new();
        cat.advertise(&[adv(1, "old")], 10, 100, "alice", TTL, 0);
        let out = cat.advertise(&[adv(1, "new")], 10, 100, "alice", TTL, 0);
        assert_eq!(
            out,
            AdvertiseOutcome {
                accepted: 1,
                total: 1
            }
        );
        let sources = cat.find(&[1; 32]);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "new", "metadata refreshed");
    }

    #[test]
    fn per_account_cap_enforced_across_sessions() {
        let cat = SwarmCatalog::new();
        // Cap 2: first two accepted, third refused — even from a second
        // session of the same account.
        let out = cat.advertise(&[adv(1, "a"), adv(2, "b")], 10, 100, "alice", TTL, 2);
        assert_eq!(out.accepted, 2);
        let out = cat.advertise(&[adv(3, "c")], 10, 101, "alice", TTL, 2);
        assert_eq!(
            out,
            AdvertiseOutcome {
                accepted: 0,
                total: 2
            }
        );
        // A different account is unaffected.
        let out = cat.advertise(&[adv(3, "c")], 11, 102, "bob", TTL, 2);
        assert_eq!(out.accepted, 1);
    }

    #[test]
    fn expiry_prunes() {
        let cat = SwarmCatalog::new();
        let t0 = Instant::now();
        cat.advertise_at(&[adv(1, "a")], 10, 100, "alice", TTL, 0, t0);
        assert_eq!(cat.find_at(&[1; 32], t0 + TTL / 2).len(), 1, "still fresh");
        assert!(
            cat.find_at(&[1; 32], t0 + TTL + Duration::from_secs(1))
                .is_empty(),
            "expired advert is gone"
        );
        // And the map slot itself was dropped.
        assert_eq!(cat.count_for_account(10), 0);
    }

    #[test]
    fn session_close_drops_only_that_session() {
        let cat = SwarmCatalog::new();
        cat.advertise(&[adv(1, "a")], 10, 100, "alice", TTL, 0);
        cat.advertise(&[adv(1, "a")], 11, 200, "bob", TTL, 0);
        assert_eq!(cat.find(&[1; 32]).len(), 2, "two sources for the root");

        cat.session_closed(100);
        let left = cat.find(&[1; 32]);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].screen_name, "bob");
    }

    #[test]
    fn rename_session_updates_catalog_names() {
        let cat = SwarmCatalog::new();
        cat.advertise(&[adv(1, "a"), adv(2, "b")], 10, 100, "alice", TTL, 0);
        cat.advertise(&[adv(1, "a")], 11, 200, "bob", TTL, 0);

        cat.rename_session(100, "cheshire");
        for root in [[1; 32], [2; 32]] {
            for a in cat.find(&root) {
                if a.session_id == 100 {
                    assert_eq!(a.screen_name, "cheshire");
                } else {
                    assert_eq!(a.screen_name, "bob", "other sessions untouched");
                }
            }
        }
    }

    #[test]
    fn two_sessions_same_account_hold_distinct_adverts() {
        let cat = SwarmCatalog::new();
        cat.advertise(&[adv(1, "a")], 10, 100, "alice", TTL, 0);
        cat.advertise(&[adv(1, "a")], 10, 101, "alice", TTL, 0);
        assert_eq!(cat.find(&[1; 32]).len(), 2);
        assert_eq!(cat.count_for_account(10), 2);
        cat.session_closed(100);
        assert_eq!(cat.find(&[1; 32]).len(), 1);
    }
}
