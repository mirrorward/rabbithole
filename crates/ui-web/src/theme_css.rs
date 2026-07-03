//! Design tokens, theme resolution, and the app stylesheet.
//!
//! The colour palette lives in [`rabbithole_core::theme`] so every client means
//! the same thing by "accent" or "surface". The [`crate::packs`] module turns a
//! [`ThemePack`] into a complete set of CSS custom properties (`--rh-*`) —
//! colours per mode plus non-colour design tokens (spacing, typography, radii)
//! — that the static [`STYLESHEET`] consumes. Applying the variables as an
//! inline `style` on the app root re-themes the whole subtree reactively — no
//! `web_sys` DOM poking.
//!
//! ## Choice model
//!
//! Appearance is a [`ThemeChoice`]: which pack (Clean / Retro / High Contrast)
//! **and** how to pick light vs dark (follow the OS, or force one) — a
//! [`ModeChoice`]. Resolution is kept **pure and host-tested**:
//! [`effective_mode`] combines the mode choice with the OS
//! `prefers-color-scheme` hint. The whole choice is persisted to
//! `localStorage` and the OS hint read via `matchMedia`, both wasm-gated in
//! [`storage`] behind this pure core.

use rabbithole_core::theme::{Mode, ThemePack};

use crate::packs::PackTokens;

/// The pack a fresh session renders with before any persisted choice.
pub const DEFAULT_PACK: ThemePack = ThemePack::Clean;

/// The full inline `style` string for the app root: every `--rh-*` variable
/// of `pack` at `mode` (colours for the mode, then the shared design tokens).
pub fn root_style(pack: ThemePack, mode: Mode) -> String {
    PackTokens::builtin(pack).style_for(mode)
}

/// Resolve the app-root style with the theme editor's **custom pack override
/// slot**: when a custom [`PackTokens`] has been applied to the session it
/// wins wholesale; otherwise the built-in `pack` renders. Pure and
/// host-tested — the reactive layer in [`crate::app`] only feeds it signals.
pub fn resolve_root_style(custom: Option<&PackTokens>, pack: ThemePack, mode: Mode) -> String {
    match custom {
        Some(tokens) => tokens.style_for(mode),
        None => root_style(pack, mode),
    }
}

/// How the user wants light vs dark chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModeChoice {
    /// Follow the operating system's `prefers-color-scheme`.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// The user's complete appearance choice: a theme pack plus a mode policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeChoice {
    /// Which token pack to render with.
    pub pack: ThemePack,
    /// How to resolve light vs dark.
    pub mode: ModeChoice,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        Self {
            pack: DEFAULT_PACK,
            mode: ModeChoice::default(),
        }
    }
}

/// Resolve the effective [`Mode`] from the user's [`ModeChoice`] and the OS's
/// dark-mode preference. Pure — the whole point of the split.
pub fn effective_mode(choice: ModeChoice, os_prefers_dark: bool) -> Mode {
    match choice {
        ModeChoice::Light => Mode::Light,
        ModeChoice::Dark => Mode::Dark,
        ModeChoice::System => {
            if os_prefers_dark {
                Mode::Dark
            } else {
                Mode::Light
            }
        }
    }
}

/// Cycle to the next mode choice for the toggle: System → Light → Dark → …
pub fn next_mode(choice: ModeChoice) -> ModeChoice {
    match choice {
        ModeChoice::System => ModeChoice::Light,
        ModeChoice::Light => ModeChoice::Dark,
        ModeChoice::Dark => ModeChoice::System,
    }
}

/// Cycle to the next pack for the picker: Clean → Retro → High Contrast → …
pub fn next_pack(pack: ThemePack) -> ThemePack {
    match pack {
        ThemePack::Clean => ThemePack::Retro,
        ThemePack::Retro => ThemePack::HighContrast,
        ThemePack::HighContrast => ThemePack::Clean,
    }
}

/// A short button label for a mode choice.
pub fn mode_label(choice: ModeChoice) -> &'static str {
    match choice {
        ModeChoice::System => "\u{25D0} Auto",
        ModeChoice::Light => "\u{2600} Light",
        ModeChoice::Dark => "\u{263D} Dark",
    }
}

