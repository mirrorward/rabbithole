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
use crate::server_theme::ServerOverlay;

/// The pack a fresh session renders with before any persisted choice.
pub const DEFAULT_PACK: ThemePack = ThemePack::Clean;

/// The full inline `style` string for the app root: every `--rh-*` variable
/// of `pack` at `mode` (colours for the mode, then the shared design tokens).
pub fn root_style(pack: ThemePack, mode: Mode) -> String {
    PackTokens::builtin(pack).style_for(mode)
}

/// Resolve the app-root style from the three appearance layers, in priority
/// order:
///
/// 1. the theme editor's **custom pack override slot** — when a custom
///    [`PackTokens`] is applied (a live edit preview) it wins wholesale, so the
///    editor shows exactly what is being edited, unlayered;
/// 2. otherwise a **server theme overlay** (PLAN §9.11) whenever the burrow
///    ships one, layered on top of the built-in `pack` — the operator's
///    accent/metric tokens nudge the chosen pack without replacing it. A
///    burrow's theme is how that place looks, so it always applies; the user's
///    pack is the app's default for where a burrow supplies nothing;
/// 3. otherwise the plain built-in `pack`.
///
/// Pure and host-tested — the reactive layer in [`crate::app`] only feeds it
/// signals (passing `None` for the server overlay switches server theming off).
pub fn resolve_root_style(
    custom: Option<&PackTokens>,
    server: Option<&ServerOverlay>,
    pack: ThemePack,
    mode: Mode,
) -> String {
    match (custom, server) {
        (Some(tokens), _) => tokens.style_for(mode),
        (None, Some(overlay)) => overlay.over(&PackTokens::builtin(pack)).style_for(mode),
        (None, None) => root_style(pack, mode),
    }
}

/// The `--rh-bg` value inside a resolved root style string — the colour of
/// "anywhere the app hasn't painted".
///
/// `index.html` paints `html` with a fixed dark pre-boot backdrop so the first
/// frame isn't a white flash. That backdrop must be *replaced* once the theme
/// is known: any later gap between the viewport and the app — a stale-cache
/// body margin, a `dvh` shortfall in some webview, rubber-banding — otherwise
/// shows up as a black border around a light app. The app root re-paints
/// `html`/`body` with this value so a gap degrades to the theme's own
/// background instead of evidence.
pub fn background_of(root_style: &str) -> &str {
    root_style
        .split("--rh-bg:")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("#14161b")
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
/// The mode's plain name, for a label or tooltip beside an icon.
pub fn mode_name(choice: ModeChoice) -> &'static str {
    match choice {
        ModeChoice::System => "Auto",
        ModeChoice::Light => "Light",
        ModeChoice::Dark => "Dark",
    }
}

