//! Chimes — the Hotline blip, kept polite.
//!
//! Hotline's little sounds are muscle memory for anyone who used it, and they
//! are also the easiest thing in a chat app to make obnoxious. So the rules are
//! strict and the same as [`crate::notify`]'s, plus an explicit opt-in:
//!
//! * **Off by default.** Nothing ever makes noise until the user turns it on.
//! * **Only when you're away.** If the window is focused you can see the message;
//!   a sound would be pure noise.
//! * **Never for your own actions** — your sent line echoing back is not news.
//! * **Two voices only**: a soft two-note rise for a DM (addressed to you), a
//!   single quieter note for room chat.
//!
//! Tones are synthesised with a short oscillator envelope rather than shipping
//! audio assets: a few hundred bytes of code instead of a binary, and it can't
//! be a jarring recorded clip. The policy is pure and host-tested; only the
//! Web Audio call is wasm-gated.

/// Which chime to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chime {
    /// Someone spoke in a room while you were away — one soft note.
    Chat,
    /// Someone messaged you directly — a two-note rise, slightly brighter.
    Dm,
}

/// `localStorage` key for the opt-in.
pub const STORAGE_KEY: &str = "rh.sound.enabled";

/// Should a chime play? Same policy as a notification, plus the opt-in gate:
/// sound is off unless enabled, you're away, and it wasn't you. Pure —
/// host-tested.
pub fn should_chime(enabled: bool, window_focused: bool, from: &str, me: &str) -> bool {
    enabled && crate::notify::should_notify(window_focused, from, me)
}

/// The note(s) for a chime: `(frequency_hz, start_offset_secs)`, plus the peak
/// gain. Kept quiet on purpose — a chime should be noticeable, not startling.
/// Pure — host-tested.
pub fn notes(chime: Chime) -> (&'static [(f32, f32)], f32) {
    match chime {
        // A single mid note: present, unobtrusive.
        Chime::Chat => (&[(660.0, 0.0)], 0.05),
        // A rising pair — a DM is addressed to you, so it gets the brighter voice.
        Chime::Dm => (&[(660.0, 0.0), (880.0, 0.09)], 0.07),
    }
}

/// How long each note rings, in seconds. Short enough never to overlap the next
/// message in a busy room — enforced at compile time, not by a runtime assert.
pub const NOTE_SECS: f32 = 0.12;
const _: () = assert!(NOTE_SECS <= 0.2);

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::{notes, Chime, NOTE_SECS, STORAGE_KEY};

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    /// Is sound turned on? Off unless the user explicitly enabled it.
    pub fn enabled() -> bool {
        storage()
            .and_then(|s| s.get_item(STORAGE_KEY).ok().flatten())
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// Persist the opt-in.
    pub fn set_enabled(on: bool) {
        if let Some(s) = storage() {
            let _ = s.set_item(STORAGE_KEY, if on { "1" } else { "0" });
        }
    }

    /// Play a chime. Best-effort: a browser that blocks audio before a gesture,
    /// or has no Web Audio, simply stays silent — never an error the user sees.
    pub fn play(chime: Chime) {
        let Ok(ctx) = web_sys::AudioContext::new() else {
            return;
        };
        let (notes, peak) = notes(chime);
        let now = ctx.current_time();
        for (freq, offset) in notes {
            let (Ok(osc), Ok(gain)) = (ctx.create_oscillator(), ctx.create_gain()) else {
                continue;
            };
            osc.set_type(web_sys::OscillatorType::Sine);
            osc.frequency().set_value(*freq);
            let start = now + *offset as f64;
            let end = start + NOTE_SECS as f64;
            // A quick attack and a smooth decay — no click at either edge.
            let g = gain.gain();
            g.set_value_at_time(0.0, start).ok();
            g.linear_ramp_to_value_at_time(peak, start + 0.012).ok();
            g.exponential_ramp_to_value_at_time(0.0001, end).ok();
            let _ = osc.connect_with_audio_node(&gain);
            let _ = gain.connect_with_audio_node(&ctx.destination());
            let _ = osc.start_with_when(start);
            let _ = osc.stop_with_when(end);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::{enabled, play, set_enabled};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_is_off_until_enabled() {
        // The whole point: silence unless the user asked for it.
        assert!(!should_chime(false, false, "alice", "bob"));
        assert!(should_chime(true, false, "alice", "bob"));
    }

    #[test]
    fn chimes_follow_the_notification_policy() {
        // Focused window: you can see it; a sound would be noise.
        assert!(!should_chime(true, true, "alice", "bob"));
        // Your own line echoed back is never news.
        assert!(!should_chime(true, false, "bob", "bob"));
        assert!(!should_chime(true, false, "BOB", "bob"));
        // A senderless/system line stays silent.
        assert!(!should_chime(true, false, "", "bob"));
    }

    #[test]
    fn a_dm_gets_the_brighter_two_note_voice() {
        let (chat, chat_peak) = notes(Chime::Chat);
        let (dm, dm_peak) = notes(Chime::Dm);
        assert_eq!(chat.len(), 1, "room chat is a single note");
        assert_eq!(dm.len(), 2, "a DM rises");
        assert!(dm[1].0 > dm[0].0, "the second note is higher");
        assert!(dm[1].1 > dm[0].1, "…and lands after the first");
        assert!(dm_peak > chat_peak, "a DM is addressed to you, so slightly louder");
        // Quiet by design: a chime should never startle.
        assert!(dm_peak <= 0.1 && chat_peak <= 0.1, "peak gain stays gentle");
        // (NOTE_SECS is guarded at compile time below — a runtime assert on a
        // constant is optimised away.)
    }
}
