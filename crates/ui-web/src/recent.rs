//! Reconnect-on-launch: the burrows you've signed into, remembered across
//! reloads. We persist only the endpoint + handle (never the password) so the
//! login screen can pre-fill and offer one-tap reconnect. The list logic is a
//! pure, host-tested reducer; load/save is wasm-only (`localStorage`).

use serde::{Deserialize, Serialize};

/// A remembered burrow: where it is, who you were there, and — when the server
/// issued one — a resume bearer token so a reload reconnects without a password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentBurrow {
    /// New account logins are exact typed names. Legacy guest display names
    /// still need their server-appended suffix removed when first read.
    #[serde(default)]
    pub typed_login: bool,
    pub endpoint: String,
    pub handle: String,
    /// Resume token from the last successful auth (`None` for guests / not yet
    /// captured). Persisted so the session survives a reload.
    #[serde(default)]
    pub token: Option<String>,
}

impl std::fmt::Debug for RecentBurrow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecentBurrow")
            .field("endpoint", &self.endpoint)
            .field("handle", &self.handle)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

fn same_endpoint(a: &str, b: &str) -> bool {
    match (
        crate::bookmarks::credential_endpoint(a),
        crate::bookmarks::credential_endpoint(b),
    ) {
        (Some(a), Some(b)) => a == b,
        // Legacy insecure entries stay displayable, without upgrading their
        // credential identity to a different transport or case-sensitive path.
        _ => a == b,
    }
}

