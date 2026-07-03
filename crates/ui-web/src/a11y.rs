//! Accessibility support: shared ids, focus management, and the audit
//! checklist for the web SPA.
//!
//! Everything testable about accessibility in this crate is kept out of the
//! DOM: the id vocabulary and label/input pairing helpers below are pure and
//! host-tested, the stylesheet's focus/motion blocks are asserted textually
//! in [`crate::theme_css`], and the focus-outline colour token is checked
//! against every pack with the same WCAG contrast math the theme editor uses
//! ([`crate::theme_editor::contrast_ratio`]). Only the two `focus_*` calls
//! below touch the DOM, and they are wasm-gated no-op-on-host edges in the
//! same style as [`crate::theme_css::storage`].
//!
//! # What the a11y pass covers (host-verified)
//!
//! - **Landmarks & structure** ([`crate::components`]): one `<header>`
//!   (status bar) per signed-in view, one `<main id="rh-main">` per route,
//!   `<nav aria-label="Primary">` for the section links, `<aside>` for the
//!   who/conversation rails. Exactly one `<h1 id="rh-view-title">` per view
//!   (visible where the design has a title, visually hidden otherwise) with
//!   headings descending without skips beneath it.
//! - **Names & labels**: every form control has a programmatic name — real
//!   `<label for=…>` pairs on the login, server-config, poll-interval, and
//!   theme-editor token inputs (ids minted by [`config_input_id`] /
//!   [`token_input_id`] / [`LOGIN_SERVER_ID`] / [`LOGIN_HANDLE_ID`]),
//!   `aria-label` on the compose/search/moderation inputs whose placeholder
//!   was previously the only hint, and on the volume slider and the
//!   import/export textareas. Icon/status glyphs (presence dots, file-kind
//!   emoji, colour swatches) are `aria-hidden` with a visually-hidden text
//!   equivalent wherever the glyph was the only signal.
//! - **Live regions**: the chat and DM scrollbacks are `role="log"`
//!   (implicitly polite) so new messages announce without stealing focus;
//!   the status-bar connection label, status line, and now-playing segment
//!   are `role="status"`; admin panel status lines likewise; validation
//!   failures (theme-editor errors, poll-interval errors) are
//!   `role="alert"`. Transfer bars are `role="progressbar"` with
//!   `aria-valuenow`.
//! - **Tabular data**: the admin matrices (accounts, classes, gateway
//!   matrix, feeds) are real `<table>`s with `<th scope="col">` headers; the
//!   file-metadata card keeps its `<dl>`.
//! - **Keyboard**: a skip link (`.rh-skip`, first focusable, `rel="external"`
//!   so the router lets the browser do the in-page jump to [`SKIP_HREF`])
//!   precedes the header; global `:focus-visible` outlines use the
//!   `--rh-focus` token (≥ 3:1 against both `--rh-bg` and `--rh-surface` in
//!   every pack × mode — see the packs test); no positive `tabindex`
//!   anywhere (only `-1` on programmatic focus targets); every interactive
//!   element is a native `<button>`, `<a>`, or form control; the login is a
//!   real `<form>` so Enter submits. Selected items in selection lists
//!   (threads, DM peers, files, stations) carry `aria-current`; the router's
//!   `<A>` already stamps `aria-current="page"` on the active nav link and
//!   the stylesheet styles that attribute directly. There are **no modal
//!   dialogs or overlays in the app today** (inventoried: every surface is a
//!   routed page or an inline panel), so no Escape-to-close handling is
//!   needed yet — add `role="dialog"` + focus-trap + Escape when the first
//!   overlay lands.
//! - **Focus management**: on route change the app focuses the new view's
//!   `<h1>` ([`focus_view_title`], falling back to `<main>`), so screen
//!   readers and keyboard users land at the top of the new context instead
//!   of being stranded. Committing a theme-editor token edit re-creates the
//!   row (rows re-key on value), so the editor calls [`focus_id`] to put
//!   focus back on the same input; send/apply/save actions never unmount the
//!   control that triggered them.
//! - **Reduced motion**: the stylesheet's `prefers-reduced-motion: reduce`
//!   block neutralises every transition/animation (the app's only motion
//!   today is the transfer-bar width transition, but the block is written as
//!   a blanket rule so future motion is covered by default).
//!
//! # What still needs a real browser (manual / Playwright later)
//!
//! These cannot be asserted from host tests because the crate has no
//! DOM-rendering path off-wasm (Leptos CSR only; no `ssr` feature):
//!
//! - Tab-order walk of every view; skip-link jump lands on `<main>`.
//! - Route-change focus actually moves (`focus_view_title` timing vs.
//!   Leptos render) and the theme-editor re-focus after a keyboard
//!   (Enter-key) commit.
//! - Screen-reader announcement of the `role="log"` scrollbacks and
//!   `role="status"` regions (NVDA/VoiceOver behaviour differs).
//! - `prefers-reduced-motion` and `prefers-color-scheme` end-to-end.
//! - Zoom/reflow at 200% and 400%, and touch-target sizes.
//! - Canvas art (`role="img"`) alternative-text quality per artwork.
//! - axe-core (or similar) scan for anything this checklist missed.

/// The id every routed view puts on its `<main>` element — the skip link's
/// target and the route-change focus fallback.
pub const MAIN_ID: &str = "rh-main";

/// The id every routed view puts on its single `<h1>` — the primary
/// route-change focus target.
pub const VIEW_TITLE_ID: &str = "rh-view-title";