/// A short button label for a mode, with its glyph. For text buttons only —
/// beside an icon the glyph is a second, worse copy of the same idea, and it
/// lands in the accessible name where it reads as punctuation.
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
///
/// Accessibility blocks (host-asserted by the shape tests below):
/// `:focus-visible` outlines on the `--rh-focus` token, the `.rh-skip` skip
/// link, the `.rh-visually-hidden` screen-reader-only helper,
/// `[aria-current=page]` styling for the active nav link, and a
/// `prefers-reduced-motion: reduce` block that neutralises all motion.
pub const STYLESHEET: &str = "\
/* The display face, self-hosted (assets/space-grotesk-latin.woff2, OFL). Latin\
   only, 500..700 variable; the system sans stands in for everything else. */\
/* impeccable-disable-next-line overused-font: Space Grotesk is pinned by docs/design/client-experience.md §8 */\
@font-face{font-family:'Space Grotesk';font-style:normal;font-weight:500 700;font-display:swap;src:url(/space-grotesk-latin.woff2) format('woff2');unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD}\
*{box-sizing:border-box}\
/* The browser's default `body{margin:8px}` was never reset, so the app -- which\
   is exactly 100dvh tall -- sat inside a body 16px taller than the viewport.\
   That made the whole window scroll by a few pixels and exposed a dark border\
   on every side: index.html paints `html` with the pre-boot backdrop, and that\
   is what was showing through. */\
html,body{margin:0;padding:0;height:100%}\
body{overflow:hidden}\
.rh-app{font-family:var(--rh-font-sans);font-size:var(--rh-font-size);line-height:1.5;color:var(--rh-text);background-color:var(--rh-bg);background-image:var(--rh-bg-image);height:100vh;height:100dvh;display:flex;flex-direction:column;-webkit-font-smoothing:antialiased;text-rendering:optimizeLegibility}\
.rh-shell{flex:1;display:flex;min-height:0}\
.rh-shell-main{flex:1;min-width:0;min-height:0;display:flex;flex-direction:column;overflow-y:auto}\
.rh-rail{flex:none;width:3.4rem;display:flex;flex-direction:column;align-items:center;gap:var(--rh-space-2);padding:var(--rh-space-3) 0;background:var(--rh-surface-2);border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-rail-hidden{display:none}\
.rh-rail-settings{margin-top:auto}\
.rh-rail-server,.rh-tab-tile,.rh-sheet-tile{font-family:var(--rh-font-display)}\
/* Segmented choices in Settings (theme pack, light/dark). */\
.rh-seg{display:inline-flex;margin:.15rem .6rem .5rem 0;border:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent);border-radius:var(--rh-radius);overflow:hidden;vertical-align:middle}\
.rh-seg-btn{padding:.38rem .85rem;border:0;background:transparent;font-family:inherit;font-size:var(--rh-font-sm);font-weight:500;color:var(--rh-muted);cursor:pointer;transition:background-color .12s ease,color .12s ease}\
.rh-seg-btn+.rh-seg-btn{border-left:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent)}\
.rh-seg-btn:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-seg-btn.on{background:color-mix(in srgb,var(--rh-accent) 14%,transparent);color:var(--rh-accent);font-weight:600}\
.rh-rail-dot{position:absolute;right:1px;bottom:1px;width:9px;height:9px;border-radius:50%;box-shadow:0 0 0 2px var(--rh-surface-2)}\
.rh-rail-dot.on{background:#3fbf7f}\
.rh-rail-badge{position:absolute;top:-5px;right:-5px;min-width:17px;height:17px;padding:0 4px;border-radius:var(--rh-radius-full);background:var(--rh-error);color:#fff;font-size:.62rem;font-weight:800;line-height:17px;text-align:center;box-shadow:0 0 0 2px var(--rh-surface-2);animation:rh-pop .18s cubic-bezier(.22,1,.36,1) both}\
.rh-rail-dot.pending{background:var(--rh-accent)}\
.rh-rail-dot.off{background:var(--rh-muted)}\
.rh-presence{font:inherit;font-size:var(--rh-font-sm);line-height:1;color:var(--rh-text);background-color:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 16%,transparent);border-radius:var(--rh-radius);padding:0 .45rem 0 .6rem;gap:.45rem;display:inline-flex;align-items:center;cursor:pointer}\
.rh-presence-wrap{position:relative;flex:none}\
.rh-presence-chevron{display:grid;color:var(--rh-muted)}\
.rh-presence-chevron svg{width:.85rem;height:.85rem}\
.rh-pres.hidden{background:transparent;box-shadow:inset 0 0 0 1.5px var(--rh-muted)}\
.rh-menu{position:absolute;top:calc(100% + .35rem);right:0;z-index:40;min-width:11rem;margin:0;padding:.3rem;list-style:none;background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius-lg);box-shadow:var(--rh-shadow-3);animation:rh-pop-in .12s cubic-bezier(.2,.9,.3,1) both}\
.rh-menu-item{display:flex;align-items:center;gap:.55rem;width:100%;padding:.4rem .5rem;border:0;border-radius:var(--rh-radius-sm);background:transparent;color:var(--rh-text);font:inherit;font-size:var(--rh-font-sm);text-align:left;cursor:default}\
.rh-menu-item:hover,.rh-menu-item:focus-visible{background:color-mix(in srgb,var(--rh-accent) 14%,transparent)}\
.rh-menu-item:focus-visible{outline-offset:-2px}\
.rh-menu-label{flex:1}\
.rh-menu-check{display:grid;width:1rem;color:var(--rh-accent)}\
.rh-menu-check svg{width:1rem;height:1rem}\
.rh-presence:hover{border-color:color-mix(in srgb,var(--rh-accent) 45%,transparent)}\
.rh-who-row{display:flex;align-items:center;gap:.45rem}\
.rh-pres{width:.5rem;height:.5rem;border-radius:50%;flex:none}\
.rh-pres.on{background:#3fbf7f}\
.rh-pres.away{background:#e8b84b}\
.rh-pres.idle{background:var(--rh-muted)}\
.rh-pres.off{background:var(--rh-muted);opacity:.5}\
.rh-rail-glyph{display:grid;place-items:center;line-height:0}\
.rh-rail-glyph svg{width:20px;height:20px}\
.rh-rail-unified{border-radius:var(--rh-radius-full);color:var(--rh-brand);background:color-mix(in srgb,var(--rh-brand) 12%,transparent);font-size:1.05rem}\
.rh-people{list-style:none;margin:0;padding:0;display:flex;flex-direction:column}\
.rh-person{display:flex;align-items:center;gap:.6rem;padding:.5rem .3rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-person-name{font-weight:600}\
.rh-welcome{margin:.75rem;padding:.85rem 1rem;border:1px solid color-mix(in srgb,var(--rh-accent) 35%,transparent);border-radius:var(--rh-radius,8px);background:color-mix(in srgb,var(--rh-accent) 7%,var(--rh-bg));box-shadow:0 1px 3px rgba(0,0,0,.06)}\
.rh-welcome-head{display:flex;align-items:center;gap:.5rem;margin-bottom:.4rem}\
.rh-welcome-title{font-weight:700;letter-spacing:.01em}\
.rh-welcome-x{margin-left:auto;border:0;background:transparent;color:var(--rh-muted);font-size:1.2rem;line-height:1;cursor:pointer;padding:.1rem .3rem;border-radius:4px}\
.rh-welcome-x:hover{background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-text)}\
.rh-welcome-motd{color:var(--rh-muted);margin:0 0 .5rem;font-style:italic}\
.rh-welcome-body{margin:0 0 .7rem;white-space:pre-wrap;max-width:70ch;line-height:1.5}\
.rh-front{display:flex;flex-direction:column;gap:.4rem;margin:0;padding:var(--rh-space-4) var(--rh-space-5);border:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);border-radius:0;background:color-mix(in srgb,var(--rh-accent) 4%,var(--rh-surface))}\
.rh-front-head{display:flex;align-items:baseline;gap:.5rem;margin-bottom:.15rem}\
.rh-front-eyebrow{font-size:var(--rh-font-sm);font-weight:700;color:var(--rh-text)}\
.rh-front-where{font-size:var(--rh-font-xs,.72rem);color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-front-motd{margin:0;line-height:1.5;max-width:70ch;white-space:pre-wrap}\
.rh-front-label{font-size:var(--rh-font-xs,.72rem);color:var(--rh-accent);font-weight:700}\
.rh-front-featured{display:flex;flex-direction:column;gap:.15rem}\
.rh-front-title{margin:0;font-weight:700}\
.rh-front-body{margin:0;color:var(--rh-muted);max-width:62ch;line-height:1.45}\
.rh-front-line{margin:0;font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-front-ticker{margin:.1rem 0 0;font-size:var(--rh-font-sm);font-style:italic;color:var(--rh-accent);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-welcome-actions{display:flex;justify-content:flex-end}\
.rh-person-idkey{color:var(--rh-muted);cursor:help;font-size:.9em}\
.rh-pip{display:inline-flex;align-items:center;justify-content:center;min-width:1.05rem;height:1.05rem;padding:0 .25rem;border-radius:999px;background:var(--rh-accent);color:var(--rh-bg);font-size:.65rem;font-weight:700;line-height:1;vertical-align:.05em;font-variant-numeric:tabular-nums}\
.rh-threadtable .rh-thread-link{display:flex;flex-direction:column;align-items:flex-start;gap:.1rem;width:100%;padding:.4rem .6rem;text-align:left;background:transparent;border:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent);cursor:pointer;font:inherit;color:inherit}\
.rh-thread-meta{font-size:var(--rh-font-sm);color:var(--rh-muted);display:flex;align-items:center;gap:.35rem}\
.rh-dot-sep{opacity:.55}\
.rh-threadtable .rh-thread-title{font-weight:600;max-width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-threadtable .rh-thread-link:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-threadtable .rh-thread-link.active{background:color-mix(in srgb,var(--rh-accent) 14%,transparent)}\
.rh-filetable-head,.rh-filetable .rh-file-link{display:grid;grid-template-columns:minmax(0,1fr) 5.5rem 5rem 8rem 6rem;gap:var(--rh-space-3);align-items:center}\
.rh-filetable-head{padding:.3rem .6rem;font-size:var(--rh-font-xs,.72rem);font-weight:600;color:var(--rh-muted);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent)}\
.rh-filetable .rh-file-link{width:100%;min-height:30px;padding:.25rem .6rem;text-align:left;background:transparent;border:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent);cursor:pointer;font:inherit;color:inherit}\
.rh-filetable .rh-file-link:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-filetable .rh-file-link.active{background:color-mix(in srgb,var(--rh-accent) 14%,transparent)}\
.rh-fcol-name{display:flex;align-items:center;gap:.45rem;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-fcol-size,.rh-fcol-kind,.rh-fcol-who,.rh-fcol-when{font-size:var(--rh-font-sm);color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-fcol-size{text-align:right;font-variant-numeric:tabular-nums}\
.rh-dropzone{display:flex;flex-direction:column;gap:var(--rh-space-3);flex:1;min-height:0;border-radius:var(--rh-radius-lg);transition:background-color .15s ease,box-shadow .15s ease}\
.rh-dropzone.dragging{background:color-mix(in srgb,var(--rh-accent) 8%,transparent);box-shadow:inset 0 0 0 2px color-mix(in srgb,var(--rh-accent) 45%,transparent)}\
.rh-toolbar-hint{font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-file-filter{width:100%;margin:.35rem 0 .5rem}\
.rh-mark{flex:none;display:inline-flex;line-height:0;border-radius:5px;overflow:hidden}\
.rh-mark svg{display:block}\
.rh-who-row{display:flex;align-items:center;gap:.5rem}\
.rh-line-mark{vertical-align:-.3em;margin-right:.35rem}\
.rh-you-mark{flex:none;border-radius:10px;overflow:hidden;box-shadow:0 0 0 2px color-mix(in srgb,var(--rh-brand) 45%,transparent),0 1px 3px color-mix(in srgb,var(--rh-text) 18%,transparent)}\
.rh-chat-empty{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:.35rem;min-height:60%;padding:var(--rh-space-6);text-align:center;animation:rh-fade-up .3s ease both}\
.rh-chat-empty-mark{display:grid;place-items:center;width:3rem;height:3rem;margin-bottom:.35rem;border-radius:var(--rh-radius-full);background:color-mix(in srgb,var(--rh-accent) 10%,transparent);color:var(--rh-accent)}\
.rh-chat-empty-mark svg{width:24px;height:24px}\
.rh-chat-empty-title{font-weight:700;margin:.3rem 0 0}\
.rh-chat-empty-sub{color:var(--rh-muted);margin:0;max-width:32ch}\
.rh-chat-empty-action{margin-top:var(--rh-space-3)}\
.rh-dm-gate{flex:1;display:flex;flex-direction:column;justify-content:center;min-width:0}\
.rh-person,.rh-xfer-item,.rh-who-row,.rh-tree-item{border-radius:0;transition:background-color .13s ease}\
.rh-person:hover,.rh-xfer-item:hover,.rh-who-row:hover,.rh-tree-item:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-empty{color:var(--rh-muted);padding:var(--rh-space-4);text-align:center;animation:rh-fade-up .25s ease both}\
.rh-person-servers{margin-left:auto;font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-xfers{list-style:none;margin:0;padding:0;display:flex;flex-direction:column}\
.rh-xfer-row{display:flex;align-items:center;gap:.7rem;padding:.5rem .3rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-xfer-dir{color:var(--rh-brand);font-family:var(--rh-font-mono,monospace)}\
.rh-xfer-name{font-weight:600;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:0 1 14rem}\
.rh-xfer-burrow{font-size:var(--rh-font-sm);color:var(--rh-muted);flex:0 0 auto}\
.rh-xfer-row .rh-bar{flex:1;min-width:4rem}\
.rh-xfer-pct{font-size:var(--rh-font-sm);color:var(--rh-muted);width:3ch;text-align:right}\
.rh-you{display:flex;gap:1rem;align-items:center;margin-bottom:.9rem}\
.rh-you-badge{width:3.2rem;height:3.2rem;flex:0 0 auto;border-radius:50%;display:flex;align-items:center;justify-content:center;font-family:var(--rh-font-mono,ui-monospace,monospace);font-weight:700;text-transform:uppercase;color:#fff;background:linear-gradient(135deg,var(--rh-accent),color-mix(in srgb,var(--rh-accent) 55%,#000))}\
.rh-you-fields{display:flex;flex-direction:column;gap:.35rem;min-width:0}\
.rh-you-row{display:flex;gap:.6rem;align-items:baseline}\
.rh-you-label{font-size:var(--rh-font-xs,.72rem);text-transform:uppercase;letter-spacing:.04em;color:var(--rh-muted);width:6.5rem;flex:0 0 auto}\
.rh-you-fp{font-family:var(--rh-font-mono,ui-monospace,monospace);font-weight:600}\
.rh-you-pub{font-family:var(--rh-font-mono,ui-monospace,monospace);font-size:var(--rh-font-sm);color:var(--rh-muted);overflow-wrap:anywhere}\
.rh-you-note{color:var(--rh-muted);max-width:60ch;line-height:1.5}\
.rh-xfer-item{border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent);padding:.5rem .3rem}\
.rh-xfer-item .rh-xfer-row{border-bottom:0;padding:0}\
.rh-xfer-detail{display:flex;gap:.7rem;align-items:center;margin-top:.25rem;padding-left:1.4rem}\
.rh-xfer-hash{font-family:var(--rh-font-mono,ui-monospace,monospace);font-size:var(--rh-font-xs,.72rem);color:var(--rh-muted)}\
.rh-swarmpill{font-family:var(--rh-font-mono,ui-monospace,monospace);font-size:var(--rh-font-xs,.72rem);color:var(--rh-brand);background:color-mix(in srgb,var(--rh-brand) 12%,transparent);border-radius:999px;padding:.05rem .5rem}\
.rh-rail-tile{transition:background-color .12s ease,color .12s ease,box-shadow .12s ease;width:40px;height:40px;display:grid;place-items:center;border:0;padding:0;cursor:pointer;border-radius:12px;background:color-mix(in srgb,var(--rh-text) 5%,transparent);color:var(--rh-muted);font-family:var(--rh-font-sans);font-weight:700;font-size:.95rem;position:relative}\
.rh-rail-tile:hover{color:var(--rh-text)}\
.rh-rail-home,.rh-rail-add{border-radius:var(--rh-radius-full)}\
.rh-rail-server{color:var(--rh-text);background:color-mix(in srgb,var(--rh-accent) 16%,var(--rh-surface));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--rh-accent) 30%,transparent)}\
.rh-rail-server.active::before{content:\"\";position:absolute;left:-9px;top:8px;bottom:8px;width:3px;border-radius:3px;background:var(--rh-brand)}\
.rh-rail-add{color:var(--rh-muted);background:transparent;box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--rh-text) 12%,transparent)}\
.rh-rail-sep{width:22px;height:1px;background:color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-rail-home{color:var(--rh-accent)}\
:focus-visible{outline:2px solid var(--rh-focus);outline-offset:2px}\
.rh-visually-hidden{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap;border:0}\
.rh-skip{position:fixed;left:-999rem;top:var(--rh-space-2);z-index:99;background:var(--rh-accent);color:var(--rh-bg);padding:var(--rh-space-2) var(--rh-space-3);border-radius:var(--rh-radius);text-decoration:none;font-weight:600;box-shadow:var(--rh-shadow-2)}\
.rh-skip:focus{left:var(--rh-space-2)}\
.rh-header{display:flex;align-items:center;gap:var(--rh-space-3);padding:0 var(--rh-space-5);min-height:3.5rem;position:sticky;top:0;z-index:20;background:color-mix(in srgb,var(--rh-surface) 82%,transparent);backdrop-filter:saturate(1.4) blur(14px);-webkit-backdrop-filter:saturate(1.4) blur(14px);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-header .rh-title{order:1;display:inline-flex;align-items:center;gap:.55rem;white-space:nowrap;font-family:var(--rh-font-display);font-weight:700;font-size:var(--rh-font-lg);letter-spacing:-.02em;color:var(--rh-text)}\
.rh-header .rh-title::before{content:'';flex:none;width:1.4rem;height:1.4rem;border-radius:var(--rh-radius-full);background:radial-gradient(circle at 50% 52%,var(--rh-surface) 0 15%,var(--rh-accent) 15% 27%,var(--rh-surface) 27% 41%,color-mix(in srgb,var(--rh-accent) 62%,var(--rh-surface)) 41% 58%,var(--rh-surface) 58% 73%,color-mix(in srgb,var(--rh-accent) 34%,var(--rh-surface)) 73% 100%);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 35%,transparent),0 2px 8px -2px color-mix(in srgb,var(--rh-accent) 60%,transparent)}\
.rh-title-text{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-dot{width:.5rem;height:.5rem;border-radius:50%;display:inline-block;flex:none}\
/* Only the header reorders its children; a dot in a list row stays where\
   the markup put it (it used to float to the far end of a server row). */\
.rh-header .rh-dot{order:2;margin-left:.2rem}\
.rh-dot.on{background:#3fbf7f;box-shadow:0 0 0 3px color-mix(in srgb,#3fbf7f 22%,transparent)}\
.rh-dot.off{background:var(--rh-muted)}\
.rh-dot.pending{background:var(--rh-accent);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-accent) 22%,transparent)}\
.rh-conn{order:3;font-size:var(--rh-font-sm);font-weight:500;color:var(--rh-muted);white-space:nowrap}\
.rh-status{order:4;color:var(--rh-muted);font-size:var(--rh-font-sm);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:14rem}\
.rh-spacer{order:5;flex:1}\
.rh-live-slot{order:6;min-width:0;flex:0 1 auto;overflow:hidden}\
/* The header's flexible parts must actually be able to shrink. Everything in\
   it was `white-space:nowrap` with no `min-width:0`, so a long now-playing\
   line -- a radio track title -- pushed the trailing controls off\
   the right edge of the window -- the same overflow that moved the section nav\
   out of here, reappearing with one long string. Title, status and now-playing\
   give way; the controls never do. */\
.rh-header{overflow-x:clip;overflow-y:visible}\
.rh-header .rh-title{min-width:0;flex:0 1 auto;overflow:visible}\
.rh-header .rh-title-text{min-width:0;overflow:hidden;text-overflow:ellipsis}\
.rh-status{flex:0 1 auto}\
.rh-presence,.rh-kbd-jump,.rh-dot,.rh-conn{flex:none}\
/* Presence and Leave sit with the icon cluster on the right: with no `order`\
   they rendered first, before the burrow's own name. */\
.rh-header .rh-presence-wrap{order:7}\
.rh-header .rh-leave{order:9;margin-left:.2rem}\
.rh-nav{order:7;display:flex;gap:.15rem;align-items:center}\
.rh-nav a,.rh-nav .rh-nav-item{color:var(--rh-muted);display:inline-flex;align-items:center;gap:.3rem;white-space:nowrap;text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;padding:.35rem .7rem;border-radius:var(--rh-radius-full);transition:background-color .15s ease,color .15s ease;border-bottom:0}\
.rh-nav a:hover,.rh-nav .rh-nav-item:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 7%,transparent)}\
.rh-nav a.active,.rh-nav a[aria-current=page],.rh-nav .rh-nav-item.active{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 14%,transparent)}\
.rh-subnav{flex:none;width:11.5rem;display:flex;flex-direction:column;gap:1px;padding:var(--rh-space-3) var(--rh-space-2);overflow-y:auto;background:color-mix(in srgb,var(--rh-surface-2) 55%,var(--rh-surface));border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);-webkit-user-select:none;user-select:none}\
.rh-subnav-link{display:flex;align-items:center;gap:var(--rh-space-2);padding:.4rem .55rem;border-radius:var(--rh-radius);color:var(--rh-muted);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;white-space:nowrap;border-bottom:0;transition:background-color .13s ease,color .13s ease}\
.rh-subnav-link:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-subnav-link[aria-current=page]{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 13%,transparent);font-weight:650}\
.rh-subnav-icon{flex:none;display:grid;place-items:center;width:18px;height:18px;opacity:.85}\
.rh-subnav-link[aria-current=page] .rh-subnav-icon{opacity:1}\
.rh-subnav-label{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis}\
/* Pops when a pip APPEARS (unread 0->n mounts it). The nav itself stays\
   mounted across scope switches precisely so this never replays on plain\
   navigation -- the replay-on-remount class of motion 0.179 removed. */\
.rh-subnav .rh-pip{flex:none;animation:rh-pop .18s cubic-bezier(.22,1,.36,1) both}\
.rh-subnav-rule{height:1px;margin:var(--rh-space-2) .55rem;background:color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-icon-btn{display:inline-flex;align-items:center;justify-content:center;padding:.3rem;min-width:2rem;line-height:0}\
.rh-icon-btn span{display:grid;place-items:center}\
.rh-sidenav-slot{display:contents}\
/* Settings + demo picker. */\
/* The About window: a standalone, chromeless view, so it sets its own\
   vertical rhythm rather than inheriting the app body's panel layout. */\
.rh-about{min-height:100%;display:flex;flex-direction:column;align-items:center;text-align:center;padding:2rem var(--rh-space-5) var(--rh-space-4);background:var(--rh-bg);color:var(--rh-text);overflow-y:auto}\
/* Deliberate vertical rhythm: the mark, then the name, then what it is, then\
   what was built -- each a step apart rather than one crowded stack. */\
.rh-about-logo{display:block;width:7rem;height:7rem;margin:0 0 .55rem;filter:drop-shadow(0 .5rem .9rem color-mix(in srgb,#000 26%,transparent))}\
.rh-about-name{margin:0 0 .45rem;font-size:1.65rem;font-weight:700;letter-spacing:-.02em}\
.rh-about-tagline{margin:0 0 .85rem;color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-about-version{margin:0 0 1.9rem;display:inline-flex;align-items:center;gap:.45rem;font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-copy-btn{display:inline-grid;place-items:center;width:1.5rem;height:1.5rem;padding:0;border:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent);border-radius:var(--rh-radius-sm);background:transparent;color:var(--rh-muted);cursor:pointer;transition:color .12s ease,border-color .12s ease,background-color .12s ease,transform .12s ease}\
.rh-copy-btn:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 7%,transparent)}\
/* The confirmation IS the animation: a tick, briefly, in the accent. */\
.rh-copy-btn.done{color:var(--rh-accent);border-color:color-mix(in srgb,var(--rh-accent) 55%,transparent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent);transform:scale(1.08)}\
.rh-about-link{color:var(--rh-muted);text-decoration:none;border-bottom:1px solid color-mix(in srgb,var(--rh-muted) 40%,transparent)}\
.rh-about-link:hover{color:var(--rh-accent);border-bottom-color:var(--rh-accent)}\
.rh-about-points{list-style:none;margin:0 0 var(--rh-space-4);padding:0;display:flex;flex-direction:column;gap:var(--rh-space-2);text-align:left;width:100%;max-width:22rem}\
.rh-about-points li{display:flex;flex-direction:column;gap:.15rem;padding-top:var(--rh-space-2);border-top:1px solid color-mix(in srgb,var(--rh-text) 9%,transparent)}\
.rh-about-points li:first-child{padding-top:0;border-top:0}\
.rh-about-point-k{font-weight:650;font-size:var(--rh-font-sm)}\
.rh-about-point-v{color:var(--rh-muted);font-size:var(--rh-font-sm);line-height:1.45}\
.rh-about-foot{margin-top:auto;padding-top:var(--rh-space-3);display:flex;align-items:center;gap:.5rem;flex-wrap:wrap;justify-content:center;color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-about-dot{opacity:.6}\
/* The You page: your mark, your fingerprint, and what the key actually does. */\
.rh-you-hero{display:flex;align-items:flex-start;gap:var(--rh-space-5);padding:var(--rh-space-3) 0 var(--rh-space-4);flex-wrap:wrap}\
.rh-you-avatar{display:flex;flex-direction:column;align-items:center;gap:var(--rh-space-2);flex:none}\
.rh-you-ident{flex:1;min-width:16rem;display:flex;flex-direction:column;gap:var(--rh-space-2)}\
.rh-you-fp-line{display:flex;align-items:center;gap:var(--rh-space-2);flex-wrap:wrap}\
.rh-you-eyebrow{font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.06em;color:var(--rh-muted)}\
.rh-you-fp{font-family:var(--rh-font-mono);font-size:var(--rh-font-lg);letter-spacing:.02em}\
.rh-you-lead{margin:0;color:var(--rh-muted);max-width:56ch;line-height:1.5}\
.rh-you-key summary{cursor:default;font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-you-key .rh-you-pub{display:block;margin:.35rem 0;font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);overflow-wrap:anywhere;color:var(--rh-muted)}\
.rh-you-facts{margin:0;display:grid;grid-template-columns:repeat(auto-fit,minmax(16rem,1fr));gap:var(--rh-space-3)}\
.rh-you-fact{border:1px solid color-mix(in srgb,var(--rh-text) 9%,transparent);border-radius:var(--rh-radius);padding:var(--rh-space-3)}\
.rh-you-fact dt{font-weight:650;margin-bottom:.25rem}\
.rh-you-fact dd{margin:0;color:var(--rh-muted);font-size:var(--rh-font-sm);line-height:1.5}\
.rh-xfer-error{display:flex;align-items:center;gap:var(--rh-space-2);flex-wrap:wrap;margin-top:.3rem;padding:.35rem .5rem;border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-error) 10%,transparent);border:1px solid color-mix(in srgb,var(--rh-error) 30%,transparent)}\
.rh-xfer-why{flex:1;min-width:0;color:var(--rh-error);font-size:var(--rh-font-sm);overflow-wrap:anywhere}\
.rh-settings-range{display:flex;flex-direction:column;gap:.25rem;max-width:26rem;margin-bottom:var(--rh-space-2)}\
.rh-settings-range-label{font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-settings-range input[type=range]{width:100%;accent-color:var(--rh-accent)}\
.rh-you-backup{display:flex;gap:var(--rh-space-2);flex-wrap:wrap;margin-bottom:var(--rh-space-2)}\
.rh-restore{border:1px solid color-mix(in srgb,var(--rh-error) 35%,transparent);border-radius:var(--rh-radius);padding:var(--rh-space-3);display:flex;flex-direction:column;gap:var(--rh-space-2);max-width:42rem}\
.rh-restore-warn{margin:0;color:var(--rh-error);font-size:var(--rh-font-sm);line-height:1.45}\
.rh-restore-text{min-height:7rem;font-family:var(--rh-font-mono);font-size:var(--rh-font-xs)}\
.rh-mark-picker{display:flex;flex-wrap:wrap;gap:.4rem;margin:.4rem 0}\
.rh-mark-choice{display:grid;place-items:center;width:2.6rem;height:2.6rem;padding:0;border:1px solid transparent;border-radius:var(--rh-radius);background:transparent;cursor:pointer;line-height:0}\
.rh-mark-choice:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-mark-choice.on{border-color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent)}\
.rh-mark-colors{display:flex;align-items:center;gap:.4rem;flex-wrap:wrap;margin-bottom:var(--rh-space-2)}\
.rh-mark-color{width:1.5rem;height:1.5rem;padding:0;border:2px solid transparent;border-radius:var(--rh-radius-full);cursor:pointer}\
.rh-mark-color.on{border-color:var(--rh-text)}\
.rh-settings-note{margin:0 0 var(--rh-space-2);color:var(--rh-muted);font-size:var(--rh-font-sm);max-width:62ch}\
.rh-tracker-list{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:1px}\
.rh-tracker-row{display:flex;align-items:center;gap:var(--rh-space-2);padding:.35rem .4rem;border-radius:var(--rh-radius)}\
.rh-tracker-row:hover{background:color-mix(in srgb,var(--rh-text) 4%,transparent)}\
.rh-tracker-host{font-family:var(--rh-font-mono);font-size:var(--rh-font-sm)}\
.rh-tracker-tag{font-size:var(--rh-font-xs);color:var(--rh-muted);border:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent);border-radius:var(--rh-radius-full);padding:0 .4rem}\
.rh-tracker-remove{margin-left:auto}\
.rh-tracker-add{display:flex;gap:var(--rh-space-2);margin-top:var(--rh-space-2);max-width:32rem}\
.rh-download-from{display:flex;flex-direction:column;align-items:flex-start;gap:var(--rh-space-2);margin-top:var(--rh-space-4)}\
.rh-download-from-label{font-size:var(--rh-font-sm);font-weight:600;color:var(--rh-text)}\
.rh-download-from .rh-hint{margin:0}\
.rh-settings-folder{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:var(--rh-space-2) var(--rh-space-4);margin:var(--rh-space-3) 0 var(--rh-space-2);max-width:70ch}\
.rh-settings-folder-line{margin:0;min-width:0;flex:1 1 16rem;overflow-wrap:anywhere}\
.rh-settings-folder-actions{display:flex;gap:var(--rh-space-2);flex:none}\
.rh-settings-check{display:flex;align-items:center;gap:var(--rh-space-2);padding:.25rem 0;font-size:var(--rh-font-sm)}\
/* The person page: one human, everything you know about them. */\
.rh-person-link{display:flex;align-items:center;gap:var(--rh-space-2);width:100%;padding:.35rem .4rem;border-radius:var(--rh-radius);color:inherit;text-decoration:none;border-bottom:0}\
.rh-person-link:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-person-hero{display:flex;align-items:flex-start;gap:var(--rh-space-4);padding:var(--rh-space-3) 0 var(--rh-space-4);flex-wrap:wrap}\
.rh-person-hero-mark{flex:none;line-height:0}\
.rh-person-hero-id{flex:1;min-width:12rem;display:flex;flex-direction:column;gap:.2rem}\
.rh-person-hero-line{display:flex;align-items:center;gap:var(--rh-space-2);flex-wrap:wrap}\
.rh-person-hero-name{font-size:var(--rh-font-lg);font-weight:700;letter-spacing:-.01em}\
.rh-person-hero-key{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-person-hero-presence{font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-person-actions{display:flex;align-items:center;gap:var(--rh-space-2);flex:none}\
/* The badge is only ever shown for a MUTUAL signed attestation; a one-sided\
   offer gets the muted `pending` treatment, never the accent. */\
.rh-friend-badge{display:inline-flex;align-items:center;gap:.3rem;padding:.1rem .5rem;border-radius:var(--rh-radius-full);background:color-mix(in srgb,var(--rh-accent) 16%,transparent);color:var(--rh-accent);font-size:var(--rh-font-xs);font-weight:700}\
.rh-friend-badge.pending{background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-muted);font-weight:600}\
.rh-person-h2{margin:var(--rh-space-5) 0 var(--rh-space-2);font-size:var(--rh-font-size);font-weight:600;color:var(--rh-text)}\
.rh-known-from,.rh-person-dm,.rh-person-files{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:1px}\
.rh-known-row{display:flex;align-items:center;gap:var(--rh-space-2);padding:.35rem .4rem;border-radius:var(--rh-radius)}\
.rh-known-row:hover{background:color-mix(in srgb,var(--rh-text) 4%,transparent)}\
.rh-known-name{font-weight:600}\
.rh-known-handle{color:var(--rh-muted);font-size:var(--rh-font-sm);font-family:var(--rh-font-mono)}\
.rh-known-when{margin-left:auto;color:var(--rh-muted);font-size:var(--rh-font-xs);font-variant-numeric:tabular-nums}\
.rh-person-dm-row{display:flex;gap:var(--rh-space-2);padding:.25rem .4rem;min-width:0}\
.rh-person-dm-from{flex:none;font-weight:600;color:var(--rh-accent);font-size:var(--rh-font-sm)}\
.rh-person-dm-text{min-width:0;overflow-wrap:anywhere;font-size:var(--rh-font-sm)}\
.rh-sidenav-slot.rh-hidden{display:none}\
/* Warren scope has no sidebar on a desktop: People, Transfers, You and Servers\
   are each one screen. On a phone the same element is the bottom tab bar and\
   stays (see the narrow block). */\
.rh-sidenav-slot.warren-scope{display:none}\
/* Phone furniture: the bottom tab bar and the section strip exist only at\
   phone width (see the narrow block). */\
.rh-tabbar{display:none}\
.rh-section-strip{display:none}\
/* The warren sheet: the rail's job on a phone, as a bottom sheet. */\
@keyframes rh-slide-up{from{opacity:0;transform:translateY(14px)}to{opacity:1;transform:none}}\
.rh-sheet-backdrop{position:fixed;inset:0;z-index:95;display:flex;align-items:flex-end;justify-content:center;background:color-mix(in srgb,var(--rh-text) 30%,transparent);backdrop-filter:blur(6px);-webkit-backdrop-filter:blur(6px);animation:rh-fade .12s ease-out both}\
.rh-sheet{width:min(30rem,100%);max-height:82vh;overflow-y:auto;background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-bottom:0;border-radius:var(--rh-radius-xl) var(--rh-radius-xl) 0 0;box-shadow:var(--rh-shadow-3);padding:var(--rh-space-3) var(--rh-space-3) calc(var(--rh-space-4) + env(safe-area-inset-bottom));animation:rh-slide-up .18s cubic-bezier(.2,.8,.2,1) both}\
.rh-sheet-title{margin:var(--rh-space-2) .5rem .25rem;font-size:var(--rh-font-sm);font-weight:600;color:var(--rh-muted)}\
.rh-sheet-list{list-style:none;margin:0 0 var(--rh-space-2);padding:0}\
.rh-sheet-row{display:flex;align-items:center;gap:var(--rh-space-3);width:100%;min-height:2.75rem;padding:.35rem .5rem;border:0;border-radius:var(--rh-radius);background:transparent;color:var(--rh-text);font-family:inherit;font-size:var(--rh-font-size);text-align:left;cursor:pointer}\
.rh-sheet-row:active{background:color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-sheet-row.active{background:color-mix(in srgb,var(--rh-accent) 12%,transparent);color:var(--rh-accent)}\
.rh-sheet-tile{position:relative;flex:none;width:2rem;height:2rem;display:grid;place-items:center;border-radius:.6rem;background:color-mix(in srgb,var(--rh-text) 6%,transparent);font-weight:700;color:var(--rh-muted)}\
.rh-sheet-row.active .rh-sheet-tile{background:color-mix(in srgb,var(--rh-accent) 18%,transparent);color:var(--rh-accent)}\
.rh-sheet-tile .rh-rail-dot{box-shadow:0 0 0 2px var(--rh-surface)}\
.rh-sheet-icon svg,.rh-sheet-add svg{width:20px;height:20px}\
.rh-sheet-name{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600}\
.rh-sheet-meta{flex:none;font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-sheet-leave{width:100%;justify-content:center;margin-top:var(--rh-space-2);color:var(--rh-error);border-color:color-mix(in srgb,var(--rh-error) 45%,transparent)}\
.rh-subnav-scope{display:block;padding:.15rem .55rem .5rem;font-family:var(--rh-font-display);font-size:var(--rh-font-sm);font-weight:700;letter-spacing:-.02em;color:var(--rh-text);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
/* The rail's warren destinations get the same lit treatment as a focused\
   burrow tile, so the rail always shows where you are -- not only which burrow\
   is selected. */\
