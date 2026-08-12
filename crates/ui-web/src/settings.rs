//! App settings: the handful of choices that belong to *you*, not to a burrow.
//!
//! Chiefly **trackers** — the directories the Looking Glass asks when you go
//! looking for burrows to join. A tracker is a discovery service, not a place
//! you're a member of: it answers "who is out there", and the answer is only as
//! good as the tracker, so the list is yours to edit and always shows where
//! each entry came from.
//!
//! Pure and host-tested; `localStorage` persistence is wasm-gated.

use serde::{Deserialize, Serialize};

/// The tracker every install starts with — the project's own directory.
pub const DEFAULT_TRACKER: &str = "tracker.rabbit.direct";

/// One discovery tracker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracker {
    /// Host (or host:port) to query.
    pub host: String,
    /// Whether it's consulted. Disabling beats deleting for the default one:
    /// you can turn the project's tracker off without losing the address.
    pub enabled: bool,
}

impl Tracker {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            enabled: true,
        }
    }
}

/// Everything on the Settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Discovery trackers, in query order.
    pub trackers: Vec<Tracker>,
    /// Reconnect to the burrows you were in when the app last closed.
    pub reconnect_on_launch: bool,
    /// Show OS notifications for DMs and mentions while the window is away.
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            trackers: vec![Tracker::new(DEFAULT_TRACKER)],
            reconnect_on_launch: true,
            notifications: true,
        }
    }
}

/// Normalise a typed tracker address: trim, drop any scheme and trailing path,
/// lowercase the host. Empty (or nothing but a scheme) yields `None`.
///
/// People paste `https://tracker.example/` out of a browser; a tracker list
/// full of near-duplicates that differ only by scheme is a support burden, so
/// one spelling wins.
pub fn normalize_tracker(input: &str) -> Option<String> {
    let s = input.trim();
    let s = s
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or("").trim();
    if s.is_empty() {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

/// Add a tracker, ignoring duplicates and junk. Returns whether it was added.
pub fn add_tracker(list: &mut Vec<Tracker>, input: &str) -> bool {
    let Some(host) = normalize_tracker(input) else {
        return false;
    };
    if list.iter().any(|t| t.host == host) {
        return false;
    }
    list.push(Tracker::new(host));
    true
}

/// The trackers actually queried, in order.
pub fn active(list: &[Tracker]) -> Vec<&Tracker> {
    list.iter().filter(|t| t.enabled).collect()
}

#[cfg(target_arch = "wasm32")]
pub mod storage {
    use super::Settings;

    const KEY: &str = "rh.settings.v1";

    fn store() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn load() -> Settings {
        store()
            .and_then(|s| s.get_item(KEY).ok().flatten())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save(settings: &Settings) {
        if let (Some(s), Ok(json)) = (store(), serde_json::to_string(settings)) {
            let _ = s.set_item(KEY, &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_install_knows_one_tracker() {
        let s = Settings::default();
        assert_eq!(s.trackers.len(), 1);
        assert_eq!(s.trackers[0].host, DEFAULT_TRACKER);
        assert!(s.trackers[0].enabled);
    }

    #[test]
    fn pasted_addresses_normalise_to_one_spelling() {
        // All of these are the same tracker; a list holding four of them would
        // query it four times and read as clutter.
        for input in [
            "tracker.rabbit.direct",
            "  tracker.rabbit.direct  ",
            "https://tracker.rabbit.direct",
            "wss://tracker.rabbit.direct/announce?x=1",
            "TRACKER.RABBIT.DIRECT",
        ] {
            assert_eq!(
                normalize_tracker(input).as_deref(),
                Some("tracker.rabbit.direct"),
                "{input}"
            );
        }
        // A port is part of the address and survives.
        assert_eq!(
            normalize_tracker("tracker.example:9443").as_deref(),
            Some("tracker.example:9443")
        );
        // Junk is not a tracker.
        for bad in ["", "   ", "https://", "//"] {
            assert_eq!(normalize_tracker(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn adding_is_idempotent_and_refuses_junk() {
        let mut list = vec![Tracker::new(DEFAULT_TRACKER)];
        assert!(!add_tracker(&mut list, "https://tracker.rabbit.direct/"), "already there");
        assert!(!add_tracker(&mut list, "   "), "junk");
        assert!(add_tracker(&mut list, "tracker.example"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].host, "tracker.example");
    }

    #[test]
    fn disabling_keeps_the_address_but_stops_the_query() {
        // Turning the default off must not lose it — that's the difference
        // between disable and delete.
        let mut list = vec![Tracker::new(DEFAULT_TRACKER), Tracker::new("t2.example")];
        list[0].enabled = false;
        assert_eq!(list.len(), 2, "still remembered");
        let on = active(&list);
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].host, "t2.example");
    }
}
