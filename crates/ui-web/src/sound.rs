//! Chimes — the Hotline blip, kept polite and personal.
//!
//! Automatic sounds follow the same away-only, never-for-yourself policy as
//! [`crate::notify`]. The existing on/off preference stays authoritative. Voice
//! and volume are independent preferences, so muting never loses your choices.
//! Tones are synthesised rather than downloaded; even at full volume they stay
//! gentle. Only the Web Audio edge is wasm-gated.

/// Which event a chime describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chime {
    Chat,
    Dm,
}

/// Existing storage key: retained so an earlier opt-out stays an opt-out.
pub const STORAGE_KEY: &str = "rh.sound.enabled";
pub const PRESET_STORAGE_KEY: &str = "rh.sound.preset";
pub const VOLUME_STORAGE_KEY: &str = "rh.sound.volume";

/// Short, distinct voices. Soft preserves RabbitHole's original chime.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SoundPreset {
    #[default]
    Soft,
    Classic,
    Subtle,
}

impl SoundPreset {
    pub const ALL: [Self; 3] = [Self::Soft, Self::Classic, Self::Subtle];

    pub fn key(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Classic => "classic",
            Self::Subtle => "subtle",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Soft => "Soft",
            Self::Classic => "Classic",
            Self::Subtle => "Subtle",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.key() == key)
    }
}

/// Volume is a percentage of an already quiet envelope, never raw Web Audio gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundPrefs {
    pub preset: SoundPreset,
    volume: u8,
}

impl Default for SoundPrefs {
    fn default() -> Self {
        Self {
            preset: SoundPreset::Soft,
            volume: 100,
        }
    }
}

impl SoundPrefs {
    /// Missing/new keys preserve the original sound; corrupt values are harmless.
    pub fn from_storage(preset: Option<&str>, volume: Option<&str>) -> Self {
        let mut prefs = Self {
            preset: preset.and_then(SoundPreset::from_key).unwrap_or_default(),
            ..Self::default()
        };
        if let Some(value) = volume.and_then(|value| value.parse::<i64>().ok()) {
            prefs.set_volume(value);
        }
        prefs
    }

    pub fn volume(self) -> u8 {
        self.volume
    }

    pub fn set_volume(&mut self, value: i64) {
        self.volume = value.clamp(0, 100) as u8;
    }

    pub fn peak_gain(self, chime: Chime) -> f32 {
        let (_, peak) = preset_notes(self.preset, chime);
        peak * f32::from(self.volume) / 100.0
    }
}

/// Preserve the legacy opt-in interpretation exactly, including the default.
pub fn enabled_from_storage(value: Option<&str>) -> bool {
    value.map(|value| value == "1").unwrap_or(true)
}

/// Sound is only automatic when enabled, you're away, and it wasn't you.
pub fn should_chime(enabled: bool, window_focused: bool, from: &str, me: &str) -> bool {
    enabled && crate::notify::should_notify(window_focused, from, me)
}

/// The original notes, retained as the default voice.
pub fn notes(chime: Chime) -> (&'static [(f32, f32)], f32) {
    preset_notes(SoundPreset::Soft, chime)
}

/// `(frequency_hz, start_offset_secs)` and peak gain for each event/voice pair.
/// Every DM rises; room messages stay a single, quieter note.
pub fn preset_notes(preset: SoundPreset, chime: Chime) -> (&'static [(f32, f32)], f32) {
    match (preset, chime) {
        (SoundPreset::Soft, Chime::Chat) => (&[(660.0, 0.0)], 0.05),
        (SoundPreset::Soft, Chime::Dm) => (&[(660.0, 0.0), (880.0, 0.09)], 0.07),
        (SoundPreset::Classic, Chime::Chat) => (&[(784.0, 0.0)], 0.045),
        (SoundPreset::Classic, Chime::Dm) => (&[(784.0, 0.0), (1046.5, 0.075)], 0.06),
        (SoundPreset::Subtle, Chime::Chat) => (&[(440.0, 0.0)], 0.025),
        (SoundPreset::Subtle, Chime::Dm) => (&[(440.0, 0.0), (554.4, 0.10)], 0.035),
    }
}