/// A short button label for a pack.
pub fn pack_label(pack: ThemePack) -> &'static str {
    match pack {
        ThemePack::Clean => "Clean",
        ThemePack::Retro => "Retro",
        ThemePack::HighContrast => "Contrast",
    }
}

/// Serialise a mode choice for persistence.
pub fn mode_to_str(choice: ModeChoice) -> &'static str {
    match choice {
        ModeChoice::System => "system",
        ModeChoice::Light => "light",
        ModeChoice::Dark => "dark",
    }
}

/// Parse a persisted mode choice; unknown strings yield `None`.
pub fn mode_from_str(s: &str) -> Option<ModeChoice> {
    match s {
        "system" => Some(ModeChoice::System),
        "light" => Some(ModeChoice::Light),
        "dark" => Some(ModeChoice::Dark),
        _ => None,
    }
}

/// Serialise a pack for persistence.
pub fn pack_to_str(pack: ThemePack) -> &'static str {
    match pack {
        ThemePack::Clean => "clean",
        ThemePack::Retro => "retro",
        ThemePack::HighContrast => "high-contrast",
    }
}

/// Parse a persisted pack; unknown strings yield `None`.
pub fn pack_from_str(s: &str) -> Option<ThemePack> {
    match s {
        "clean" => Some(ThemePack::Clean),
        "retro" => Some(ThemePack::Retro),
        "high-contrast" => Some(ThemePack::HighContrast),
        _ => None,
    }
}

/// Serialise the full choice for persistence: `pack:mode`.
pub fn choice_to_str(choice: ThemeChoice) -> String {
    format!("{}:{}", pack_to_str(choice.pack), mode_to_str(choice.mode))
}

/// Parse a persisted choice; unknown strings yield `None`.
///
/// Bare mode strings (`"dark"`) — the pre-pack storage format — still parse,
/// resolving to the default pack, so an existing user's mode survives the
/// upgrade.
pub fn choice_from_str(s: &str) -> Option<ThemeChoice> {
    match s.split_once(':') {
        Some((pack, mode)) => Some(ThemeChoice {
            pack: pack_from_str(pack)?,
            mode: mode_from_str(mode)?,
        }),
        None => Some(ThemeChoice {
            pack: DEFAULT_PACK,
            mode: mode_from_str(s)?,
        }),
    }
}

/// Browser-side theme persistence and OS preference query (`wasm32` only).
///
/// This is the untestable DOM edge over the pure resolution core above.
#[cfg(target_arch = "wasm32")]
pub mod storage {
    use super::{choice_from_str, choice_to_str, ThemeChoice};

    /// `localStorage` key the theme choice is stored under.
    const KEY: &str = "rh-theme";

    /// The persisted theme choice, if any.
    pub fn load_choice() -> Option<ThemeChoice> {
        let storage = web_sys::window()?.local_storage().ok()??;
        let raw = storage.get_item(KEY).ok()??;
        choice_from_str(&raw)
    }

    /// Persist the theme choice (best-effort; storage may be unavailable).
    pub fn save_choice(choice: ThemeChoice) {
        if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
            let _ = storage.set_item(KEY, &choice_to_str(choice));
        }
    }

    /// Whether the OS currently prefers a dark colour scheme.
    pub fn os_prefers_dark() -> bool {
        web_sys::window()
            .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
            .is_some_and(|mql| mql.matches())
    }
}

