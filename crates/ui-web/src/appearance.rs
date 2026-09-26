//! Personal presentation, layered after the burrow's theme. Values are enums,
//! never arbitrary CSS; older settings retain their tracker and transfer choices.

use leptos::*;
use rabbithole_core::theme::{Mode, ThemePack};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::packs::PackTokens;
use crate::server_theme::ServerOverlay;
use crate::theme_css::{ModeChoice, ThemeChoice};

pub const STYLES: &str = include_str!("appearance.css");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    Blue,
    Violet,
    Rose,
    Forest,
    Amber,
    #[default]
    #[serde(other)]
    Theme,
}

impl Accent {
    pub const ALL: [(Self, &'static str); 6] = [
        (Self::Theme, "Theme"),
        (Self::Blue, "Blue"),
        (Self::Violet, "Violet"),
        (Self::Rose, "Rose"),
        (Self::Forest, "Forest"),
        (Self::Amber, "Amber"),
    ];

    pub fn color(self, mode: Mode) -> Option<&'static str> {
        Some(match (self, mode) {
            (Self::Theme, _) => return None,
            (Self::Blue, Mode::Light) => "#2455a4",
            (Self::Blue, Mode::Dark) => "#9bc2ff",
            (Self::Violet, Mode::Light) => "#6941a5",
            (Self::Violet, Mode::Dark) => "#c7b0ff",
            (Self::Rose, Mode::Light) => "#a12d55",
            (Self::Rose, Mode::Dark) => "#ffadc7",
            (Self::Forest, Mode::Light) => "#256344",
            (Self::Forest, Mode::Dark) => "#8dd8ae",
            (Self::Amber, Mode::Light) => "#855009",
            (Self::Amber, Mode::Dark) => "#edc27e",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatFont {
    System,
    Mono,
    Serif,
    #[default]
    #[serde(other)]
    Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    #[serde(other)]
    Comfortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Timestamps {
    Always,
    Hidden,
    #[default]
    #[serde(other)]
    Contextual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub accent: Accent,
    pub chat_font: ChatFont,
    pub chat_size: u8,
    pub density: Density,
    pub timestamps: Timestamps,
    pub show_avatars: bool,
    pub use_burrow_theme: bool,
    pub reduce_motion: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            accent: Accent::Theme,
            chat_font: ChatFont::Theme,
            chat_size: 16,
            density: Density::Comfortable,
            timestamps: Timestamps::Contextual,
            show_avatars: true,
            use_burrow_theme: true,
            reduce_motion: false,
        }
    }
}

impl Appearance {
    /// Keep preference files (including manually edited ones) within readable bounds.
    pub fn chat_size(&self) -> u8 {
        self.chat_size.clamp(13, 22)
    }

    pub fn chat_style(&self) -> String {
        let font = match self.chat_font {
            ChatFont::Theme => "var(--rh-font-sans)",
            ChatFont::System => "-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif",
            ChatFont::Mono => "ui-monospace,'SFMono-Regular',Consolas,monospace",
            ChatFont::Serif => "ui-serif,Georgia,serif",
        };
        format!(
            "--rh-chat-font:{font};--rh-chat-size:{}px;",
            self.chat_size()
        )
    }
}

/// An admin's live preview remains exact. High Contrast retains its stronger
/// palette; personal typography and spacing still apply to either theme.
pub fn resolve_style(
    appearance: &Appearance,
    custom: Option<&PackTokens>,
    server: Option<&ServerOverlay>,
    pack: ThemePack,
    mode: Mode,
) -> String {
    let server = server.filter(|_| appearance.use_burrow_theme && pack != ThemePack::HighContrast);
    let mut style = crate::theme_css::resolve_root_style(custom, server, pack, mode);
    if custom.is_none() && pack != ThemePack::HighContrast {
        if let Some(color) = readable_accent(appearance.accent, mode, &style) {
            let ink = if crate::theme_editor::contrast_ratio(
                crate::theme_editor::parse_hex(color).unwrap(),
                (255, 255, 255),
            ) >= 4.5
            {
                "#ffffff"
            } else {
                "#14161b"
            };
            style.push_str(&format!(
                "--rh-accent:{color};--rh-focus:{color};--rh-brand:{color};--rh-on-brand:{ink};"
            ));
        }
    }
    style.push_str(if mode == Mode::Light {
        "color-scheme:light;"
    } else {
        "color-scheme:dark;"
    });
    style.push_str(&appearance.chat_style());
    style
}

// Burrows can set backgrounds independently of the mode. Try both shades of
// the chosen hue, keeping the server accent when neither is readable everywhere.
fn readable_accent(accent: Accent, mode: Mode, style: &str) -> Option<&'static str> {
    use crate::theme_editor::{contrast_ratio, parse_hex};
    let other = if mode == Mode::Light {
        Mode::Dark
    } else {
        Mode::Light
    };
    [mode, other]
        .into_iter()
        .filter_map(|m| accent.color(m))
        .find(|color| {
            ["--rh-bg", "--rh-surface", "--rh-surface-2"]
                .into_iter()
                .all(|name| {
                    let value = style
                        .split(';')
                        .filter_map(|declaration| declaration.split_once(':'))
                        .find(|(key, _)| *key == name)
                        .and_then(|(_, value)| parse_hex(value));
                    value.is_some_and(|background| {
                        contrast_ratio(parse_hex(color).unwrap(), background) >= 4.5
                    })
                })
        })
}

fn change(app: AppState, update: impl FnOnce(&mut Appearance)) {
    app.settings.update(|s| update(&mut s.appearance));
    app.save_settings();
}

#[cfg(target_arch = "wasm32")]
pub fn watch_system_mode(dark: RwSignal<bool>) {
    use wasm_bindgen::{closure::Closure, JsCast};
    let Some(query) = web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
    else {
        return;
    };
    dark.set(query.matches());
    let observed = query.clone();
    let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| dark.set(observed.matches()));
    if query
        .add_event_listener_with_callback("change", listener.as_ref().unchecked_ref())
        .is_ok()
    {
        on_cleanup(move || {
            let _ = query
                .remove_event_listener_with_callback("change", listener.as_ref().unchecked_ref());
        });
    }
}

#[component]
pub fn AppearanceSettings() -> impl IntoView {
    let app = expect_context::<AppState>();
    let prefs = move || app.settings.get().appearance;
    view! {
        <section class="rh-pref-section" id="appearance" aria-labelledby="rh-appearance-title">
            <div class="rh-pref-heading">
                <h3 id="rh-appearance-title">"Appearance"</h3>
                <button class="rh-btn ghost small" type="button"
                    disabled=move || prefs() == Appearance::default() && app.theme.get() == ThemeChoice::default()
                    on:click=move |_| {
                        change(app, |p| *p = Appearance::default());
                        app.set_pack(ThemePack::Clean);
                        app.set_mode(ModeChoice::System);
                    }>"Reset appearance"</button>
            </div>
            <p class="rh-settings-note">"Make yourself at home. Changes save automatically on this device."</p>
            <div class="rh-pref-row">
                <span class="rh-pref-label" id="rh-theme-label">"Theme"</span>
                <div class="rh-seg" role="group" aria-labelledby="rh-theme-label">
                    {[ThemePack::Clean, ThemePack::Retro, ThemePack::HighContrast].into_iter().map(|pack| view! {
                        <button type="button" class="rh-seg-btn"
                            class:on=move || app.theme.get().pack == pack
                            aria-pressed=move || (app.theme.get().pack == pack).to_string()
                            on:click=move |_| app.set_pack(pack)>
                            {if pack == ThemePack::HighContrast { "High contrast" } else { crate::theme_css::pack_label(pack) }}
                        </button>
                    }).collect_view()}
                </div>
            </div>
            <div class="rh-pref-row">
                <span class="rh-pref-label" id="rh-mode-label">"Light & dark"</span>
                <div class="rh-seg" role="group" aria-labelledby="rh-mode-label">
                    {[ModeChoice::System, ModeChoice::Light, ModeChoice::Dark].into_iter().map(|mode| view! {
                        <button type="button" class="rh-seg-btn"
                            class:on=move || app.theme.get().mode == mode
                            aria-pressed=move || (app.theme.get().mode == mode).to_string()
                            on:click=move |_| app.set_mode(mode)>
                            {if mode == ModeChoice::System { "System" } else { crate::theme_css::mode_name(mode) }}
                        </button>
                    }).collect_view()}
                </div>
            </div>
            <div class="rh-pref-row">
                <span class="rh-pref-label" id="rh-accent-label">"Accent color"</span>
                <div class="rh-accent-options" role="group" aria-labelledby="rh-accent-label">
                    {Accent::ALL.into_iter().map(|(accent, label)| view! {
                        <button type="button" class="rh-accent-option"
                            aria-label=label title=label
                            aria-pressed=move || (prefs().accent == accent).to_string()
                            disabled=move || app.theme.get().pack == ThemePack::HighContrast
                            on:click=move |_| change(app, |p| p.accent = accent)>
                            <span class="rh-accent-dot" aria-hidden="true"
                                style=move || format!("background:{}", accent.color(app.mode()).unwrap_or("var(--rh-accent)"))></span>
                            <span>{label}</span>
                        </button>
                    }).collect_view()}
                </div>
            </div>
            <Show when=move || app.theme.get().pack == ThemePack::HighContrast>
                <p class="rh-settings-note">"High contrast keeps its own palette for readability."</p>
            </Show>
            <label class="rh-settings-check rh-pref-check">
                <input type="checkbox" prop:checked=move || prefs().use_burrow_theme
                    on:change=move |ev| change(app, |p| p.use_burrow_theme = event_target_checked(&ev))/>
                <span>"Use each burrow’s theme"<small>"Your accent choice still takes priority."</small></span>
            </label>
            <div class="rh-pref-row">
                <label class="rh-pref-label" for="rh-chat-font">"Chat font"</label>
                <select class="rh-input" id="rh-chat-font"
                    prop:value=move || match prefs().chat_font { ChatFont::Theme => "theme", ChatFont::System => "system", ChatFont::Mono => "mono", ChatFont::Serif => "serif" }
                    on:change=move |ev| change(app, |p| p.chat_font = match event_target_value(&ev).as_str() {
                        "system" => ChatFont::System, "mono" => ChatFont::Mono, "serif" => ChatFont::Serif, _ => ChatFont::Theme,
                    })>
                    <option value="theme" selected=move || prefs().chat_font == ChatFont::Theme>"Match theme"</option><option value="system" selected=move || prefs().chat_font == ChatFont::System>"System"</option>
                    <option value="mono" selected=move || prefs().chat_font == ChatFont::Mono>"Monospace"</option><option value="serif" selected=move || prefs().chat_font == ChatFont::Serif>"Serif"</option>
                </select>
            </div>
            <div class="rh-pref-row">
                <label class="rh-pref-label" for="rh-chat-size">"Chat text size"</label>
                <div class="rh-pref-slider">
                    <input id="rh-chat-size" type="range" min="13" max="22" step="1"
                        prop:value=move || prefs().chat_size()
                        on:input=move |ev| { if let Ok(n) = event_target_value(&ev).parse::<u8>() { change(app, |p| p.chat_size = n.clamp(13,22)); } }/>
                    <output for="rh-chat-size">{move || format!("{} px", prefs().chat_size())}</output>
                </div>
            </div>
            <div class="rh-pref-row">
                <label class="rh-pref-label" for="rh-density">"Spacing"</label>
                <select class="rh-input" id="rh-density" prop:value=move || if prefs().density == Density::Compact { "compact" } else { "comfortable" }
                    on:change=move |ev| change(app, |p| p.density = if event_target_value(&ev) == "compact" { Density::Compact } else { Density::Comfortable })>
                    <option value="comfortable" selected=move || prefs().density == Density::Comfortable>"Comfortable"</option><option value="compact" selected=move || prefs().density == Density::Compact>"Compact"</option>
                </select>
            </div>
            <div class="rh-pref-row">
                <label class="rh-pref-label" for="rh-timestamps">"Message times"</label>
                <select class="rh-input" id="rh-timestamps" prop:value=move || match prefs().timestamps { Timestamps::Always => "always", Timestamps::Hidden => "hidden", Timestamps::Contextual => "contextual" }
                    on:change=move |ev| change(app, |p| p.timestamps = match event_target_value(&ev).as_str() { "always" => Timestamps::Always, "hidden" => Timestamps::Hidden, _ => Timestamps::Contextual })>
                    <option value="contextual" selected=move || prefs().timestamps == Timestamps::Contextual>"Grouped · reveal on hover"</option><option value="always" selected=move || prefs().timestamps == Timestamps::Always>"Always show"</option><option value="hidden" selected=move || prefs().timestamps == Timestamps::Hidden>"Hide"</option>
                </select>
            </div>
            <label class="rh-settings-check rh-pref-check">
                <input type="checkbox" prop:checked=move || prefs().show_avatars
                    on:change=move |ev| change(app, |p| p.show_avatars = event_target_checked(&ev))/>
                <span>"Show profile icons in chat"</span>
            </label>
            <label class="rh-settings-check rh-pref-check">
                <input type="checkbox" prop:checked=move || prefs().reduce_motion
                    on:change=move |ev| change(app, |p| p.reduce_motion = event_target_checked(&ev))/>
                <span>"Reduce motion"<small>"Your system’s reduced motion setting is always respected."</small></span>
            </label>
            <div class="rh-chat-preview" aria-label="Chat appearance preview">
                <div class="rh-preview-heading">"Chat preview"</div>
                <ul class="rh-lines">
                    <li class="rh-line rh-line-head">
                        <span class="rh-mark rh-line-mark" aria-hidden="true" inner_html=crate::avatar::mark_svg("white-rabbit", 20)></span>
                        <span class="rh-from">"white-rabbit"</span><span class="rh-line-time">"10:42"</span>
                        <span class="rh-rich rh-line-text">"There’s always another rabbit hole."</span>
                    </li>
                    <li class="rh-line rh-line-cont"><span class="rh-line-time">"10:43"</span>
                        <span class="rh-rich rh-line-text">"Make this little corner yours."</span>
                    </li>
                </ul>
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_and_unknown_choices_keep_the_users_data() {
        let old = r#"{"trackers":[{"host":"my.directory","enabled":false}],"reconnect_on_launch":false,"notifications":false}"#;
        let mut settings: crate::settings::Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.appearance, Appearance::default());
        settings.appearance = Appearance {
            accent: Accent::Forest,
            chat_font: ChatFont::Mono,
            chat_size: 20,
            ..Default::default()
        };
        let saved = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<crate::settings::Settings>(&saved).unwrap(),
            settings
        );
        let future: Appearance =
            serde_json::from_str(r#"{"accent":"future","chat_font":"future","chat_size":255}"#)
                .unwrap();
        assert_eq!(future.accent, Accent::Theme);
        assert_eq!(future.chat_font, ChatFont::Theme);
        assert_eq!(future.chat_size(), 22);
        assert!(!future.chat_style().contains("255"));
    }

    #[test]
    fn personal_choices_respect_preview_and_high_contrast() {
        let p = Appearance {
            accent: Accent::Forest,
            ..Default::default()
        };
        let light = resolve_style(&p, None, None, ThemePack::Clean, Mode::Light);
        assert!(light.contains("--rh-accent:#256344;"));
        let preview = PackTokens::builtin(ThemePack::Retro);
        assert!(
            !resolve_style(&p, Some(&preview), None, ThemePack::Clean, Mode::Light)
                .contains("#256344")
        );
        assert!(
            !resolve_style(&p, None, None, ThemePack::HighContrast, Mode::Light)
                .contains("#256344")
        );
    }

    #[test]
    fn server_personality_is_optional_and_cannot_replace_high_contrast() {
        let mut server = ServerOverlay::default();
        server.light.insert("--rh-accent".into(), "#aa3300".into());
        let mut p = Appearance::default();
        assert!(
            resolve_style(&p, None, Some(&server), ThemePack::Clean, Mode::Light)
                .contains("--rh-accent:#aa3300;")
        );
        p.use_burrow_theme = false;
        assert!(
            !resolve_style(&p, None, Some(&server), ThemePack::Clean, Mode::Light)
                .contains("#aa3300")
        );
        p.use_burrow_theme = true;
        assert!(!resolve_style(
            &p,
            None,
            Some(&server),
            ThemePack::HighContrast,
            Mode::Light
        )
        .contains("#aa3300"));
    }

    #[test]
    fn personal_accents_adapt_to_a_burrows_backgrounds() {
        let mut server = ServerOverlay::default();
        for key in ["--rh-bg", "--rh-surface", "--rh-surface-2"] {
            server.light.insert(key.into(), "#14161b".into());
        }
        let p = Appearance {
            accent: Accent::Blue,
            ..Default::default()
        };
        let style = resolve_style(&p, None, Some(&server), ThemePack::Clean, Mode::Light);
        assert!(style.contains("--rh-accent:#9bc2ff;"));
        assert!(style.contains("--rh-on-brand:#14161b;"));
    }

    #[test]
    fn accents_keep_text_and_buttons_readable_in_both_modes() {
        use crate::theme_editor::{contrast_ratio, parse_hex};
        for (accent, _) in Accent::ALL {
            for mode in [Mode::Light, Mode::Dark] {
                let Some(color) = accent.color(mode) else {
                    continue;
                };
                let color = parse_hex(color).unwrap();
                let pack = PackTokens::builtin(ThemePack::Clean);
                let colors = if mode == Mode::Light {
                    pack.light
                } else {
                    pack.dark
                };
                for key in ["--rh-bg", "--rh-surface", "--rh-surface-2"] {
                    assert!(
                        contrast_ratio(color, parse_hex(&colors[key]).unwrap()) >= 4.5,
                        "{accent:?} {mode:?} {key}"
                    );
                }
            }
        }
    }
}
