//! Stick-to-bottom scrolling for chat logs.
//!
//! A conversation view should follow the newest line — but *only* while the
//! reader is actually at the bottom. Someone who has scrolled up to read
//! history must never be yanked back down by an arriving message; instead a
//! small "new messages" pill offers the jump. The decision of *whether* the
//! reader counts as "following along" is the pure, host-tested
//! [`is_near_bottom`]; [`ChatScroll`] is the thin reactive/DOM glue the
//! [`crate::components`] chat views (lobby + DMs) attach to their scrollback.

use leptos::html::Div;
use leptos::*;

/// How close to the bottom (px) still counts as "following the conversation".
///
/// Generous enough that a line mid-render or a sub-line nudge doesn't unstick
/// the reader, small enough that half a screen of history clearly does.
pub const STICK_THRESHOLD_PX: i32 = 48;

/// Whether a scroll position is close enough to the bottom that the reader is
/// following the newest lines. Pure — host-tested below.
///
/// The three inputs are the DOM's `scrollTop` / `clientHeight` /
/// `scrollHeight` for the scrollable log. A log too short to scroll always
/// counts as at-bottom.
pub fn is_near_bottom(scroll_top: i32, client_height: i32, scroll_height: i32) -> bool {
    scroll_height - (scroll_top + client_height) <= STICK_THRESHOLD_PX
}

/// Stick-to-bottom behaviour for one chat log.
///
/// Construct with [`ChatScroll::install`] (passing a reactive line count),
/// attach [`ChatScroll::node`] to the scrollable element via `node_ref`, and
/// wire `on:scroll` to [`ChatScroll::on_scroll`]. While the reader is at the
/// bottom, growth auto-scrolls; while they're up in history, growth raises
/// [`ChatScroll::unseen`] instead (the "new messages" pill), and
/// [`ChatScroll::jump`] is the pill's click.
#[derive(Clone, Copy)]
pub struct ChatScroll {
    /// Attach to the scrollable log element (`node_ref=log.node`).
    pub node: NodeRef<Div>,
    /// True when lines arrived while the reader was scrolled up into history.
    pub unseen: RwSignal<bool>,
    /// Whether the reader is currently following the bottom of the log.
    stick: StoredValue<bool>,
}

impl ChatScroll {
    /// Create the behaviour and its follow effect: whenever `count` changes
    /// (or the log element mounts), either snap to the bottom (still
    /// following) or raise the `unseen` pill (scrolled up and lines grew).
    pub fn install(count: impl Fn() -> usize + 'static) -> Self {
        let this = Self {
            node: create_node_ref::<Div>(),
            unseen: create_rw_signal(false),
            stick: store_value(true),
        };
        // A *render* effect, deliberately: `create_effect` queues its first
        // run in a microtask under the creating owner, and a view that is
        // mounted and disposed in the same tick (the shell remounts <Routes>
        // when the focused burrow changes, e.g. right at login) panics that
        // microtask with `OwnerDisposed`. The render effect runs its first
        // pass synchronously during setup instead — verified live.
        create_render_effect(move |prev: Option<usize>| {
            // Track the mount too: the first run often precedes the element,
            // and this re-fires the snap once the log is actually in the DOM.
            let el = this.node.get();
            let n = count();
            let grew = prev.is_some_and(|p| n > p);
            match el {
                Some(el) if this.stick.get_value() => snap_soon(el),
                Some(_) if grew => this.unseen.set(true),
                _ => {}
            }
            n
        });
        // A viewport resize (window resize, mobile soft keyboard opening,
        // orientation change) shrinks the log: keep a following reader
        // pinned to the newest line through it.
        #[cfg(target_arch = "wasm32")]
        {
            let handle = window_event_listener(ev::resize, move |_| {
                if this.stick.get_value() {
                    if let Some(el) = this.node.get_untracked() {
                        snap_soon(el);
                    }
                }
            });
            on_cleanup(move || handle.remove());
        }
        this
    }

    /// `on:scroll` handler: re-derive stickiness from the live position, and
    /// clear the pill once the reader gets back to the bottom on their own.
    pub fn on_scroll(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(el) = self.node.get_untracked() {
            let near = is_near_bottom(el.scroll_top(), el.client_height(), el.scroll_height());
            self.stick.set_value(near);
            if near {
                self.unseen.set(false);
            }
        }
    }

    /// Jump to the newest line and resume following — the pill's click, and
    /// the reset a view calls when the conversation itself changes.
    pub fn jump(&self) {
        self.stick.set_value(true);
        self.unseen.set(false);
        if let Some(el) = self.node.get_untracked() {
            snap_soon(el);
        }
    }
}

/// Schedule a snap of `el` to its bottom edge on the next animation frame —
/// after the rows from the current update are laid out.
///
/// Deliberately takes the plain element handle and touches **no reactive
/// state** inside the callback: a frame later the owning view may already be
/// disposed (route change, burrow-focus remount), and reading a disposed
/// signal/NodeRef panics. Scrolling a detached element is a harmless no-op.
#[cfg(target_arch = "wasm32")]
fn snap_soon(el: HtmlElement<Div>) {
    request_animation_frame(move || {
        el.set_scroll_top(el.scroll_height());
    });
}

/// Host stand-in (no DOM): nothing to scroll.
#[cfg(not(target_arch = "wasm32"))]
fn snap_soon(_el: HtmlElement<Div>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_the_exact_bottom_sticks() {
        // 300px viewport at the very end of an 800px log.
        assert!(is_near_bottom(500, 300, 800));
    }

    #[test]
    fn within_the_threshold_still_sticks() {
        assert!(is_near_bottom(500 - STICK_THRESHOLD_PX, 300, 800));
    }

    #[test]
    fn one_pixel_past_the_threshold_unsticks() {
        assert!(!is_near_bottom(500 - STICK_THRESHOLD_PX - 1, 300, 800));
    }

    #[test]
    fn scrolled_up_into_history_unsticks() {
        assert!(!is_near_bottom(120, 300, 800));
    }

    #[test]
    fn a_log_too_short_to_scroll_always_sticks() {
        // scrollHeight <= clientHeight: nothing to follow, never show a pill.
        assert!(is_near_bottom(0, 300, 180));
        assert!(is_near_bottom(0, 300, 300));
    }
}
