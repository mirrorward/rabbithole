//! Bookmarks: the burrows you chose to keep.
//!
//! Hotline kept a bookmarks list beside the tracker, and it mattered for the
//! same reason here: a tracker only lists who is announcing *right now*. A
//! burrow that drops off the Looking Glass, a friend's machine that never
//! announced, a burrow on your own network: a bookmark keeps the way back.
//!
//! The list logic is a pure, host-tested reducer; load/save is wasm-only
//! (`localStorage`), the same split [`crate::recent`] uses. A bookmark holds
//! an address and the name you know it by. Never a password, never a token.

use serde::{Deserialize, Serialize};

/// One kept burrow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub endpoint: String,
    /// What you call it. Starts as the directory's name or the address, and
    /// is yours to change: a listing can rename itself, your bookmark can't.
    pub name: String,
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
                "An address can\u{2019}t contain spaces. Check it and try again."
            }
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
        endpoint: endpoint.to_string(),
        name: if name.is_empty() {
            default_name(endpoint)
        } else {
            name.to_string()
        },
    });
    Ok(list)
}

/// Remove a bookmark by place. Pure.
pub fn remove(mut list: Vec<Bookmark>, endpoint: &str) -> Vec<Bookmark> {
    list.retain(|b| !crate::connect::same_place(&b.endpoint, endpoint));
    list
}

/// Rename a bookmark. A blank name falls back to the address rather than
/// leaving a row with nothing to read. Pure.
pub fn rename(mut list: Vec<Bookmark>, endpoint: &str, name: &str) -> Vec<Bookmark> {
    if let Some(b) = list
        .iter_mut()
        .find(|b| crate::connect::same_place(&b.endpoint, endpoint))
    {
        let name = name.trim();
        b.name = if name.is_empty() {
            default_name(&b.endpoint)
        } else {
            name.to_string()
        };
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
            .unwrap_or_default()
    }

    pub fn save(list: &[Bookmark]) {
        if let (Some(s), Ok(json)) = (storage(), serde_json::to_string(list)) {
            let _ = s.set_item(KEY, &json);
        }
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
        let list = rename(list, "ws://192.168.1.20:4654", "Attic server");
        assert_eq!(list[0].name, "Attic server");
        let list = rename(list, "ws://192.168.1.20:4654", "");
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
        let list = remove(list, "ws://B3.example/");
        assert_eq!(list.len(), MAX_BOOKMARKS - 1);
        assert!(!is_bookmarked(&list, "wss://b3.example"));
    }
}
