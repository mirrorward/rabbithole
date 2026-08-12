//! The **sightings ledger**: where you know each person *from*.
//!
//! The live People view can only say who is on the burrows you're connected to
//! right now. A person page needs provenance — "you know maria from The Warren
//! and the Night Pool, last seen there two weeks ago" — which is a memory, not
//! a connection. This module is that memory: every roster that arrives leaves a
//! trace of (person, burrow, handle-they-used, when).
//!
//! People are keyed the same way [`crate::state::merge_people`] coalesces them:
//! by verified identity key when they carry one, else by bare handle. The
//! per-burrow **handle** is recorded too, because the same key can wear a
//! different name on every burrow — and DM threads and file uploads on a burrow
//! are filed under that burrow's handle.
//!
//! Pure and host-tested; only the `localStorage` load/save is wasm-gated.

use serde::{Deserialize, Serialize};

/// One person's trail across burrows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sighting {
    /// Coalescing seed: identity-key hex, else the bare handle.
    pub seed: String,
    /// The display name from their most recent sighting anywhere.
    pub name: String,
    /// Everywhere they've been seen, most recent first.
    pub burrows: Vec<BurrowSeen>,
}

/// A person on one burrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BurrowSeen {
    /// The burrow's endpoint (the session id — stable across renames).
    pub endpoint: String,
    /// The burrow's display name at last sighting.
    pub burrow_name: String,
    /// The handle this person used *on this burrow* (DMs and uploads there are
    /// filed under it).
    pub handle: String,
    /// Unix ms of the most recent sighting.
    pub last_seen_unix_ms: i64,
}

/// Most burrows remembered per person, and most people remembered overall.
/// Oldest-seen fall off first: this is a memory, not an archive.
pub const MAX_BURROWS_PER_PERSON: usize = 12;
pub const MAX_PEOPLE: usize = 400;

/// The coalescing seed for a roster entry — mirrors `merge_people`.
pub fn seed_of(key: Option<&str>, handle: &str) -> String {
    match key {
        Some(k) if !k.trim().is_empty() => k.trim().to_lowercase(),
        _ => handle.trim().to_lowercase(),
    }
}

/// Record one sighting. Updates in place: a person's entry for a burrow is
/// keyed by endpoint, so a rename updates the stored burrow name rather than
/// duplicating it, and re-seeing someone refreshes `last_seen`.
pub fn note(
    ledger: &mut Vec<Sighting>,
    key: Option<&str>,
    handle: &str,
    endpoint: &str,
    burrow_name: &str,
    now_unix_ms: i64,
) {
    let seed = seed_of(key, handle);
    if seed.is_empty() || endpoint.is_empty() {
        return;
    }
    let entry = match ledger.iter_mut().find(|s| s.seed == seed) {
        Some(e) => e,
        None => {
            ledger.push(Sighting {
                seed: seed.clone(),
                name: handle.to_string(),
                burrows: Vec::new(),
            });
            ledger.last_mut().expect("just pushed")
        }
    };
    entry.name = handle.to_string();
    match entry.burrows.iter_mut().find(|b| b.endpoint == endpoint) {
        Some(b) => {
            b.burrow_name = burrow_name.to_string();
            b.handle = handle.to_string();
            b.last_seen_unix_ms = now_unix_ms;
        }
        None => entry.burrows.push(BurrowSeen {
            endpoint: endpoint.to_string(),
            burrow_name: burrow_name.to_string(),
            handle: handle.to_string(),
            last_seen_unix_ms: now_unix_ms,
        }),
    }
    // Most recent first, oldest off the end.
    entry.burrows.sort_by_key(|b| -b.last_seen_unix_ms);
    entry.burrows.truncate(MAX_BURROWS_PER_PERSON);
    if ledger.len() > MAX_PEOPLE {
        // Drop the person least recently seen anywhere.
        ledger.sort_by_key(|s| -s.burrows.iter().map(|b| b.last_seen_unix_ms).max().unwrap_or(0));
        ledger.truncate(MAX_PEOPLE);
    }
}

/// The trail for one person, most recent burrow first.
pub fn burrows_for<'a>(ledger: &'a [Sighting], seed: &str) -> Option<&'a Sighting> {
    ledger.iter().find(|s| s.seed == seed)
}

/// The handle a person used on a specific burrow, if they've been seen there.
pub fn handle_on(ledger: &[Sighting], seed: &str, endpoint: &str) -> Option<String> {
    burrows_for(ledger, seed)?
        .burrows
        .iter()
        .find(|b| b.endpoint == endpoint)
        .map(|b| b.handle.clone())
}

