//! Bookmarks: the burrows you chose to keep.
//!
//! Hotline kept a bookmarks list beside the tracker, and it mattered for the
//! same reason here: a tracker only lists who is announcing *right now*. A
//! burrow that drops off the Looking Glass, a friend's machine that never
//! announced, a burrow on your own network: a bookmark keeps the way back.
//!
//! The list logic is a pure, host-tested reducer; load/save is wasm-only
//! (`localStorage`), the same split [`crate::recent`] uses. A bookmark holds
//! an address and the name you know it by. An account bookmark can opt into
//! keeping a server-issued resume token. Passwords are never persisted.

use serde::{Deserialize, Serialize};

/// One kept burrow.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// Stable identity, independent of the label, endpoint and signed-in user.
    #[serde(default)]
    pub id: String,
    pub endpoint: String,
    /// What you call it. Starts as the directory's name or the address, and
    /// is yours to change: a listing can rename itself, your bookmark can't.
    pub name: String,
    /// Account login typed at sign-in, not the active persona's screen name.
    #[serde(default)]
    pub login: Option<String>,
    /// Only a token acknowledged by successful authentication, never a password.
    #[serde(default)]
    pub token: Option<String>,
}

impl std::fmt::Debug for Bookmark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bookmark")
            .field("id", &self.id)
            .field("endpoint", &self.endpoint)
            .field("name", &self.name)
            .field("login", &self.login)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Canonical credential identity. Unlike the display-only `same_place`, this
/// preserves scheme, case-sensitive paths and queries, and refuses remote WS.
pub fn credential_endpoint(endpoint: &str) -> Option<String> {
    let endpoint = endpoint.trim();
    if endpoint.chars().any(char::is_whitespace) {
        return None;
    }
    // URL parsing canonicalizes scheme/host casing and default ports. The core
    // transport policy still decides whether this is safe to send credentials.
    let candidate = if endpoint.contains("://") {
        url::Url::parse(endpoint).ok()?.to_string()
    } else {
        rabbithole_core::api::normalize_secure_ws_endpoint(endpoint).ok()?
    };
    let endpoint = rabbithole_core::api::normalize_secure_ws_endpoint(&candidate).ok()?;
    let parsed = url::Url::parse(&endpoint).ok()?;
    if parsed.fragment().is_some() {
        return None;
    }
    Some(parsed.to_string())
}

pub fn same_account(endpoint: &str, login: &str, other_endpoint: &str, other_login: &str) -> bool {
    let Some(endpoint) = credential_endpoint(endpoint) else {
        return false;
    };
    !login.trim().is_empty()
        && login.trim().eq_ignore_ascii_case(other_login.trim())
        && credential_endpoint(other_endpoint).as_ref() == Some(&endpoint)
}

pub fn can_save(endpoint: &str, login: &str) -> bool {
    credential_endpoint(endpoint).is_some() && !login.trim().is_empty()
}

fn identified(seed: &str) -> String {
    blake3::hash(seed.as_bytes()).to_hex().to_string()
}

/// Old address-only entries have no ID. Derive one from their original data
/// without consuming a token or making migration depend on the current time.
pub fn migrate(mut list: Vec<Bookmark>) -> Vec<Bookmark> {
    let mut used = std::collections::HashSet::new();
    list.truncate(MAX_BOOKMARKS);
    for bookmark in &mut list {
        bookmark.login = bookmark.login.take().and_then(|login| {
            let login = login.trim();
            (!login.is_empty()).then(|| login.to_string())
        });
        if bookmark.login.is_none() || credential_endpoint(&bookmark.endpoint).is_none() {
            bookmark.token = None;
        }
        if bookmark
            .token
            .as_ref()
            .is_some_and(|token| token.is_empty())
        {
            bookmark.token = None;
        }
        if bookmark.id.is_empty() || !used.insert(bookmark.id.clone()) {
            let seed = format!(
                "bookmark-v1\0{}\0{}",
                bookmark.endpoint,
                bookmark.login.as_deref().unwrap_or_default()
            );
            let mut collision = 0_u64;
            loop {
                let id = identified(&format!("{seed}\0{collision}"));
                if used.insert(id.clone()) {
                    bookmark.id = id;
                    break;
                }
                collision += 1;
            }
        }
    }
    list
}

