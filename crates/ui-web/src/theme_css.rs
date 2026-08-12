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
.rh-rail{flex:none;width:3.4rem;display:flex;flex-direction:column;align-items:center;gap:var(--rh-space-2);padding:var(--rh-space-3) 0;background:color-mix(in srgb,var(--rh-accent) 6%,var(--rh-surface));border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-rail-hidden{display:none}\
.rh-rail-dot{position:absolute;right:1px;bottom:1px;width:9px;height:9px;border-radius:50%;box-shadow:0 0 0 2px color-mix(in srgb,var(--rh-accent) 6%,var(--rh-surface))}\
.rh-rail-dot.on{background:#3fbf7f}\
.rh-rail-badge{position:absolute;top:-5px;right:-5px;min-width:17px;height:17px;padding:0 4px;border-radius:var(--rh-radius-full);background:var(--rh-error);color:#fff;font-size:.62rem;font-weight:800;line-height:17px;text-align:center;box-shadow:0 0 0 2px color-mix(in srgb,var(--rh-accent) 6%,var(--rh-surface));animation:rh-pop .18s cubic-bezier(.2,.9,.3,1.2) both}\
.rh-rail-dot.pending{background:var(--rh-accent)}\
.rh-rail-dot.off{background:var(--rh-muted)}\
.rh-presence{font:inherit;font-size:var(--rh-font-sm);color:var(--rh-text);background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius);padding:.3rem .5rem;cursor:pointer}\
.rh-presence:hover{border-color:color-mix(in srgb,var(--rh-accent) 45%,transparent)}\
.rh-who-row{display:flex;align-items:center;gap:.45rem}\
.rh-pres{width:.5rem;height:.5rem;border-radius:50%;flex:none}\
.rh-pres.on{background:#3fbf7f}\
.rh-pres.away{background:#e8b84b}\
.rh-pres.idle{background:var(--rh-muted)}\
.rh-pres.off{background:var(--rh-muted);opacity:.5}\
.rh-rail-unified{border-radius:var(--rh-radius-full);color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent);font-size:1.05rem}\
.rh-people{list-style:none;margin:0;padding:0;display:flex;flex-direction:column}\
.rh-person{display:flex;align-items:center;gap:.6rem;padding:.5rem .3rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-person-name{font-weight:600}\
.rh-recent{display:flex;flex-wrap:wrap;align-items:center;gap:.4rem;margin:.2rem 0 .4rem}\
.rh-recent-label{font-size:var(--rh-font-xs,.72rem);text-transform:uppercase;letter-spacing:.04em;color:var(--rh-muted)}\
.rh-recent-chip{border:1px solid color-mix(in srgb,var(--rh-accent) 30%,transparent);background:color-mix(in srgb,var(--rh-accent) 8%,transparent);color:var(--rh-text);border-radius:999px;padding:.15rem .6rem;font-size:var(--rh-font-sm);cursor:pointer;font-family:inherit}\
.rh-recent-chip:hover{background:color-mix(in srgb,var(--rh-accent) 16%,transparent)}\
.rh-welcome{margin:.75rem;padding:.85rem 1rem;border:1px solid color-mix(in srgb,var(--rh-accent) 35%,transparent);border-left:3px solid var(--rh-accent);border-radius:var(--rh-radius,8px);background:color-mix(in srgb,var(--rh-accent) 7%,var(--rh-bg));box-shadow:0 1px 3px rgba(0,0,0,.06)}\
.rh-welcome-head{display:flex;align-items:center;gap:.5rem;margin-bottom:.4rem}\
.rh-welcome-title{font-weight:700;letter-spacing:.01em}\
.rh-welcome-x{margin-left:auto;border:0;background:transparent;color:var(--rh-muted);font-size:1.2rem;line-height:1;cursor:pointer;padding:.1rem .3rem;border-radius:4px}\
.rh-welcome-x:hover{background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-text)}\
.rh-welcome-motd{color:var(--rh-muted);margin:0 0 .5rem;font-style:italic}\
.rh-welcome-body{margin:0 0 .7rem;white-space:pre-wrap;max-width:70ch;line-height:1.5}\
.rh-front{display:flex;flex-direction:column;gap:.4rem;margin:0 0 .6rem;padding:.6rem .75rem .7rem;border:1px solid color-mix(in srgb,var(--rh-accent) 20%,transparent);border-radius:var(--rh-radius);background:color-mix(in srgb,var(--rh-accent) 5%,var(--rh-surface))}\
.rh-front-head{display:flex;align-items:baseline;gap:.5rem;margin-bottom:.15rem}\
.rh-front-eyebrow{font-size:var(--rh-font-xs,.72rem);text-transform:uppercase;letter-spacing:.08em;font-weight:700;color:var(--rh-accent)}\
.rh-front-where{font-size:var(--rh-font-xs,.72rem);color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-front-motd{margin:0;line-height:1.5;max-width:70ch;white-space:pre-wrap}\
.rh-front-label{font-size:var(--rh-font-xs,.72rem);text-transform:uppercase;letter-spacing:.05em;color:var(--rh-accent);font-weight:700}\
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
.rh-filetable-head{padding:.3rem .6rem;font-size:var(--rh-font-xs,.72rem);text-transform:uppercase;letter-spacing:.04em;color:var(--rh-muted);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent)}\
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
.rh-you-mark{flex:none;border-radius:10px;overflow:hidden;box-shadow:0 1px 3px color-mix(in srgb,var(--rh-text) 18%,transparent)}\
.rh-chat-empty{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:.35rem;min-height:60%;padding:var(--rh-space-6);text-align:center;animation:rh-fade-up .3s ease both}\
.rh-chat-empty-mark{font-size:2rem;color:var(--rh-accent);opacity:.7;line-height:1}\
.rh-chat-empty-title{font-weight:700;margin:.3rem 0 0}\
.rh-chat-empty-sub{color:var(--rh-muted);margin:0;max-width:32ch}\
.rh-person,.rh-xfer-item,.rh-who-row,.rh-tree-item{border-radius:8px;transition:background-color .13s ease}\
.rh-person:hover,.rh-xfer-item:hover,.rh-who-row:hover,.rh-tree-item:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-empty{color:var(--rh-muted);font-style:italic;padding:var(--rh-space-4);text-align:center;animation:rh-fade-up .25s ease both}\
.rh-person-servers{margin-left:auto;font-size:var(--rh-font-sm);color:var(--rh-muted)}\
.rh-xfers{list-style:none;margin:0;padding:0;display:flex;flex-direction:column}\
.rh-xfer-row{display:flex;align-items:center;gap:.7rem;padding:.5rem .3rem;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-xfer-dir{color:var(--rh-accent);font-family:var(--rh-font-mono,monospace)}\
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
.rh-swarmpill{font-family:var(--rh-font-mono,ui-monospace,monospace);font-size:var(--rh-font-xs,.72rem);color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent);border-radius:999px;padding:.05rem .5rem}\
.rh-rail-tile{width:40px;height:40px;display:grid;place-items:center;border:0;padding:0;cursor:pointer;border-radius:12px;background:color-mix(in srgb,var(--rh-text) 5%,transparent);color:var(--rh-muted);font-family:var(--rh-font-sans);font-weight:700;font-size:.95rem;position:relative}\
.rh-rail-tile:hover{color:var(--rh-text)}\
.rh-rail-home,.rh-rail-add{border-radius:var(--rh-radius-full)}\
.rh-rail-server{color:var(--rh-text);background:color-mix(in srgb,var(--rh-accent) 16%,var(--rh-surface));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--rh-accent) 30%,transparent)}\
.rh-rail-server.active::before{content:\"\";position:absolute;left:-9px;top:8px;bottom:8px;width:3px;border-radius:3px;background:var(--rh-accent)}\
.rh-rail-add{color:var(--rh-muted);background:transparent;box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--rh-text) 12%,transparent);font-size:1.15rem}\
.rh-rail-sep{width:22px;height:1px;background:color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-rail-hole{width:22px;height:22px;border-radius:50%;background:radial-gradient(circle at 50% 52%,var(--rh-surface) 0 16%,var(--rh-accent) 16% 30%,var(--rh-surface) 30% 46%,color-mix(in srgb,var(--rh-accent) 55%,var(--rh-surface)) 46% 64%,var(--rh-surface) 64% 100%);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 35%,transparent)}\
:focus-visible{outline:2px solid var(--rh-focus);outline-offset:2px}\
.rh-visually-hidden{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap;border:0}\
.rh-skip{position:fixed;left:-999rem;top:var(--rh-space-2);z-index:99;background:var(--rh-accent);color:var(--rh-bg);padding:var(--rh-space-2) var(--rh-space-3);border-radius:var(--rh-radius);text-decoration:none;font-weight:600;box-shadow:var(--rh-shadow-2)}\
.rh-skip:focus{left:var(--rh-space-2)}\
.rh-header{display:flex;align-items:center;gap:var(--rh-space-3);padding:0 var(--rh-space-5);min-height:3.5rem;position:sticky;top:0;z-index:20;background:color-mix(in srgb,var(--rh-surface) 82%,transparent);backdrop-filter:saturate(1.4) blur(14px);-webkit-backdrop-filter:saturate(1.4) blur(14px);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-header .rh-title{order:1;display:inline-flex;align-items:center;gap:.55rem;white-space:nowrap;font-weight:700;font-size:var(--rh-font-lg);letter-spacing:-.01em;color:var(--rh-text)}\
.rh-header .rh-title::before{content:'';flex:none;width:1.4rem;height:1.4rem;border-radius:var(--rh-radius-full);background:radial-gradient(circle at 50% 52%,var(--rh-surface) 0 15%,var(--rh-accent) 15% 27%,var(--rh-surface) 27% 41%,color-mix(in srgb,var(--rh-accent) 62%,var(--rh-surface)) 41% 58%,var(--rh-surface) 58% 73%,color-mix(in srgb,var(--rh-accent) 34%,var(--rh-surface)) 73% 100%);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 35%,transparent),0 2px 8px -2px color-mix(in srgb,var(--rh-accent) 60%,transparent)}\
.rh-title-text{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-dot{order:2;width:.5rem;height:.5rem;border-radius:50%;display:inline-block;flex:none;margin-left:.2rem}\
.rh-dot.on{background:#3fbf7f;box-shadow:0 0 0 3px color-mix(in srgb,#3fbf7f 22%,transparent)}\
.rh-dot.off{background:var(--rh-muted)}\
.rh-dot.pending{background:var(--rh-accent);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-accent) 22%,transparent)}\
.rh-conn{order:3;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);text-transform:uppercase;letter-spacing:.06em;white-space:nowrap}\
.rh-status{order:4;color:var(--rh-muted);font-size:var(--rh-font-sm);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:14rem}\
.rh-spacer{order:5;flex:1}\
.rh-live-slot{order:6;min-width:0;flex:0 1 auto;overflow:hidden}\
/* The header's flexible parts must actually be able to shrink. Everything in\
   it was `white-space:nowrap` with no `min-width:0`, so a long now-playing\
   line -- a radio track title -- pushed the trailing controls off\
   the right edge of the window -- the same overflow that moved the section nav\
   out of here, reappearing with one long string. Title, status and now-playing\
   give way; the controls never do. */\
.rh-header{overflow:hidden}\
.rh-header .rh-title{min-width:0;flex:0 1 auto;overflow:hidden;text-overflow:ellipsis}\
.rh-header .rh-title-text{min-width:0;overflow:hidden;text-overflow:ellipsis}\
.rh-status{flex:0 1 auto}\
.rh-theme-menu,.rh-presence,.rh-kbd-jump,.rh-dot,.rh-conn{flex:none}\
.rh-theme-menu{order:8;display:inline-flex;gap:.35rem;align-items:center}\
.rh-nav{order:7;display:flex;gap:.15rem;align-items:center}\
.rh-nav a,.rh-nav .rh-nav-item{color:var(--rh-muted);display:inline-flex;align-items:center;gap:.3rem;white-space:nowrap;text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;padding:.35rem .7rem;border-radius:var(--rh-radius-full);transition:background-color .15s ease,color .15s ease;border-bottom:0}\
.rh-nav a:hover,.rh-nav .rh-nav-item:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 7%,transparent)}\
.rh-nav a.active,.rh-nav a[aria-current=page],.rh-nav .rh-nav-item.active{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 14%,transparent)}\
.rh-subnav{flex:none;width:11.5rem;display:flex;flex-direction:column;gap:1px;padding:var(--rh-space-3) var(--rh-space-2);overflow-y:auto;background:color-mix(in srgb,var(--rh-accent) 3%,var(--rh-surface));border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);-webkit-user-select:none;user-select:none}\
.rh-subnav-link{display:flex;align-items:center;gap:var(--rh-space-2);padding:.4rem .55rem;border-radius:var(--rh-radius);color:var(--rh-muted);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;white-space:nowrap;border-bottom:0;transition:background-color .13s ease,color .13s ease}\
.rh-subnav-link:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-subnav-link[aria-current=page]{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 13%,transparent);font-weight:650}\
.rh-subnav-icon{flex:none;display:grid;place-items:center;width:18px;height:18px;opacity:.85}\
.rh-subnav-link[aria-current=page] .rh-subnav-icon{opacity:1}\
.rh-subnav-label{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis}\
.rh-subnav .rh-pip{flex:none}\
.rh-subnav-rule{height:1px;margin:var(--rh-space-2) .55rem;background:color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-icon-btn{display:inline-flex;align-items:center;justify-content:center;padding:.3rem;min-width:2rem;line-height:0}\
.rh-icon-btn span{display:grid;place-items:center}\
.rh-subnav-scope{display:block;padding:.1rem .55rem .35rem;font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.07em;font-weight:700;color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
/* The rail's warren destinations get the same lit treatment as a focused\
   burrow tile, so the rail always shows where you are -- not only which burrow\
   is selected. */\
.rh-rail-tile.active{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 16%,transparent)}\
.rh-rail-home.active,.rh-rail-unified.active{box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 45%,transparent)}\
.rh-rail.warren .rh-rail-server.active{background:color-mix(in srgb,var(--rh-text) 5%,transparent);color:var(--rh-muted);box-shadow:none}\
/* --- Native feel. What separates an app from a web page is mostly the small
   things a browser does by default and a native app never does: rubber-band
   scrolling past the end, a grey flash when you tap, a text cursor over
   furniture, drag-selecting the sidebar. --- */\