.rh-rail-tile.active{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 16%,transparent)}\
.rh-rail-home.active,.rh-rail-tile.rh-rail-unified.active{color:var(--rh-brand);background:color-mix(in srgb,var(--rh-brand) 20%,transparent);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-brand) 45%,transparent)}\
.rh-rail.warren .rh-rail-server.active{background:color-mix(in srgb,var(--rh-text) 5%,transparent);color:var(--rh-muted);box-shadow:none}\
/* --- Native feel. What separates an app from a web page is mostly the small
   things a browser does by default and a native app never does: rubber-band
   scrolling past the end, a grey flash when you tap, a text cursor over
   furniture, drag-selecting the sidebar. --- */\
.rh-app{overscroll-behavior:none;-webkit-tap-highlight-color:transparent;cursor:default;touch-action:manipulation}\
/* AppKit reserves the hand cursor for hyperlinks; a hand over every control is\
   the classic wrapper tell. Content links keep it. Browser builds keep web\
   conventions untouched. */\
.rh-app.native .rh-btn,.rh-app.native .rh-rail-tile,.rh-app.native .rh-format-btn,.rh-app.native .rh-glass-tool,.rh-app.native .rh-presence,.rh-app.native .rh-crumb,.rh-app.native .rh-board-link,.rh-app.native .rh-thread-link,.rh-app.native .rh-file-link,.rh-app.native .rh-member-link,.rh-app.native .rh-station-link,.rh-app.native .rh-dm-peer,.rh-app.native .rh-subnav-link,.rh-app.native .rh-palette-item,.rh-app.native .rh-kbd-jump,.rh-app.native .rh-icon-btn{cursor:default}\
.rh-app.native .rh-rich a{cursor:pointer}\
/* Dragging a nav link must not lift a translucent URL ghost out of the\
   sidebar. Selection rules already protect content; this stops element drag. */\
.rh-rail,.rh-header,.rh-subnav,.rh-format-bar{-webkit-user-drag:none}\
.rh-subnav a,.rh-header a,img{-webkit-user-drag:none}\
.rh-rail,.rh-header,.rh-format-bar{-webkit-user-select:none;user-select:none}\
/* …but never at the cost of copying content: messages, posts, filenames and\
   fingerprints stay selectable, and the stylesheet's own test enforces that\
   `user-select:none` appears only on the chrome selectors above. */\
.rh-rich,.rh-line,.rh-post,.rh-scroll,.rh-filetable,.rh-fingerprint{-webkit-user-select:text;user-select:text}\
.rh-scroll,.rh-panel,.rh-who,.rh-subnav{overscroll-behavior:contain;scrollbar-width:thin}\
::selection{background:color-mix(in srgb,var(--rh-accent) 30%,transparent)}\
.rh-input,textarea.rh-input{caret-color:var(--rh-accent)}\
/* The desktop shell hides the system title bar, so the header is the title bar:\
   it drags the window, and every control in it has to opt back out or it can't\
   be clicked. The rail starts below the traffic lights. */\
/* The traffic lights sit at the window's top-LEFT, which in this layout is over\
   the burrow rail and the top of the sidebar -- not over the header. So the\
   whole window content shifts down by a title-bar's height and that strip\
   becomes the drag region: the lights get clear space, and there is one\
   unambiguous place to grab the window (what VS Code does with the same\
   constraint). The header is draggable too, with its controls opting back out\
   or they couldn't be clicked. */\
.rh-app.native{padding-top:1.75rem}\
/* The strip under the traffic lights. Layout only: dragging comes from the\
   element's data-tauri-drag-region attribute, which Tauri's own handler\
   watches. (-webkit-app-region, used here previously, is a Chromium extension\
   WKWebView ignores -- with the system title bar hidden that no-op left the\
   window unmovable by its own chrome.) */\
.rh-drag-strip{position:fixed;top:0;left:0;right:0;height:1.75rem;z-index:100}\
html.rh-fullscreen .rh-drag-strip{display:none}\
html.rh-fullscreen .rh-app.native{padding-top:0}\
.rh-composer{display:flex;flex-direction:column;gap:var(--rh-space-2);padding:var(--rh-space-3) var(--rh-space-5);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-format-bar{display:flex;align-items:center;gap:.15rem;flex-wrap:nowrap;overflow-x:auto;scrollbar-width:none}\
.rh-format-bar::-webkit-scrollbar{display:none}\
.rh-format-btn{flex:none}\
.rh-format-btn{display:inline-flex;align-items:center;justify-content:center;min-width:1.9rem;height:1.9rem;padding:0 .4rem;border:1px solid transparent;border-radius:var(--rh-radius-sm);background:transparent;color:var(--rh-muted);font-family:var(--rh-font-sans);font-size:var(--rh-font-sm);font-weight:700;cursor:pointer;transition:background-color .12s ease,color .12s ease}\
.rh-format-btn:hover{background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-text)}\
.rh-format-btn.on{background:color-mix(in srgb,var(--rh-accent) 15%,transparent);color:var(--rh-accent)}\
.rh-format-mode{min-width:auto;font-size:var(--rh-font-xs);font-weight:600;letter-spacing:.02em}\
.rh-format-spacer{flex:1}\
.rh-compose-area{min-height:2.4rem;max-height:40vh;resize:vertical;font-family:var(--rh-font-sans);line-height:1.45}\
.rh-compose-area.tall{min-height:7rem}\
/* The chat composer: one growing row, a formatting toggle, a send button. */\
.rh-composer.chat{gap:.35rem;padding:var(--rh-space-2) var(--rh-space-4)}\
.rh-compose-row{display:flex;align-items:flex-end;gap:.4rem}\
.rh-composer.chat .rh-compose-area{flex:1;min-height:2.3rem;max-height:32vh;resize:none;padding:.45rem .7rem}\
.rh-compose-iconbtn{flex:none;width:2.3rem;height:2.3rem;display:grid;place-items:center;border:1px solid transparent;border-radius:var(--rh-radius);background:transparent;color:var(--rh-muted);font-family:inherit;font-size:var(--rh-font-sm);font-weight:700;cursor:pointer;transition:background-color .12s ease,color .12s ease}\
.rh-compose-iconbtn:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent);color:var(--rh-text)}\
.rh-compose-iconbtn.on{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent)}\
.rh-compose-send{background:var(--rh-accent);color:var(--rh-bg)}\
.rh-compose-send:hover{background:var(--rh-accent);color:var(--rh-bg);filter:brightness(1.06)}\
.rh-compose-send:disabled{background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-muted);cursor:default;filter:none}\
.rh-compose-send svg{width:18px;height:18px}\
.rh-panel-head{display:flex;align-items:baseline;justify-content:space-between;gap:var(--rh-space-3)}\
.rh-composer.markdown .rh-compose-area{font-family:var(--rh-font-mono);font-size:var(--rh-font-sm)}\
.rh-compose-actions{display:flex;align-items:center;gap:var(--rh-space-3);justify-content:flex-end}\
.rh-compose-hint{flex:1;min-width:0;font-size:var(--rh-font-xs);color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-preview{border:1px dashed color-mix(in srgb,var(--rh-text) 18%,transparent);border-radius:var(--rh-radius);padding:var(--rh-space-2) var(--rh-space-3)}\
.rh-preview-label{display:block;font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.06em;color:var(--rh-muted);margin-bottom:.25rem}\
.rh-rich{min-width:0;overflow-wrap:anywhere}\
.rh-rich p{margin:0 0 .45rem}\
.rh-rich p:last-child{margin-bottom:0}\
.rh-rich h1,.rh-rich h2,.rh-rich h3{margin:.5rem 0 .3rem;line-height:1.25}\
.rh-rich h1{font-size:1.16em}.rh-rich h2{font-size:1.09em}.rh-rich h3{font-size:1.03em}\
.rh-rich code{font-family:var(--rh-font-mono);font-size:.92em;padding:.08em .32em;border-radius:var(--rh-radius-sm);background:color-mix(in srgb,var(--rh-text) 9%,transparent)}\
.rh-rich pre{margin:.4rem 0;padding:var(--rh-space-2) var(--rh-space-3);border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-text) 7%,transparent);overflow-x:auto}\
.rh-rich pre code{background:none;padding:0}\
.rh-rich blockquote{margin:.4rem 0;padding:.1rem 0 .1rem var(--rh-space-3);border-left:1px solid color-mix(in srgb,var(--rh-text) 25%,transparent);color:var(--rh-muted)}\
.rh-rich ul,.rh-rich ol{margin:.3rem 0;padding-left:1.35rem}\
.rh-rich li{margin:.1rem 0}\
.rh-rich hr{margin:.6rem 0;border:0;border-top:1px solid color-mix(in srgb,var(--rh-text) 15%,transparent)}\
.rh-rich a{color:var(--rh-accent)}\
.rh-line-text{display:inline}\
.rh-btn{font:inherit;font-weight:600;cursor:pointer;border:1px solid transparent;background:var(--rh-accent);color:var(--rh-bg);border-radius:var(--rh-radius);padding:.5rem .9rem;line-height:1.2;display:inline-flex;align-items:center;gap:.4rem;transition:transform .12s ease,box-shadow .15s ease,background-color .15s ease;box-shadow:var(--rh-shadow-1)}\
.rh-btn:hover{background:color-mix(in srgb,var(--rh-accent) 88%,var(--rh-text));box-shadow:var(--rh-shadow-2);transform:translateY(-1px)}\
.rh-btn:active{transform:translateY(0);box-shadow:var(--rh-shadow-1)}\
.rh-btn.ghost{background:transparent;color:var(--rh-accent);border-color:color-mix(in srgb,var(--rh-accent) 40%,transparent);box-shadow:none}\
.rh-btn.ghost:hover{background:color-mix(in srgb,var(--rh-accent) 12%,transparent);border-color:var(--rh-accent);transform:none}\
.rh-btn.small{padding:.3rem .6rem;font-size:var(--rh-font-xs);border-radius:var(--rh-radius-sm)}\
.rh-btn:disabled{opacity:.45;cursor:not-allowed;box-shadow:none;transform:none;background:color-mix(in srgb,var(--rh-text) 12%,transparent);color:var(--rh-muted);border-color:transparent}\
.rh-btn.ghost:disabled{background:transparent}\
.rh-input{font:inherit;padding:.5rem .7rem;border-radius:var(--rh-radius);border:1px solid color-mix(in srgb,var(--rh-text) 16%,transparent);background:color-mix(in srgb,var(--rh-bg) 60%,var(--rh-surface));color:var(--rh-text);transition:border-color .15s ease,box-shadow .15s ease}\
.rh-input::placeholder{color:var(--rh-muted)}\
.rh-input:hover{border-color:color-mix(in srgb,var(--rh-text) 26%,transparent)}\
/* Text inputs match :focus-visible even on mouse click, so the global outline\
   stacked on the input's own ring -- the classic web double ring, on the most\
   clicked control in the app. The box-shadow ring remains for everyone,\
   keyboard included. */\
