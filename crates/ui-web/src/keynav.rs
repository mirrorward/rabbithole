//! Arrow-key navigation for lists.
//!
//! Native file browsers, mail clients and BBS readers all share a contract:
//! land in a list, drive it with ↑/↓, jump with Home/End, activate with
//! Enter. The web's default — one Tab stop per row, forty presses to cross a
//! file table — is the definitive web-form experience, and it was the largest
//! interaction gap left by the native-feel audit.
//!
//! The index arithmetic is pure and host-tested here; the wasm half finds
//! rows inside their own list and recovers focus when a row disappears.
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

/// Find the nearest surviving neighbor after a focused row disappears.
/// Prefer the next row on a tie, then the previous row at the end. Compare
/// identities rather than reusing an index: several rows may disappear in
/// the same render. If every row was replaced, keep the nearest position.
pub fn recovery_index<T: PartialEq>(before: &[T], after: &[T], focused: &T) -> Option<usize> {
    if after.is_empty() {
        return None;
    }
    let current = before.iter().position(|row| row == focused)?;
    for distance in 1..before.len() {
        for candidate in [current.checked_add(distance), current.checked_sub(distance)] {
            if let Some(next) = candidate
                .and_then(|i| before.get(i))
                .and_then(|row| after.iter().position(|remaining| remaining == row))
            {
                return Some(next);
            }
        }
    }
    Some(current.min(after.len() - 1))
}

/// Bind to a list's `node_ref` alongside its keydown handler. Focus tracking
/// is independent of arrow navigation, so clicked/programmatically focused
/// rows recover too. The observer and listeners share the mounted list's
/// lifetime; disposal never focuses a replacement route or another list.
pub fn track_removal<T: leptos::html::ElementDescriptor + Clone + 'static>(
    _row_selector: &'static str,
) -> leptos::NodeRef<T> {
    let node = leptos::create_node_ref::<T>();
    #[cfg(target_arch = "wasm32")]
    leptos::create_render_effect(move |_| {
        // Synchronous setup avoids a deferred effect running after the
        // owning route has already been disposed during a burrow switch.
        if let Some(list) = node.get() {
            removal::install((*list.into_any()).clone(), _row_selector);
        }
    });
    node
}

#[cfg(target_arch = "wasm32")]
mod removal {
    use std::{cell::RefCell, rc::Rc};

    use leptos::{ev, on_cleanup, queue_microtask, window_event_listener};
    use wasm_bindgen::{closure::Closure, JsCast};
    use web_sys::{Element, HtmlElement, MutationObserver, MutationObserverInit};

    struct FocusedRow {
        focused: Element,
        row: Element,
        before: Vec<Element>,
    }

    fn rows(list: &HtmlElement, selector: &str) -> Vec<Element> {
        let Ok(nodes) = list.query_selector_all(selector) else {
            return Vec::new();
        };
        (0..nodes.length())
            .filter_map(|i| nodes.get(i)?.dyn_into::<Element>().ok())
            .collect()
    }

    fn snapshot(list: &HtmlElement, selector: &str, focused: Element) -> Option<FocusedRow> {
        if !list.contains(Some(&focused)) {
            return None;
        }
        let before = rows(list, selector);
        let row = before
            .iter()
            .find(|row| row.contains(Some(&focused)))?
            .clone();
        Some(FocusedRow {
            focused,
            row,
            before,
        })
    }

    pub(super) fn install(list: HtmlElement, selector: &'static str) {
        let Some(document) = list.owner_document() else {
            return;
        };
        let tracked = Rc::new(RefCell::new(
            document
                .active_element()
                .and_then(|active| snapshot(&list, selector, active)),
        ));
        let changed = {
            let list = list.clone();
            let document = document.clone();
            let tracked = tracked.clone();
            Closure::<dyn FnMut(js_sys::Array, MutationObserver)>::new(move |_, _| {
                // Take the state before calling focus(), whose synchronous
                // focus-in event must be free to record the replacement row.
                let Some(previous) = tracked.borrow_mut().take() else {
                    return;
                };
                if !list.is_connected() || !document.has_focus().unwrap_or(false) {
                    return;
                }
                let active = document.active_element();
                if let Some(current) = active.clone().and_then(|a| snapshot(&list, selector, a)) {
                    *tracked.borrow_mut() = Some(current);
                    return;
                }
                // A surviving focused row needs no recovery. Nor does a
                // removal after focus was deliberately handed elsewhere.
                if list.contains(Some(&previous.row))
                    || active.as_ref().is_some_and(|a| {
                        Some(a) != document.body().as_ref().map(|body| body.as_ref())
                    })
                {
                    return;
                }
                let remaining = rows(&list, selector);
                let replacement =
                    super::recovery_index(&previous.before, &remaining, &previous.row)
                        .and_then(|i| remaining[i].dyn_ref::<HtmlElement>())
                        .unwrap_or(&list);
                let _ = replacement.focus();
            })
        };
        let Ok(observer) = MutationObserver::new(changed.as_ref().unchecked_ref()) else {
            return;
        };
        let options = MutationObserverInit::new();
        options.set_child_list(true);
        options.set_subtree(true);
        if observer.observe_with_options(&list, &options).is_err() {
            return;
        }
        let focus_in = {
            let list = list.clone();
            let tracked = tracked.clone();
            window_event_listener(ev::focusin, move |event| {
                *tracked.borrow_mut() = event
                    .target()
                    .and_then(|target| target.dyn_into::<Element>().ok())
                    .and_then(|target| snapshot(&list, selector, target));
            })
        };
        let focus_out = {
            let tracked = tracked.clone();
            window_event_listener(ev::focusout, move |event| {
                let Some(target) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
                    return;
                };
                if event.related_target().is_some() {
                    tracked.borrow_mut().take();
                    return;
                }
                // Browsers differ on firing blur during removal. Check at
                // the end of the render: explicit blur of a surviving row
                // relinquishes focus, while detachment retains its snapshot.
                let tracked = tracked.clone();
                queue_microtask(move || {
                    let mut state = tracked.borrow_mut();
                    if state
                        .as_ref()
                        .is_some_and(|s| s.focused == target && s.row.is_connected())
                    {
                        state.take();
                    }
                });
            })
        };
        on_cleanup(move || {
            observer.disconnect();
            focus_in.remove();
            focus_out.remove();
            tracked.borrow_mut().take();
            drop(changed);
        });
    }
}