fn new_id() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let mut random = [0_u8; 32];
        if getrandom::getrandom(&mut random).is_ok() {
            return hex::encode(random);
        }
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_arch = "wasm32")]
    let instant = js_sys::Date::now().to_bits();
    #[cfg(not(target_arch = "wasm32"))]
    let instant = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    identified(&format!("bookmark-new\0{instant}\0{serial}"))
}

pub fn by_id<'a>(list: &'a [Bookmark], id: &str) -> Option<&'a Bookmark> {
    list.iter().find(|bookmark| bookmark.id == id)
}

pub fn find_account<'a>(list: &'a [Bookmark], endpoint: &str, login: &str) -> Option<&'a Bookmark> {
    list.iter().find(|bookmark| {
        bookmark
            .login
            .as_deref()
            .is_some_and(|saved| same_account(&bookmark.endpoint, saved, endpoint, login))
    })
}

/// Plenty for a person's warren, few enough that the shelf stays a shelf.
pub const MAX_BOOKMARKS: usize = 64;

/// Why a bookmark could not be added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// Nothing to bookmark.
    NoAddress,
    /// An address cannot contain whitespace; this is a typo, not a burrow.
    BadAddress,
    NoLogin,
    NoToken,
    /// Already on the shelf.
    Duplicate,
    /// The shelf is full.
    Full,
}

impl AddError {
    /// Says what is wrong and what to do about it.
    pub fn message(self) -> &'static str {
        match self {
            AddError::NoAddress => "Type the burrow\u{2019}s address first.",
            AddError::BadAddress => {
                "Check the address. Saved sign-ins need WSS, except on localhost."
            }
            AddError::NoLogin => "Enter the account login before saving a sign-in.",
            AddError::NoToken => "Sign in successfully before saving this account.",
            AddError::Duplicate => "That burrow is already bookmarked.",
            AddError::Full => "The bookmark shelf is full. Remove one first.",
        }
    }
}

/// The name a bookmark gets when none is given: the address a person would
/// say out loud.
fn default_name(endpoint: &str) -> String {
    crate::connect::host(endpoint)
}

/// Add a bookmark. Deduplicated by *place* (scheme, case and a trailing slash
/// aside), appended so the shelf keeps the order you built it in. Pure.
pub fn add(mut list: Vec<Bookmark>, endpoint: &str, name: &str) -> Result<Vec<Bookmark>, AddError> {
    list = migrate(list);
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(AddError::NoAddress);
    }
    if endpoint.chars().any(char::is_whitespace) || crate::connect::host(endpoint).is_empty() {
        return Err(AddError::BadAddress);
    }
    if is_bookmarked(&list, endpoint) {
        return Err(AddError::Duplicate);
    }
    if list.len() >= MAX_BOOKMARKS {
        return Err(AddError::Full);
    }
    let name = name.trim();
    list.push(Bookmark {
        id: new_id(),
        endpoint: endpoint.to_string(),
        name: if name.is_empty() {
            default_name(endpoint)
        } else {
            name.to_string()
        },
        login: None,
        token: None,
    });
    Ok(list)
}