.rh-app{overscroll-behavior:none;-webkit-tap-highlight-color:transparent;cursor:default}\
.rh-rail,.rh-header,.rh-format-bar{-webkit-user-select:none;user-select:none}\
/* …but never at the cost of copying content: messages, posts, filenames and\
   fingerprints stay selectable, and the stylesheet's own test enforces that\
   `user-select:none` appears only on the chrome selectors above. */\
.rh-rich,.rh-line,.rh-post,.rh-scroll,.rh-filetable,.rh-fingerprint{-webkit-user-select:text;user-select:text}\
.rh-scroll,.rh-panel,.rh-who,.rh-subnav{overscroll-behavior:contain;scrollbar-width:thin}\
::selection{background:color-mix(in srgb,var(--rh-accent) 30%,transparent)}\
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
.rh-app.native::before{content:'';position:fixed;top:0;left:0;right:0;height:1.75rem;-webkit-app-region:drag;z-index:100}\
.rh-app.native .rh-header{-webkit-app-region:drag}\
.rh-app.native .rh-header button,.rh-app.native .rh-header a,.rh-app.native .rh-header select,.rh-app.native .rh-header input{-webkit-app-region:no-drag}\
.rh-composer{display:flex;flex-direction:column;gap:var(--rh-space-2);padding:var(--rh-space-3) var(--rh-space-5);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-format-bar{display:flex;align-items:center;gap:.15rem;flex-wrap:wrap}\
.rh-format-btn{display:inline-flex;align-items:center;justify-content:center;min-width:1.9rem;height:1.9rem;padding:0 .4rem;border:1px solid transparent;border-radius:var(--rh-radius-sm);background:transparent;color:var(--rh-muted);font-family:var(--rh-font-sans);font-size:var(--rh-font-sm);font-weight:700;cursor:pointer;transition:background-color .12s ease,color .12s ease}\
.rh-format-btn:hover{background:color-mix(in srgb,var(--rh-text) 8%,transparent);color:var(--rh-text)}\
.rh-format-btn.on{background:color-mix(in srgb,var(--rh-accent) 15%,transparent);color:var(--rh-accent)}\
.rh-format-mode{min-width:auto;font-size:var(--rh-font-xs);font-weight:600;letter-spacing:.02em}\
.rh-format-spacer{flex:1}\
.rh-compose-area{min-height:2.4rem;max-height:40vh;resize:vertical;font-family:var(--rh-font-sans);line-height:1.45}\
.rh-compose-area.tall{min-height:7rem}\
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
.rh-rich blockquote{margin:.4rem 0;padding:.1rem 0 .1rem var(--rh-space-3);border-left:3px solid color-mix(in srgb,var(--rh-accent) 45%,transparent);color:var(--rh-muted)}\
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
.rh-input:focus{border-color:var(--rh-accent);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-accent) 24%,transparent)}\
.rh-kbd-jump{font:inherit;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);background:color-mix(in srgb,var(--rh-text) 6%,transparent);border:1px solid color-mix(in srgb,var(--rh-text) 14%,transparent);border-radius:var(--rh-radius);padding:.22rem .5rem;cursor:pointer;line-height:1.4;letter-spacing:.03em;white-space:nowrap;transition:background-color .15s ease,color .15s ease,border-color .15s ease}\
.rh-kbd-jump:hover{color:var(--rh-accent);border-color:color-mix(in srgb,var(--rh-accent) 40%,transparent);background:color-mix(in srgb,var(--rh-accent) 10%,transparent)}\
.rh-palette-backdrop{position:fixed;inset:0;z-index:100;display:flex;align-items:flex-start;justify-content:center;padding:14vh var(--rh-space-4) var(--rh-space-4);background:color-mix(in srgb,var(--rh-text) 30%,transparent);backdrop-filter:blur(6px);-webkit-backdrop-filter:blur(6px)}\
.rh-palette{width:min(34rem,94vw);max-height:72vh;display:flex;flex-direction:column;background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-radius:var(--rh-radius-xl);box-shadow:var(--rh-shadow-3);overflow:hidden}\
.rh-palette-input{margin:var(--rh-space-3);font-size:var(--rh-font-lg)}\
.rh-palette-list{list-style:none;margin:0;padding:0 var(--rh-space-2) var(--rh-space-2);overflow-y:auto}\
.rh-palette-item{display:flex;align-items:center;justify-content:space-between;gap:var(--rh-space-3);padding:.55rem .7rem;border-radius:var(--rh-radius);cursor:pointer;transition:background-color .12s ease}\
.rh-palette-item.selected{background:color-mix(in srgb,var(--rh-accent) 16%,transparent)}\
.rh-palette-label{font-weight:600;color:var(--rh-text)}\
.rh-palette-item.selected .rh-palette-label{color:var(--rh-accent)}\
.rh-palette-hint{font-size:var(--rh-font-xs);color:var(--rh-muted);text-transform:uppercase;letter-spacing:.05em}\
.rh-servers{flex:1;padding:var(--rh-space-5);overflow-y:auto}\
.rh-server-list{list-style:none;margin:var(--rh-space-4) 0 0;padding:0;display:grid;gap:var(--rh-space-4);grid-template-columns:repeat(auto-fill,minmax(20rem,1fr))}\
.rh-server-card{display:flex;flex-direction:column;gap:var(--rh-space-2);background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent);border-radius:var(--rh-radius-xl);padding:var(--rh-space-4);box-shadow:var(--rh-shadow-1);transition:box-shadow .15s ease,transform .12s ease}\
.rh-server-card:hover{box-shadow:var(--rh-shadow-2);transform:translateY(-2px)}\
.rh-server-head{display:flex;align-items:center;gap:var(--rh-space-2)}\
.rh-server-name{font-weight:700;font-size:var(--rh-font-lg);color:var(--rh-text)}\
.rh-server-users{margin-left:auto;font-size:var(--rh-font-xs);color:var(--rh-muted);text-transform:uppercase;letter-spacing:.05em;white-space:nowrap}\
.rh-server-desc{margin:0;color:var(--rh-muted);font-size:var(--rh-font-sm);line-height:1.45}\
.rh-server-foot{display:flex;align-items:center;gap:var(--rh-space-2);margin-top:auto;flex-wrap:wrap}\
.rh-server-uptime{font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 12%,transparent);padding:.15rem .5rem;border-radius:var(--rh-radius-full);white-space:nowrap}\
.rh-server-endpoint{flex:1;min-width:6rem;font-size:var(--rh-font-xs);color:var(--rh-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.rh-server-foot .rh-btn{margin-left:auto;padding:.35rem .8rem;font-size:var(--rh-font-sm)}\
.rh-toasts{position:fixed;top:4.2rem;right:var(--rh-space-4);z-index:90;display:flex;flex-direction:column;gap:var(--rh-space-2);width:min(22rem,90vw)}\
.rh-toast{display:flex;align-items:center;gap:var(--rh-space-2);background:var(--rh-surface);border:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);border-left:3px solid var(--rh-accent);border-radius:var(--rh-radius);box-shadow:var(--rh-shadow-2);padding:.6rem .7rem;font-size:var(--rh-font-sm)}\
.rh-toast-glyph{flex:none;font-size:var(--rh-font-lg);line-height:1;color:var(--rh-accent)}\
.rh-toast-text{flex:1;color:var(--rh-text);min-width:0}\
.rh-toast-close{flex:none;background:transparent;border:0;color:var(--rh-muted);cursor:pointer;font-size:var(--rh-font-lg);line-height:1;padding:0 .2rem;border-radius:var(--rh-radius-sm)}\
.rh-toast-close:hover{color:var(--rh-text)}\
.rh-toast.success{border-left-color:#2f9e44}.rh-toast.success .rh-toast-glyph{color:#2f9e44}\
.rh-toast.warn{border-left-color:#e8890c}.rh-toast.warn .rh-toast-glyph{color:#e8890c}\
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
.rh-login{position:relative;max-width:23rem;margin:5rem auto;display:flex;flex-direction:column;gap:var(--rh-space-3);background:var(--rh-surface);padding:var(--rh-space-8);border-radius:var(--rh-radius-xl);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);box-shadow:var(--rh-shadow-3)}\
.rh-login::before{content:'';position:absolute;inset:-40% 10% auto;height:60%;z-index:-1;background:radial-gradient(60% 100% at 50% 0,color-mix(in srgb,var(--rh-accent) 40%,transparent),transparent 70%);filter:blur(30px)}\
.rh-login h1{margin:0 0 var(--rh-space-2);text-align:center;font-size:var(--rh-font-2xl);letter-spacing:-.02em;display:flex;flex-direction:column;align-items:center;gap:.7rem;color:var(--rh-text)}\
.rh-login h1::before{content:'';width:3.25rem;height:3.25rem;border-radius:var(--rh-radius-full);background:radial-gradient(circle at 50% 52%,var(--rh-surface) 0 15%,var(--rh-accent) 15% 27%,var(--rh-surface) 27% 41%,color-mix(in srgb,var(--rh-accent) 62%,var(--rh-surface)) 41% 58%,var(--rh-surface) 58% 73%,color-mix(in srgb,var(--rh-accent) 34%,var(--rh-surface)) 73% 100%);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 35%,transparent),0 8px 24px -6px color-mix(in srgb,var(--rh-accent) 70%,transparent)}\
.rh-login label{font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);text-transform:uppercase;letter-spacing:.05em;margin-bottom:-.35rem}\
.rh-login .rh-btn{justify-content:center;margin-top:var(--rh-space-2);padding:.6rem;font-size:var(--rh-font-size)}\
.rh-live-toggle{display:flex;align-items:center;gap:.5rem;font-size:var(--rh-font-sm);color:var(--rh-muted);cursor:pointer;text-transform:none;letter-spacing:normal;margin-bottom:0}\
.rh-live-toggle input{accent-color:var(--rh-accent);cursor:pointer}\
.rh-body{flex:1;display:flex;min-height:0}\
.rh-chat{flex:1;display:flex;flex-direction:column;min-width:0;position:relative}\
.rh-jump-new{position:absolute;left:50%;bottom:4.6rem;transform:translateX(-50%);z-index:15;font:inherit;font-size:var(--rh-font-sm);font-weight:600;color:var(--rh-bg);background:var(--rh-accent);border:0;border-radius:var(--rh-radius-full);padding:.35rem .95rem;cursor:pointer;box-shadow:var(--rh-shadow-2);white-space:nowrap;animation:rh-pop .2s cubic-bezier(.2,.9,.3,1.2) both;transition:box-shadow .15s ease,background-color .15s ease}\
.rh-jump-new:hover{background:color-mix(in srgb,var(--rh-accent) 88%,var(--rh-text));box-shadow:var(--rh-shadow-3)}\
.rh-scroll{flex:1;overflow-y:auto;padding:var(--rh-space-5);display:flex;flex-direction:column;gap:.1rem}\
.rh-lines{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.1rem}\
.rh-line{position:relative;padding:.35rem 3.6rem .35rem .6rem;border-radius:var(--rh-radius);transition:background-color .12s ease}\
.rh-line:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-line .rh-from{color:var(--rh-accent);font-weight:600;margin-right:var(--rh-space-2)}\
.rh-line-time{position:absolute;right:.6rem;top:.45rem;font-size:var(--rh-font-xs);color:var(--rh-muted);font-variant-numeric:tabular-nums}\
.rh-line-head{margin-top:.4rem}\
.rh-line-head:first-child{margin-top:0}\
.rh-line-cont{padding-top:.1rem;padding-bottom:.1rem}\
.rh-line-cont .rh-line-time{opacity:0;transition:opacity .12s ease}\
.rh-line-cont:hover .rh-line-time{opacity:1}\
.rh-compose{display:flex;gap:var(--rh-space-2);padding:var(--rh-space-3) var(--rh-space-5);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);background:color-mix(in srgb,var(--rh-surface) 50%,transparent)}\
.rh-compose .rh-input{flex:1;border-radius:var(--rh-radius-full);padding-left:var(--rh-space-4)}\
.rh-compose .rh-btn{border-radius:var(--rh-radius-full);padding-left:var(--rh-space-5);padding-right:var(--rh-space-5)}\
.rh-who{width:14rem;background:color-mix(in srgb,var(--rh-surface) 55%,var(--rh-bg));padding:var(--rh-space-4) var(--rh-space-3);overflow-y:auto;border-left:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-who h2{font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.06em;font-weight:700;color:var(--rh-muted);margin:.2rem .4rem var(--rh-space-3)}\
.rh-who ul{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.1rem}\
.rh-who li{display:flex;align-items:center;gap:.55rem;padding:.4rem .5rem;border-radius:var(--rh-radius);font-size:var(--rh-font-sm);transition:background-color .12s ease}\
.rh-who li:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-panel{flex:1;padding:var(--rh-space-5);overflow-y:auto;min-width:0}\
.rh-panel-title{font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.06em;font-weight:700;color:var(--rh-muted);margin:.2rem 0 var(--rh-space-4)}\
.rh-tree{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:var(--rh-space-2)}\
.rh-board-link,.rh-thread-link,.rh-member-link,.rh-file-link,.rh-station-link{display:flex;flex-direction:column;gap:.2rem;width:100%;text-align:left;text-decoration:none;font:inherit;cursor:pointer;background:var(--rh-surface);color:var(--rh-text);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);border-radius:var(--rh-radius-lg);padding:.7rem var(--rh-space-4);transition:border-color .15s ease,box-shadow .15s ease,transform .12s ease}\
.rh-board-link:hover,.rh-thread-link:hover,.rh-member-link:hover,.rh-file-link:hover,.rh-station-link:hover{border-color:color-mix(in srgb,var(--rh-accent) 45%,transparent);box-shadow:var(--rh-shadow-2);transform:translateY(-1px)}\
.rh-thread-link.active,.rh-file-link.active,.rh-station-link.active{border-color:var(--rh-accent);box-shadow:0 0 0 1px var(--rh-accent),var(--rh-shadow-2)}\
.rh-board-name,.rh-thread-title{font-weight:600;color:var(--rh-text);font-size:var(--rh-font-size)}\
.rh-board-desc,.rh-thread-author,.rh-member-handle{font-size:var(--rh-font-xs);color:var(--rh-muted)}\
.rh-back{display:inline-flex;align-items:center;gap:.3rem;margin-bottom:var(--rh-space-3);color:var(--rh-muted);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;background:none;border:0;cursor:pointer;padding:0;transition:color .15s ease}\
.rh-back:hover{color:var(--rh-accent)}\
.rh-threads{max-width:22rem;border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-reader{flex:2}\
.rh-posts{display:flex;flex-direction:column;gap:var(--rh-space-3)}\
.rh-post{background:var(--rh-surface);border-radius:var(--rh-radius-lg);padding:var(--rh-space-4);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-post .rh-from{color:var(--rh-accent);font-weight:600}\
.rh-post-body{margin:.4rem 0 0;line-height:1.6}\
.rh-empty{color:var(--rh-muted);font-style:italic;padding:var(--rh-space-4);text-align:center}\
.rh-dm-peer{width:100%;text-align:left;font:inherit;cursor:pointer;background:transparent;color:var(--rh-text);border:1px solid transparent;border-radius:var(--rh-radius);padding:.45rem .6rem;display:flex;align-items:center;gap:.55rem;transition:background-color .12s ease,color .12s ease}\
.rh-dm-peer::before{content:'';flex:none;width:1.7rem;height:1.7rem;border-radius:var(--rh-radius-full);background:linear-gradient(135deg,color-mix(in srgb,var(--rh-accent) 75%,var(--rh-surface)),color-mix(in srgb,var(--rh-accent) 25%,var(--rh-surface)))}\
.rh-dm-peer:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-dm-peer.active{background:color-mix(in srgb,var(--rh-accent) 14%,transparent);color:var(--rh-accent);font-weight:600}\
.rh-member-link{flex-direction:row;align-items:center;gap:var(--rh-space-3);padding:.6rem var(--rh-space-3)}\
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
.rh-bar-fill{height:100%;border-radius:var(--rh-radius-full);background:linear-gradient(90deg,color-mix(in srgb,var(--rh-accent) 70%,var(--rh-surface)),var(--rh-accent));transition:width .3s ease}\
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
.rh-slider{accent-color:var(--rh-accent);flex:1}\
.rh-hint{color:var(--rh-muted);font-size:var(--rh-font-sm);margin:.3rem 0;line-height:1.5}\
.rh-radio-now{color:var(--rh-accent);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:18rem;display:inline-flex;align-items:center;gap:.4rem}\
.rh-radio-now::before{content:'';width:.5rem;height:.5rem;border-radius:50%;background:var(--rh-error);flex:none;box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-error) 25%,transparent)}\
.rh-radio-now:hover{text-decoration:underline}\
.rh-live-slot:empty{display:none}\
.rh-admin-status{padding:var(--rh-space-2) var(--rh-space-5);color:var(--rh-muted);font-size:var(--rh-font-sm)}\
.rh-admin-main{flex:1;display:flex;flex-direction:column;min-width:0}\
.rh-config-row,.rh-account-row{flex-direction:row;align-items:center;gap:var(--rh-space-2);flex-wrap:wrap}\
.rh-table{width:100%;border-collapse:collapse;font-size:var(--rh-font-sm);margin:0 0 var(--rh-space-4)}\
.rh-table th{text-align:left;font-size:var(--rh-font-xs);font-weight:700;text-transform:uppercase;letter-spacing:.05em;color:var(--rh-muted);padding:.3rem var(--rh-space-3) .5rem 0}\
.rh-table td{padding:.5rem var(--rh-space-3) .5rem 0;vertical-align:middle}\
.rh-table tbody tr{border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-table tbody tr:hover{background:color-mix(in srgb,var(--rh-text) 4%,transparent)}\
.rh-fieldset{border:0;padding:0;margin:0;min-width:0}\
.rh-fieldset legend{float:left;padding:0}\
.rh-config-key{font-weight:600;min-width:12rem;font-family:var(--rh-font-mono);font-size:var(--rh-font-xs)}\
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
.rh-scroll::-webkit-scrollbar,.rh-panel::-webkit-scrollbar,.rh-who::-webkit-scrollbar{width:10px;height:10px}\
.rh-scroll::-webkit-scrollbar-thumb,.rh-panel::-webkit-scrollbar-thumb,.rh-who::-webkit-scrollbar-thumb{background:color-mix(in srgb,var(--rh-text) 18%,transparent);border-radius:var(--rh-radius-full);border:3px solid transparent;background-clip:padding-box}\
.rh-scroll::-webkit-scrollbar-thumb:hover{background:color-mix(in srgb,var(--rh-text) 30%,transparent);background-clip:padding-box}\
/* Between the desktop row and the phone grid there is a band where the header\
   has more controls than room. Two things go first, in this order, because\
   neither is load-bearing: the status line (the connection banner says the same\
   thing, louder) and the Cmd-K hint (the shortcut still works without a button\
   advertising it). Measured: without this the header overflows by ~29px at\
   760px wide, which is exactly where a small desktop window lands. */\
@media (max-width:860px){.rh-status,.rh-kbd-jump{display:none}}\
@media (max-width:720px){.rh-header{display:grid;grid-template-columns:minmax(0,1fr) auto auto;grid-template-areas:\"title dot presence\" \"live live theme\" \"nav nav nav\";align-items:center;padding:var(--rh-space-2) var(--rh-space-3);min-height:0;gap:var(--rh-space-2)}.rh-header .rh-title{grid-area:title;font-size:var(--rh-font-size);min-width:0;overflow:hidden}.rh-dot{grid-area:dot}.rh-presence{grid-area:presence;justify-self:end;padding:.25rem .4rem;font-size:var(--rh-font-xs)}.rh-live-slot{grid-area:live;min-width:0;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}\
.rh-live-slot .rh-radio-now{display:block;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}.rh-nav{grid-area:nav;min-width:0;overflow-x:auto;padding-bottom:.15rem}.rh-subnav{position:fixed;left:0;right:0;bottom:0;z-index:30;width:auto;flex-direction:row;gap:0;padding:.3rem var(--rh-space-2) calc(.3rem + env(safe-area-inset-bottom));overflow-x:auto;overflow-y:hidden;border-right:0;border-top:1px solid color-mix(in srgb,var(--rh-text) 12%,transparent);background:color-mix(in srgb,var(--rh-surface) 92%,transparent);backdrop-filter:saturate(1.4) blur(14px);-webkit-backdrop-filter:saturate(1.4) blur(14px)}.rh-subnav-link{flex:none;flex-direction:column;gap:.1rem;padding:.3rem .6rem;font-size:var(--rh-font-xs);min-width:3.7rem;justify-content:center}.rh-subnav-label{flex:none}.rh-subnav-rule{display:none}.rh-subnav .rh-pip{position:absolute;top:.15rem;right:.5rem}.rh-subnav-link{position:relative}.rh-shell-main{padding-bottom:3.6rem}.rh-theme-menu{grid-area:theme;justify-self:end}.rh-theme-menu button{padding:.25rem .5rem;font-size:var(--rh-font-xs)}.rh-status,.rh-kbd-jump,.rh-spacer{display:none}.rh-conn{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap;border:0}.rh-toasts{top:6.4rem}.rh-body{flex-direction:column}.rh-who,.rh-threads,.rh-members,.rh-files,.rh-stations{max-width:none;width:auto;border-right:0;border-left:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}.rh-who{max-height:35vh}.rh-chat{min-height:0}.rh-filetable-head,.rh-filetable .rh-file-link{grid-template-columns:minmax(0,1fr) 5rem}.rh-fcol-kind,.rh-fcol-who,.rh-fcol-when{display:none}.rh-scroll{padding:var(--rh-space-3)}.rh-compose{padding:var(--rh-space-2) var(--rh-space-3)}.rh-compose .rh-input{min-width:0}.rh-compose .rh-btn{padding-left:var(--rh-space-4);padding-right:var(--rh-space-4)}.rh-present{order:-1;display:flex;align-items:center;gap:var(--rh-space-2);padding:.4rem var(--rh-space-3);max-height:none}.rh-present h2{margin:0;flex:none}.rh-present ul{flex-direction:row;flex:1;min-width:0;overflow-x:auto;gap:.4rem;padding-bottom:.15rem}.rh-present li{flex:none;white-space:nowrap}.rh-reader{min-height:14rem}.rh-login{margin:var(--rh-space-6) var(--rh-space-4)}}\
@keyframes rh-shimmer{0%{background-position:-180% 0}100%{background-position:180% 0}}\
.rh-skeleton{display:flex;flex-direction:column;gap:.55rem;padding:var(--rh-space-4)}\
.rh-skeleton-row{height:.85rem;border-radius:var(--rh-radius-full,999px);background:linear-gradient(90deg,color-mix(in srgb,var(--rh-text) 7%,transparent) 25%,color-mix(in srgb,var(--rh-text) 13%,transparent) 50%,color-mix(in srgb,var(--rh-text) 7%,transparent) 75%);background-size:180% 100%;animation:rh-shimmer 1.35s ease-in-out infinite}\
@keyframes rh-fade-up{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:none}}\
@keyframes rh-slide-down{from{opacity:0;transform:translateY(-8px)}to{opacity:1;transform:none}}\
@keyframes rh-toast-in{from{opacity:0;transform:translateX(14px) scale(.98)}to{opacity:1;transform:none}}\
@keyframes rh-pop{from{opacity:0;transform:scale(.85)}to{opacity:1;transform:none}}\
@keyframes rh-pulse-ring{0%{box-shadow:0 0 0 0 color-mix(in srgb,#3fbf7f 55%,transparent)}70%{box-shadow:0 0 0 4px color-mix(in srgb,#3fbf7f 0%,transparent)}100%{box-shadow:0 0 0 0 color-mix(in srgb,#3fbf7f 0%,transparent)}}\
.rh-body{animation:rh-fade-up .19s ease both}\
.rh-welcome{animation:rh-slide-down .24s cubic-bezier(.2,.8,.2,1) both}\
.rh-toast{animation:rh-toast-in .22s cubic-bezier(.2,.8,.2,1) both}\
.rh-pres.on{box-shadow:0 0 0 2px color-mix(in srgb,#3fbf7f 22%,transparent)}\
.rh-rail-dot.on{animation:rh-pulse-ring 2.8s ease-out infinite}\
.rh-rail-tile{transition:background-color .15s ease,color .15s ease,transform .12s ease,box-shadow .15s ease}\
.rh-rail-tile:hover{transform:translateY(-1px);box-shadow:0 3px 8px color-mix(in srgb,var(--rh-text) 14%,transparent)}\
.rh-rail-tile:active{transform:translateY(0) scale(.95)}\
.rh-person,.rh-xfer-item,.rh-who-row{animation:rh-fade-up .2s ease both}\
.rh-btn:active{transform:scale(.97)}\
.rh-rail-server.active{animation:rh-pop .22s cubic-bezier(.2,.9,.3,1.2) both}\
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
        let last_transition = STYLESHEET.rfind("transition:width").unwrap();
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
                assert!(!bg.contains("--"), "{pack:?}/{mode:?} grabbed a var, not a value");
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
        const CHROME: [&str; 5] = [
            ".rh-subnav",
            ".rh-rail",
            ".rh-header",
            ".rh-format-bar",
            ".rh-tabs",
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