/// A descendant with its own focus (an input, select, or secondary action)
/// is distinct from entering the list through its single tab stop.
#[derive(Clone, Copy)]
#[cfg(any(target_arch = "wasm32", test))]
enum ListFocus {
    Container,
    Row(usize),
    Other,
}

#[cfg(any(target_arch = "wasm32", test))]
fn next_for_focus(focus: ListFocus, len: usize, key: &str) -> Option<usize> {
    match focus {
        ListFocus::Container => next_index(None, len, key),
        ListFocus::Row(index) => next_index(Some(index), len, key),
        ListFocus::Other => None,
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
/// Only the list and its rows own navigation keys. Inputs and other separately
/// focused descendants retain arrows, Home, and End for their own editing.
#[cfg(target_arch = "wasm32")]
pub fn handle(ev: &leptos::ev::KeyboardEvent, row_selector: &str) {
    use wasm_bindgen::JsCast;

    // Modified keys are never ours: ⌘↓ is "end of document", ⌥/⇧-arrows are
    // selection and word movement. Swallowing those turns system-wide muscle
    // memory into single-row moves (WAI-ARIA APG: pass modified keys through).
    if ev.default_prevented()
        || ev.is_composing()
        || ev.alt_key()
        || ev.ctrl_key()
        || ev.meta_key()
        || ev.shift_key()
    {
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
    // A contenteditable row can match row_selector too. Its caret, along
    // with native form controls, takes precedence over list navigation.
    if active.as_ref().is_some_and(|element| {
        matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
            || element
                .dyn_ref::<web_sys::HtmlElement>()
                .is_some_and(|element| element.is_content_editable())
    }) {
        return;
    }
    let current = active.as_ref().and_then(|a| {
        (0..rows.length())
            .find(|i| rows.get(*i).as_deref() == Some(a.as_ref()))
            .map(|i| i as usize)
    });
    let focus = match current {
        Some(index) => ListFocus::Row(index),
        None if active.as_ref() == Some(&list) => ListFocus::Container,
        None => ListFocus::Other,
    };
    let Some(next) = next_for_focus(focus, len, &key) else {
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
    fn removal_prefers_the_nearest_survivor_and_empty_lists_have_no_row() {
        let before = ["a", "b", "c", "d", "e"];
        assert_eq!(
            recovery_index(&before, &["a", "b", "d", "e"], &"c"),
            Some(2)
        );
        assert_eq!(
            recovery_index(&before, &["a", "b", "c", "d"], &"e"),
            Some(3)
        );
        assert_eq!(
            recovery_index(&before, &["b", "c", "d", "e"], &"a"),
            Some(0)
        );
        assert_eq!(recovery_index(&before, &[], &"c"), None);
        // An earlier removal changes positions without changing neighbors.
        assert_eq!(recovery_index(&before, &["b", "d", "e"], &"c"), Some(1));
        assert_eq!(recovery_index(&before, &["b", "e"], &"c"), Some(0));
        // A keyed render may replace every element (for example on rename).
        assert_eq!(recovery_index(&before, &["x", "y", "z"], &"b"), Some(1));
        assert_eq!(recovery_index(&before, &["x"], &"e"), Some(0));
    }

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

    #[test]
    fn independently_focused_descendants_keep_all_editing_keys() {
        // A nested input/select/secondary button is not the list's tab stop:
        // Home/End must keep moving its caret, and arrows keep its own action.
        for key in ["ArrowDown", "ArrowUp", "Home", "End", "a", "Enter"] {
            assert_eq!(next_for_focus(ListFocus::Other, 5, key), None, "{key}");
        }
        // The same keys still drive the list when its own container or a row
        // has focus, so protecting descendants does not disable navigation.
        assert_eq!(next_for_focus(ListFocus::Container, 5, "Home"), Some(0));
        assert_eq!(next_for_focus(ListFocus::Container, 5, "End"), Some(4));
        assert_eq!(next_for_focus(ListFocus::Row(2), 5, "ArrowDown"), Some(3));
        assert_eq!(next_for_focus(ListFocus::Row(2), 5, "Home"), Some(0));
    }
}