/// Save a successfully authenticated account, preserving an existing bookmark
/// identity and custom name. An address-only bookmark is promoted in place.
pub fn upsert_account(
    list: Vec<Bookmark>,
    endpoint: &str,
    login: &str,
    name: &str,
    token: &str,
) -> Result<(Vec<Bookmark>, String), AddError> {
    let endpoint = credential_endpoint(endpoint).ok_or(AddError::BadAddress)?;
    let login = login.trim();
    if login.is_empty() {
        return Err(AddError::NoLogin);
    }
    if token.trim().is_empty() {
        return Err(AddError::NoToken);
    }
    let mut list = migrate(list);
    let existing = list
        .iter()
        .position(|bookmark| {
            bookmark
                .login
                .as_deref()
                .is_some_and(|saved| same_account(&bookmark.endpoint, saved, &endpoint, login))
        })
        .or_else(|| {
            list.iter().position(|bookmark| {
                bookmark.login.is_none()
                    && credential_endpoint(&bookmark.endpoint).as_ref() == Some(&endpoint)
            })
        });
    let is_new = existing.is_none();
    let index = match existing {
        Some(index) => index,
        None => {
            if list.len() >= MAX_BOOKMARKS {
                return Err(AddError::Full);
            }
            list.push(Bookmark {
                id: new_id(),
                endpoint: endpoint.clone(),
                name: default_name(&endpoint),
                login: None,
                token: None,
            });
            list.len() - 1
        }
    };
    let bookmark = &mut list[index];
    bookmark.endpoint = endpoint;
    bookmark.login = Some(login.to_string());
    bookmark.token = Some(token.to_string());
    if is_new && !name.trim().is_empty() {
        bookmark.name = name.trim().to_string();
    }
    let id = bookmark.id.clone();
    Ok((list, id))
}

/// Remove only the selected bookmark and its saved sign-in.
pub fn remove(mut list: Vec<Bookmark>, id: &str) -> Vec<Bookmark> {
    list.retain(|b| b.id != id);
    list
}

/// Rename a bookmark. A blank name falls back to the address rather than
/// leaving a row with nothing to read. Pure.
pub fn rename(mut list: Vec<Bookmark>, id: &str, name: &str) -> Vec<Bookmark> {
    if let Some(b) = list.iter_mut().find(|b| b.id == id) {
        let name = name.trim();
        b.name = if name.is_empty() {
            default_name(&b.endpoint)
        } else {
            name.to_string()
        };
    }
    list
}

pub fn clear_token(mut list: Vec<Bookmark>, id: &str) -> Vec<Bookmark> {
    if let Some(bookmark) = list.iter_mut().find(|b| b.id == id) {
        bookmark.token = None;
    }
    list
}

/// Is this place on the shelf?
pub fn is_bookmarked(list: &[Bookmark], endpoint: &str) -> bool {
    list.iter()
        .any(|b| crate::connect::same_place(&b.endpoint, endpoint))
}

#[cfg(target_arch = "wasm32")]
mod persist {
    use super::Bookmark;

    const KEY: &str = "rh.bookmarks.v1";

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    /// The shelf, in the order it was built (empty if none / unreadable).
    pub fn load() -> Vec<Bookmark> {
        storage()
            .and_then(|s| s.get_item(KEY).ok().flatten())
            .and_then(|json| serde_json::from_str(&json).ok())
            .map(super::migrate)
            .unwrap_or_default()
    }

