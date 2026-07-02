//! Design tokens, theme resolution, and the app stylesheet.
//!
//! The colour palette lives in [`rabbithole_core::theme`] so every client means
//! the same thing by "accent" or "surface". This module turns a resolved
//! [`Palette`] plus a set of non-colour **design tokens** (spacing, typography,
//! radii) into CSS custom properties (`--rh-*`) that the static
//! [`STYLESHEET`] consumes. Applying the variables as an inline `style` on the
//! app root re-themes the whole subtree reactively — no `web_sys` DOM poking.
//!
//! ## Light / dark
//!
//! Appearance is a two-part decision, kept **pure and host-tested**:
//!
//! - the user's [`ThemeChoice`] — follow the OS, or force light/dark, and
//! - the OS `prefers-color-scheme` hint,
//!
//! combined by [`effective_mode`]. The choice is persisted to `localStorage`
//! and the OS hint read via `matchMedia`, both wasm-gated in [`storage`] behind
//! this pure core.
//!
//! ## Packs
//!
//! [`ThemePack`] (Clean / Retro / HighContrast) already lives in the core; the
//! SPA ships [`DEFAULT_PACK`] (Clean) but [`root_style`] takes the pack as a
//! parameter, so wiring a pack selector later is additive.

use rabbithole_core::theme::{Mode, Palette, Rgb, ThemePack};

/// The pack the SPA renders with today. Retro / HighContrast are one signal
/// away once a selector is added.
pub const DEFAULT_PACK: ThemePack = ThemePack::Clean;

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Render a palette as `--rh-*: #rrggbb; …` for a `style` attribute.
pub fn palette_vars(p: &Palette) -> String {
    format!(
        "--rh-bg:{};--rh-surface:{};--rh-text:{};--rh-muted:{};--rh-accent:{};--rh-error:{};",
        hex(p.background),
        hex(p.surface),
        hex(p.text),
        hex(p.muted),
        hex(p.accent),
        hex(p.error),
    )
}

/// Non-colour design tokens (spacing, radii, typography). Mode-independent, so
/// they are a single static string appended after the palette variables.
pub const DESIGN_TOKENS: &str = "\
--rh-space-1:.25rem;--rh-space-2:.5rem;--rh-space-3:.75rem;--rh-space-4:1rem;\
--rh-space-6:1.5rem;--rh-radius:.4rem;--rh-radius-lg:.6rem;\
--rh-font-sans:system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;\
--rh-font-mono:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;\
--rh-font-size:1rem;--rh-font-sm:.85rem;--rh-font-xs:.8rem;";

/// The full inline `style` string for the app root: the palette for `pack` at
/// `mode`, followed by the design tokens.
pub fn root_style(pack: ThemePack, mode: Mode) -> String {
    format!(
        "{}{DESIGN_TOKENS}",
        palette_vars(&Palette::builtin(pack, mode))
    )
}

/// How the user wants the appearance chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Follow the operating system's `prefers-color-scheme`.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// Resolve the effective [`Mode`] from the user's [`ThemeChoice`] and the OS's
/// dark-mode preference. Pure — the whole point of the split.
pub fn effective_mode(choice: ThemeChoice, os_prefers_dark: bool) -> Mode {
    match choice {
        ThemeChoice::Light => Mode::Light,
        ThemeChoice::Dark => Mode::Dark,
        ThemeChoice::System => {
            if os_prefers_dark {
                Mode::Dark
            } else {
                Mode::Light
            }
        }
    }
}

/// Cycle to the next choice for the toggle: System → Light → Dark → System.
pub fn next_choice(choice: ThemeChoice) -> ThemeChoice {
    match choice {
        ThemeChoice::System => ThemeChoice::Light,
        ThemeChoice::Light => ThemeChoice::Dark,
        ThemeChoice::Dark => ThemeChoice::System,
    }
}

/// A short button label for a choice.
pub fn choice_label(choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::System => "\u{25D0} Auto",
        ThemeChoice::Light => "\u{2600} Light",
        ThemeChoice::Dark => "\u{263D} Dark",
    }
}

/// Serialise a choice for persistence.
pub fn choice_to_str(choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::System => "system",
        ThemeChoice::Light => "light",
        ThemeChoice::Dark => "dark",
    }
}

/// Parse a persisted choice; unknown strings yield `None`.
pub fn choice_from_str(s: &str) -> Option<ThemeChoice> {
    match s {
        "system" => Some(ThemeChoice::System),
        "light" => Some(ThemeChoice::Light),
        "dark" => Some(ThemeChoice::Dark),
        _ => None,
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
            let _ = storage.set_item(KEY, choice_to_str(choice));
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
color:var(--rh-text);background:var(--rh-bg);min-height:100vh;\
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
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_mode_honors_explicit_choice() {
        assert_eq!(effective_mode(ThemeChoice::Light, true), Mode::Light);
        assert_eq!(effective_mode(ThemeChoice::Dark, false), Mode::Dark);
    }

    #[test]
    fn system_choice_follows_os() {
        assert_eq!(effective_mode(ThemeChoice::System, true), Mode::Dark);
        assert_eq!(effective_mode(ThemeChoice::System, false), Mode::Light);
    }

    #[test]
    fn choice_cycles_through_all_three() {
        let mut c = ThemeChoice::default();
        assert_eq!(c, ThemeChoice::System);
        c = next_choice(c);
        assert_eq!(c, ThemeChoice::Light);
        c = next_choice(c);
        assert_eq!(c, ThemeChoice::Dark);
        c = next_choice(c);
        assert_eq!(c, ThemeChoice::System);
    }

    #[test]
    fn choice_serialisation_roundtrips() {
        for c in [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark] {
            assert_eq!(choice_from_str(choice_to_str(c)), Some(c));
        }
        assert_eq!(choice_from_str("nonsense"), None);
    }

    #[test]
    fn root_style_carries_palette_and_tokens() {
        let style = root_style(DEFAULT_PACK, Mode::Dark);
        assert!(style.contains("--rh-accent:"));
        assert!(style.contains("--rh-space-4:"));
        assert!(style.contains("--rh-font-mono:"));
    }

    #[test]
    fn light_and_dark_styles_differ() {
        let light = root_style(DEFAULT_PACK, Mode::Light);
        let dark = root_style(DEFAULT_PACK, Mode::Dark);
        assert_ne!(light, dark);
    }
}
