//! Desktop notifications for messages that arrive while you're away.
//!
//! The unread story has three levels, quietest first: the rail badge (which
//! burrow), the tab title (how many, across burrows), and — only when the window
//! itself isn't focused — an OS notification. Permission is requested **in
//! context**: the first time a message actually arrives while you're away, never
//! on load. A denial is final (the browser keeps that state; we never re-ask).
//!
//! The decision is a pure function so the policy is host-tested; only the
//! `Notification` call itself is wasm-gated.

/// Should an arriving chat line raise an OS notification?
///
/// * `window_focused` — is the app window focused right now? If you're looking
///   at it, the rail badge and tab title are enough; a notification would be noise.
/// * `from` / `me` — never notify for your own message (the echo of your send).
///
/// Pure — host-tested.
pub fn should_notify(window_focused: bool, from: &str, me: &str) -> bool {
    if window_focused {
        return false;
    }
    if from.is_empty() {
        return false;
    }
    // Your own line, echoed back by the server, is not news.
    !from.eq_ignore_ascii_case(me)
}

/// The notification body for a chat line: the message, trimmed to a sensible
/// length so a wall of text can't fill the screen. Pure — host-tested.
pub fn notification_body(text: &str) -> String {
    const MAX: usize = 140;
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// The notification title: who said it, and where. Pure — host-tested.
pub fn notification_title(from: &str, burrow: &str) -> String {
    if burrow.is_empty() {
        from.to_string()
    } else {
        format!("{from} \u{00b7} {burrow}")
    }
}

/// The title for a direct message — named as such, because a DM is addressed to
/// you personally and shouldn't read like room chatter. Pure — host-tested.
pub fn dm_notification_title(from: &str) -> String {
    format!("{from} \u{00b7} direct message")
}

/// Notification tag for room chat: repeats collapse into one slot.
pub const TAG_CHAT: &str = "rabbithole-chat";
/// Notification tag for DMs — separate from chat, so a direct message never
/// silently replaces (or is replaced by) a lobby notification.
pub const TAG_DM: &str = "rabbithole-dm";

#[cfg(target_arch = "wasm32")]
mod browser {
    use wasm_bindgen::JsValue;

    /// Is the app window focused right now? Treated as focused when we can't
    /// tell, so an unknown state never spams notifications.
    pub fn window_focused() -> bool {
        web_sys::window()
            .and_then(|w| w.document())
            .map(|d| d.has_focus().unwrap_or(true))
            .unwrap_or(true)
    }

    /// Show an OS notification, requesting permission the first time (only ever
    /// called when a message really arrived while the user was away). A denied
    /// permission is respected silently — the browser remembers it, and we never
    /// re-prompt.
    pub fn notify(title: String, body: String, tag: &'static str) {
        use wasm_bindgen_futures::{spawn_local, JsFuture};
        match web_sys::Notification::permission() {
            web_sys::NotificationPermission::Granted => show(&title, &body, tag),
            web_sys::NotificationPermission::Denied => {}
            // Default (not yet asked): ask now — in context, with a real message
            // waiting — and show it only if the user says yes.
            _ => {
                let Ok(promise) = web_sys::Notification::request_permission() else {
                    return;
                };
                spawn_local(async move {
                    if let Ok(result) = JsFuture::from(promise).await {
                        if result == JsValue::from_str("granted") {
                            show(&title, &body, tag);
                        }
                    }
                });
            }
        }
    }

    fn show(title: &str, body: &str, tag: &str) {
        let opts = web_sys::NotificationOptions::new();
        opts.set_body(body);
        // Collapse repeats of the same kind into one notification slot rather
        // than stacking a tower of them (chat and DMs use separate tags).
        opts.set_tag(tag);
        let _ = web_sys::Notification::new_with_options(title, &opts);
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::{notify, window_focused};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_notifies_when_away_and_not_your_own_line() {
        // Focused window: the rail badge + tab title already say it.
        assert!(!should_notify(true, "alice", "bob"));
        // Away, someone else spoke: notify.
        assert!(should_notify(false, "alice", "bob"));
        // Away, but it's your own line echoed back: not news.
        assert!(!should_notify(false, "bob", "bob"));
        assert!(!should_notify(false, "BOB", "bob"), "handle case-insensitive");
        // A line with no sender (system/blank) never notifies.
        assert!(!should_notify(false, "", "bob"));
    }

    #[test]
    fn body_is_trimmed_and_bounded() {
        assert_eq!(notification_body("  hello  "), "hello");
        let long = "x".repeat(300);
        let body = notification_body(&long);
        assert_eq!(body.chars().count(), 141, "140 chars plus an ellipsis");
        assert!(body.ends_with('\u{2026}'));
        // A short line is untouched (no stray ellipsis).
        assert!(!notification_body("short").ends_with('\u{2026}'));
    }

    #[test]
    fn a_dm_is_titled_as_a_direct_message() {
        // A DM is addressed to you personally — it must not read like room chat,
        // and it uses its own tag so it never collapses a lobby notification.
        assert_eq!(dm_notification_title("alice"), "alice · direct message");
        assert_ne!(TAG_DM, TAG_CHAT);
    }

    #[test]
    fn title_names_the_sender_and_burrow() {
        assert_eq!(notification_title("alice", "The Warren"), "alice · The Warren");
        // Before the burrow name is known, the sender alone is enough.
        assert_eq!(notification_title("alice", ""), "alice");
    }
}