    pub fn save(list: &[Bookmark]) -> Result<(), String> {
        let s = storage().ok_or_else(|| {
            "This browser could not save your bookmarks. Check its storage settings.".to_string()
        })?;
        let json = serde_json::to_string(list)
            .map_err(|_| "Your bookmark could not be saved.".to_string())?;
        s.set_item(KEY, &json).map_err(|_| {
            "This browser could not save your bookmarks. Free some storage and try again."
                .to_string()
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use persist::{load, save};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bookmark_is_one_place_however_it_is_spelled() {
        let list = add(Vec::new(), " wss://Warren.example/ ", "The Warren").unwrap();
        assert_eq!(list[0].endpoint, "wss://Warren.example/");
        assert_eq!(list[0].name, "The Warren");
        assert_eq!(
            add(list.clone(), "ws://warren.example", ""),
            Err(AddError::Duplicate),
            "scheme, case and slash aside"
        );
        assert!(is_bookmarked(&list, "WSS://WARREN.EXAMPLE"));
        assert!(!is_bookmarked(&list, "wss://other.example"));
    }

    #[test]
    fn a_custom_bookmark_with_no_name_is_called_by_its_address() {
        let list = add(Vec::new(), "ws://192.168.1.20:4654", "  ").unwrap();
        assert_eq!(list[0].name, "192.168.1.20:4654");
        // And renaming to nothing goes back to that, never to a blank row.
        let id = list[0].id.clone();
        let list = rename(list, &id, "Attic server");
        assert_eq!(list[0].name, "Attic server");
        let list = rename(list, &id, "");
        assert_eq!(list[0].name, "192.168.1.20:4654");
    }

    #[test]
    fn typos_are_refused_with_something_to_do_about_them() {
        assert_eq!(add(Vec::new(), "   ", "x"), Err(AddError::NoAddress));
        assert_eq!(
            add(Vec::new(), "warren example", "x"),
            Err(AddError::BadAddress)
        );
        assert_eq!(add(Vec::new(), "wss://", "x"), Err(AddError::BadAddress));
        for e in [
            AddError::NoAddress,
            AddError::BadAddress,
            AddError::NoLogin,
            AddError::NoToken,
            AddError::Duplicate,
            AddError::Full,
        ] {
            assert!(e.message().ends_with('.'), "{e:?} is a sentence");
        }
    }

    #[test]
    fn the_shelf_keeps_its_order_and_has_an_end() {
        let mut list = Vec::new();
        for i in 0..MAX_BOOKMARKS {
            list = add(list, &format!("wss://b{i}.example"), "").unwrap();
        }
        assert_eq!(list[0].name, "b0.example");
        assert_eq!(
            list[MAX_BOOKMARKS - 1].name,
            format!("b{}.example", MAX_BOOKMARKS - 1)
        );
        assert_eq!(
            add(list.clone(), "wss://one-more.example", ""),
            Err(AddError::Full)
        );
        let id = list[3].id.clone();
        let list = remove(list, &id);
        assert_eq!(list.len(), MAX_BOOKMARKS - 1);
        assert!(!is_bookmarked(&list, "wss://b3.example"));
    }

    #[test]
    fn legacy_migration_is_deterministic_and_preserves_labels_and_order() {
        let json = r#"[{"endpoint":"wss://one.example","name":"My place"},{"endpoint":"ws://localhost:4654","name":"Local"}]"#;
        let old: Vec<Bookmark> = serde_json::from_str(json).unwrap();
        let migrated = migrate(old.clone());
        assert_eq!(migrated, migrate(old));
        assert_eq!(migrated, migrate(migrated.clone()));
        assert!(!migrated[0].id.is_empty());
        assert_ne!(migrated[0].id, migrated[1].id);
        assert_eq!(migrated[0].name, "My place");
        assert_eq!(migrated[1].endpoint, "ws://localhost:4654");
        assert!(migrated
            .iter()
            .all(|b| b.login.is_none() && b.token.is_none()));
        let round_trip = serde_json::to_string(&migrated).unwrap();
        assert_eq!(
            migrate(serde_json::from_str(&round_trip).unwrap()),
            migrated
        );
    }

    #[test]
    fn accounts_at_one_burrow_have_independent_ids_tokens_and_actions() {
        let (list, alice) = upsert_account(
            Vec::new(),
            "wss://BURROW.example:443",
            "Alice",
            "Work",
            "alice-secret",
        )
        .unwrap();
        let (list, bob) =
            upsert_account(list, "wss://burrow.example/", "bob", "Play", "bob-secret").unwrap();
        assert_ne!(alice, bob);
        assert_eq!(list.len(), 2);
        assert_eq!(
            find_account(&list, "burrow.example", "ALICE").unwrap().id,
            alice
        );
        let list = rename(list, &alice, "My account");
        let list = clear_token(list, &alice);
        assert_eq!(by_id(&list, &alice).unwrap().name, "My account");
        assert_eq!(
            by_id(&list, &alice).unwrap().login.as_deref(),
            Some("Alice")
        );
        assert!(by_id(&list, &alice).unwrap().token.is_none());
        assert_eq!(
            by_id(&list, &bob).unwrap().token.as_deref(),
            Some("bob-secret")
        );
        let (list, refreshed) =
            upsert_account(list, "wss://burrow.example", "alice", "", "fresh-secret").unwrap();
        assert_eq!(refreshed, alice);
        assert_eq!(by_id(&list, &alice).unwrap().name, "My account");
        let list = remove(list, &alice);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, bob);
        assert_eq!(list[0].token.as_deref(), Some("bob-secret"));
        assert!(!format!("{list:?}").contains("bob-secret"));
    }

    #[test]
    fn saving_promotes_a_legacy_bookmark_without_changing_its_identity_or_name() {
        let list = add(Vec::new(), "wss://burrow.example", "The old name").unwrap();
        let id = list[0].id.clone();
        let (list, saved) = upsert_account(
            list,
            "wss://burrow.example/",
            "alice",
            "Server-reported name",
            "token",
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(id, saved);
        assert_eq!(list[0].name, "The old name");
        assert_eq!(list[0].login.as_deref(), Some("alice"));
    }

    #[test]
    fn credential_matching_preserves_transport_path_case_and_query() {
        assert!(same_account(
            "WSS://EXAMPLE.COM:443/Users?a=B",
            " Alice ",
            "wss://example.com/Users?a=B",
            "alice"
        ));
        for other in [
            "wss://example.com/users?a=B",
            "wss://example.com/Users?a=b",
            "wss://example.com:444/Users?a=B",
            "ws://example.com/Users?a=B",
        ] {
            assert!(
                !same_account("wss://example.com/Users?a=B", "alice", other, "alice"),
                "{other}"
            );
        }
        assert!(!same_account(
            "ws://localhost:4654",
            "alice",
            "wss://localhost:4654",
            "alice"
        ));
        for bad in [
            "ws://remote.example",
            "https://remote.example",
            "wss://user:pass@example.com",
            "wss://example.com/#fragment",
            "wss://bad host",
        ] {
            assert!(!can_save(bad, "alice"), "{bad}");
        }
        assert!(!can_save("wss://example.com", "  "));
    }

    #[test]
    fn a_full_shelf_can_refresh_an_existing_account_but_cannot_add_another() {
        let (mut list, id) =
            upsert_account(Vec::new(), "wss://one.example", "alice", "", "first").unwrap();
        for i in 1..MAX_BOOKMARKS {
            list = add(list, &format!("wss://b{i}.example"), "").unwrap();
        }
        assert_eq!(
            upsert_account(list.clone(), "wss://one.example", "bob", "", "bob"),
            Err(AddError::Full)
        );
        let (updated, kept) =
            upsert_account(list, "wss://one.example/", "ALICE", "", "renewed").unwrap();
        assert_eq!(kept, id);
        assert_eq!(updated.len(), MAX_BOOKMARKS);
        assert_eq!(
            by_id(&updated, &id).unwrap().token.as_deref(),
            Some("renewed")
        );
        assert_eq!(
            upsert_account(updated, "wss://one.example", "alice", "", ""),
            Err(AddError::NoToken)
        );
    }

    #[test]
    fn migration_repairs_duplicate_ids_and_drops_unscoped_or_insecure_bearers() {
        let rows = vec![
            Bookmark {
                id: "duplicate".into(),
                endpoint: "wss://one.example".into(),
                token: Some("orphan".into()),
                ..Bookmark::default()
            },
            Bookmark {
                id: "duplicate".into(),
                endpoint: "ws://remote.example".into(),
                login: Some("alice".into()),
                token: Some("insecure".into()),
                ..Bookmark::default()
            },
        ];
        let migrated = migrate(rows);
        assert_ne!(migrated[0].id, migrated[1].id);
        assert!(migrated.iter().all(|b| b.token.is_none()));
        assert_eq!(migrate(migrated.clone()), migrated);
    }
}