/// A quick attack and a smooth decay keep each note brief and click-free.
pub const NOTE_SECS: f32 = 0.12;
const _: () = assert!(NOTE_SECS <= 0.2);

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;

    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::{future_to_promise, spawn_local, JsFuture};

    use super::{
        enabled_from_storage, preset_notes, Chime, SoundPrefs, SoundPreset, NOTE_SECS,
        PRESET_STORAGE_KEY, STORAGE_KEY, VOLUME_STORAGE_KEY,
    };

    thread_local! {
        // One context for this document, rather than leaking one per message.
        static CONTEXT: RefCell<Option<web_sys::AudioContext>> = const { RefCell::new(None) };
        // Changes work for this session even when private mode blocks storage.
        static PREFS: RefCell<Option<SoundPrefs>> = const { RefCell::new(None) };
    }

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn enabled() -> bool {
        let saved = storage().and_then(|s| s.get_item(STORAGE_KEY).ok().flatten());
        enabled_from_storage(saved.as_deref())
    }

    pub fn set_enabled(on: bool) {
        if let Some(s) = storage() {
            let _ = s.set_item(STORAGE_KEY, if on { "1" } else { "0" });
        }
    }

    pub fn preferences() -> SoundPrefs {
        PREFS.with(|slot| {
            *slot.borrow_mut().get_or_insert_with(|| {
                let read = |key| storage().and_then(|s| s.get_item(key).ok().flatten());
                SoundPrefs::from_storage(
                    read(PRESET_STORAGE_KEY).as_deref(),
                    read(VOLUME_STORAGE_KEY).as_deref(),
                )
            })
        })
    }

    pub fn set_preferences(prefs: SoundPrefs) {
        PREFS.with(|slot| *slot.borrow_mut() = Some(prefs));
        if let Some(s) = storage() {
            let _ = s.set_item(PRESET_STORAGE_KEY, prefs.preset.key());
            let _ = s.set_item(VOLUME_STORAGE_KEY, &prefs.volume().to_string());
        }
    }

    fn context() -> Option<web_sys::AudioContext> {
        CONTEXT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = web_sys::AudioContext::new().ok();
            }
            slot.clone()
        })
    }

    /// Explicit preview may sound while focused or muted. It does not change the
    /// automatic notification policy or opt-in. Start resume synchronously, while
    /// the button's user activation is still available to the browser.
    pub fn preview(chime: Chime) -> impl std::future::Future<Output = Result<(), &'static str>> {
        preview_with(chime, |_| {})
    }

    pub fn preview_with(
        chime: Chime,
        on_started: impl FnOnce(u32) + 'static,
    ) -> impl std::future::Future<Output = Result<(), &'static str>> {
        playback(chime, preferences(), on_started)
    }

    /// Called only after the automatic notification policy passes.
    pub fn play(chime: Chime) {
        play_with(chime, |_| {});
    }

    pub fn play_with(chime: Chime, on_started: impl FnOnce(u32) + 'static) {
        let prefs = preferences();
        if prefs.volume() == 0 {
            return;
        }
        let playback = playback(chime, prefs, on_started);
        spawn_local(async move {
            // An unsolicited sound must never become an unsolicited error toast.
            let _ = playback.await;
        });
    }

    fn playback(
        chime: Chime,
        prefs: SoundPrefs,
        on_started: impl FnOnce(u32) + 'static,
    ) -> impl std::future::Future<Output = Result<(), &'static str>> {
        let context = context().ok_or("This browser cannot play chimes.");
        let resume = context.as_ref().ok().map(|ctx| ctx.resume());
        async move {
            if prefs.volume() == 0 {
                return Ok(());
            }
            let ctx = context?;
            if ctx.state() != web_sys::AudioContextState::Running {
                let resume = resume
                    .and_then(Result::ok)
                    .ok_or("Sound could not start. Select Preview chime to try again.")?;
                // Autoplay can leave resume pending. Do not queue old message
                // chimes for the next visit, or leave Preview busy indefinitely.
                let timeout = future_to_promise(async {
                    gloo_timers::future::TimeoutFuture::new(1000).await;
                    Err(JsValue::from_str("audio resume timed out"))
                });
                JsFuture::from(js_sys::Promise::race(&js_sys::Array::of2(
                    &resume, &timeout,
                )))
                .await
                .map_err(|_| "Sound could not start. Select Preview chime to try again.")?;
            }
            if ctx.state() != web_sys::AudioContextState::Running {
                return Err("Sound could not start. Select Preview chime to try again.");
            }
            let (notes, _) = preset_notes(prefs.preset, chime);
            let now = ctx.current_time();
            let mut nodes = Vec::new();
            for (freq, offset) in notes {
                let (Ok(osc), Ok(gain)) = (ctx.create_oscillator(), ctx.create_gain()) else {
                    continue;
                };
                osc.set_type(match prefs.preset {
                    SoundPreset::Classic => web_sys::OscillatorType::Triangle,
                    _ => web_sys::OscillatorType::Sine,
                });
                osc.frequency().set_value(*freq);
                let start = now + f64::from(*offset);
                let end = start + f64::from(NOTE_SECS);
                let g = gain.gain();
                let _ = g.set_value_at_time(0.0, start);
                let _ = g.linear_ramp_to_value_at_time(prefs.peak_gain(chime), start + 0.012);
                let _ = g.exponential_ramp_to_value_at_time(0.0001, end);
                if osc.connect_with_audio_node(&gain).is_ok()
                    && gain.connect_with_audio_node(&ctx.destination()).is_ok()
                    && osc.start_with_when(start).is_ok()
                    && osc.stop_with_when(end).is_ok()
                {
                    nodes.push((osc, gain));
                } else {
                    let _ = osc.disconnect();
                    let _ = gain.disconnect();
                }
            }
            if nodes.is_empty() {
                return Err("Sound could not start. Select Preview chime to try again.");
            }
            // The player should duck only for audio that was actually scheduled,
            // not for a blocked resume, a silent preference, or a policy refusal.
            let duration = notes
                .last()
                .map(|(_, offset)| offset + NOTE_SECS)
                .unwrap_or(0.0);
            on_started((f64::from(duration) * 1000.0).ceil() as u32);
            // Stop times run on the audio clock; this only releases the finished
            // graph. A throttled background timer cannot lengthen a note.
            spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(400).await;
                for (osc, gain) in nodes {
                    let _ = osc.disconnect();
                    let _ = gain.disconnect();
                }
            });
            Ok(())
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::{
    enabled, play, play_with, preferences, preview, preview_with, set_enabled, set_preferences,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_enable_flag_gates_every_chime() {
        assert!(!should_chime(false, false, "alice", "bob"));
        assert!(should_chime(true, false, "alice", "bob"));
    }

    #[test]
    fn chimes_follow_the_notification_policy() {
        assert!(!should_chime(true, true, "alice", "bob"));
        assert!(!should_chime(true, false, "bob", "bob"));
        assert!(!should_chime(true, false, "BOB", "bob"));
        assert!(!should_chime(true, false, "", "bob"));
    }

    #[test]
    fn every_preset_preserves_quiet_distinct_chat_and_dm_events() {
        for preset in SoundPreset::ALL {
            let (chat, chat_peak) = preset_notes(preset, Chime::Chat);
            let (dm, dm_peak) = preset_notes(preset, Chime::Dm);
            assert_eq!(chat.len(), 1);
            assert_eq!(dm.len(), 2);
            assert!(dm[1].0 > dm[0].0);
            assert!(dm[1].1 > dm[0].1);
            assert!(dm_peak > chat_peak);
            assert!(dm_peak <= 0.1 && chat_peak <= 0.1);
        }
    }

    #[test]
    fn preferences_retain_the_original_sound_and_saved_opt_out() {
        assert_eq!(SoundPrefs::from_storage(None, None), SoundPrefs::default());
        assert_eq!(notes(Chime::Dm), (&[(660.0, 0.0), (880.0, 0.09)][..], 0.07));
        assert_eq!(
            SoundPrefs::default().peak_gain(Chime::Dm),
            notes(Chime::Dm).1
        );
        assert!(enabled_from_storage(None));
        assert!(enabled_from_storage(Some("1")));
        assert!(!enabled_from_storage(Some("0")));
        assert!(!enabled_from_storage(Some("corrupt")));
        assert_eq!(STORAGE_KEY, "rh.sound.enabled");
    }

    #[test]
    fn presets_round_trip_and_change_the_sound() {
        for preset in SoundPreset::ALL {
            assert_eq!(SoundPreset::from_key(preset.key()), Some(preset));
            let prefs = SoundPrefs::from_storage(Some(preset.key()), Some("37"));
            assert_eq!(prefs.preset, preset);
            assert_eq!(prefs.volume(), 37);
        }
        assert_ne!(
            preset_notes(SoundPreset::Classic, Chime::Dm),
            notes(Chime::Dm)
        );
        assert_ne!(
            preset_notes(SoundPreset::Subtle, Chime::Dm),
            notes(Chime::Dm)
        );
        assert_eq!(
            SoundPrefs::from_storage(Some("missing"), None),
            SoundPrefs::default()
        );
    }

    #[test]
    fn volume_is_clamped_and_scales_every_voice() {
        for preset in SoundPreset::ALL {
            for chime in [Chime::Chat, Chime::Dm] {
                let mut prefs = SoundPrefs {
                    preset,
                    ..SoundPrefs::default()
                };
                let full = prefs.peak_gain(chime);
                prefs.set_volume(50);
                assert!((prefs.peak_gain(chime) - full / 2.0).abs() < 0.00001);
                prefs.set_volume(-1);
                assert_eq!(prefs.peak_gain(chime), 0.0);
                prefs.set_volume(i64::MAX);
                assert_eq!(prefs.peak_gain(chime), full);
            }
        }
        assert_eq!(SoundPrefs::from_storage(None, Some("-99")).volume(), 0);
        assert_eq!(SoundPrefs::from_storage(None, Some("101")).volume(), 100);
        for invalid in ["", "garbage", "NaN", "inf", "50.5", "99999999999999999999"] {
            assert_eq!(
                SoundPrefs::from_storage(None, Some(invalid)),
                SoundPrefs::default()
            );
        }
    }
}
