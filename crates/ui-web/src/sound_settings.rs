//! Personal sound controls shared by the browser and desktop client.

use leptos::*;

use crate::{
    app::AppState,
    sound::{SoundPrefs, SoundPreset},
};

/// Preferences save immediately. Preview is an explicit user action, so it is
/// available even when automatic message sounds are disabled.
#[component]
pub fn SoundSettings() -> impl IntoView {
    let app = expect_context::<AppState>();
    #[cfg(target_arch = "wasm32")]
    let initial = crate::sound::preferences();
    #[cfg(not(target_arch = "wasm32"))]
    let initial = SoundPrefs::default();
    let prefs = create_rw_signal(initial);
    let previewing = create_rw_signal(false);
    let preview_error = create_rw_signal(String::new());
    let save = move |value: SoundPrefs| {
        prefs.set(value);
        preview_error.set(String::new());
        #[cfg(target_arch = "wasm32")]
        crate::sound::set_preferences(value);
    };

    view! {
        <label class="rh-settings-check">
            <input
                type="checkbox"
                prop:checked=move || app.sound_on.get()
                on:change=move |_| {
                    let on = !app.sound_on.get_untracked();
                    app.sound_on.set(on);
                    #[cfg(target_arch = "wasm32")]
                    crate::sound::set_enabled(on);
                }
            />
            <span>"Play a chime for new messages while I'm away"</span>
        </label>
        <div class="rh-pref-row">
            <label class="rh-pref-label" for="rh-sound-preset">"Chime voice"</label>
            <select
                id="rh-sound-preset"
                class="rh-input"
                prop:value=move || prefs.get().preset.key()
                on:change=move |event| {
                    if let Some(preset) = SoundPreset::from_key(&event_target_value(&event)) {
                        let mut value = prefs.get_untracked();
                        value.preset = preset;
                        save(value);
                    }
                }
            >
                {SoundPreset::ALL.into_iter().map(|preset| view! {
                    <option value=preset.key() selected=move || prefs.get().preset == preset>
                        {preset.label()}
                    </option>
                }).collect_view()}
            </select>
            <button
                type="button"
                class="rh-btn ghost small"
                disabled=move || previewing.get() || prefs.get().volume() == 0
                on:click=move |_| {
                    preview_error.set(String::new());
                    #[cfg(target_arch = "wasm32")]
                    {
                        previewing.set(true);
                        let playback = crate::sound::preview(crate::sound::Chime::Dm);
                        wasm_bindgen_futures::spawn_local(async move {
                            if let Err(error) = playback.await {
                                preview_error.try_set(error.to_string());
                            }
                            previewing.try_set(false);
                        });
                    }
                }
            >
                {move || if previewing.get() { "Starting…" } else { "Preview chime" }}
            </button>
        </div>
        <div class="rh-pref-row">
            <label class="rh-pref-label" for="rh-sound-volume">"Chime volume"</label>
            <div class="rh-pref-slider">
                <input
                    id="rh-sound-volume"
                    type="range"
                    min="0"
                    max="100"
                    step="1"
                    prop:value=move || prefs.get().volume().to_string()
                    aria-valuetext=move || format!("{} percent", prefs.get().volume())
                    on:input=move |event| {
                        if let Ok(volume) = event_target_value(&event).parse::<i64>() {
                            let mut value = prefs.get_untracked();
                            value.set_volume(volume);
                            save(value);
                        }
                    }
                />
                <output for="rh-sound-volume">{move || format!("{}%", prefs.get().volume())}</output>
            </div>
        </div>
        <p class="rh-settings-note">
            {move || if prefs.get().volume() == 0 {
                "Chime volume is zero. Raise it to hear messages or a preview."
            } else {
                "Preview plays your direct-message chime, even when message sounds are off."
            }}
        </p>
        <Show when=move || !preview_error.get().is_empty()>
            <p class="rh-settings-note" role="status">{move || preview_error.get()}</p>
        </Show>
    }
}
