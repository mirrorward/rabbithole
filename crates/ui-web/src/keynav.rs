//! Arrow-key navigation for lists.
//!
//! Native file browsers, mail clients and BBS readers all share a contract:
//! land in a list, drive it with ↑/↓, jump with Home/End, activate with
//! Enter. The web's default — one Tab stop per row, forty presses to cross a
//! file table — is the definitive web-form experience, and it was the largest
//! interaction gap left by the native-feel audit.
//!
//! The index arithmetic is pure and host-tested here; the wasm half only
//! finds the rows inside the event's own list container and moves focus.
//! Rows are real `<button>`/`<a>` elements, so Enter/Space activation and
//! scroll-into-view-on-focus come from the platform for free.

/// Where focus should move within a `len`-row list when `key` is pressed at
/// `current` (`None` = focus is on the list but not on any row yet).
///
/// The edges *clamp* rather than wrap: holding ↓ in Finder parks you on the
/// last row, it doesn't teleport you to the top. Entering the list fresh, ↓
/// starts at the first row and ↑ at the last — both directions "come from
/// outside" the list.
pub fn next_index(current: Option<usize>, len: usize, key: &str) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    match key {
        "ArrowDown" => Some(match current {
            Some(i) => (i + 1).min(last),
            None => 0,
        }),
        "ArrowUp" => Some(match current {
            Some(i) => i.saturating_sub(1),
            None => last,
        }),
        "Home" => Some(0),
        "End" => Some(last),
        _ => None,
    }
}

/// Handle a keydown that happened inside a list: move focus to the row
/// `next_index` picks among the elements matching `row_selector` under the
/// event's own `currentTarget`.
///
/// Scoped to `currentTarget` — the list the handler is attached to — so two
/// lists on screen (boards and threads, say) never steal each other's arrows.
/// Call sites must bind with `on:keydown:undelegated`: Leptos delegates plain
/// `on:` events to the document root, and a delegated listener sees the root
/// as `currentTarget`, which silently breaks the container scoping.
///
/// The tabindex handshake: the **list container carries `tabindex="0"`** (one
/// Tab stop; also what makes the list reachable at all in WKWebView, where
/// bare buttons aren't Tab-focusable unless Full Keyboard Access is on) and
/// **rows carry `tabindex="-1"`** (skipped by Tab, focusable by arrows). Tab
/// lands on the list, ↓ enters at the first row, Tab again leaves past it —
/// instead of one stop per row, forty presses to cross a file table.
/// Non-navigation keys fall through untouched; typing in an input inside the
/// list is unaffected because those keys are never claimed.
#[cfg(target_arch = "wasm32")]
pub fn handle(ev: &leptos::ev::KeyboardEvent, row_selector: &str) {
    use wasm_bindgen::JsCast;

    // Modified keys are never ours: ⌘↓ is "end of document", ⌥/⇧-arrows are
    // selection and word movement. Swallowing those turns system-wide muscle
    // memory into single-row moves (WAI-ARIA APG: pass modified keys through).
    if ev.alt_key() || ev.ctrl_key() || ev.meta_key() || ev.shift_key() {
        return;
    }
    let key = ev.key();
    if !matches!(key.as_str(), "ArrowDown" | "ArrowUp" | "Home" | "End") {
        return;
    }
    let Some(list) = ev
        .current_target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let Ok(rows) = list.query_selector_all(row_selector) else {
        return;
    };
    let len = rows.length() as usize;
    let active = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element());
    let current = active.as_ref().and_then(|a| {
        (0..rows.length())
            .find(|i| rows.get(*i).as_deref() == Some(a.as_ref()))
            .map(|i| i as usize)
    });
    let Some(next) = next_index(current, len, &key) else {
        return;
    };
    if let Some(row) = rows
        .get(next as u32)
        .and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok())
    {
        // Consume the key even at a clamped edge (next == current): a native
        // list pins you at its boundary rather than handing ↓ to the scroll
        // pane — hold ↓ in Finder's last row and nothing scrolls. Only a key
        // that found no row at all falls through.
        ev.prevent_default();
        if Some(row.as_ref() as &web_sys::Element) != active.as_ref() {
            let _ = row.focus();
        }
    }
}

/// Host stand-in: components compile (and are testable) without a DOM; the
/// index arithmetic above is the part with behaviour, and it's host-tested.
#[cfg(not(target_arch = "wasm32"))]
pub fn handle(_ev: &leptos::ev::KeyboardEvent, _row_selector: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_walk_the_list_and_clamp_at_the_edges() {
        // Finder's contract: holding ↓ parks on the last row, no wrap.
        assert_eq!(next_index(Some(0), 5, "ArrowDown"), Some(1));
        assert_eq!(
            next_index(Some(4), 5, "ArrowDown"),
            Some(4),
            "clamps, never wraps"
        );
        assert_eq!(next_index(Some(4), 5, "ArrowUp"), Some(3));
        assert_eq!(
            next_index(Some(0), 5, "ArrowUp"),
            Some(0),
            "clamps at the top"
        );
    }

    #[test]
    fn entering_the_list_starts_at_the_near_edge() {
        // ↓ from outside lands on the first row; ↑ on the last — each arrow
        // "enters" from the direction it travels.
        assert_eq!(next_index(None, 5, "ArrowDown"), Some(0));
        assert_eq!(next_index(None, 5, "ArrowUp"), Some(4));
    }

    #[test]
    fn home_and_end_jump() {
        assert_eq!(next_index(Some(2), 9, "Home"), Some(0));
        assert_eq!(next_index(Some(2), 9, "End"), Some(8));
        assert_eq!(next_index(None, 9, "End"), Some(8));
    }

    #[test]
    fn other_keys_and_empty_lists_are_never_claimed() {
        // Typing, Tab, Enter: not ours. An empty list: nothing to focus, so
        // the key must fall through rather than being swallowed.
        for key in ["a", "Tab", "Enter", " ", "PageDown"] {
            assert_eq!(next_index(Some(1), 5, key), None, "{key}");
        }
        for key in ["ArrowDown", "ArrowUp", "Home", "End"] {
            assert_eq!(next_index(None, 0, key), None, "{key} on empty");
        }
    }
}
