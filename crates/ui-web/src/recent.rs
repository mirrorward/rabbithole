//! Reconnect-on-launch: the burrows you've signed into, remembered across
//! reloads. We persist only the endpoint + handle (never the password) so the
//! login screen can pre-fill and offer one-tap reconnect. The list logic is a
//! pure, host-tested reducer; load/save is wasm-only (`localStorage`).

use serde::{Deserialize, Serialize};

/// A remembered burrow: where it is and who you were there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentBurrow {
    pub endpoint: String,
    pub handle: String,
}

/// Most we keep — enough to cover a person's warren, few enough to stay tidy.
const MAX_RECENT: usize = 8;

/// Fold a fresh sign-in into the recent list: dedup by endpoint (a re-login
/// updates the handle + jumps to front), most-recent first, capped. Pure.
pub fn add_recent(mut list: Vec<RecentBurrow>, entry: RecentBurrow) -> Vec<RecentBurrow> {
    list.retain(|b| b.endpoint != entry.endpoint);
    list.insert(0, entry);
    list.truncate(MAX_RECENT);
    list
}

#[cfg(target_arch = "wasm32")]
mod persist {
    use super::RecentBurrow;

    const KEY: &str = "rh.recent.burrows";

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    /// The remembered burrows, most-recent first (empty if none / unreadable).
    pub fn load() -> Vec<RecentBurrow> {
        storage()
            .and_then(|s| s.get_item(super::persist::KEY).ok().flatten())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Remember a successful sign-in (endpoint + handle; never the password).
    pub fn remember(endpoint: &str, handle: &str) {
        if endpoint.is_empty() || handle.is_empty() {
            return;
        }
        let list = super::add_recent(
            load(),
            RecentBurrow {
                endpoint: endpoint.to_string(),
                handle: handle.to_string(),
            },
        );
        if let (Some(s), Ok(json)) = (storage(), serde_json::to_string(&list)) {
            let _ = s.set_item(KEY, &json);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use persist::{load, remember};

#[cfg(test)]
mod tests {
    use super::*;

    fn b(endpoint: &str, handle: &str) -> RecentBurrow {
        RecentBurrow {
            endpoint: endpoint.into(),
            handle: handle.into(),
        }
    }

    #[test]
    fn add_recent_dedups_by_endpoint_and_orders_most_recent_first() {
        let list = add_recent(vec![], b("ws://a", "alice"));
        let list = add_recent(list, b("ws://b", "bob"));
        // Re-signing into `a` under a new handle moves it to front + updates it.
        let list = add_recent(list, b("ws://a", "alice2"));
        assert_eq!(list.len(), 2, "no duplicate endpoint");
        assert_eq!(list[0], b("ws://a", "alice2"), "most recent first, handle updated");
        assert_eq!(list[1], b("ws://b", "bob"));
    }

    #[test]
    fn add_recent_caps_the_list() {
        let mut list = Vec::new();
        for i in 0..20 {
            list = add_recent(list, b(&format!("ws://{i}"), "u"));
        }
        assert_eq!(list.len(), MAX_RECENT);
        // The newest (last inserted) is at the front.
        assert_eq!(list[0].endpoint, "ws://19");
    }
}