.rh-input:focus-visible{outline:2px solid transparent}\
.rh-input:focus{border-color:var(--rh-accent);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-accent) 24%,transparent)}\
.rh-kbd-jump{font:inherit;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);background:color-mix(in srgb,var(--rh-text) 6%,transparent);border:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent);border-radius:var(--rh-radius);padding:.22rem .5rem;cursor:pointer;line-height:1.4;letter-spacing:.03em;white-space:nowrap;transition:background-color .15s ease,color .15s ease,border-color .15s ease}\
.rh-kbd-jump:hover{color:var(--rh-accent);border-color:color-mix(in srgb,var(--rh-accent) 40%,transparent);background:color-mix(in srgb,var(--rh-accent) 10%,transparent)}\
.rh-palette-backdrop{animation:rh-fade .12s ease-out both}\
.rh-palette{animation:rh-pop-in .13s cubic-bezier(.2,.9,.3,1) both}\
.rh-palette-backdrop{position:fixed;inset:0;z-index:100;display:flex;align-items:flex-start;justify-content:center;padding:14vh var(--rh-space-4) var(--rh-space-4);background:color-mix(in srgb,var(--rh-text) 30%,transparent);backdrop-filter:blur(6px);-webkit-backdrop-filter:blur(6px)}\
.rh-palette{width:min(34rem,94vw);max-height:72vh;display:flex;flex-direction:column;background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius-xl);box-shadow:var(--rh-shadow-3);overflow:hidden}\
.rh-palette-input{margin:var(--rh-space-3);font-size:var(--rh-font-lg)}\
.rh-palette-list{list-style:none;margin:0;padding:0 var(--rh-space-2) var(--rh-space-2);overflow-y:auto}\
.rh-palette-item{display:flex;align-items:center;justify-content:space-between;gap:var(--rh-space-3);padding:.55rem .7rem;border-radius:var(--rh-radius);cursor:pointer;transition:background-color .12s ease}\
.rh-palette-item.selected{background:color-mix(in srgb,var(--rh-accent) 16%,transparent)}\
.rh-palette-label{font-weight:600;color:var(--rh-text)}\
.rh-palette-item.selected .rh-palette-label{color:var(--rh-accent)}\
.rh-palette-hint{font-size:var(--rh-font-xs);color:var(--rh-muted);text-transform:uppercase;letter-spacing:.05em}\
.rh-toasts{position:fixed;top:4.2rem;right:var(--rh-space-4);z-index:90;display:flex;flex-direction:column;gap:var(--rh-space-2);width:min(22rem,90vw)}\
.rh-toast{display:flex;align-items:center;gap:var(--rh-space-2);background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius);box-shadow:var(--rh-shadow-2);padding:.6rem .7rem;font-size:var(--rh-font-sm)}\
.rh-toast-glyph{flex:none;font-size:var(--rh-font-lg);line-height:1;color:var(--rh-accent)}\
.rh-toast-text{flex:1;color:var(--rh-text);min-width:0}\
.rh-toast-close{flex:none;background:transparent;border:0;color:var(--rh-muted);cursor:pointer;font-size:var(--rh-font-lg);line-height:1;padding:0 .2rem;border-radius:var(--rh-radius-sm)}\
.rh-toast-close:hover{color:var(--rh-text)}\
.rh-toast.success .rh-toast-glyph{color:#2f9e44}\
.rh-toast.warn .rh-toast-glyph{color:#e8890c}\
.rh-banner{display:flex;align-items:center;gap:var(--rh-space-3);padding:.5rem var(--rh-space-5);font-size:var(--rh-font-sm);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-banner-text{flex:1;min-width:0}\
.rh-banner.pending{background:color-mix(in srgb,var(--rh-accent) 14%,var(--rh-surface));color:var(--rh-text)}\
.rh-banner.offline{background:color-mix(in srgb,#e8890c 18%,var(--rh-surface));color:var(--rh-text)}\
.rh-banner .rh-btn{padding:.3rem .7rem;font-size:var(--rh-font-sm)}\
.rh-newthread{display:flex;flex-direction:column;gap:var(--rh-space-2);margin-top:var(--rh-space-4);padding-top:var(--rh-space-4);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-newthread textarea{font:inherit;min-height:4rem;resize:vertical}\
.rh-newthread .rh-btn{align-self:flex-start}\
.rh-reply{display:flex;flex-direction:column;gap:var(--rh-space-2);margin-top:var(--rh-space-4)}\
.rh-reply textarea{font:inherit;min-height:3.5rem;resize:vertical}\
.rh-reply .rh-btn{align-self:flex-start}\
.rh-dm-start{margin:0 .4rem var(--rh-space-3)}\
.rh-dm-start .rh-input{width:100%;font-size:var(--rh-font-sm)}\
.rh-card-field{margin:.35rem 0;font-size:var(--rh-font-sm);color:var(--rh-text)}\
.rh-card-label{display:inline-block;min-width:5rem;color:var(--rh-muted);font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.05em;margin-right:.5rem}\
.rh-card-avatar{width:4rem;height:4rem;border-radius:var(--rh-radius-full);object-fit:cover;margin-bottom:var(--rh-space-2);border:2px solid color-mix(in srgb,var(--rh-accent) 40%,transparent)}\
/* A connect dialog, not a marketing hero: the blurred accent glow and heavy\
   drop shadow are the SaaS-login look. A quiet panel on the window ground is\
   what Transmit or Screens put in front of you. */\
/* The connect window. A Mac app's welcome window, not a web sign-in card: who\
   this is and the way in on the left, the places you could go on the right.\
   It is the warren layer, so its one colour is the ember, not a burrow's accent. */\
.rh-connect{flex:1;min-height:0;display:grid;grid-template-columns:minmax(17.5rem,21rem) minmax(0,1fr)}\
.rh-connect-side{display:flex;flex-direction:column;gap:var(--rh-space-5);min-height:0;overflow-y:auto;padding:var(--rh-space-8) var(--rh-space-6) var(--rh-space-4);background:var(--rh-surface-2);border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-connect-brand{display:flex;flex-direction:column;align-items:center;text-align:center}\
.rh-connect-logo{display:block;width:7rem;height:7rem;filter:drop-shadow(0 .5rem .9rem color-mix(in srgb,#000 26%,transparent))}\
.rh-connect-brand h1{margin:var(--rh-space-2) 0 0;font-family:var(--rh-font-display);font-size:1.75rem;font-weight:700;letter-spacing:-.025em;line-height:1.1;color:var(--rh-text)}\
.rh-connect-tagline{margin:.2rem 0 0;color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-login{display:flex;flex-direction:column;gap:.35rem}\
.rh-login label{margin-top:var(--rh-space-2);font-size:var(--rh-font-sm);font-weight:600;color:var(--rh-text)}\
.rh-login label:first-child{margin-top:0}\
.rh-login .rh-input{width:100%}\
.rh-login-address{font-family:var(--rh-font-mono);font-size:var(--rh-font-sm);font-variant-numeric:slashed-zero}\
.rh-connect .rh-input{caret-color:var(--rh-brand)}\
.rh-connect .rh-input:focus{border-color:var(--rh-brand);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-brand) 24%,transparent)}\
.rh-connect .rh-input[aria-invalid=true]{border-color:var(--rh-error)}\
.rh-connect ::selection{background:color-mix(in srgb,var(--rh-brand) 32%,transparent)}\
.rh-field-hint{margin:0;font-size:var(--rh-font-xs);color:var(--rh-muted);line-height:1.4}\
.rh-field-hint.error{color:var(--rh-error)}\
.rh-btn.rh-connect-go{justify-content:center;width:100%;min-width:0;margin-top:var(--rh-space-4);padding:.6rem .9rem;background:var(--rh-brand);color:var(--rh-on-brand)}\
.rh-btn.rh-connect-go:hover{background:color-mix(in srgb,var(--rh-brand) 90%,var(--rh-text))}\
.rh-connect-go span{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-connect-foot{margin-top:auto;padding-top:var(--rh-space-4);display:flex;flex-direction:column;align-items:center;gap:var(--rh-space-2)}\
.rh-connect-back{display:inline-flex;align-items:center;gap:.2rem;max-width:100%;padding:.25rem .6rem .25rem .3rem;border-radius:var(--rh-radius-sm);color:var(--rh-text);font-size:var(--rh-font-sm);text-decoration:none;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}\
.rh-connect-back:hover{background:color-mix(in srgb,var(--rh-text) 7%,transparent)}\
.rh-connect-back>span{display:grid;flex:none;color:var(--rh-muted)}\
.rh-connect-back svg{width:.95rem;height:.95rem}\
.rh-connect-version{margin:0;text-align:center;font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums}\
/* The burrow browser: Hotline's tracker window. One dense list, a face per\
   burrow, numbers in the mono face, and a status strip underneath. */\
.rh-connect-main{display:grid;grid-template-rows:auto auto minmax(0,1fr) auto;min-width:0;min-height:0}\
.rh-glass-head{display:flex;align-items:center;gap:var(--rh-space-2);padding:var(--rh-space-4) var(--rh-space-5) var(--rh-space-3);-webkit-user-select:none;user-select:none}\
.rh-glass-head h2{margin:0 auto 0 0;white-space:nowrap;font-family:var(--rh-font-display);font-size:var(--rh-font-lg);font-weight:600;letter-spacing:-.015em;color:var(--rh-text)}\
.rh-glass-search{position:relative;display:flex;align-items:center;width:min(14rem,45%)}\
.rh-glass-search>span{position:absolute;left:.5rem;display:grid;color:var(--rh-muted)}\
.rh-glass-search svg{width:.95rem;height:.95rem}\
.rh-glass-search .rh-input{width:100%;padding:.3rem .6rem .3rem 1.8rem;font-size:var(--rh-font-sm);border-radius:var(--rh-radius-sm)}\
.rh-glass-tool{flex:none;display:grid;place-items:center;width:1.9rem;height:1.9rem;padding:0;border:1px solid transparent;border-radius:var(--rh-radius-sm);background:transparent;color:var(--rh-muted);cursor:pointer;transition:background-color .15s ease,color .15s ease}\
.rh-glass-tool:hover{background:color-mix(in srgb,var(--rh-text) 7%,transparent);color:var(--rh-text)}\
.rh-glass-tool:disabled{cursor:default}\
.rh-glass-tool>span{display:grid}\
.rh-glass-tool[aria-expanded=true]{background:color-mix(in srgb,var(--rh-brand) 16%,transparent);color:var(--rh-text)}\
.rh-glass-refresh.busy svg{animation:rh-spin .9s linear infinite}\
@keyframes rh-spin{to{transform:rotate(360deg)}}\
.rh-glass-cols,.rh-glass-row{display:grid;grid-template-columns:1.6rem minmax(9rem,1.35fr) minmax(0,2fr) 4.25rem 3.5rem;column-gap:var(--rh-space-3);align-items:center}\
.rh-glass-cols{padding:0 calc(var(--rh-space-5) + .5rem) .35rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);-webkit-user-select:none;user-select:none}\
.rh-glass-cols .num{text-align:right}\
.rh-glass-scroll{min-height:0;overflow-y:auto;overscroll-behavior:contain;padding:var(--rh-space-1) var(--rh-space-5) var(--rh-space-4)}\
.rh-glass-scroll:focus-visible{outline-offset:-2px}\
.rh-glass-group{margin:var(--rh-space-4) .5rem .25rem;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted)}\
.rh-glass-list{list-style:none;margin:0;padding:0}\
.rh-glass-list li+li{border-top:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-glass-list li.selected,.rh-glass-list li.selected+li{border-top-color:transparent}\
.rh-glass-item{border-radius:var(--rh-radius-sm)}\
.rh-glass-item.selected{background:color-mix(in srgb,var(--rh-brand) 20%,transparent)}\
.rh-glass-row{width:100%;min-height:2rem;padding:.25rem .5rem;border:0;border-radius:var(--rh-radius-sm);background:transparent;color:var(--rh-text);font:inherit;font-size:var(--rh-font-sm);line-height:1.35;text-align:left;cursor:default}\
.rh-glass-row:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-glass-item.selected .rh-glass-row{align-items:start}\
.rh-glass-item.selected .rh-glass-row:hover{background:transparent}\
.rh-glass-row:focus-visible{outline:2px solid var(--rh-focus);outline-offset:-2px}\
.rh-glass-mark{position:relative;display:grid;place-items:center;width:1.6rem;height:1.6rem}\
.rh-glass-mark>span:first-child{display:grid}\
.rh-glass-mark svg{display:block;border-radius:.3rem}\
.rh-glass-mark .rh-dot{position:absolute;right:-.2rem;bottom:-.15rem;box-shadow:0 0 0 2px var(--rh-bg)}\
.rh-glass-name{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--rh-font-display);font-weight:600;letter-spacing:-.01em}\
.rh-glass-desc{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--rh-muted)}\
.rh-glass-as{margin-right:.6rem;color:var(--rh-text)}\
.rh-glass-users,.rh-glass-uptime{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);font-variant-numeric:tabular-nums slashed-zero;text-align:right;white-space:nowrap}\
.rh-glass-uptime,.rh-glass-down{color:var(--rh-muted)}\
.rh-glass-row.off .rh-glass-name{color:var(--rh-muted)}\
.rh-glass-item.selected .rh-glass-desc{white-space:normal;overflow:visible;color:var(--rh-text)}\
.rh-glass-detail{display:flex;flex-wrap:wrap;align-items:center;gap:.35rem var(--rh-space-4);padding:0 .5rem .5rem calc(2.1rem + var(--rh-space-3))}\
.rh-glass-more{flex:1 1 14rem;display:flex;flex-wrap:wrap;gap:.1rem var(--rh-space-3);min-width:0;margin:0;font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-glass-actions{display:flex;flex-wrap:wrap;gap:var(--rh-space-2)}\
.rh-glass-actions .rh-btn.ghost{color:var(--rh-text);border-color:color-mix(in srgb,var(--rh-text) 22%,transparent)}\
.rh-glass-actions .rh-btn.ghost:hover{background:color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-glass-rename{flex:1 1 100%;display:flex;align-items:center;gap:var(--rh-space-2)}\
.rh-glass-rename .rh-input{flex:1;min-width:0;padding:.3rem .55rem;font-size:var(--rh-font-sm)}\
.rh-glass-add{display:grid;grid-template-columns:minmax(7rem,1fr) minmax(11rem,1.6fr) auto auto;align-items:center;gap:var(--rh-space-2);margin:var(--rh-space-2) 0 var(--rh-space-1);padding:var(--rh-space-3);border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-surface-2) 70%,transparent);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-glass-add .rh-input{min-width:0;padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-glass-add .rh-field-hint{grid-column:1/-1}\
.rh-glass-page>.rh-connect-main{flex:1}\
.rh-glass-more code{font:inherit;overflow-wrap:anywhere}\
.rh-glass-scroll .rh-chat-empty{min-height:0;padding:var(--rh-space-8) var(--rh-space-4)}\
.rh-glass-scroll .rh-chat-empty-mark{background:color-mix(in srgb,var(--rh-brand) 12%,transparent);color:var(--rh-brand)}\
.rh-glass-scroll .rh-skeleton{padding:var(--rh-space-2) .5rem}\
.rh-glass-status{display:flex;align-items:center;justify-content:space-between;gap:var(--rh-space-3);padding:.4rem calc(var(--rh-space-5) + .5rem);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);background:color-mix(in srgb,var(--rh-surface-2) 55%,transparent);font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums;-webkit-user-select:none;user-select:none}\
.rh-glass-via{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
/* In the desktop shell this window owns the title bar: both panes run to the\
   top edge, under the traffic lights, the way a Mac sidebar does. The fixed\
   drag strip still sits over that band, so nothing clickable goes in it. */\
.rh-app.native:has(.rh-connect){padding-top:0}\
.rh-app.native .rh-connect-side{padding-top:calc(1.75rem + var(--rh-space-5))}\
.rh-app.native .rh-connect .rh-glass-head{padding-top:calc(1.75rem + var(--rh-space-1))}\
html.rh-fullscreen .rh-app.native .rh-connect-side{padding-top:var(--rh-space-8)}\
html.rh-fullscreen .rh-app.native .rh-connect .rh-glass-head{padding-top:var(--rh-space-4)}\
.rh-login-notice{margin:0 0 .2rem;padding:.55rem .75rem;border-radius:var(--rh-radius);background:color-mix(in srgb,#e8890c 14%,transparent);color:var(--rh-text);font-size:var(--rh-font-sm);line-height:1.45}\
.rh-load-failed{display:flex;align-items:center;gap:var(--rh-space-3);padding:var(--rh-space-3) .6rem;color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-body{flex:1;display:flex;min-height:0}\
.rh-chat{flex:1;display:flex;flex-direction:column;min-width:0;position:relative}\
.rh-jump-new{position:absolute;left:50%;bottom:4.6rem;transform:translateX(-50%);z-index:15;font:inherit;font-size:var(--rh-font-sm);font-weight:600;color:var(--rh-bg);background:var(--rh-accent);border:0;border-radius:var(--rh-radius-full);padding:.35rem .95rem;cursor:pointer;box-shadow:var(--rh-shadow-2);white-space:nowrap;animation:rh-pop .2s cubic-bezier(.22,1,.36,1) both;transition:box-shadow .15s ease,background-color .15s ease}\
.rh-jump-new:hover{background:color-mix(in srgb,var(--rh-accent) 88%,var(--rh-text));box-shadow:var(--rh-shadow-3)}\
.rh-scroll{flex:1;overflow-y:auto;padding:var(--rh-space-5);display:flex;flex-direction:column;gap:.1rem}\
.rh-lines{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.1rem}\
.rh-line{position:relative;padding:.35rem 3.6rem .35rem .6rem;border-radius:var(--rh-radius);transition:background-color .12s ease}\
.rh-line:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-line .rh-from{color:var(--rh-accent);font-weight:600;margin-right:var(--rh-space-2);text-decoration:none;border-radius:var(--rh-radius-sm)}\
.rh-line a.rh-from:hover{text-decoration:underline;text-underline-offset:.18em}\
.rh-who li>a.rh-who-row{flex:1;min-width:0;color:inherit;text-decoration:none}\
.rh-who-name{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-line-time{position:absolute;right:.6rem;top:.45rem;font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums}\
.rh-line-head{margin-top:.4rem}\
.rh-line-head:first-child{margin-top:0}\
.rh-line-cont{padding-top:.1rem;padding-bottom:.1rem}\
.rh-line-cont .rh-line-time{opacity:0;transition:opacity .12s ease}\
.rh-line-cont:hover .rh-line-time{opacity:1}\
.rh-who{width:14rem;background:color-mix(in srgb,var(--rh-surface) 55%,var(--rh-bg));padding:var(--rh-space-4) var(--rh-space-3);overflow-y:auto;border-left:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-who h2{font-size:var(--rh-font-sm);font-weight:600;color:var(--rh-muted);margin:.2rem .4rem var(--rh-space-3)}\
.rh-who ul{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.1rem}\
.rh-who li{display:flex;align-items:center;gap:.55rem;padding:.4rem .5rem;border-radius:var(--rh-radius);font-size:var(--rh-font-sm);transition:background-color .12s ease}\
.rh-who li:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-panel{flex:1;padding:var(--rh-space-5);overflow-y:auto;min-width:0}\
.rh-panel-title{display:flex;align-items:baseline;gap:.5rem;font-family:var(--rh-font-display);font-size:var(--rh-font-lg);font-weight:700;letter-spacing:-.02em;color:var(--rh-text);margin:0 0 var(--rh-space-3)}\
.rh-panel-sub{font-size:var(--rh-font-sm);font-weight:500;letter-spacing:0;color:var(--rh-muted)}\
/* A title that follows content gets more space above than below, so it reads\
   as the head of what comes next rather than the tail of what came before. */\
.rh-panel-title:not(:first-child){margin-top:var(--rh-space-6)}\
.rh-tree{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:0}\
/* Lists are rows with hairlines, not a stack of same-size cards: the same\
   vocabulary as the thread list and the file table, everywhere. */\
.rh-board-link,.rh-thread-link,.rh-member-link,.rh-file-link,.rh-station-link{display:flex;flex-direction:column;gap:.15rem;width:100%;text-align:left;text-decoration:none;font:inherit;cursor:pointer;background:transparent;color:var(--rh-text);border:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent);border-radius:0;padding:.5rem .6rem;transition:background-color .12s ease}\
.rh-row{flex-direction:row;align-items:center;gap:var(--rh-space-3);min-height:2.6rem}\
.rh-row-icon{flex:none;display:grid;place-items:center;width:1.75rem;height:1.75rem;border-radius:.5rem;background:color-mix(in srgb,var(--rh-accent) 9%,transparent);color:var(--rh-accent)}\
.rh-row-icon svg{width:16px;height:16px}\
.rh-row-main{flex:1;min-width:0;display:flex;flex-direction:column;gap:.05rem}\
.rh-row .rh-board-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-row .rh-board-desc{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-board-link:hover,.rh-thread-link:hover,.rh-member-link:hover,.rh-file-link:hover,.rh-station-link:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
/* Pressed states. A native row visibly darkens the instant the mouse goes\
   down; a row that shows nothing between hover and navigation reads as a\
   hyperlink. Instant, untransitioned -- also right under reduced motion. */\
.rh-board-link:active,.rh-thread-link:active,.rh-member-link:active,.rh-file-link:active,.rh-station-link:active,.rh-subnav-link:active,.rh-dm-peer:active,.rh-crumb:active,.rh-format-btn:active,.rh-glass-row:active,.rh-palette-item:active,.rh-back:active{background:color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-thread-link.active,.rh-file-link.active,.rh-station-link.active{border-color:var(--rh-accent);box-shadow:0 0 0 1px var(--rh-accent),var(--rh-shadow-2)}\
.rh-board-name,.rh-thread-title{font-weight:600;color:var(--rh-text);font-size:var(--rh-font-size)}\
.rh-board-desc,.rh-thread-author,.rh-member-handle{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-back-icon,.rh-btn-icon,.rh-inline-icon{display:inline-flex;line-height:0;vertical-align:-.15em}\
.rh-back-icon svg,.rh-btn-icon svg{width:16px;height:16px}\
.rh-inline-icon svg{width:14px;height:14px;margin-right:.3rem}\
.rh-inline-dot{display:inline-block;margin-right:.4rem;vertical-align:.05em}\
.rh-xfer-dir svg{width:16px;height:16px;display:block}\
.rh-format-btn svg{width:16px;height:16px}\
.rh-person-idkey svg{width:14px;height:14px;vertical-align:-.15em}\
.rh-welcome-x svg,.rh-toast-close svg{width:16px;height:16px;display:block}\
.rh-back{display:inline-flex;align-items:center;gap:.15rem;margin-bottom:var(--rh-space-3);color:var(--rh-muted);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;background:none;border:0;cursor:pointer;padding:0;transition:color .15s ease}\
.rh-back:hover{color:var(--rh-accent)}\
.rh-threads{max-width:22rem;border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-reader{flex:2}\
.rh-posts{display:flex;flex-direction:column;gap:0}\
.rh-post{padding:var(--rh-space-4) .25rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-post-head{display:flex;align-items:center;gap:.5rem}\
.rh-post-when{margin-left:auto;font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums;white-space:nowrap}\
.rh-post .rh-from{color:var(--rh-text);font-weight:600;margin:0}\
.rh-post-body{margin:.4rem 0 0;line-height:1.6}\
.rh-empty{color:var(--rh-muted);padding:var(--rh-space-4);text-align:center}\
.rh-dm-peer{width:100%;text-align:left;font:inherit;cursor:pointer;background:transparent;color:var(--rh-text);border:1px solid transparent;border-radius:var(--rh-radius);padding:.45rem .6rem;display:flex;align-items:center;gap:.6rem;transition:background-color .12s ease,color .12s ease}\
/* Conversation rows carry a preview line, so the list is a little wider than\
   the lobby roster it shares a base style with. */\
.rh-convos{width:18rem}\
.rh-dm-mark{border-radius:7px}\
.rh-dm-main{flex:1;min-width:0;display:flex;flex-direction:column;gap:.05rem}\
.rh-dm-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600}\
.rh-dm-preview{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:var(--rh-font-xs);color:var(--rh-muted);font-weight:400}\
.rh-dm-side{flex:none;display:flex;flex-direction:column;align-items:flex-end;gap:.2rem}\
.rh-dm-when{font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums;font-weight:400}\
.rh-dm-peer:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-dm-peer.active{background:color-mix(in srgb,var(--rh-accent) 14%,transparent);color:var(--rh-accent);font-weight:600}\
.rh-member-link{flex-direction:row;align-items:center;gap:.6rem;padding:.45rem .6rem;min-height:2.6rem}\
.rh-member-link .rh-mark{border-radius:6px}\
.rh-member-name{font-weight:600}\
.rh-members{max-width:24rem;border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-card{background:var(--rh-surface);border-radius:var(--rh-radius-xl);padding:var(--rh-space-6);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);box-shadow:var(--rh-shadow-2)}\
.rh-card-name{margin:0;color:var(--rh-text);font-size:var(--rh-font-xl);letter-spacing:-.01em}\
.rh-card-handle,.rh-card-status{margin:.25rem 0;color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-card-status{display:inline-block;font-size:var(--rh-font-xs);font-weight:600;padding:.15rem .55rem;border-radius:var(--rh-radius-full);background:color-mix(in srgb,#3fbf7f 18%,transparent);color:color-mix(in srgb,#3fbf7f 75%,var(--rh-text))}\
.rh-card-bio{margin:var(--rh-space-3) 0 0;line-height:1.6}\
.rh-files{flex:1 1 auto;min-width:0;border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-file-detail{flex:0 0 22rem;max-width:22rem}\
.rh-crumbs{display:flex;flex-wrap:wrap;gap:.3rem;align-items:center;font-size:var(--rh-font-sm);margin-bottom:var(--rh-space-3)}\
.rh-crumb{color:var(--rh-accent);background:none;border:none;font:inherit;cursor:pointer;padding:.1rem .4rem;border-radius:var(--rh-radius-sm);transition:background-color .12s ease}\
.rh-crumb:hover{background:color-mix(in srgb,var(--rh-accent) 12%,transparent)}\
.rh-crumb.sep{color:var(--rh-muted);cursor:default;padding:0}\
.rh-crumb.sep:hover{background:none}\
.rh-toolbar{display:flex;gap:var(--rh-space-2);align-items:center;margin-bottom:var(--rh-space-3);flex-wrap:wrap}\
.rh-file-link{flex-direction:row;align-items:center;gap:var(--rh-space-3);padding:.55rem var(--rh-space-3)}\
.rh-file-icon{font-size:1.2rem;line-height:1}\
.rh-file-name{font-weight:600;color:var(--rh-text)}\
.rh-file-meta{margin-left:auto;font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums}\
.rh-meta-grid{display:grid;grid-template-columns:auto 1fr;gap:.45rem var(--rh-space-4);font-size:var(--rh-font-sm);margin:var(--rh-space-4) 0}\
.rh-meta-grid dt{color:var(--rh-muted);font-weight:500}\
.rh-meta-grid dd{margin:0}\
.rh-queue{list-style:none;margin:var(--rh-space-3) 0 0;padding:0;display:flex;flex-direction:column;gap:var(--rh-space-2)}\
.rh-queue-item{background:var(--rh-surface);border-radius:var(--rh-radius-lg);padding:.65rem var(--rh-space-4);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-queue-head{display:flex;gap:var(--rh-space-2);align-items:center}\
.rh-queue-name{font-weight:600}\
.rh-queue-pct{margin-left:auto;font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums}\
.rh-bar{height:.45rem;border-radius:var(--rh-radius-full);background:color-mix(in srgb,var(--rh-text) 12%,transparent);margin-top:.55rem;overflow:hidden}\
.rh-bar-fill{height:100%;border-radius:var(--rh-radius-full);background:linear-gradient(90deg,color-mix(in srgb,var(--rh-brand) 70%,var(--rh-surface)),var(--rh-brand));width:100%;transform-origin:left;transition:transform .3s ease}\
.rh-bar-fill.failed{background:var(--rh-error)}\
.rh-badge{font-size:var(--rh-font-xs);font-weight:600;padding:.1rem .5rem;border-radius:var(--rh-radius-full);background:color-mix(in srgb,var(--rh-text) 10%,transparent);color:var(--rh-muted);text-transform:uppercase;letter-spacing:.04em}\
.rh-badge.active{background:color-mix(in srgb,var(--rh-accent) 16%,transparent);color:var(--rh-accent)}\
.rh-badge.done{background:color-mix(in srgb,#3fbf7f 18%,transparent);color:color-mix(in srgb,#3fbf7f 72%,var(--rh-text))}\
.rh-badge.failed{background:color-mix(in srgb,var(--rh-error) 16%,transparent);color:var(--rh-error)}\
.rh-badge.live{background:var(--rh-error);color:#fff;letter-spacing:.06em;box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-error) 22%,transparent)}\
.rh-stations{max-width:32rem;border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-station-head{display:flex;gap:var(--rh-space-2);align-items:center;width:100%}\
.rh-station-name{font-weight:600;color:var(--rh-text)}\
.rh-station-track{font-size:var(--rh-font-sm);color:var(--rh-muted)}\
/* The radio player: what is on (with its sleeve), the controls, what came before. */\
.rh-player{flex:1;min-width:0;overflow-y:auto}\
.rh-player-now{display:flex;align-items:flex-end;gap:var(--rh-space-5);margin-bottom:var(--rh-space-5)}\
.rh-player-cover{flex:none;display:block;width:11rem;height:11rem;border-radius:var(--rh-radius-lg);object-fit:cover;background-color:var(--rh-surface-2);box-shadow:var(--rh-shadow-2)}\
.rh-player-track{min-width:0;display:flex;flex-direction:column;gap:.15rem;padding-bottom:.2rem}\
.rh-player-title{margin:0;font-family:var(--rh-font-display);font-size:var(--rh-font-xl);font-weight:700;letter-spacing:-.02em;line-height:1.15;color:var(--rh-text);text-wrap:balance;overflow-wrap:anywhere}\
.rh-player-artist{margin:0;font-size:var(--rh-font-lg);color:var(--rh-text)}\
.rh-player-artist:empty{display:none}\
.rh-player-station{margin:.35rem 0 0;font-size:var(--rh-font-sm);color:var(--rh-muted);font-variant-numeric:tabular-nums;text-wrap:pretty}\
.rh-player-controls{align-items:center;margin-bottom:var(--rh-space-2)}\
.rh-player-controls .rh-slider{flex:1;max-width:16rem}\
.rh-player-heading{margin:var(--rh-space-6) 0 var(--rh-space-2);font-family:var(--rh-font-display);font-size:var(--rh-font-size);font-weight:600;color:var(--rh-text)}\
.rh-player-recent{list-style:none;margin:0;padding:0;counter-reset:rh-played;max-width:40rem}\
.rh-player-recent li{counter-increment:rh-played;display:grid;grid-template-columns:1.6rem minmax(0,1.4fr) minmax(0,1fr) auto;align-items:baseline;column-gap:var(--rh-space-3);min-height:2rem;padding:.3rem 0;border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);font-size:var(--rh-font-sm)}\
.rh-player-recent li::before{content:counter(rh-played);font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums;text-align:right}\
.rh-player-recent-title{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--rh-text)}\
.rh-player-recent-artist{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--rh-muted)}\
.rh-player-recent-when{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums}\
.rh-slider{accent-color:var(--rh-accent);flex:1}\
.rh-hint{color:var(--rh-muted);font-size:var(--rh-font-sm);margin:.3rem 0;line-height:1.5;max-width:70ch}\
.rh-settings-note{max-width:70ch}\
.rh-radio-now{color:var(--rh-accent);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:18rem;display:inline-flex;align-items:center;gap:.4rem}\
.rh-radio-now::before{content:'';width:.5rem;height:.5rem;border-radius:50%;background:var(--rh-error);flex:none;box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-error) 25%,transparent)}\
.rh-radio-now:hover{text-decoration:underline}\
.rh-live-slot:empty{display:none}\
.rh-admin-main{flex:1;display:flex;flex-direction:column;min-width:0;min-height:0}\
/* The admin console: a sections navbar beside one pane. Settings rows sit in\
   bordered groups, name and meaning on the left, the control on the right;\
   nothing has a Save of its own (the bar at the pane's foot saves what is\
   staged). Ember marks what is not saved yet: it is the operator's own\
   pending work, the one warm thing on a burrow's cool surface. */\
.rh-adm{flex:1;min-height:0;display:grid;grid-template-columns:13.5rem minmax(0,1fr)}\
.rh-adm-nav{overflow-y:auto;padding:var(--rh-space-4) var(--rh-space-2) var(--rh-space-5) var(--rh-space-3);border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);background:color-mix(in srgb,var(--rh-surface) 55%,var(--rh-bg))}\
.rh-adm-nav ul{list-style:none;margin:0 0 var(--rh-space-4);padding:0;display:flex;flex-direction:column;gap:1px}\
.rh-adm-nav-h{margin:0 0 .3rem;padding:0 .6rem;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted)}\
.rh-adm-link{display:flex;align-items:center;justify-content:space-between;gap:.5rem;padding:.38rem .6rem;border-radius:var(--rh-radius-sm);color:var(--rh-text);text-decoration:none;font-size:var(--rh-font-sm);transition:background-color .12s ease,color .12s ease}\
.rh-adm-link:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-adm-link[aria-current=page]{background:color-mix(in srgb,var(--rh-accent) 14%,transparent);color:var(--rh-accent);font-weight:650}\
.rh-adm-link-dot{flex:none;width:.45rem;height:.45rem;border-radius:50%;background:var(--rh-brand)}\
.rh-adm-jump{display:none}\
.rh-adm-pane{overflow-y:auto;min-width:0;display:flex;flex-direction:column;padding:var(--rh-space-5) var(--rh-space-5) 0}\
.rh-adm-pane>*{width:100%;max-width:46rem;flex:none}\
.rh-adm-head{margin:0 0 var(--rh-space-5)}\
.rh-adm-title{margin:0 0 .3rem;font-family:var(--rh-font-display);font-size:1.45rem;font-weight:700;letter-spacing:-.02em;color:var(--rh-text)}\
.rh-adm-blurb{margin:0;color:var(--rh-muted);max-width:60ch;line-height:1.5}\
.rh-adm-group{margin:0 0 var(--rh-space-5)}\
.rh-adm-group-h{margin:0 0 .4rem;font-size:var(--rh-font-size);font-weight:650;color:var(--rh-text)}\
.rh-adm-group .rh-adm-group-h:not(:first-child){margin-top:var(--rh-space-5)}\
.rh-adm-group-blurb{margin:-.1rem 0 var(--rh-space-2);color:var(--rh-muted);font-size:var(--rh-font-sm);max-width:62ch;line-height:1.5}\
.rh-adm-rows{border:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent);border-radius:var(--rh-radius);background:var(--rh-surface);overflow:clip}\
.rh-adm-row{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:start;gap:var(--rh-space-2) var(--rh-space-4);padding:.8rem var(--rh-space-4);transition:background-color .18s ease}\
.rh-adm-row+.rh-adm-row{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-adm-row.long{grid-template-columns:minmax(0,1fr)}\
.rh-adm-row.edited{background:color-mix(in srgb,var(--rh-brand) 8%,transparent)}\
.rh-adm-label{display:block;font-weight:600;font-size:var(--rh-font-sm);color:var(--rh-text)}\
.rh-adm-help{margin:.15rem 0 0;color:var(--rh-muted);font-size:var(--rh-font-sm);line-height:1.45;max-width:58ch}\
.rh-adm-meta{margin:.35rem 0 0;display:flex;flex-wrap:wrap;align-items:center;gap:.15rem .75rem;font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-adm-meta:empty{display:none}\
.rh-adm-edited{font-weight:600;color:color-mix(in srgb,var(--rh-brand) 62%,var(--rh-text))}\
.rh-adm-reset{appearance:none;border:0;background:none;padding:0;font:inherit;color:var(--rh-accent);cursor:pointer;text-decoration:underline;text-underline-offset:.18em;border-radius:2px}\
.rh-adm-reset:hover{color:color-mix(in srgb,var(--rh-accent) 78%,var(--rh-text))}\
.rh-adm-surface{margin:.4rem 0 0;display:flex;align-items:baseline;gap:.45rem;font-size:var(--rh-font-xs);font-weight:600;color:color-mix(in srgb,#3fbf7f 62%,var(--rh-text))}\
.rh-adm-surface-dot{flex:none;align-self:center;width:.45rem;height:.45rem;border-radius:50%;background:#3fbf7f}\
.rh-adm-surface.bad{color:var(--rh-error)}\
.rh-adm-surface.bad .rh-adm-surface-dot{background:var(--rh-error)}\
.rh-adm-surface.waiting{color:var(--rh-muted)}\
.rh-adm-surface.waiting .rh-adm-surface-dot{background:var(--rh-muted)}\
.rh-adm-state{margin:.35rem 0 0;font-size:var(--rh-font-xs);font-weight:600;color:color-mix(in srgb,#3fbf7f 62%,var(--rh-text))}\
.rh-adm-state.bad{color:var(--rh-error)}\
.rh-adm-control{display:flex;align-items:center;justify-content:flex-end;min-height:2.1rem}\
.rh-adm-row.long .rh-adm-control{justify-content:stretch}\
.rh-adm-text{width:17rem;max-width:100%}\
.rh-adm-long{width:100%;min-height:5.5rem;resize:vertical;line-height:1.45}\
.rh-adm-number{display:flex;flex-direction:column;align-items:flex-end;gap:.2rem}\
.rh-adm-num{width:9.5rem;text-align:right;font-variant-numeric:tabular-nums}\
.rh-adm-num[aria-invalid=true]{border-color:var(--rh-error);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-error) 18%,transparent)}\
.rh-adm-echo{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-adm-echo:empty{display:none}\
.rh-adm-fixed{max-width:17rem;font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted);text-align:right;overflow-wrap:anywhere}\
.rh-adm-choice{min-width:13rem;max-width:17rem}\
.rh-adm-note{margin:0 0 var(--rh-space-4);padding:.7rem var(--rh-space-4);border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-text) 5%,transparent);color:var(--rh-muted);font-size:var(--rh-font-sm);line-height:1.5}\
.rh-adm-status{margin:0 0 var(--rh-space-3);color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-adm-status:empty{display:none}\
.rh-adm-skeleton div{height:3.6rem;background:linear-gradient(90deg,transparent,color-mix(in srgb,var(--rh-text) 5%,transparent),transparent);background-size:200% 100%;animation:rh-adm-sheen 1.4s ease-in-out infinite}\
.rh-adm-skeleton div+div{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
@keyframes rh-adm-sheen{from{background-position:200% 0}to{background-position:-200% 0}}\
/* People: accounts, classes and invitations share one row vocabulary. A row\
   is a summary line that opens its own controls in place, so an operator\
   never leaves the list to act on what is in it. */\
.rh-adm-group-head{display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:var(--rh-space-2);margin:0 0 .5rem}\
.rh-adm-group-head .rh-adm-group-h{margin:0}\
.rh-adm-group-tools{display:flex;align-items:center;gap:var(--rh-space-2);flex-wrap:wrap}\
.rh-adm-count{margin-left:.45rem;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);font-variant-numeric:tabular-nums}\
.rh-adm-filter{width:11rem;padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-adm-empty{margin:0;padding:var(--rh-space-4);color:var(--rh-muted);font-size:var(--rh-font-sm);text-align:center}\
.rh-adm-acct+.rh-adm-acct,.rh-adm-invite+.rh-adm-invite{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-adm-acct-line{appearance:none;width:100%;border:0;background:none;font:inherit;color:inherit;text-align:left;cursor:pointer;display:grid;grid-template-columns:minmax(0,1.5fr) minmax(0,1fr) minmax(0,1fr) 5rem 1rem;align-items:center;gap:var(--rh-space-3);padding:.65rem var(--rh-space-4);font-size:var(--rh-font-sm);transition:background-color .12s ease}\
.rh-adm-class-line{grid-template-columns:minmax(0,1.5fr) minmax(0,1fr) minmax(0,1fr) 1rem}\
.rh-adm-acct-line:hover{background:color-mix(in srgb,var(--rh-text) 4%,transparent)}\
.rh-adm-acct.open>.rh-adm-acct-line{background:color-mix(in srgb,var(--rh-accent) 8%,transparent)}\
.rh-adm-acct-name{display:flex;align-items:center;gap:.5rem;min-width:0;font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-acct-role,.rh-adm-acct-class,.rh-adm-acct-state{color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-acct.off .rh-adm-acct-name{color:var(--rh-muted);text-decoration:line-through}\
.rh-adm-acct.off .rh-adm-acct-state{color:var(--rh-error);font-weight:600}\
.rh-adm-chevron{line-height:0;color:var(--rh-muted);transition:transform .18s cubic-bezier(.16,1,.3,1)}\
.rh-adm-chevron svg{width:1rem;height:1rem}\
.rh-adm-acct.open .rh-adm-chevron{transform:rotate(180deg)}\
.rh-adm-acct-detail{padding:var(--rh-space-3) var(--rh-space-4) var(--rh-space-4);border-top:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent);background:color-mix(in srgb,var(--rh-bg) 55%,var(--rh-surface))}\
.rh-adm-acct-fields{display:grid;grid-template-columns:repeat(auto-fit,minmax(13rem,1fr));gap:var(--rh-space-3) var(--rh-space-4);margin:0 0 var(--rh-space-3)}\
.rh-adm-acct-actions{display:flex;flex-wrap:wrap;justify-content:flex-end;gap:var(--rh-space-2)}\
.rh-adm-field{display:flex;flex-direction:column;gap:.3rem;min-width:0;font-size:var(--rh-font-sm)}\
.rh-adm-field>span:first-child{font-weight:600}\
.rh-adm-field small{color:var(--rh-muted);font-size:var(--rh-font-xs);line-height:1.4}\
.rh-adm-field .rh-input,.rh-adm-field .rh-select{width:100%}\
.rh-adm-inline{display:flex;gap:var(--rh-space-2);align-items:center}\
.rh-adm-inline .rh-input{flex:1;min-width:0;font-family:var(--rh-font-mono);font-size:var(--rh-font-sm)}\
.rh-adm-inline .rh-btn{flex:none}\
.rh-adm-form,.rh-adm-reset-form{display:grid;gap:var(--rh-space-3)}\
.rh-adm-form{grid-template-columns:repeat(auto-fit,minmax(14rem,1fr));margin:0 0 var(--rh-space-3);padding:var(--rh-space-4);border:1px solid color-mix(in srgb,var(--rh-accent) 30%,transparent);border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-accent) 5%,var(--rh-surface))}\
.rh-adm-form-actions{grid-column:1/-1;display:flex;justify-content:flex-end;gap:var(--rh-space-2)}\
.rh-adm-reset-form{margin-top:var(--rh-space-3)}\
.rh-btn.ghost.rh-adm-danger{color:var(--rh-error);border-color:color-mix(in srgb,var(--rh-error) 40%,transparent)}\
.rh-btn.ghost.rh-adm-danger:hover{background:color-mix(in srgb,var(--rh-error) 10%,transparent);border-color:var(--rh-error)}\
.rh-adm-add{display:flex;gap:var(--rh-space-2);margin-top:var(--rh-space-2)}\
.rh-adm-add .rh-input{width:14rem;max-width:100%;padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-adm-caps{margin:0 0 var(--rh-space-3);padding:0;border:0;display:grid;grid-template-columns:repeat(auto-fill,minmax(15rem,1fr));gap:.15rem var(--rh-space-4)}\
.rh-adm-caps legend{grid-column:1/-1;padding:0;margin:0 0 .25rem;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted)}\
.rh-adm-cap{display:flex;align-items:flex-start;gap:.55rem;padding:.3rem 0;font-size:var(--rh-font-sm);cursor:pointer}\
.rh-adm-cap input{margin:.2rem 0 0;flex:none;accent-color:var(--rh-accent)}\
.rh-adm-cap strong{display:block;font-weight:600}\
.rh-adm-cap small{display:block;color:var(--rh-muted);font-size:var(--rh-font-xs);line-height:1.4}\
.rh-adm-cap.locked{cursor:not-allowed;opacity:.55}\
.rh-post-remove{appearance:none;margin-left:auto;border:0;background:none;padding:.1rem .3rem;font:inherit;font-size:var(--rh-font-xs);color:var(--rh-muted);cursor:pointer;border-radius:var(--rh-radius-sm);opacity:0;transition:opacity .12s ease,color .12s ease}\
.rh-post:hover .rh-post-remove,.rh-post-remove:focus-visible{opacity:1}\
.rh-post-remove:hover{color:var(--rh-error)}\
.rh-post-gone{margin:.25rem 0 0;color:var(--rh-muted);font-size:var(--rh-font-sm);font-style:italic}\
@media (hover:none){.rh-post-remove{opacity:1}}\
.rh-filetable .rh-tree-item{position:relative}\
.rh-file-row-remove{appearance:none;position:absolute;right:.5rem;top:50%;transform:translateY(-50%);border:0;background:none;padding:.15rem .4rem;font:inherit;font-size:var(--rh-font-xs);color:var(--rh-muted);cursor:pointer;border-radius:var(--rh-radius-sm);opacity:0;transition:opacity .12s ease,color .12s ease}\
.rh-tree-item:hover .rh-file-row-remove,.rh-file-row-remove:focus-visible{opacity:1}\
.rh-file-row-remove:hover{color:var(--rh-error)}\
.rh-file-row-send{appearance:none;position:absolute;right:.5rem;top:50%;transform:translateY(-50%);border:0;background:none;padding:.15rem .4rem;font:inherit;font-size:var(--rh-font-xs);color:var(--rh-muted);cursor:pointer;border-radius:var(--rh-radius);opacity:0}\
.rh-file-row-send.beside{right:4.5rem}\
.rh-tree-item:hover .rh-file-row-send,.rh-file-row-send:focus-visible{opacity:1}\
.rh-file-row-send:hover{color:var(--rh-accent)}\
@media (hover:none){.rh-file-row-send{opacity:1}}\
.rh-card-actions{display:flex;flex-wrap:wrap;gap:var(--rh-space-2)}\
.rh-confirm.rh-send{width:min(34rem,100%)}\
.rh-send .rh-send-lead{margin:.25rem 0 0;color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-send .rh-send-step{margin:var(--rh-space-4) 0 var(--rh-space-2);font-size:var(--rh-font-xs);font-weight:600;letter-spacing:.04em;text-transform:uppercase;color:var(--rh-muted)}\
.rh-send-burrows{display:flex;flex-wrap:wrap;gap:var(--rh-space-2)}\
.rh-send-burrow{appearance:none;border:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent);background:none;color:var(--rh-text);font:inherit;font-size:var(--rh-font-sm);padding:.4rem .8rem;border-radius:var(--rh-radius-full);cursor:pointer}\
.rh-send-burrow:hover{border-color:var(--rh-accent)}\
.rh-send-burrow.active{border-color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent);color:var(--rh-accent);font-weight:600}\
.rh-send-place{border:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent);border-radius:var(--rh-radius);max-height:16rem;overflow-y:auto}\
.rh-send-crumbs{display:flex;flex-wrap:wrap;align-items:center;gap:.15rem;padding:.4rem .6rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);font-size:var(--rh-font-sm)}\
.rh-send-list{list-style:none;margin:0;padding:.25rem}\
.rh-send-row{appearance:none;display:flex;align-items:center;gap:.5rem;width:100%;border:0;background:none;color:var(--rh-text);font:inherit;font-size:var(--rh-font-sm);text-align:left;padding:.4rem .5rem;border-radius:var(--rh-radius);cursor:pointer}\
.rh-send-row:hover,.rh-send-row:focus-visible{background:color-mix(in srgb,var(--rh-accent) 10%,transparent)}\
.rh-send-note{margin-left:auto;color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-send .rh-send-empty{padding:.6rem .5rem;color:var(--rh-muted);font-size:var(--rh-font-sm);list-style:none}\
.rh-send .rh-send-problem{margin:var(--rh-space-3) 0 0;color:var(--rh-error);font-size:var(--rh-font-sm)}\
@media (hover:none){.rh-file-row-remove{opacity:1}}\
.rh-newfolder{display:flex;flex-wrap:wrap;align-items:center;gap:var(--rh-space-2);width:100%;padding:var(--rh-space-2) 0}\
.rh-newfolder .rh-input{flex:1 1 12rem;min-width:0;padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-node-manage{display:flex;flex-direction:column;gap:var(--rh-space-2);margin-top:var(--rh-space-4);padding-top:var(--rh-space-3);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-node-manage-row{display:flex;gap:var(--rh-space-2);flex-wrap:wrap}\
.rh-toolbar-limits{margin-left:auto;color:var(--rh-muted);font-size:var(--rh-font-xs);font-variant-numeric:tabular-nums;white-space:nowrap}\
.rh-queue-why{margin:.3rem 0 0;color:var(--rh-error);font-size:var(--rh-font-xs)}\
.rh-move-bar{display:flex;align-items:center;justify-content:space-between;gap:var(--rh-space-3);flex-wrap:wrap;margin:0 0 var(--rh-space-3);padding:.55rem var(--rh-space-4);border:1px solid color-mix(in srgb,var(--rh-accent) 35%,transparent);background:color-mix(in srgb,var(--rh-accent) 8%,transparent);border-radius:var(--rh-radius);font-size:var(--rh-font-sm)}\
.rh-move-bar-why{color:var(--rh-muted)}\
.rh-move-bar-actions{display:flex;gap:var(--rh-space-2);flex-shrink:0}\
.rh-node-manage>.rh-btn{align-self:flex-start}\
.rh-adm-tabs{display:flex;flex-wrap:wrap;gap:.25rem;margin:0 0 var(--rh-space-2)}\
.rh-adm-tab{appearance:none;border:1px solid transparent;background:none;padding:.3rem .7rem;font:inherit;font-size:var(--rh-font-sm);font-weight:500;color:var(--rh-muted);border-radius:var(--rh-radius-full);cursor:pointer;transition:background-color .12s ease,color .12s ease}\
.rh-adm-tab:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-adm-tab[aria-selected=true]{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent);font-weight:600}\
.rh-adm-report{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:var(--rh-space-2) var(--rh-space-4);padding:.7rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-report+.rh-adm-report,.rh-adm-session+.rh-adm-session,.rh-adm-deny+.rh-adm-deny,.rh-adm-audit-line+.rh-adm-audit-line{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-adm-report-line{margin:0;font-weight:600}\
.rh-adm-report-meta{margin:.15rem 0 0;color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-adm-report-actions{display:flex;flex-wrap:wrap;gap:var(--rh-space-2);align-items:center;justify-content:flex-end}\
.rh-adm-report-note{grid-column:1/-1;display:flex;gap:var(--rh-space-2)}\
.rh-adm-report-note .rh-input{flex:1;min-width:0;padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-adm-session{display:grid;grid-template-columns:minmax(0,1.4fr) minmax(0,1fr) minmax(0,1fr) auto;align-items:center;gap:var(--rh-space-3);padding:.55rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-session-name{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-session-meta{color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-deny{display:grid;grid-template-columns:auto minmax(0,1fr) auto;align-items:center;gap:var(--rh-space-3);padding:.55rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-deny-why{color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-audit{max-height:24rem;overflow-y:auto}\
.rh-adm-audit-line{display:grid;grid-template-columns:7rem minmax(0,1fr);gap:var(--rh-space-3);padding:.4rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-audit-when{color:var(--rh-muted);font-variant-numeric:tabular-nums;white-space:nowrap}\
.rh-adm-broadcast{display:flex;gap:var(--rh-space-2);align-items:flex-start}\
.rh-adm-broadcast .rh-input{flex:1;min-width:0;resize:vertical;min-height:3.2rem}\
.rh-adm-peer{display:grid;grid-template-columns:minmax(0,1.6fr) minmax(0,1fr) auto;align-items:center;gap:var(--rh-space-3);padding:.6rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-peer+.rh-adm-peer,.rh-adm-origin+.rh-adm-origin,.rh-adm-backup+.rh-adm-backup{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-adm-peer-who,.rh-adm-backup-who{display:flex;flex-direction:column;gap:.15rem;min-width:0}\
.rh-adm-peer-name,.rh-adm-backup-name,.rh-adm-origin-name{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-peer-meta,.rh-adm-backup-meta{color:var(--rh-muted);font-size:var(--rh-font-xs);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-peer-state{color:var(--rh-muted)}\
.rh-adm-peer.pending .rh-adm-peer-state{color:var(--rh-accent);font-weight:600}\
.rh-adm-peer-waiting{color:var(--rh-accent);font-weight:600}\
.rh-adm-peer-actions,.rh-adm-backup-actions{display:flex;gap:var(--rh-space-2);align-items:center;justify-content:flex-end;flex-wrap:wrap}\
.rh-adm-peer-actions .rh-input{width:13rem;max-width:100%;padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-requests{display:flex;flex-direction:column;gap:var(--rh-space-2);margin-top:var(--rh-space-4);padding-top:var(--rh-space-3);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-requests-head{display:flex;align-items:center;justify-content:space-between;gap:var(--rh-space-2)}\
.rh-requests-title{margin:0;font-size:var(--rh-font-sm);font-weight:600}\
.rh-requests-empty{margin:0;color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-requests-list{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.35rem}\
.rh-request{display:grid;grid-template-columns:2.6rem minmax(0,1fr) auto;align-items:center;gap:var(--rh-space-2);font-size:var(--rh-font-sm)}\
.rh-request-place{color:var(--rh-muted);font-size:var(--rh-font-xs);font-variant-numeric:tabular-nums}\
.rh-request-what{min-width:0;display:flex;flex-direction:column}\
.rh-request-title{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-request-meta{color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-requests-browse{display:flex;flex-direction:column;gap:var(--rh-space-2);padding-top:var(--rh-space-2);border-top:1px dashed color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-requests-find{padding:.35rem .6rem;font-size:var(--rh-font-sm)}\
.rh-requests-offer{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.25rem;max-height:16rem;overflow:auto}\
.rh-requests-more{margin:0;color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-requests-offer li{display:flex;align-items:center;justify-content:space-between;gap:var(--rh-space-2);font-size:var(--rh-font-sm);min-width:0}\
.rh-room-bar{display:flex;flex-wrap:wrap;align-items:center;gap:var(--rh-space-2);padding:.4rem var(--rh-space-4);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);font-size:var(--rh-font-sm)}\
.rh-room-topic{flex:1 1 12rem;min-width:0;color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-room-bar .rh-input{flex:1 1 10rem;min-width:0;padding:.3rem .55rem;font-size:var(--rh-font-sm)}\
.rh-room-invite{max-width:16rem}\
.rh-check{display:flex;align-items:center;gap:.4rem;font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-rooms{display:flex;flex-wrap:wrap;align-items:center;gap:var(--rh-space-2);padding:.45rem var(--rh-space-4);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-rooms .rh-btn[aria-selected=true]{background:color-mix(in srgb,var(--rh-accent) 16%,transparent);color:var(--rh-accent)}\
.rh-rooms .rh-badge{margin-left:.35rem}\
.rh-wish-tools{display:flex;flex-wrap:wrap;align-items:center;gap:var(--rh-space-2);margin:0 0 var(--rh-space-3)}\
.rh-wish-tools .rh-art-areas{margin:0;flex:1 1 auto}\
.rh-wish-form{display:flex;flex-direction:column;gap:var(--rh-space-2);padding:var(--rh-space-3);margin:0 0 var(--rh-space-3);border:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent);border-radius:var(--rh-radius);background:var(--rh-surface)}\
.rh-wish-form .rh-field{display:flex;flex-direction:column;gap:.2rem;font-size:var(--rh-font-sm)}\
.rh-wish-form .rh-field>span{color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-wish-list{list-style:none;margin:0;padding:0;border:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent);border-radius:var(--rh-radius);background:var(--rh-surface);overflow:clip}\
.rh-wish{display:flex;flex-direction:column;gap:.3rem;padding:.8rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-wish+.rh-wish{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-wish-head{display:flex;align-items:center;gap:var(--rh-space-2);flex-wrap:wrap;min-width:0}\
.rh-wish-title{font-weight:600;min-width:0;overflow:hidden;text-overflow:ellipsis}\
.rh-wish-details{margin:0;white-space:pre-wrap}\
.rh-wish-meta{margin:0;color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-wish-done{margin:0;font-size:var(--rh-font-xs);color:var(--rh-brand)}\
.rh-wish .rh-wish-tools{margin:.1rem 0 0}\
.rh-art-areas{display:flex;flex-wrap:wrap;gap:var(--rh-space-2);margin:0 0 var(--rh-space-3)}\
.rh-art-list{list-style:none;display:flex;flex-wrap:wrap;gap:var(--rh-space-2);margin:0 0 var(--rh-space-3);padding:0}\
.rh-art-list .rh-btn[aria-pressed=true],.rh-art-areas .rh-btn[aria-selected=true]{background:color-mix(in srgb,var(--rh-accent) 16%,transparent);color:var(--rh-accent)}\
.rh-adm-station{display:flex;flex-direction:column;gap:.3rem;padding:.7rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-station+.rh-adm-station{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-adm-station-head{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:start;gap:var(--rh-space-3)}\
.rh-adm-station-who{display:flex;flex-direction:column;gap:.15rem;min-width:0}\
.rh-adm-station-name{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-station-meta{color:var(--rh-muted);font-size:var(--rh-font-xs)}\
.rh-adm-station-state{display:flex;align-items:center;gap:var(--rh-space-2);color:var(--rh-muted);white-space:nowrap}\
.rh-adm-station-left{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-adm-station-left>summary{cursor:pointer;padding:.1rem 0;border-radius:var(--rh-radius-sm)}\
.rh-adm-station-left>summary:focus-visible{outline:2px solid var(--rh-brand);outline-offset:2px}\
.rh-adm-station-left ul{list-style:none;margin:.3rem 0 0;padding:0 0 0 var(--rh-space-3);display:flex;flex-direction:column;gap:.15rem}\
.rh-adm-station-left li{display:flex;gap:var(--rh-space-2);min-width:0}\
.rh-adm-station-track{font-weight:600;flex:0 1 auto;max-width:45%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-station-why{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-origin{display:grid;grid-template-columns:minmax(0,1.2fr) auto minmax(0,1fr);align-items:center;gap:var(--rh-space-3);padding:.55rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-origin-trust{color:var(--rh-muted);text-align:right}\
.rh-adm-backup{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:center;gap:var(--rh-space-3);padding:.6rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-backup-check{font-size:var(--rh-font-xs);color:var(--rh-muted);white-space:normal}\
.rh-adm-backup-check.rh-adm-bad{color:var(--rh-error)}\
.rh-adm-code{margin:var(--rh-space-2) 0 0;padding:.6rem var(--rh-space-4);background:color-mix(in srgb,var(--rh-text) 5%,transparent);border-radius:var(--rh-radius);font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);white-space:pre-wrap}\
.rh-adm-board-line{grid-template-columns:minmax(0,1.4fr) minmax(0,1fr) 6rem 1rem}\
.rh-adm-mono{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs)}\
.rh-adm-wide{grid-column:1/-1}\
.rh-adm-field small.rh-adm-bad{color:var(--rh-error)}\
.rh-adm-field .rh-input[aria-invalid=true]{border-color:var(--rh-error)}\
.rh-adm-invite{display:grid;grid-template-columns:minmax(0,1.4fr) minmax(0,.8fr) minmax(0,1fr) auto;align-items:center;gap:var(--rh-space-3);padding:.55rem var(--rh-space-4);font-size:var(--rh-font-sm)}\
.rh-adm-invite-code{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-invite-by,.rh-adm-invite-state{color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-adm-invite.spent .rh-adm-invite-code{color:var(--rh-muted);text-decoration:line-through}\
.rh-adm-invite-actions{display:flex;gap:var(--rh-space-2);justify-content:flex-end;min-width:0}\
/* A switch is a checkbox that says so (role=switch): the native control keeps\
   the keyboard, the form semantics and the focus ring for free. The off track\
   is dark enough to be seen as a control against the row (3:1). */\
.rh-switch{appearance:none;-webkit-appearance:none;position:relative;flex:none;width:2.5rem;height:1.5rem;margin:0;border-radius:var(--rh-radius-full);background:color-mix(in srgb,var(--rh-text) 44%,transparent);cursor:pointer;transition:background-color .18s ease}\
.rh-switch::before{content:\"\";position:absolute;top:.15rem;left:.15rem;width:1.2rem;height:1.2rem;border-radius:50%;background:#fff;box-shadow:0 1px 3px color-mix(in srgb,#000 38%,transparent);transition:transform .2s cubic-bezier(.16,1,.3,1)}\
.rh-switch:hover{background:color-mix(in srgb,var(--rh-text) 54%,transparent)}\
.rh-switch:checked{background:var(--rh-accent)}\
.rh-switch:checked:hover{background:color-mix(in srgb,var(--rh-accent) 88%,var(--rh-text))}\
.rh-switch:checked::before{transform:translateX(1rem)}\
.rh-switch:active::before{width:1.4rem}\
.rh-switch:checked:active::before{transform:translateX(.8rem)}\
.rh-switch:focus-visible{outline:2px solid var(--rh-accent);outline-offset:2px}\
.rh-switch:disabled{opacity:.5;cursor:not-allowed}\
/* A dropdown in the text field's clothes. The arrow is two gradients, so it\
   takes the theme's colour (a data: image could not). */\
.rh-select{appearance:none;-webkit-appearance:none;font:inherit;font-size:var(--rh-font-sm);padding:.48rem 2rem .48rem .7rem;border-radius:var(--rh-radius);border:1px solid color-mix(in srgb,var(--rh-text) 16%,transparent);background-color:color-mix(in srgb,var(--rh-bg) 60%,var(--rh-surface));color:var(--rh-text);cursor:pointer;background-image:linear-gradient(45deg,transparent 50%,var(--rh-muted) 50%),linear-gradient(135deg,var(--rh-muted) 50%,transparent 50%);background-position:calc(100% - 1.02rem) 52%,calc(100% - .7rem) 52%;background-size:.32rem .32rem;background-repeat:no-repeat;transition:border-color .15s ease,box-shadow .15s ease}\
.rh-select:hover{border-color:color-mix(in srgb,var(--rh-text) 26%,transparent)}\
.rh-select:focus-visible{outline:2px solid transparent}\
.rh-select:focus{border-color:var(--rh-accent);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-accent) 24%,transparent)}\
/* The one Save. It rises when something is staged and sinks when nothing is;\
   sticky, so it is in reach however far down the pane you are. */\
.rh-adm-savebar{position:sticky;bottom:var(--rh-space-4);z-index:2;margin:auto 0 var(--rh-space-4);display:flex;align-items:center;gap:var(--rh-space-2);padding:.65rem .75rem .65rem var(--rh-space-4);border:1px solid color-mix(in srgb,var(--rh-brand) 38%,transparent);border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-brand) 10%,var(--rh-surface));box-shadow:0 .7rem 1.8rem -.5rem color-mix(in srgb,#000 34%,transparent);transform:translateY(160%);opacity:0;visibility:hidden;transition:transform .24s cubic-bezier(.16,1,.3,1),opacity .18s ease,visibility 0s linear .24s}\
.rh-adm-savebar.show{transform:none;opacity:1;visibility:visible;transition-delay:0s}\
.rh-adm-savebar-text{flex:1;min-width:0;margin:0;display:flex;flex-wrap:wrap;gap:0 .5rem;font-size:var(--rh-font-sm);color:var(--rh-text)}\
.rh-adm-savebar-text span{color:var(--rh-muted)}\
.rh-table{width:100%;border-collapse:collapse;font-size:var(--rh-font-sm);margin:0 0 var(--rh-space-4)}\
.rh-table th{text-align:left;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);padding:.3rem var(--rh-space-3) .5rem 0}\
.rh-table td{padding:.5rem var(--rh-space-3) .5rem 0;vertical-align:middle}\
.rh-table tbody tr{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-table tbody tr:hover{background:color-mix(in srgb,var(--rh-text) 4%,transparent)}\
.rh-fieldset{border:0;padding:0;margin:0;min-width:0}\
.rh-fieldset legend{float:left;padding:0}\
/* A config row is a three-column grid: key, value, Save. As a wrapping flex\
   row, long keys pushed their input into the next line and short ones left\
   the inputs ragged. */\
.rh-account-role{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-editor{display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-editor-row{display:flex;gap:var(--rh-space-2);align-items:center}\
.rh-var-name{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);color:var(--rh-muted);min-width:8.5rem}\
.rh-swatch{width:1.3rem;height:1.3rem;flex:none;display:inline-block;border:1px solid color-mix(in srgb,var(--rh-text) 20%,transparent);border-radius:var(--rh-radius-sm)}\
.rh-warn{color:var(--rh-error);font-size:var(--rh-font-sm);margin:.2rem 0}\
.rh-textarea{font-family:var(--rh-font-mono);font-size:var(--rh-font-xs);width:100%;min-height:8rem;background:color-mix(in srgb,var(--rh-bg) 60%,var(--rh-surface));color:var(--rh-text);border:1px solid color-mix(in srgb,var(--rh-text) 16%,transparent);border-radius:var(--rh-radius);padding:var(--rh-space-2)}\
.rh-preview{font-family:var(--rh-font-sans);font-size:var(--rh-font-sm);color:var(--rh-text);background-color:var(--rh-bg);background-image:var(--rh-bg-image);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius-lg);overflow:hidden;margin:var(--rh-space-2) 0;box-shadow:var(--rh-shadow-1)}\
.rh-preview-body{padding:var(--rh-space-4);display:flex;flex-direction:column;gap:var(--rh-space-2);align-items:flex-start}\
.rh-art-wrap{padding:var(--rh-space-5);overflow:auto}\
.rh-art{background:#000;border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius-lg);image-rendering:pixelated;max-width:100%;box-shadow:var(--rh-shadow-2)}\
/* Every scroller, not an opt-in list: mismatched scrollbars between adjacent\
   panes is a pure web-page artifact (the old whitelist missed the main\
   content pane and the command palette). */\
*{scrollbar-width:thin;scrollbar-color:color-mix(in srgb,var(--rh-text) 25%,transparent) transparent}\
::-webkit-scrollbar{width:10px;height:10px}\
::-webkit-scrollbar-thumb{background:color-mix(in srgb,var(--rh-text) 18%,transparent);border-radius:var(--rh-radius-full);border:3px solid transparent;background-clip:padding-box}\
::-webkit-scrollbar-thumb:hover{background:color-mix(in srgb,var(--rh-text) 30%,transparent);background-clip:padding-box}\
::-webkit-scrollbar-corner{background:transparent}\
/* Between the desktop row and the phone grid there is a band where the header\
   has more controls than room. Two things go first, in this order, because\
   neither is load-bearing: the status line (the connection banner says the same\
   thing, louder) and the Cmd-K hint (the shortcut still works without a button\
   advertising it). Measured: without this the header overflows by ~29px at\
   760px wide, which is exactly where a small desktop window lands. */\
/* The title bar's controls read as one row: same height, same quiet border.\
   Before this the Cmd-K chip, the presence menu and the icon buttons were\
   three heights with three border treatments -- assorted widgets, not a\
   toolbar. */\
.rh-header .rh-kbd-jump,.rh-header .rh-presence,.rh-header .rh-leave{height:1.75rem;display:inline-flex;align-items:center;border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius);background-color:transparent;color:var(--rh-muted);font-size:var(--rh-font-sm);font-weight:500}\
.rh-header .rh-leave{padding:0 .6rem;color:var(--rh-error);border-color:color-mix(in srgb,var(--rh-error) 40%,transparent)}\
.rh-header .rh-leave:hover{color:var(--rh-error);border-color:color-mix(in srgb,var(--rh-error) 65%,transparent);background-color:color-mix(in srgb,var(--rh-error) 12%,transparent);box-shadow:none;transform:none}\
.rh-btn.danger{background:var(--rh-error);color:var(--rh-bg)}\
.rh-btn.danger:hover{background:color-mix(in srgb,var(--rh-error) 88%,var(--rh-text))}\
.rh-confirm-backdrop{position:fixed;inset:0;z-index:110;display:grid;place-items:center;padding:var(--rh-space-4);background:color-mix(in srgb,var(--rh-text) 30%,transparent);backdrop-filter:blur(6px);-webkit-backdrop-filter:blur(6px);animation:rh-fade .12s ease-out both}\
.rh-confirm{width:min(24rem,100%);padding:var(--rh-space-6);background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius-xl);box-shadow:var(--rh-shadow-3);animation:rh-pop-in .13s cubic-bezier(.2,.9,.3,1) both}\
.rh-confirm h2{margin:0 0 var(--rh-space-2);font-family:var(--rh-font-display);font-size:var(--rh-font-lg);font-weight:700;letter-spacing:-.015em;color:var(--rh-text)}\
.rh-confirm p{margin:0;color:var(--rh-muted);font-size:var(--rh-font-sm);line-height:1.5}\
.rh-confirm-actions{display:flex;justify-content:flex-end;gap:var(--rh-space-2);margin-top:var(--rh-space-5)}\
.rh-header .rh-presence{background-color:transparent}\
.rh-header .rh-kbd-jump:hover,.rh-header .rh-presence:hover{color:var(--rh-text);background-color:color-mix(in srgb,var(--rh-text) 6%,transparent);border-color:color-mix(in srgb,var(--rh-text) 22%,transparent)}\
@media (hover:none){.rh-line-cont .rh-line-time{opacity:1}}\
@media (max-width:860px){.rh-status,.rh-kbd-jump,.rh-header .rh-kbd-jump{display:none}}\
@media (max-width:720px){.rh-header{display:grid;grid-template-columns:auto minmax(0,1fr) auto;grid-template-areas:\"dot title presence\" \"live live live\" \"nav nav nav\";align-items:center;padding:var(--rh-space-2) var(--rh-space-3);min-height:2.9rem;gap:var(--rh-space-2)}.rh-header .rh-leave{display:none}.rh-rail{display:none}.rh-sidenav-slot{display:none}.rh-header .rh-title{grid-area:title;font-size:var(--rh-font-size);min-width:0;overflow:hidden}.rh-dot{grid-area:dot}.rh-presence-wrap{grid-area:presence;justify-self:end}.rh-presence{font-size:var(--rh-font-xs)}.rh-live-slot{grid-area:live;min-width:0;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}\
.rh-live-slot .rh-radio-now{display:block;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}.rh-nav{grid-area:nav;min-width:0;overflow-x:auto;padding-bottom:.15rem}.rh-tabbar{position:fixed;left:0;right:0;bottom:0;z-index:30;display:flex;padding:.3rem var(--rh-space-2) calc(.3rem + env(safe-area-inset-bottom));border-top:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);background:color-mix(in srgb,var(--rh-surface) 92%,transparent);backdrop-filter:saturate(1.4) blur(14px);-webkit-backdrop-filter:saturate(1.4) blur(14px);-webkit-user-select:none;user-select:none}.rh-tabbar.rh-hidden{display:none}.rh-tab{flex:1 1 0;min-width:0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:.15rem;min-height:2.9rem;padding:.25rem .1rem;border:0;background:transparent;color:var(--rh-muted);font-family:inherit;font-size:.7rem;font-weight:500;border-radius:var(--rh-radius);cursor:pointer}.rh-tab.active{color:var(--rh-brand)}.rh-tab-burrow.active{color:var(--rh-accent)}.rh-tab:active{background:color-mix(in srgb,var(--rh-text) 8%,transparent)}.rh-tab-icon{position:relative;display:grid;place-items:center;width:1.6rem;height:1.6rem}.rh-tab-icon svg{width:22px;height:22px}.rh-tab-tile{width:1.5rem;height:1.5rem;border-radius:.45rem;display:grid;place-items:center;font-weight:700;font-size:.8rem;background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-muted)}.rh-tab.active .rh-tab-tile{background:color-mix(in srgb,var(--rh-accent) 18%,transparent);color:var(--rh-accent)}.rh-tab-label{max-width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.rh-tab .rh-rail-badge{top:-.35rem;right:-.5rem;box-shadow:0 0 0 2px var(--rh-surface)}.rh-section-strip{display:flex;gap:.35rem;overflow-x:auto;scrollbar-width:none;padding:.45rem var(--rh-space-3);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);background:var(--rh-surface);-webkit-user-select:none;user-select:none}.rh-section-strip::-webkit-scrollbar{display:none}.rh-section-strip .rh-subnav-link{flex:none;flex-direction:row;align-items:center;gap:.35rem;padding:.3rem .75rem;min-height:2rem;border-radius:var(--rh-radius-full);font-size:var(--rh-font-sm);color:var(--rh-muted);background:color-mix(in srgb,var(--rh-text) 5%,transparent)}.rh-section-strip .rh-subnav-link[aria-current=page]{background:color-mix(in srgb,var(--rh-accent) 14%,transparent);color:var(--rh-accent)}.rh-section-strip .rh-subnav-icon{width:16px;height:16px}.rh-section-strip .rh-subnav-icon svg{width:16px;height:16px}.rh-section-strip .rh-pip{position:static;margin-left:.1rem}.rh-shell-main{padding-bottom:3.6rem}.rh-status,.rh-kbd-jump,.rh-spacer{display:none}.rh-header .rh-kbd-jump{display:none}.rh-conn{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap;border:0}.rh-toasts{top:auto;bottom:calc(3.9rem + env(safe-area-inset-bottom));left:var(--rh-space-3);right:var(--rh-space-3);width:auto}.rh-body{flex-direction:column}.rh-who,.rh-threads,.rh-members,.rh-files,.rh-stations{max-width:none;width:auto;border-right:0;border-left:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}.rh-who{max-height:35vh}.rh-chat{min-height:0}.rh-filetable-head,.rh-filetable .rh-file-link{grid-template-columns:minmax(0,1fr) 5rem}.rh-fcol-kind,.rh-fcol-who,.rh-fcol-when{display:none}.rh-scroll{padding:var(--rh-space-3)}.rh-present{order:-1;display:flex;align-items:center;gap:var(--rh-space-2);padding:.4rem var(--rh-space-3);max-height:none}.rh-present h2{margin:0;flex:none}.rh-present ul{flex-direction:row;flex:1;min-width:0;overflow-x:auto;gap:.4rem;padding-bottom:.15rem}.rh-present li{flex:none;white-space:nowrap}.rh-present:has(> ul:empty){display:none}.rh-server-row{flex-wrap:wrap;row-gap:.5rem}.rh-server-main{flex:1 1 0;min-width:60%}.rh-server-foot{flex-basis:100%}.rh-server-foot .rh-btn{width:100%;justify-content:center}.rh-server-listeners{white-space:normal}.rh-reader{min-height:14rem}}\
@keyframes rh-shimmer{0%{background-position:-180% 0}100%{background-position:180% 0}}\
.rh-skeleton{display:flex;flex-direction:column;gap:.55rem;padding:var(--rh-space-4)}\
.rh-skeleton-row{height:.85rem;border-radius:var(--rh-radius-full,999px);background:linear-gradient(90deg,color-mix(in srgb,var(--rh-text) 7%,transparent) 25%,color-mix(in srgb,var(--rh-text) 13%,transparent) 50%,color-mix(in srgb,var(--rh-text) 7%,transparent) 75%);background-size:180% 100%;animation:rh-shimmer 1.35s ease-in-out infinite}\
@keyframes rh-fade-up{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:none}}\
@keyframes rh-fade{from{opacity:0}to{opacity:1}}\
@keyframes rh-pop-in{from{opacity:0;transform:scale(.985) translateY(-5px)}to{opacity:1;transform:none}}\
@keyframes rh-slide-down{from{opacity:0;transform:translateY(-8px)}to{opacity:1;transform:none}}\
@keyframes rh-toast-in{from{opacity:0;transform:translateX(14px) scale(.98)}to{opacity:1;transform:none}}\
@keyframes rh-pop{from{opacity:0;transform:scale(.85)}to{opacity:1;transform:none}}\
@keyframes rh-pulse-ring{0%{box-shadow:0 0 0 0 color-mix(in srgb,#3fbf7f 55%,transparent)}70%{box-shadow:0 0 0 4px color-mix(in srgb,#3fbf7f 0%,transparent)}100%{box-shadow:0 0 0 0 color-mix(in srgb,#3fbf7f 0%,transparent)}}\
/* Panes swap instantly. The .19s fade+rise on every route change (and every\
   burrow-focus remount) was a web page-transition; Finder and Slack cut. */\
.rh-welcome{animation:rh-slide-down .24s cubic-bezier(.2,.8,.2,1) both}\
.rh-toast{animation:rh-toast-in .22s cubic-bezier(.2,.8,.2,1) both}\
.rh-pres.on{box-shadow:0 0 0 2px color-mix(in srgb,#3fbf7f 22%,transparent)}\
.rh-rail-tile{transition:background-color .15s ease,color .15s ease,transform .12s ease,box-shadow .15s ease}\
.rh-rail-tile:hover{transform:translateY(-1px);box-shadow:0 3px 8px color-mix(in srgb,var(--rh-text) 14%,transparent)}\
.rh-rail-tile:active{transform:translateY(0) scale(.95)}\
/* Live rows appear instantly: the who-list is keyed by (name, state), so\
   presence churn recreates rows -- re-fading them made live data read as a\
   page reload. */\
.rh-btn:active{transform:scale(.97)}\
.rh-rail-server.active{animation:rh-pop .22s cubic-bezier(.22,1,.36,1) both}\
@media (hover:none){.rh-glass-row{min-height:2.75rem}}\
@media (max-width:1000px){.rh-glass-cols,.rh-glass-row{grid-template-columns:1.6rem minmax(8rem,1.2fr) minmax(0,1.6fr) 4.25rem}.rh-glass-uptime,.rh-glass-cols .uptime{display:none}}\
/* A phone gets one scrolling page: a compact masthead and the form, then the\
   browser, its rows two lines tall so a name and its blurb both fit. */\
@media (max-width:720px){.rh-connect{display:block;flex:none}.rh-connect-side{overflow:visible;gap:var(--rh-space-4);padding:var(--rh-space-5) var(--rh-space-4);border-right:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}.rh-connect-brand{display:grid;grid-template-columns:auto minmax(0,1fr);column-gap:var(--rh-space-3);align-items:center;text-align:left}.rh-connect-logo{grid-row:1/3;width:3.5rem;height:3.5rem}.rh-connect-brand h1{margin:0;align-self:end;font-size:1.35rem}.rh-connect-tagline{align-self:start}.rh-connect-version{display:none}.rh-connect-foot{padding-top:0}.rh-connect-main{display:block}.rh-glass-head{padding:var(--rh-space-4) var(--rh-space-4) var(--rh-space-2)}.rh-glass-search{flex:1;width:auto;min-width:0}.rh-glass-cols{display:none}.rh-glass-scroll{overflow:visible;padding:0 var(--rh-space-2) var(--rh-space-3)}.rh-glass-row{grid-template-columns:1.6rem minmax(0,1fr) auto;grid-template-areas:\"mark name users\" \"mark desc desc\";row-gap:0;min-height:2.75rem;padding:.4rem .5rem}.rh-glass-mark{grid-area:mark}.rh-glass-name{grid-area:name}.rh-glass-desc{grid-area:desc}.rh-glass-users{grid-area:users}.rh-glass-detail{padding-left:calc(2.1rem + var(--rh-space-3))}.rh-glass-add{grid-template-columns:1fr 1fr}.rh-glass-add .rh-input{grid-column:1/-1}.rh-glass-page{display:block;overflow-y:auto}.rh-player-now{flex-direction:column;align-items:flex-start;gap:var(--rh-space-3)}.rh-player-cover{width:9rem;height:9rem}.rh-player-recent li{grid-template-columns:1.4rem minmax(0,1fr) auto}.rh-player-recent-artist{grid-column:2;grid-row:2}.rh-glass-status{padding:.5rem var(--rh-space-4) calc(.5rem + env(safe-area-inset-bottom))}}\
@media (max-width:960px){.rh-adm{grid-template-columns:minmax(0,1fr);grid-template-rows:auto minmax(0,1fr)}.rh-adm-nav{display:none}.rh-adm-jump{display:block;padding:var(--rh-space-3) var(--rh-space-5) 0}.rh-adm-jump .rh-select{width:100%;max-width:46rem}}\
@media (max-width:720px){.rh-adm-jump{padding:var(--rh-space-3) var(--rh-space-4) 0}.rh-adm-pane{padding:var(--rh-space-4) var(--rh-space-4) 0}.rh-adm-row:not(.inline){grid-template-columns:minmax(0,1fr)}.rh-adm-row:not(.inline) .rh-adm-control{justify-content:flex-start}.rh-adm-number{align-items:flex-start}.rh-adm-num{text-align:left}.rh-adm-text,.rh-adm-choice{width:100%;max-width:none}.rh-adm-fixed{max-width:none;text-align:left}.rh-adm-savebar{flex-wrap:wrap}.rh-adm-savebar-text{flex-basis:100%}}\
@media (max-width:720px){.rh-adm-acct-line{grid-template-columns:minmax(0,1fr) auto 1rem;row-gap:.1rem}.rh-adm-acct-line .rh-adm-acct-class{display:none}.rh-adm-acct-line .rh-adm-acct-state{grid-column:1;grid-row:2;font-size:var(--rh-font-xs)}.rh-adm-class-line .rh-adm-acct-role{display:none}.rh-adm-invite{grid-template-columns:minmax(0,1fr) auto}.rh-adm-invite-by{display:none}.rh-adm-invite-state{grid-column:1;font-size:var(--rh-font-xs)}.rh-adm-invite-actions{grid-column:2;grid-row:1/3}.rh-adm-add{flex-wrap:wrap}.rh-adm-add .rh-input{width:100%;flex-basis:100%}.rh-adm-peer{grid-template-columns:minmax(0,1fr) auto}.rh-adm-peer-state{grid-column:1;font-size:var(--rh-font-xs)}.rh-adm-peer-actions{grid-column:2;grid-row:1/3}.rh-adm-station-head{grid-template-columns:minmax(0,1fr)}.rh-adm-station-left li{flex-direction:column;gap:0}.rh-adm-station-track{max-width:100%}.rh-adm-station-why{white-space:normal}.rh-adm-origin{grid-template-columns:minmax(0,1fr) auto}.rh-adm-origin-trust{grid-column:1;text-align:left;font-size:var(--rh-font-xs)}.rh-adm-filter{width:100%}.rh-adm-group-tools{width:100%}.rh-adm-inline{flex-wrap:wrap}}\
@media (prefers-reduced-motion:reduce){*,*::before,*::after{transition-duration:.01ms!important;transition-delay:0s!important;animation-duration:.01ms!important;animation-delay:0s!important;animation-iteration-count:1!important;scroll-behavior:auto!important}}\
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
        // No override of either kind: the built-in pack renders.
        assert_eq!(
            resolve_root_style(None, None, ThemePack::Retro, Mode::Dark),
            root_style(ThemePack::Retro, Mode::Dark)
        );
        // An applied custom pack overrides wholesale, per mode.
        let mut custom = PackTokens::builtin(ThemePack::Clean);
        custom.dark.insert("--rh-accent".into(), "#ff00ff".into());
        for mode in MODES {
            let style = resolve_root_style(Some(&custom), None, ThemePack::Retro, mode);
            assert_eq!(style, custom.style_for(mode), "{mode:?}");
        }
        assert!(
            resolve_root_style(Some(&custom), None, ThemePack::Retro, Mode::Dark)
                .contains("--rh-accent:#ff00ff;")
        );
        // Light mode is untouched by the dark-only edit.
        assert_eq!(
            resolve_root_style(Some(&custom), None, ThemePack::Retro, Mode::Light),
            root_style(ThemePack::Clean, Mode::Light)
        );
        // The editor's custom slot also wins over a server overlay (a live
        // edit preview is shown unlayered).
        let mut server = ServerOverlay::default();
        server.dark.insert("--rh-accent".into(), "#00ff00".into());
        assert_eq!(
            resolve_root_style(Some(&custom), Some(&server), ThemePack::Retro, Mode::Dark),
            custom.style_for(Mode::Dark),
            "custom preview beats the server overlay"
        );
    }

    #[test]
    fn server_overlay_layers_on_the_pack_when_no_custom_preview() {
        // A server overlay nudges the chosen pack: the accent changes, the
        // rest of the pack (its type/elevation extras) stays put.
        let mut server = ServerOverlay::default();
        server.dark.insert("--rh-accent".into(), "#00c2ff".into());
        let style = resolve_root_style(None, Some(&server), ThemePack::Clean, Mode::Dark);
        assert!(
            style.contains("--rh-accent:#00c2ff;"),
            "server accent applied"
        );
        // A pack token the overlay didn't name still comes from Clean.
        let base = root_style(ThemePack::Clean, Mode::Dark);
        let shadow = base
            .split(';')
            .find(|d| d.starts_with("--rh-shadow-2:"))
            .unwrap();
        assert!(style.contains(shadow), "unnamed tokens keep the pack value");
    }

    #[test]
    fn packs_render_distinct_styles() {
        for mode in MODES {
            let styles: BTreeSet<String> = PACKS.iter().map(|&p| root_style(p, mode)).collect();
            assert_eq!(styles.len(), PACKS.len(), "{mode:?}");
        }
    }

    // ---- a11y shape tests -------------------------------------------------
    //
    // The crate has no DOM-rendering path on the host (CSR-only Leptos), so
    // the stylesheet's accessibility contract is asserted textually — the
    // same style as the PWA shell-asset tests in `crate::pwa`.

    #[test]
    fn stylesheet_has_a_visible_focus_indicator_on_the_focus_token() {
        // A global :focus-visible outline, driven by the theme token so it
        // re-colours with every pack/mode (contrast asserted in
        // `crate::packs`), offset so it reads against the control's fill.
        assert!(STYLESHEET.contains(":focus-visible{outline:2px solid var(--rh-focus)"));
        assert!(STYLESHEET.contains("outline-offset:2px"));
        // Nothing suppresses outlines wholesale.
        assert!(
            !STYLESHEET.contains("outline:none") && !STYLESHEET.contains("outline:0"),
            "no rule may blanket-remove focus outlines"
        );
    }

    #[test]
    fn stylesheet_ships_skip_link_and_screen_reader_only_helper() {
        // The skip link parks off-screen and snaps into view on focus.
        assert!(STYLESHEET.contains(".rh-skip{position:fixed;left:-999rem"));
        assert!(STYLESHEET.contains(".rh-skip:focus{left:var(--rh-space-2)}"));
        // The sr-only helper uses the standard clip/clip-path recipe.
        assert!(STYLESHEET.contains(".rh-visually-hidden{position:absolute;width:1px;height:1px"));
        assert!(STYLESHEET.contains("clip-path:inset(50%)"));
    }

    #[test]
    fn stylesheet_styles_router_aria_current_nav_state() {
        // leptos_router's <A> stamps aria-current="page" on the active link;
        // the stylesheet must key the active style off that attribute (not
        // only off a class the router never sets).
        assert!(STYLESHEET.contains(".rh-nav a[aria-current=page]"));
    }

    #[test]
    fn stylesheet_neutralises_motion_under_reduced_motion() {
        let block = STYLESHEET
            .split("@media (prefers-reduced-motion:reduce){")
            .nth(1)
            .expect("reduced-motion media block present");
        for marker in [
            "transition-duration:.01ms!important",
            "animation-duration:.01ms!important",
            "animation-iteration-count:1!important",
            "scroll-behavior:auto!important",
        ] {
            assert!(block.contains(marker), "reduced-motion block: {marker}");
        }
        // The block sits at the end of the sheet so it wins the cascade over
        // every transition declared above it (the transfer bar today).
        let media_at = STYLESHEET.find("@media (prefers-reduced-motion").unwrap();
        let last_transition = STYLESHEET.rfind("transition:transform .3s ease").unwrap();
        assert!(
            media_at > last_transition,
            "reduced-motion block must follow the motion it neutralises"
        );
    }

    #[test]
    fn stylesheet_carries_a11y_layout_helpers() {
        // Chat/DM scrollback list reset (real <ul> message lists).
        assert!(STYLESHEET.contains(".rh-lines{list-style:none"));
        // Admin matrices are real tables.
        assert!(STYLESHEET.contains(".rh-table{width:100%;border-collapse:collapse"));
        assert!(STYLESHEET.contains(".rh-table th{text-align:left"));
        // Grouped controls keep their toolbar layout inside real fieldsets.
        assert!(STYLESHEET.contains(".rh-fieldset{border:0"));
        assert!(STYLESHEET.contains(".rh-fieldset legend{float:left"));
        // The header's live now-playing slot collapses when empty, so the
        // always-present role=status wrapper never leaves a phantom flex gap.
        assert!(STYLESHEET.contains(".rh-live-slot:empty{display:none}"));
    }

    #[test]
    fn stylesheet_keeps_mobile_chat_usable() {
        let block = STYLESHEET
            .split("@media (max-width:720px){")
            .nth(1)
            .expect("narrow-screen media block present");
        // The chat column may shrink below its content, so the log scrolls
        // internally and the compose box stays pinned on screen — without
        // this the whole pane scrolls and compose sits below the fold.
        assert!(block.contains(".rh-chat{min-height:0}"));
        // The lobby roster flips to a horizontal presence strip above the
        // chat instead of a full column that buries the conversation.
        assert!(block.contains(".rh-present{order:-1"));
        assert!(block.contains(".rh-present ul{flex-direction:row"));
        // Keyboard-only affordances leave the touch layout…
        assert!(block.contains(".rh-kbd-jump,.rh-spacer{display:none}"));
        // …but the connection state stays in the accessibility tree (the
        // sr-only recipe, not display:none — it is a role=status region).
        assert!(block.contains(".rh-conn{position:absolute;width:1px;height:1px"));
    }

    #[test]
    fn stylesheet_anchors_the_new_messages_jump_pill() {
        // The pill positions against the chat pane, so the pane must be a
        // containing block — lose `position:relative` and the pill would
        // anchor to the viewport instead.
        assert!(STYLESHEET.contains(
            ".rh-chat{flex:1;display:flex;flex-direction:column;min-width:0;position:relative}"
        ));
        assert!(STYLESHEET.contains(".rh-jump-new{position:absolute"));
    }

    #[test]
    fn stylesheet_never_sets_a_positive_tabindex_or_hides_focus() {
        // Belt-and-braces textual checks mirroring the markup rules: CSS
        // cannot set tabindex, but it can break keyboard UX with these.
        assert!(!STYLESHEET.contains("pointer-events:none"));
    }

    #[test]
    fn every_resolved_style_yields_a_background_for_the_page_itself() {
        // Whatever the pack/mode/overlay, the html/body repaint must find a
        // real colour — a miss leaves the dark pre-boot backdrop in place and
        // any viewport gap renders as a black frame around the app.
        for pack in [ThemePack::Clean, ThemePack::Retro, ThemePack::HighContrast] {
            for mode in [Mode::Light, Mode::Dark] {
                let style = resolve_root_style(None, None, pack, mode);
                let bg = background_of(&style);
                assert!(
                    bg.starts_with('#') || bg.starts_with("rgb") || bg.starts_with("color"),
                    "{pack:?}/{mode:?} background looks wrong: {bg:?}"
                );
                assert!(
                    !bg.contains("--"),
                    "{pack:?}/{mode:?} grabbed a var, not a value"
                );
            }
        }
        // Absent or malformed: fall back to the pre-boot backdrop, never panic.
        assert_eq!(background_of(""), "#14161b");
        assert_eq!(background_of("--rh-text:#fff;"), "#14161b");
        assert_eq!(background_of("--rh-bg:;"), "#14161b");
    }

    #[test]
    fn the_page_itself_never_scrolls() {
        // The app is exactly one viewport tall, so anything that adds height
        // around it makes the whole window scroll a few pixels and reveals the
        // pre-boot backdrop as a dark border. The browser's default body margin
        // did exactly that until it was reset.
        assert!(STYLESHEET.contains("html,body{margin:0;padding:0;height:100%}"));
        assert!(STYLESHEET.contains("body{overflow:hidden}"));
        assert!(STYLESHEET.contains(".rh-app{"));
        // The app still owns the viewport height it assumes.
        assert!(STYLESHEET.contains("height:100vh;height:100dvh"));
    }

    #[test]
    fn only_chrome_is_unselectable_never_content() {
        // Native apps don't let you drag-select the sidebar, and a web app that
        // does feels like a web page. But `user-select:none` on anything a user
        // might want to *copy* — a message, a filename, a fingerprint — is a
        // real harm, so it's allowed only on navigation furniture.
        const CHROME: [&str; 10] = [
            ".rh-glass-head",
            ".rh-glass-cols",
            ".rh-glass-status",
            ".rh-subnav",
            ".rh-rail",
            ".rh-header",
            ".rh-format-bar",
            ".rh-tabs",
            ".rh-tabbar",
            ".rh-section-strip",
        ];
        for rule in STYLESHEET.split('}') {
            let Some((head, decls)) = rule.rsplit_once('{') else {
                continue;
            };
            if !decls.contains("user-select:none") {
                continue;
            }
            // `head` may still carry an enclosing `@media (...){`.
            let selector = head.rsplit('{').next().unwrap_or_default().trim();
            // Check *every* selector in the list, not the string as a whole:
            // otherwise `.rh-rail,.rh-scroll{user-select:none}` passes on the
            // strength of its first name and takes the scrollback with it.
            for one in selector.split(',') {
                let one = one.trim();
                assert!(
                    CHROME.iter().any(|c| one.starts_with(c)),
                    "`user-select:none` on `{one}` — that's content, not chrome"
                );
            }
        }
    }
}