fn same_handle(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Most we keep — enough to cover a person's warren, few enough to stay tidy.
const MAX_RECENT: usize = 8;

/// The handle a person typed, without the ` (guest)` a burrow appends to a
/// guest's screen name.
///
/// What gets remembered is the screen name the burrow sent back, and for a
/// guest that is `"test (guest)"`. Prefilling the connect form with it sent it
/// back as the login, so the next visit was `"test (guest) (guest)"`, and the
/// one after that grew another. The suffix is the burrow's annotation, not
/// part of anyone's name: strip every copy.
pub fn bare_handle(handle: &str) -> &str {
    let mut bare = handle.trim();
    while let Some(rest) = bare.strip_suffix("(guest)") {
        bare = rest.trim_end();
    }
    bare
}

/// Clean a stored list on the way in: entries saved before [`bare_handle`]
/// existed still carry their suffixes.
pub fn without_guest_suffixes(mut list: Vec<RecentBurrow>) -> Vec<RecentBurrow> {
    for b in &mut list {
        if !b.typed_login && b.token.is_none() {
            b.handle = bare_handle(&b.handle).to_string();
        }
    }
    list
}

/// Fold a fresh sign-in into the recent list: dedup by endpoint (a re-login
/// updates the handle + jumps to front), most-recent first, capped. If the new
/// entry carries no token, only a prior entry for the same endpoint AND account
/// can supply one. An account switch must never inherit somebody else's token.
pub fn add_recent(mut list: Vec<RecentBurrow>, mut entry: RecentBurrow) -> Vec<RecentBurrow> {
    let account = entry.handle.trim().to_string();
    entry.handle = if entry.token.is_some() {
        account.clone()
    } else {
        bare_handle(&entry.handle).to_string()
    };
    if entry.token.is_none() {
        if let Some(prior) = list.iter().find(|b| {
            same_endpoint(&b.endpoint, &entry.endpoint) && same_handle(&b.handle, &account)
        }) {
            entry.token = prior.token.clone();
        }
    }
    list.retain(|b| !same_endpoint(&b.endpoint, &entry.endpoint));
    list.insert(0, entry);
    list.truncate(MAX_RECENT);
    list
}

/// Record a freshly authenticated account exactly as typed. A fresh sign-in
/// clears the old bearer even for the same login; the caller persists the new
/// AuthOk token only when remembering that sign-in was explicitly requested.
pub fn account_recent(
    mut list: Vec<RecentBurrow>,
    endpoint: &str,
    login: &str,
) -> Vec<RecentBurrow> {
    list.retain(|b| !same_endpoint(&b.endpoint, endpoint));
    list.insert(
        0,
        RecentBurrow {
            typed_login: true,
            endpoint: endpoint.trim().to_string(),
            handle: login.trim().to_string(),
            token: None,
        },
    );
    list.truncate(MAX_RECENT);
    list
}

/// Drop an endpoint from the list entirely — used when the user leaves a
/// burrow, so it stops auto-reconnecting and its resume token stops being
/// stored. Pure.
pub fn forget_endpoint(mut list: Vec<RecentBurrow>, endpoint: &str) -> Vec<RecentBurrow> {
    list.retain(|b| !same_endpoint(&b.endpoint, endpoint));
    list
}

/// Set (or clear) the resume token for an endpoint already in the list. Pure.
pub fn set_token(
    mut list: Vec<RecentBurrow>,
    endpoint: &str,
    token: Option<String>,
) -> Vec<RecentBurrow> {
    if let Some(b) = list
        .iter_mut()
        .find(|b| same_endpoint(&b.endpoint, endpoint))
    {
        b.token = token;
    }
    list
}

/// Account-scoped variants are safe for late authentication callbacks: they
/// cannot clear or replace another account's newer recent entry.
pub fn forget_account(
    mut list: Vec<RecentBurrow>,
    endpoint: &str,
    login: &str,
) -> Vec<RecentBurrow> {
    list.retain(|b| !(same_endpoint(&b.endpoint, endpoint) && same_handle(&b.handle, login)));
    list
}

pub fn set_account_token(
    mut list: Vec<RecentBurrow>,
    endpoint: &str,
    login: &str,
    token: Option<String>,
) -> Vec<RecentBurrow> {
    if let Some(b) = list
        .iter_mut()
        .find(|b| same_endpoint(&b.endpoint, endpoint) && same_handle(&b.handle, login))
    {
        b.token = token.filter(|token| !token.is_empty());
    }
    list
}

/// Split remembered bearer sessions into safe auto-resume targets and entries
/// that need the user to replace a remote `ws://` URL with `wss://`. Blocked
/// entries stay persisted; their bearer is never dispatched over plaintext.
pub fn secure_resumable(list: &[RecentBurrow]) -> (Vec<(String, String)>, Vec<String>) {
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for burrow in list {
        let canonical = crate::bookmarks::credential_endpoint(&burrow.endpoint);
        // The newest account owns this endpoint even when it has opted out.
        // Never silently resume an older account from a legacy duplicate row.
        if !seen.insert(canonical.clone().unwrap_or_else(|| burrow.endpoint.clone())) {
            continue;
        }
        let Some(token) = &burrow.token else {
            continue;
        };
        if token.is_empty() {
            continue;
        }
        match canonical {
            Some(endpoint) => ready.push((endpoint, token.clone())),
            None => blocked.push(burrow.endpoint.clone()),
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
            .map(super::without_guest_suffixes)
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
                typed_login: false,
                endpoint: endpoint.to_string(),
                handle: handle.to_string(),
                token: None,
            },
        );
        save(&list);
    }

    pub fn remember_login(endpoint: &str, login: &str) {
        if endpoint.trim().is_empty() || login.trim().is_empty() {
            return;
        }
        save(&super::account_recent(load(), endpoint, login));
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

    pub fn remember_account_token(endpoint: &str, login: &str, token: &str) {
        let tok = (!token.is_empty()).then(|| token.to_string());
        save(&super::set_account_token(load(), endpoint, login, tok));
    }
}

#[cfg(target_arch = "wasm32")]
pub use persist::{forget, load, remember, remember_account_token, remember_login, remember_token};

#[cfg(test)]
mod tests {
    #[test]
    fn a_guest_suffix_is_never_remembered_as_part_of_the_name() {
        assert_eq!(super::bare_handle("test (guest)"), "test");
        assert_eq!(super::bare_handle("test (guest) (guest)"), "test");
        assert_eq!(
            super::bare_handle("  White Rabbit (guest) "),
            "White Rabbit"
        );
        assert_eq!(super::bare_handle("alice"), "alice");
        assert_eq!(super::bare_handle("guestbook"), "guestbook");
        // Saving strips it...
        let list = super::add_recent(
            Vec::new(),
            super::RecentBurrow {
                typed_login: false,
                endpoint: "ws://localhost:4654".into(),
                handle: "test (guest)".into(),
                token: None,
            },
        );
        assert_eq!(list[0].handle, "test");
        // ...and so does reading a list saved before this rule existed.
        let old = vec![super::RecentBurrow {
            typed_login: false,
            endpoint: "ws://localhost:4654".into(),
            handle: "test (guest) (guest)".into(),
            token: None,
        }];
        assert_eq!(super::without_guest_suffixes(old)[0].handle, "test");
    }

    use super::*;

    fn b(endpoint: &str, handle: &str) -> RecentBurrow {
        RecentBurrow {
            typed_login: false,
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
            RecentBurrow {
                typed_login: false,
                endpoint: "ws://a".into(),
                handle: "me".into(),
                token: Some("t".into()),
            },
            RecentBurrow {
                typed_login: false,
                endpoint: "ws://b".into(),
                handle: "me".into(),
                token: None,
            },
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
                typed_login: false,
                endpoint: "ws://burrow.example:4654".into(),
                handle: "alice".into(),
                token: Some("secret".into()),
            },
            RecentBurrow {
                typed_login: false,
                endpoint: "ws://127.0.0.1:4654".into(),
                handle: "local".into(),
                token: Some("local-token".into()),
            },
            RecentBurrow {
                typed_login: false,
                endpoint: "wss://safe.example/rhp".into(),
                handle: "safe".into(),
                token: Some("safe-token".into()),
            },
        ];
        let (ready, blocked) = secure_resumable(&list);
        assert_eq!(
            ready,
            vec![
                ("ws://127.0.0.1:4654/".into(), "local-token".into()),
                ("wss://safe.example/rhp".into(), "safe-token".into())
            ]
        );
        assert_eq!(blocked, vec!["ws://burrow.example:4654"]);
    }

    #[test]
    fn switching_accounts_never_inherits_or_later_replaces_the_previous_token() {
        let list = vec![RecentBurrow {
            typed_login: false,
            endpoint: "wss://one.example".into(),
            handle: "alice".into(),
            token: Some("alice-secret".into()),
        }];
        let list = add_recent(list, b("wss://ONE.example:443/", "bob"));
        assert_eq!(list.len(), 1);
        assert!(list[0].token.is_none());
        let list = set_account_token(list, "wss://one.example", "bob", Some("bob-secret".into()));
        let list = set_account_token(list, "wss://one.example", "alice", None);
        let list = forget_account(list, "wss://one.example", "alice");
        assert_eq!(list[0].token.as_deref(), Some("bob-secret"));
        let list = add_recent(list, b("wss://one.example", "BOB"));
        assert_eq!(list[0].token.as_deref(), Some("bob-secret"));
        assert!(!format!("{list:?}").contains("bob-secret"));
    }

    #[test]
    fn newest_recent_owns_auto_resume_even_without_a_saved_signin() {
        let mut newest = b("wss://one.example", "bob");
        let old = RecentBurrow {
            typed_login: false,
            endpoint: "wss://ONE.example:443/".into(),
            handle: "alice".into(),
            token: Some("alice-secret".into()),
        };
        assert!(secure_resumable(&[newest.clone(), old.clone()])
            .0
            .is_empty());
        newest.token = Some("bob-secret".into());
        assert_eq!(
            secure_resumable(&[newest, old]).0,
            vec![("wss://one.example/".into(), "bob-secret".into())]
        );
    }

    #[test]
    fn recent_tokens_do_not_cross_case_sensitive_routes() {
        let old = RecentBurrow {
            typed_login: false,
            endpoint: "wss://one.example/A".into(),
            handle: "alice".into(),
            token: Some("secret".into()),
        };
        let list = add_recent(vec![old], b("wss://one.example/a", "alice"));
        assert_eq!(list.len(), 2);
        assert!(list[0].token.is_none());
        assert_eq!(list[1].token.as_deref(), Some("secret"));
    }

    #[test]
    fn a_guest_annotation_cannot_inherit_an_accounts_bearer() {
        let old = RecentBurrow {
            typed_login: false,
            endpoint: "wss://one.example".into(),
            handle: "alice".into(),
            token: Some("alice-secret".into()),
        };
        let list = add_recent(vec![old], b("wss://one.example", "alice (guest)"));
        assert_eq!(list[0].handle, "alice");
        assert!(list[0].token.is_none());
    }

    #[test]
    fn typed_account_logins_keep_literal_guest_suffixes_across_storage() {
        let list = account_recent(Vec::new(), "wss://one.example", "alice (guest)");
        assert_eq!(list[0].handle, "alice (guest)");
        let json = serde_json::to_string(&list).unwrap();
        let loaded = without_guest_suffixes(serde_json::from_str(&json).unwrap());
        assert_eq!(loaded, list);
        let saved = set_account_token(
            loaded,
            "wss://one.example",
            "alice (guest)",
            Some("saved".into()),
        );
        assert_eq!(saved[0].token.as_deref(), Some("saved"));
        let list = account_recent(saved, "wss://one.example", "alice");
        assert_eq!(list.len(), 1);
        assert!(list[0].token.is_none());
        let saved = set_account_token(list, "wss://one.example", "alice", Some("saved".into()));
        assert!(account_recent(saved, "wss://one.example", "alice")[0]
            .token
            .is_none());
    }
}
