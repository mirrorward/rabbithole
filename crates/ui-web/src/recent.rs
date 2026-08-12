//! Reconnect-on-launch: the burrows you've signed into, remembered across
//! reloads. We persist only the endpoint + handle (never the password) so the
//! login screen can pre-fill and offer one-tap reconnect. The list logic is a
//! pure, host-tested reducer; load/save is wasm-only (`localStorage`).

use serde::{Deserialize, Serialize};

/// A remembered burrow: where it is, who you were there, and — when the server
/// issued one — a resume bearer token so a reload reconnects without a password.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentBurrow {
    pub endpoint: String,
    pub handle: String,
    /// Resume token from the last successful auth (`None` for guests / not yet
    /// captured). Persisted so the session survives a reload.
    #[serde(default)]
    pub token: Option<String>,
}

/// Most we keep — enough to cover a person's warren, few enough to stay tidy.
const MAX_RECENT: usize = 8;

/// Fold a fresh sign-in into the recent list: dedup by endpoint (a re-login
/// updates the handle + jumps to front), most-recent first, capped. If the new
/// entry carries no token but a prior entry for the same endpoint had one, the
/// token is preserved (a reconnect shouldn't drop a still-valid session). Pure.
pub fn add_recent(mut list: Vec<RecentBurrow>, mut entry: RecentBurrow) -> Vec<RecentBurrow> {
    if entry.token.is_none() {
        if let Some(prior) = list.iter().find(|b| b.endpoint == entry.endpoint) {
            entry.token = prior.token.clone();
        }
    }
    list.retain(|b| b.endpoint != entry.endpoint);
    list.insert(0, entry);
    list.truncate(MAX_RECENT);
    list
}

/// Drop an endpoint from the list entirely — used when the user leaves a
/// burrow, so it stops auto-reconnecting and its resume token stops being
/// stored. Pure.
pub fn forget_endpoint(mut list: Vec<RecentBurrow>, endpoint: &str) -> Vec<RecentBurrow> {
    list.retain(|b| b.endpoint != endpoint);
    list
}

/// Set (or clear) the resume token for an endpoint already in the list. Pure.
pub fn set_token(
    mut list: Vec<RecentBurrow>,
    endpoint: &str,
    token: Option<String>,
) -> Vec<RecentBurrow> {
    if let Some(b) = list.iter_mut().find(|b| b.endpoint == endpoint) {
        b.token = token;
    }
    list
}

/// Split remembered bearer sessions into safe auto-resume targets and entries
/// that need the user to replace a remote `ws://` URL with `wss://`. Blocked
/// entries stay persisted; their bearer is never dispatched over plaintext.
pub fn secure_resumable(list: &[RecentBurrow]) -> (Vec<(String, String)>, Vec<String>) {
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for burrow in list {
        let Some(token) = &burrow.token else {
            continue;
        };
        match rabbithole_core::api::normalize_secure_ws_endpoint(&burrow.endpoint) {
            Ok(endpoint) => ready.push((endpoint, token.clone())),
            Err(_) => blocked.push(burrow.endpoint.clone()),
        }
    }
    (ready, blocked)
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

    fn save(list: &[RecentBurrow]) {
        if let (Some(s), Ok(json)) = (storage(), serde_json::to_string(list)) {
            let _ = s.set_item(KEY, &json);
        }
    }

    /// Remember a connect (endpoint + handle; never the password). Preserves any
    /// existing resume token for this endpoint.
    pub fn remember(endpoint: &str, handle: &str) {
        if endpoint.is_empty() || handle.is_empty() {
            return;
        }
        let list = super::add_recent(
            load(),
            RecentBurrow {
                endpoint: endpoint.to_string(),
                handle: handle.to_string(),
                token: None,
            },
        );
        save(&list);
    }

    /// Store the resume token for an endpoint after a successful auth (empty =
    /// guest / not resumable → clear it).
    /// Forget a burrow: no auto-reconnect, and the stored resume token goes
    /// with it (leaving a burrow should not leave a credential behind).
    pub fn forget(endpoint: &str) {
        let list = super::forget_endpoint(load(), endpoint);
        save(&list);
    }

    pub fn remember_token(endpoint: &str, token: &str) {
        let tok = (!token.is_empty()).then(|| token.to_string());
        save(&super::set_token(load(), endpoint, tok));
    }
}

#[cfg(target_arch = "wasm32")]
pub use persist::{forget, load, remember, remember_token};

#[cfg(test)]
mod tests {
    use super::*;

    fn b(endpoint: &str, handle: &str) -> RecentBurrow {
        RecentBurrow {
            endpoint: endpoint.into(),
            handle: handle.into(),
            token: None,
        }
    }

    #[test]
    fn token_survives_reconnect_and_set_token_updates_it() {
        // Auth captured a token for `a`.
        let list = set_token(
            add_recent(vec![], b("ws://a", "alice")),
            "ws://a",
            Some("tok1".into()),
        );
        assert_eq!(list[0].token.as_deref(), Some("tok1"));
        // A later reconnect (no token in the fresh entry) preserves the stored one.
        let list = add_recent(list, b("ws://a", "alice"));
        assert_eq!(
            list[0].token.as_deref(),
            Some("tok1"),
            "reconnect keeps the resume token"
        );
        // Signing out / a guest auth clears it.
        let list = set_token(list, "ws://a", None);
        assert_eq!(list[0].token, None);
    }

    #[test]
    fn forgetting_a_burrow_drops_it_and_its_token() {
        // Leaving a burrow must not leave a resume credential behind.
        let list = vec![
            RecentBurrow { endpoint: "ws://a".into(), handle: "me".into(), token: Some("t".into()) },
            RecentBurrow { endpoint: "ws://b".into(), handle: "me".into(), token: None },
        ];
        let after = forget_endpoint(list, "ws://a");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].endpoint, "ws://b");
        // Forgetting something absent is a no-op, not a panic.
        assert_eq!(forget_endpoint(after.clone(), "ws://zz").len(), 1);
    }

    #[test]
    fn add_recent_dedups_by_endpoint_and_orders_most_recent_first() {
        let list = add_recent(vec![], b("ws://a", "alice"));
        let list = add_recent(list, b("ws://b", "bob"));
        // Re-signing into `a` under a new handle moves it to front + updates it.
        let list = add_recent(list, b("ws://a", "alice2"));
        assert_eq!(list.len(), 2, "no duplicate endpoint");
        assert_eq!(
            list[0],
            b("ws://a", "alice2"),
            "most recent first, handle updated"
        );
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

    #[test]
    fn auto_resume_never_dispatches_a_bearer_to_remote_plaintext() {
        let list = vec![
            RecentBurrow {
                endpoint: "ws://burrow.example:4654".into(),
                handle: "alice".into(),
                token: Some("secret".into()),
            },
            RecentBurrow {
                endpoint: "ws://127.0.0.1:4654".into(),
                handle: "local".into(),
                token: Some("local-token".into()),
            },
            RecentBurrow {
                endpoint: "wss://safe.example/rhp".into(),
                handle: "safe".into(),
                token: Some("safe-token".into()),
            },
        ];
        let (ready, blocked) = secure_resumable(&list);
        assert_eq!(
            ready,
            vec![
                ("ws://127.0.0.1:4654".into(), "local-token".into()),
                ("wss://safe.example/rhp".into(), "safe-token".into())
            ]
        );
        assert_eq!(blocked, vec!["ws://burrow.example:4654"]);
    }
}