/// The skip link's `href`: an in-page fragment jump to [`MAIN_ID`].
pub const SKIP_HREF: &str = "#rh-main";

/// The class that visually hides an element while keeping it readable by
/// assistive technology. Defined in [`crate::theme_css::STYLESHEET`].
pub const SR_ONLY: &str = "rh-visually-hidden";

/// The login form's server-endpoint input id (paired with its `<label>`).
pub const LOGIN_SERVER_ID: &str = "rh-login-server";

/// The login form's handle input id (paired with its `<label>`).
pub const LOGIN_HANDLE_ID: &str = "rh-login-handle";

/// Reduce a config key / token name to id-safe characters: ASCII
/// alphanumerics, `-`, and `_` pass through; everything else becomes `-`.
/// Deterministic, so a `<label for=…>` minted from the same input always
/// matches its control.
fn id_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The input id for a server-config row, e.g. `server.name` →
/// `rh-cfg-server-name`. Distinct keys yield distinct ids because config
/// keys themselves are id-safe apart from their dots.
pub fn config_input_id(key: &str) -> String {
    format!("rh-cfg-{}", id_safe(key))
}

/// The input id for one theme-editor token row. `scope` namespaces the
/// per-mode colour maps and the shared map (`"light"`, `"dark"`,
/// `"shared"`), so the same variable name edited in two maps gets two ids.
pub fn token_input_id(scope: &str, var: &str) -> String {
    format!("rh-tok-{}-{}", id_safe(scope), id_safe(var))
}

/// Move keyboard/reader focus to the current view's `<h1>` (falling back to
/// `<main>`). Called by the app root on every route change; a no-op on the
/// host, where there is no DOM.
pub fn focus_view_title() {
    #[cfg(target_arch = "wasm32")]
    {
        if !dom::focus_id(VIEW_TITLE_ID) {
            dom::focus_id(MAIN_ID);
        }
    }
}

/// Move focus to the element with `id`, if it exists and is focusable. Used
/// to keep focus on a theme-editor input after its row re-renders. No-op on
/// the host.
pub fn focus_id(id: &str) {
    #[cfg(target_arch = "wasm32")]
    dom::focus_id(id);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = id;
}

/// The wasm-only DOM edge: `getElementById(...).focus()`, best-effort.
#[cfg(target_arch = "wasm32")]
mod dom {
    use wasm_bindgen::JsCast;

    /// Focus `id`; `true` if the element existed and was an `HtmlElement`.
    pub fn focus_id(id: &str) -> bool {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return false;
        };
        let Some(el) = doc.get_element_by_id(id) else {
            return false;
        };
        match el.dyn_into::<web_sys::HtmlElement>() {
            Ok(html) => {
                let _ = html.focus();
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_link_targets_the_main_landmark() {
        // The skip link and the `<main>` id must never drift apart.
        assert_eq!(SKIP_HREF, format!("#{MAIN_ID}"));
        // And the two focus-target ids are distinct, so the h1-then-main
        // fallback in `focus_view_title` is meaningful.
        assert_ne!(VIEW_TITLE_ID, MAIN_ID);
    }

    #[test]
    fn config_ids_are_sanitised_and_distinct() {
        assert_eq!(config_input_id("server.name"), "rh-cfg-server-name");
        assert_eq!(
            config_input_id("chat.slowmode_secs"),
            "rh-cfg-chat-slowmode_secs"
        );
        // The console's seeded keys all mint distinct ids.
        let keys = [
            "server.name",
            "server.motd",
            "registration.mode",
            "chat.slowmode_secs",
        ];
        let ids: std::collections::BTreeSet<_> = keys.iter().map(|k| config_input_id(k)).collect();
        assert_eq!(ids.len(), keys.len());
    }

    #[test]
    fn token_ids_are_scoped_per_map() {
        // The same variable edited in the light, dark, and shared maps needs
        // three distinct control ids for correct label pairing.
        let ids = [
            token_input_id("light", "--rh-accent"),
            token_input_id("dark", "--rh-accent"),
            token_input_id("shared", "--rh-radius"),
        ];
        assert_eq!(ids[0], "rh-tok-light---rh-accent");
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
    }

    #[test]
    fn ids_never_contain_spaces_or_quotes() {
        for hostile in ["a b", "x\"y", "p'q", "m<n>", "tab\there"] {
            for id in [config_input_id(hostile), token_input_id("light", hostile)] {
                assert!(
                    id.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                    "{id:?} must be id-safe"
                );
            }
        }
    }

    #[test]
    fn host_focus_helpers_are_safe_no_ops() {
        // On the host there is no DOM; both helpers must simply return.
        focus_view_title();
        focus_id("rh-anything");
    }

    #[test]
    fn stylesheet_defines_the_classes_this_layer_relies_on() {
        // The markup mints `.rh-visually-hidden` spans and the `.rh-skip`
        // link; the stylesheet must actually define both (their behaviour
        // is shape-tested in `crate::theme_css`).
        let css = crate::theme_css::STYLESHEET;
        assert!(css.contains(&format!(".{SR_ONLY}{{")));
        assert!(css.contains(".rh-skip{"));
        assert!(css.contains(".rh-skip:focus"));
    }

    #[test]
    fn shell_document_declares_its_language() {
        // WCAG 3.1.1: the page language must be programmatically
        // determinable. trunk injects the app into this checked-in shell.
        let index = include_str!("../index.html");
        assert!(index.contains("<html lang=\"en\">"));
    }
}