/// A compact, framework-free stylesheet mounted once by the app root. All
/// colours and metrics reference the `--rh-*` custom properties emitted by
/// [`root_style`].
pub const STYLESHEET: &str = "\
*{box-sizing:border-box}\
.rh-app{font-family:var(--rh-font-sans);font-size:var(--rh-font-size);\
color:var(--rh-text);background-color:var(--rh-bg);\
background-image:var(--rh-bg-image);min-height:100vh;\
display:flex;flex-direction:column}\
.rh-header{display:flex;align-items:center;gap:var(--rh-space-3);\
padding:.6rem var(--rh-space-4);\
background:var(--rh-surface);border-bottom:1px solid var(--rh-bg)}\
.rh-header .rh-title{font-weight:600;color:var(--rh-accent)}\
.rh-header .rh-status{color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-spacer{flex:1}\
.rh-dot{width:.6rem;height:.6rem;border-radius:50%;display:inline-block}\
.rh-dot.on{background:var(--rh-accent)}\
.rh-dot.off{background:var(--rh-muted)}\
.rh-btn{font:inherit;cursor:pointer;border:1px solid var(--rh-accent);\
background:var(--rh-accent);color:var(--rh-bg);border-radius:var(--rh-radius);\
padding:var(--rh-space-2) var(--rh-space-3)}\
.rh-btn.ghost{background:transparent;color:var(--rh-accent)}\
.rh-btn.small{padding:.2rem var(--rh-space-2);font-size:var(--rh-font-xs)}\
.rh-theme-menu{display:inline-flex;gap:.35rem;align-items:center}\
.rh-input{font:inherit;padding:.45rem var(--rh-space-2);\
border-radius:var(--rh-radius);\
border:1px solid var(--rh-muted);background:var(--rh-bg);color:var(--rh-text)}\
.rh-login{max-width:22rem;margin:4rem auto;display:flex;flex-direction:column;\
gap:var(--rh-space-3);background:var(--rh-surface);padding:var(--rh-space-6);\
border-radius:var(--rh-radius-lg)}\
.rh-login h1{margin:0 0 var(--rh-space-2);color:var(--rh-accent)}\
.rh-login label{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-body{flex:1;display:flex;min-height:0}\
.rh-chat{flex:1;display:flex;flex-direction:column;min-width:0}\
.rh-scroll{flex:1;overflow-y:auto;padding:var(--rh-space-4);display:flex;\
flex-direction:column;gap:var(--rh-space-2)}\
.rh-line .rh-from{color:var(--rh-accent);font-weight:600;margin-right:var(--rh-space-2)}\
.rh-compose{display:flex;gap:var(--rh-space-2);padding:var(--rh-space-3);\
border-top:1px solid var(--rh-surface)}\
.rh-compose .rh-input{flex:1}\
.rh-who{width:12rem;background:var(--rh-surface);padding:var(--rh-space-3);\
overflow-y:auto;border-left:1px solid var(--rh-bg)}\
.rh-who h2{font-size:var(--rh-font-xs);text-transform:uppercase;\
letter-spacing:.05em;color:var(--rh-muted);margin:.2rem 0 var(--rh-space-2)}\
.rh-who ul{list-style:none;margin:0;padding:0;display:flex;\
flex-direction:column;gap:.35rem}\
.rh-who li::before{content:'\\2022';color:var(--rh-accent);margin-right:var(--rh-space-2)}\
.rh-nav{display:flex;gap:var(--rh-space-3);align-items:center}\
.rh-nav a{color:var(--rh-muted);text-decoration:none;font-size:.9rem;\
padding:.2rem .1rem;border-bottom:2px solid transparent}\
.rh-nav a:hover{color:var(--rh-text)}\
.rh-nav a.active{color:var(--rh-accent);border-bottom-color:var(--rh-accent)}\
.rh-panel{flex:1;padding:var(--rh-space-4);overflow-y:auto;min-width:0}\
.rh-panel-title{font-size:var(--rh-font-xs);text-transform:uppercase;\
letter-spacing:.05em;color:var(--rh-muted);margin:.2rem 0 var(--rh-space-3)}\
.rh-tree{list-style:none;margin:0;padding:0;display:flex;\
flex-direction:column;gap:var(--rh-space-2)}\
.rh-board-link,.rh-thread-link,.rh-member-link,.rh-file-link{\
display:flex;flex-direction:column;\
gap:.15rem;width:100%;text-align:left;text-decoration:none;font:inherit;\
cursor:pointer;background:var(--rh-surface);color:var(--rh-text);\
border:1px solid var(--rh-surface);border-radius:var(--rh-radius);\
padding:.6rem var(--rh-space-3)}\
.rh-board-link:hover,.rh-thread-link:hover,.rh-member-link:hover,\
.rh-file-link:hover{border-color:var(--rh-accent)}\
.rh-thread-link.active,.rh-file-link.active{border-color:var(--rh-accent)}\
.rh-board-name,.rh-thread-title{font-weight:600;color:var(--rh-text)}\
.rh-board-desc,.rh-thread-author,.rh-member-handle{font-size:var(--rh-font-xs);\
color:var(--rh-muted)}\
.rh-back{display:inline-block;margin-bottom:.6rem;color:var(--rh-accent);\
text-decoration:none;font-size:var(--rh-font-sm)}\
.rh-threads{max-width:22rem;border-right:1px solid var(--rh-surface)}\
.rh-reader{flex:2}\
.rh-posts{display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-post{background:var(--rh-surface);border-radius:var(--rh-radius);\
padding:var(--rh-space-3) .9rem}\
.rh-post-body{margin:.3rem 0 0}\
.rh-empty{color:var(--rh-muted);font-style:italic}\
.rh-dm-peer,.rh-member-link{align-items:flex-start}\
.rh-dm-peer{width:100%;text-align:left;font:inherit;cursor:pointer;\
background:transparent;color:var(--rh-text);border:none;padding:.35rem 0}\
.rh-dm-peer:hover{color:var(--rh-accent)}\
.rh-dm-peer.active{color:var(--rh-accent);font-weight:600}\
.rh-member-link{flex-direction:row;align-items:center;gap:var(--rh-space-2)}\
.rh-member-name{font-weight:600}\
.rh-members{max-width:24rem;border-right:1px solid var(--rh-surface);\
display:flex;flex-direction:column;gap:.6rem}\
.rh-card{background:var(--rh-surface);border-radius:var(--rh-radius-lg);\
padding:1.2rem}\
.rh-card-name{margin:0;color:var(--rh-accent)}\
.rh-card-handle,.rh-card-status{margin:.2rem 0;color:var(--rh-muted);\
font-size:var(--rh-font-sm)}\
.rh-card-bio{margin:var(--rh-space-2) 0 0}\
.rh-files{max-width:32rem;border-right:1px solid var(--rh-surface);\
display:flex;flex-direction:column;gap:.6rem}\
.rh-crumbs{display:flex;flex-wrap:wrap;gap:.35rem;align-items:center;\
font-size:var(--rh-font-sm);margin-bottom:var(--rh-space-2)}\
.rh-crumb{color:var(--rh-accent);background:none;border:none;font:inherit;\
cursor:pointer;padding:0}\
.rh-crumb.sep{color:var(--rh-muted);cursor:default}\
.rh-toolbar{display:flex;gap:var(--rh-space-2);align-items:center;\
margin-bottom:var(--rh-space-2)}\
.rh-file-link{flex-direction:row;align-items:baseline;gap:var(--rh-space-2)}\
.rh-file-icon{font-size:1.1rem}\
.rh-file-name{font-weight:600;color:var(--rh-text)}\
.rh-file-meta{margin-left:auto;font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-meta-grid{display:grid;grid-template-columns:auto 1fr;gap:.3rem .8rem;\
font-size:var(--rh-font-sm);margin:var(--rh-space-3) 0}\
.rh-meta-grid dt{color:var(--rh-muted)}\
.rh-meta-grid dd{margin:0}\
.rh-queue{list-style:none;margin:var(--rh-space-3) 0 0;padding:0;display:flex;\
flex-direction:column;gap:var(--rh-space-2)}\
.rh-queue-item{background:var(--rh-surface);border-radius:var(--rh-radius);\
padding:var(--rh-space-2) var(--rh-space-3)}\
.rh-queue-head{display:flex;gap:var(--rh-space-2);align-items:baseline}\
.rh-queue-name{font-weight:600}\
.rh-queue-pct{margin-left:auto;font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-badge{font-size:var(--rh-font-xs);padding:.05rem .4rem;border-radius:.3rem;\
background:var(--rh-bg);color:var(--rh-muted);text-transform:uppercase;\
letter-spacing:.04em}\
.rh-badge.active{color:var(--rh-accent)}\
.rh-badge.done{color:var(--rh-text)}\
.rh-badge.failed{color:var(--rh-error)}\
.rh-bar{height:.4rem;border-radius:.2rem;background:var(--rh-bg);\
margin-top:var(--rh-space-2);overflow:hidden}\
.rh-bar-fill{height:100%;background:var(--rh-accent);transition:width .2s}\
.rh-bar-fill.failed{background:var(--rh-error)}\
.rh-art-wrap{padding:var(--rh-space-4);overflow:auto}\
.rh-art{background:#000;border:1px solid var(--rh-surface);\
border-radius:var(--rh-radius);image-rendering:pixelated;max-width:100%}\
.rh-dot.pending{background:var(--rh-accent);opacity:.6}\
.rh-conn{font-size:var(--rh-font-xs);color:var(--rh-muted);\
text-transform:uppercase;letter-spacing:.04em;margin-right:var(--rh-space-2)}\
.rh-admin-status{padding:var(--rh-space-2) var(--rh-space-4);\
color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-config-row,.rh-account-row{flex-direction:row;align-items:center;\
gap:var(--rh-space-2);flex-wrap:wrap}\
.rh-config-key{font-weight:600;min-width:12rem}\
.rh-account-role{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-editor{display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-editor-row{display:flex;gap:var(--rh-space-2);align-items:center}\
.rh-var-name{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);\
color:var(--rh-muted);min-width:8.5rem}\
.rh-swatch{width:1.1rem;height:1.1rem;flex:none;display:inline-block;\
border:1px solid var(--rh-muted);border-radius:var(--rh-radius)}\
.rh-warn{color:var(--rh-error);font-size:var(--rh-font-sm);margin:.2rem 0}\
.rh-textarea{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);\
width:100%;min-height:8rem;background:var(--rh-bg);color:var(--rh-text);\
border:1px solid var(--rh-muted);border-radius:var(--rh-radius);\
padding:var(--rh-space-2)}\
.rh-preview{font-family:var(--rh-font-sans);font-size:var(--rh-font-sm);\
color:var(--rh-text);background-color:var(--rh-bg);\
background-image:var(--rh-bg-image);border:1px solid var(--rh-muted);\
border-radius:var(--rh-radius-lg);overflow:hidden;margin:var(--rh-space-2) 0}\
.rh-preview-body{padding:var(--rh-space-3);display:flex;\
flex-direction:column;gap:var(--rh-space-2);align-items:flex-start}\
";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const PACKS: [ThemePack; 3] = [ThemePack::Clean, ThemePack::Retro, ThemePack::HighContrast];
    const MODES: [Mode; 2] = [Mode::Light, Mode::Dark];

    #[test]
    fn effective_mode_honors_explicit_choice() {
        assert_eq!(effective_mode(ModeChoice::Light, true), Mode::Light);
        assert_eq!(effective_mode(ModeChoice::Dark, false), Mode::Dark);
    }

    #[test]
    fn system_choice_follows_os() {
        assert_eq!(effective_mode(ModeChoice::System, true), Mode::Dark);
        assert_eq!(effective_mode(ModeChoice::System, false), Mode::Light);
    }

    #[test]
    fn mode_choice_cycles_through_all_three() {
        let mut c = ModeChoice::default();
        assert_eq!(c, ModeChoice::System);
        c = next_mode(c);
        assert_eq!(c, ModeChoice::Light);
        c = next_mode(c);
        assert_eq!(c, ModeChoice::Dark);
        c = next_mode(c);
        assert_eq!(c, ModeChoice::System);
    }

    #[test]
    fn pack_cycles_through_all_three() {
        let mut p = DEFAULT_PACK;
        assert_eq!(p, ThemePack::Clean);
        p = next_pack(p);
        assert_eq!(p, ThemePack::Retro);
        p = next_pack(p);
        assert_eq!(p, ThemePack::HighContrast);
        p = next_pack(p);
        assert_eq!(p, ThemePack::Clean);
    }

    #[test]
    fn choice_serialisation_roundtrips_all_nine_combinations() {
        for pack in PACKS {
            for mode in [ModeChoice::System, ModeChoice::Light, ModeChoice::Dark] {
                let choice = ThemeChoice { pack, mode };
                assert_eq!(choice_from_str(&choice_to_str(choice)), Some(choice));
            }
        }
    }

    #[test]
    fn legacy_bare_mode_strings_resolve_to_the_default_pack() {
        // The pre-pack storage format was just the mode.
        for (raw, mode) in [
            ("system", ModeChoice::System),
            ("light", ModeChoice::Light),
            ("dark", ModeChoice::Dark),
        ] {
            assert_eq!(
                choice_from_str(raw),
                Some(ThemeChoice {
                    pack: DEFAULT_PACK,
                    mode
                })
            );
        }
    }

    #[test]
    fn unknown_persisted_strings_are_rejected() {
        assert_eq!(choice_from_str("nonsense"), None);
        assert_eq!(choice_from_str("retro:banana"), None);
        assert_eq!(choice_from_str("banana:dark"), None);
        assert_eq!(choice_from_str(""), None);
    }

    /// Every `--rh-*` variable the stylesheet references.
    fn referenced_vars(css: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = css;
        while let Some(i) = rest.find("var(--") {
            let name = &rest[i + 4..];
            let end = name
                .find([')', ','])
                .expect("var() reference is terminated");
            out.insert(name[..end].to_string());
            rest = &name[end..];
        }
        out
    }

    #[test]
    fn every_referenced_variable_exists_in_every_pack_and_mode() {
        let vars = referenced_vars(STYLESHEET);
        assert!(
            vars.len() >= 15,
            "sanity: the stylesheet references a real token set, got {vars:?}"
        );
        for pack in PACKS {
            for mode in MODES {
                let style = root_style(pack, mode);
                for var in &vars {
                    assert!(
                        style.contains(&format!("{var}:")),
                        "{pack:?}/{mode:?} is missing {var}"
                    );
                }
            }
        }
    }

    #[test]
    fn root_style_carries_palette_and_tokens() {
        let style = root_style(DEFAULT_PACK, Mode::Dark);
        assert!(style.contains("--rh-accent:"));
        assert!(style.contains("--rh-space-4:"));
        assert!(style.contains("--rh-font-mono:"));
    }

    #[test]
    fn light_and_dark_styles_differ_in_every_pack() {
        for pack in PACKS {
            assert_ne!(
                root_style(pack, Mode::Light),
                root_style(pack, Mode::Dark),
                "{pack:?}"
            );
        }
    }

    #[test]
    fn custom_override_slot_wins_over_the_builtin_pack() {
        // No override: the built-in pack renders.
        assert_eq!(
            resolve_root_style(None, ThemePack::Retro, Mode::Dark),
            root_style(ThemePack::Retro, Mode::Dark)
        );
        // An applied custom pack overrides wholesale, per mode.
        let mut custom = PackTokens::builtin(ThemePack::Clean);
        custom.dark.insert("--rh-accent".into(), "#ff00ff".into());
        for mode in MODES {
            let style = resolve_root_style(Some(&custom), ThemePack::Retro, mode);
            assert_eq!(style, custom.style_for(mode), "{mode:?}");
        }
        assert!(
            resolve_root_style(Some(&custom), ThemePack::Retro, Mode::Dark)
                .contains("--rh-accent:#ff00ff;")
        );
        // Light mode is untouched by the dark-only edit.
        assert_eq!(
            resolve_root_style(Some(&custom), ThemePack::Retro, Mode::Light),
            root_style(ThemePack::Clean, Mode::Light)
        );
    }

    #[test]
    fn packs_render_distinct_styles() {
        for mode in MODES {
            let styles: BTreeSet<String> = PACKS.iter().map(|&p| root_style(p, mode)).collect();
            assert_eq!(styles.len(), PACKS.len(), "{mode:?}");
        }
    }
}
