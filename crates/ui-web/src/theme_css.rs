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
/// 2. otherwise a **server theme overlay** (PLAN §9.11), when present and not
///    disabled by the user, layered on top of the built-in `pack` — the
///    operator's accent/metric tokens nudge the chosen pack without replacing
///    it;
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
///
/// Accessibility blocks (host-asserted by the shape tests below):
/// `:focus-visible` outlines on the `--rh-focus` token, the `.rh-skip` skip
/// link, the `.rh-visually-hidden` screen-reader-only helper,
/// `[aria-current=page]` styling for the active nav link, and a
/// `prefers-reduced-motion: reduce` block that neutralises all motion.
pub const STYLESHEET: &str = "\
*{box-sizing:border-box}\
.rh-app{font-family:var(--rh-font-sans);font-size:var(--rh-font-size);line-height:1.5;color:var(--rh-text);background-color:var(--rh-bg);background-image:var(--rh-bg-image);min-height:100vh;display:flex;flex-direction:column;-webkit-font-smoothing:antialiased;text-rendering:optimizeLegibility}\
:focus-visible{outline:2px solid var(--rh-focus);outline-offset:2px}\
.rh-visually-hidden{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap;border:0}\
.rh-skip{position:fixed;left:-999rem;top:var(--rh-space-2);z-index:99;background:var(--rh-accent);color:var(--rh-bg);padding:var(--rh-space-2) var(--rh-space-3);border-radius:var(--rh-radius);text-decoration:none;font-weight:600;box-shadow:var(--rh-shadow-2)}\
.rh-skip:focus{left:var(--rh-space-2)}\
.rh-header{display:flex;align-items:center;gap:var(--rh-space-3);padding:0 var(--rh-space-5);min-height:3.5rem;position:sticky;top:0;z-index:20;background:color-mix(in srgb,var(--rh-surface) 82%,transparent);backdrop-filter:saturate(1.4) blur(14px);-webkit-backdrop-filter:saturate(1.4) blur(14px);border-bottom:1px solid color-mix(in srgb,var(--rh-text) 10%,transparent)}\
.rh-header .rh-title{order:1;display:inline-flex;align-items:center;gap:.55rem;white-space:nowrap;font-weight:700;font-size:var(--rh-font-lg);letter-spacing:-.01em;color:var(--rh-text)}\
.rh-header .rh-title::before{content:'';flex:none;width:1.4rem;height:1.4rem;border-radius:var(--rh-radius-full);background:radial-gradient(circle at 50% 52%,var(--rh-surface) 0 15%,var(--rh-accent) 15% 27%,var(--rh-surface) 27% 41%,color-mix(in srgb,var(--rh-accent) 62%,var(--rh-surface)) 41% 58%,var(--rh-surface) 58% 73%,color-mix(in srgb,var(--rh-accent) 34%,var(--rh-surface)) 73% 100%);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 35%,transparent),0 2px 8px -2px color-mix(in srgb,var(--rh-accent) 60%,transparent)}\
.rh-dot{order:2;width:.5rem;height:.5rem;border-radius:50%;display:inline-block;flex:none;margin-left:.2rem}\
.rh-dot.on{background:#3fbf7f;box-shadow:0 0 0 3px color-mix(in srgb,#3fbf7f 22%,transparent)}\
.rh-dot.off{background:var(--rh-muted)}\
.rh-dot.pending{background:var(--rh-accent);box-shadow:0 0 0 3px color-mix(in srgb,var(--rh-accent) 22%,transparent)}\
.rh-conn{order:3;font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);text-transform:uppercase;letter-spacing:.06em;white-space:nowrap}\
.rh-status{order:4;color:var(--rh-muted);font-size:var(--rh-font-sm);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:14rem}\
.rh-spacer{order:5;flex:1}\
.rh-live-slot{order:6}\
.rh-theme-menu{order:8;display:inline-flex;gap:.35rem;align-items:center}\
.rh-nav{order:7;display:flex;gap:.15rem;align-items:center}\
.rh-nav a,.rh-nav .rh-nav-item{color:var(--rh-muted);text-decoration:none;font-size:var(--rh-font-sm);font-weight:500;padding:.35rem .7rem;border-radius:var(--rh-radius-full);transition:background-color .15s ease,color .15s ease;border-bottom:0}\
.rh-nav a:hover,.rh-nav .rh-nav-item:hover{color:var(--rh-text);background:color-mix(in srgb,var(--rh-text) 7%,transparent)}\
.rh-nav a.active,.rh-nav a[aria-current=page],.rh-nav .rh-nav-item.active{color:var(--rh-accent);background:color-mix(in srgb,var(--rh-accent) 14%,transparent)}\
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
.rh-login{position:relative;max-width:23rem;margin:5rem auto;display:flex;flex-direction:column;gap:var(--rh-space-3);background:var(--rh-surface);padding:var(--rh-space-8);border-radius:var(--rh-radius-xl);border:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);box-shadow:var(--rh-shadow-3)}\
.rh-login::before{content:'';position:absolute;inset:-40% 10% auto;height:60%;z-index:-1;background:radial-gradient(60% 100% at 50% 0,color-mix(in srgb,var(--rh-accent) 40%,transparent),transparent 70%);filter:blur(30px)}\
.rh-login h1{margin:0 0 var(--rh-space-2);text-align:center;font-size:var(--rh-font-2xl);letter-spacing:-.02em;display:flex;flex-direction:column;align-items:center;gap:.7rem;color:var(--rh-text)}\
.rh-login h1::before{content:'';width:3.25rem;height:3.25rem;border-radius:var(--rh-radius-full);background:radial-gradient(circle at 50% 52%,var(--rh-surface) 0 15%,var(--rh-accent) 15% 27%,var(--rh-surface) 27% 41%,color-mix(in srgb,var(--rh-accent) 62%,var(--rh-surface)) 41% 58%,var(--rh-surface) 58% 73%,color-mix(in srgb,var(--rh-accent) 34%,var(--rh-surface)) 73% 100%);box-shadow:0 0 0 1px color-mix(in srgb,var(--rh-accent) 35%,transparent),0 8px 24px -6px color-mix(in srgb,var(--rh-accent) 70%,transparent)}\
.rh-login label{font-size:var(--rh-font-xs);font-weight:600;color:var(--rh-muted);text-transform:uppercase;letter-spacing:.05em;margin-bottom:-.35rem}\
.rh-login .rh-btn{justify-content:center;margin-top:var(--rh-space-2);padding:.6rem;font-size:var(--rh-font-size)}\
.rh-live-toggle{display:flex;align-items:center;gap:.5rem;font-size:var(--rh-font-sm);color:var(--rh-muted);cursor:pointer;text-transform:none;letter-spacing:normal;margin-bottom:0}\
.rh-live-toggle input{accent-color:var(--rh-accent);cursor:pointer}\
.rh-body{flex:1;display:flex;min-height:0}\
.rh-chat{flex:1;display:flex;flex-direction:column;min-width:0}\
.rh-scroll{flex:1;overflow-y:auto;padding:var(--rh-space-5);display:flex;flex-direction:column;gap:.1rem}\
.rh-lines{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.1rem}\
.rh-line{padding:.35rem .6rem;border-radius:var(--rh-radius);transition:background-color .12s ease}\
.rh-line:hover{background:color-mix(in srgb,var(--rh-text) 5%,transparent)}\
.rh-line .rh-from{color:var(--rh-accent);font-weight:600;margin-right:var(--rh-space-2)}\
.rh-compose{display:flex;gap:var(--rh-space-2);padding:var(--rh-space-3) var(--rh-space-5);border-top:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);background:color-mix(in srgb,var(--rh-surface) 50%,transparent)}\
.rh-compose .rh-input{flex:1;border-radius:var(--rh-radius-full);padding-left:var(--rh-space-4)}\
.rh-compose .rh-btn{border-radius:var(--rh-radius-full);padding-left:var(--rh-space-5);padding-right:var(--rh-space-5)}\
.rh-who{width:14rem;background:color-mix(in srgb,var(--rh-surface) 55%,var(--rh-bg));padding:var(--rh-space-4) var(--rh-space-3);overflow-y:auto;border-left:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}\
.rh-who h2{font-size:var(--rh-font-xs);text-transform:uppercase;letter-spacing:.06em;font-weight:700;color:var(--rh-muted);margin:.2rem .4rem var(--rh-space-3)}\
.rh-who ul{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:.1rem}\
.rh-who li{display:flex;align-items:center;gap:.55rem;padding:.4rem .5rem;border-radius:var(--rh-radius);font-size:var(--rh-font-sm);transition:background-color .12s ease}\
.rh-who li:hover{background:color-mix(in srgb,var(--rh-text) 6%,transparent)}\
.rh-who li::before{content:'';flex:none;width:1.6rem;height:1.6rem;border-radius:var(--rh-radius-full);background:linear-gradient(135deg,color-mix(in srgb,var(--rh-accent) 75%,var(--rh-surface)),color-mix(in srgb,var(--rh-accent) 30%,var(--rh-surface)));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--rh-text) 10%,transparent)}\
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
.rh-files{max-width:32rem;border-right:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent);display:flex;flex-direction:column;gap:var(--rh-space-3)}\
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
@media (max-width:720px){.rh-header{flex-wrap:wrap;padding:var(--rh-space-2) var(--rh-space-4);min-height:0;gap:var(--rh-space-2) var(--rh-space-3)}.rh-status{display:none}.rh-nav{order:9;width:100%;overflow-x:auto;padding-bottom:.15rem}.rh-body{flex-direction:column}.rh-who,.rh-threads,.rh-members,.rh-files,.rh-stations{max-width:none;width:auto;border-right:0;border-left:0;border-bottom:1px solid color-mix(in srgb,var(--rh-text) 8%,transparent)}.rh-reader{min-height:14rem}.rh-login{margin:var(--rh-space-6) var(--rh-space-4)}}\
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
    fn stylesheet_never_sets_a_positive_tabindex_or_hides_focus() {
        // Belt-and-braces textual checks mirroring the markup rules: CSS
        // cannot set tabindex, but it can break keyboard UX with these.
        assert!(!STYLESHEET.contains("pointer-events:none"));
        assert!(!STYLESHEET.contains("user-select:none"));
    }
}