#[cfg(target_arch = "wasm32")]
pub mod storage {
    //! `localStorage` persistence, keyed like [`crate::recent`]'s.
    use super::Sighting;

    const KEY: &str = "rh.sightings.v1";

    fn store() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn load() -> Vec<Sighting> {
        store()
            .and_then(|s| s.get_item(KEY).ok().flatten())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save(ledger: &[Sighting]) {
        if let (Some(s), Ok(json)) = (store(), serde_json::to_string(ledger)) {
            let _ = s.set_item(KEY, &json);
        }
    }

    /// Note a whole roster in one load-mutate-save pass.
    pub fn note_roster(
        endpoint: &str,
        burrow_name: &str,
        roster: &[crate::state::Presence],
        now_unix_ms: i64,
    ) {
        let mut ledger = load();
        for p in roster {
            super::note(
                &mut ledger,
                p.key.as_deref(),
                &p.screen_name,
                endpoint,
                burrow_name,
                now_unix_ms,
            );
        }
        save(&ledger);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sighting_is_remembered_with_where_and_as_whom() {
        let mut l = Vec::new();
        note(&mut l, Some("KEY1"), "maria", "ws://a", "The Warren", 1_000);
        note(&mut l, Some("key1"), "mre", "ws://b", "Night Pool", 2_000);
        // One person (key-coalesced, case-insensitive), two burrows, the
        // per-burrow handle kept — DMs on each burrow are filed under it.
        assert_eq!(l.len(), 1);
        let s = burrows_for(&l, "key1").unwrap();
        assert_eq!(s.burrows.len(), 2);
        assert_eq!(s.name, "mre", "display follows the latest sighting");
        assert_eq!(handle_on(&l, "key1", "ws://a").as_deref(), Some("maria"));
        assert_eq!(handle_on(&l, "key1", "ws://b").as_deref(), Some("mre"));
        // Most recent burrow first.
        assert_eq!(s.burrows[0].endpoint, "ws://b");
    }

    #[test]
    fn reseeing_updates_in_place_never_duplicates() {
        let mut l = Vec::new();
        note(&mut l, None, "bob", "ws://a", "Warren", 1_000);
        note(&mut l, None, "bob", "ws://a", "The Warren (renamed)", 5_000);
        let s = burrows_for(&l, "bob").unwrap();
        assert_eq!(s.burrows.len(), 1, "keyed by endpoint, not name");
        assert_eq!(s.burrows[0].burrow_name, "The Warren (renamed)");
        assert_eq!(s.burrows[0].last_seen_unix_ms, 5_000);
    }

    #[test]
    fn keyless_people_stay_distinct_from_keyed_ones() {
        let mut l = Vec::new();
        note(&mut l, None, "rabbit", "ws://a", "A", 1);
        note(&mut l, Some("kk"), "rabbit", "ws://b", "B", 2);
        // Same handle, one with a key: two different people.
        assert_eq!(l.len(), 2);
        assert_eq!(seed_of(None, "Rabbit"), "rabbit");
        assert_eq!(seed_of(Some("KK"), "rabbit"), "kk");
        assert_eq!(seed_of(Some("  "), "x"), "x", "blank key is no key");
    }

    #[test]
    fn the_ledger_is_a_memory_not_an_archive() {
        let mut l = Vec::new();
        // One person's burrow list caps at the most recent N.
        for i in 0..(MAX_BURROWS_PER_PERSON as i64 + 6) {
            note(&mut l, Some("k"), "kim", &format!("ws://{i}"), "B", i);
        }
        let s = burrows_for(&l, "k").unwrap();
        assert_eq!(s.burrows.len(), MAX_BURROWS_PER_PERSON);
        assert_eq!(s.burrows[0].last_seen_unix_ms, MAX_BURROWS_PER_PERSON as i64 + 5);
        // The people list caps too, dropping the least recently seen.
        let mut l = Vec::new();
        for i in 0..(MAX_PEOPLE as i64 + 10) {
            note(&mut l, None, &format!("p{i}"), "ws://a", "A", i);
        }
        assert_eq!(l.len(), MAX_PEOPLE);
        assert!(burrows_for(&l, "p0").is_none(), "oldest fell off");
        assert!(burrows_for(&l, &format!("p{}", MAX_PEOPLE + 9)).is_some());
    }
}
