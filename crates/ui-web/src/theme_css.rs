//! Mapping [`rabbithole_core::theme`] design tokens to CSS.
//!
//! The palette lives in the core so every client means the same thing by
//! "accent" or "surface". Here we render a [`Palette`] into CSS custom
//! properties (`--rh-*`) that the [static stylesheet](STYLESHEET) consumes.
//! The toggle applies these variables as an inline `style` on the app root,
//! so no `web_sys` DOM poking is needed and the whole subtree re-themes
//! reactively.

use rabbithole_core::theme::{Mode, Palette, Rgb, ThemePack};

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Render a palette as a `--rh-*: #rrggbb; ...` string suitable for a `style`
/// attribute.
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

/// The CSS variables for the built-in Clean pack at the given [`Mode`].
pub fn root_vars(mode: Mode) -> String {
    palette_vars(&Palette::builtin(ThemePack::Clean, mode))
}

/// Flip a mode; used by the theme toggle.
pub fn toggle(mode: Mode) -> Mode {
    match mode {
        Mode::Light => Mode::Dark,
        Mode::Dark => Mode::Light,
    }
}

/// A compact, framework-free stylesheet mounted once by the app root. All
/// colors reference the `--rh-*` custom properties emitted by [`root_vars`].
pub const STYLESHEET: &str = "\
*{box-sizing:border-box}\
.rh-app{font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;\
color:var(--rh-text);background:var(--rh-bg);min-height:100vh;\
display:flex;flex-direction:column}\
.rh-header{display:flex;align-items:center;gap:.75rem;padding:.6rem 1rem;\
background:var(--rh-surface);border-bottom:1px solid var(--rh-bg)}\
.rh-header .rh-title{font-weight:600;color:var(--rh-accent)}\
.rh-header .rh-status{color:var(--rh-muted);font-size:.85rem}\
.rh-spacer{flex:1}\
.rh-dot{width:.6rem;height:.6rem;border-radius:50%;display:inline-block}\
.rh-dot.on{background:var(--rh-accent)}\
.rh-dot.off{background:var(--rh-muted)}\
.rh-btn{font:inherit;cursor:pointer;border:1px solid var(--rh-accent);\
background:var(--rh-accent);color:var(--rh-bg);border-radius:.4rem;\
padding:.4rem .8rem}\
.rh-btn.ghost{background:transparent;color:var(--rh-accent)}\
.rh-input{font:inherit;padding:.45rem .6rem;border-radius:.4rem;\
border:1px solid var(--rh-muted);background:var(--rh-bg);color:var(--rh-text)}\
.rh-login{max-width:22rem;margin:4rem auto;display:flex;flex-direction:column;\
gap:.75rem;background:var(--rh-surface);padding:1.5rem;border-radius:.6rem}\
.rh-login h1{margin:0 0 .5rem;color:var(--rh-accent)}\
.rh-login label{font-size:.8rem;color:var(--rh-muted)}\
.rh-body{flex:1;display:flex;min-height:0}\
.rh-chat{flex:1;display:flex;flex-direction:column;min-width:0}\
.rh-scroll{flex:1;overflow-y:auto;padding:1rem;display:flex;\
flex-direction:column;gap:.4rem}\
.rh-line .rh-from{color:var(--rh-accent);font-weight:600;margin-right:.4rem}\
.rh-compose{display:flex;gap:.5rem;padding:.75rem;\
border-top:1px solid var(--rh-surface)}\
.rh-compose .rh-input{flex:1}\
.rh-who{width:12rem;background:var(--rh-surface);padding:.75rem;\
overflow-y:auto;border-left:1px solid var(--rh-bg)}\
.rh-who h2{font-size:.8rem;text-transform:uppercase;letter-spacing:.05em;\
color:var(--rh-muted);margin:.2rem 0 .6rem}\
.rh-who ul{list-style:none;margin:0;padding:0;display:flex;\
flex-direction:column;gap:.35rem}\
.rh-who li::before{content:'\\2022';color:var(--rh-accent);margin-right:.4rem}\
.rh-nav{display:flex;gap:.75rem;align-items:center}\
.rh-nav a{color:var(--rh-muted);text-decoration:none;font-size:.9rem;\
padding:.2rem .1rem;border-bottom:2px solid transparent}\
.rh-nav a:hover{color:var(--rh-text)}\
.rh-nav a.active{color:var(--rh-accent);border-bottom-color:var(--rh-accent)}\
.rh-panel{flex:1;padding:1rem;overflow-y:auto;min-width:0}\
.rh-panel-title{font-size:.8rem;text-transform:uppercase;letter-spacing:.05em;\
color:var(--rh-muted);margin:.2rem 0 .8rem}\
.rh-tree{list-style:none;margin:0;padding:0;display:flex;\
flex-direction:column;gap:.5rem}\
.rh-board-link,.rh-thread-link,.rh-member-link{display:flex;flex-direction:column;\
gap:.15rem;width:100%;text-align:left;text-decoration:none;font:inherit;\
cursor:pointer;background:var(--rh-surface);color:var(--rh-text);\
border:1px solid var(--rh-surface);border-radius:.4rem;padding:.6rem .8rem}\
.rh-board-link:hover,.rh-thread-link:hover,.rh-member-link:hover{\
border-color:var(--rh-accent)}\
.rh-thread-link.active{border-color:var(--rh-accent)}\
.rh-board-name,.rh-thread-title{font-weight:600;color:var(--rh-text)}\
.rh-board-desc,.rh-thread-author,.rh-member-handle{font-size:.8rem;\
color:var(--rh-muted)}\
.rh-back{display:inline-block;margin-bottom:.6rem;color:var(--rh-accent);\
text-decoration:none;font-size:.85rem}\
.rh-threads{max-width:22rem;border-right:1px solid var(--rh-surface)}\
.rh-reader{flex:2}\
.rh-posts{display:flex;flex-direction:column;gap:.75rem}\
.rh-post{background:var(--rh-surface);border-radius:.4rem;padding:.75rem .9rem}\
.rh-post-body{margin:.3rem 0 0}\
.rh-empty{color:var(--rh-muted);font-style:italic}\
.rh-dm-peer,.rh-member-link{align-items:flex-start}\
.rh-dm-peer{width:100%;text-align:left;font:inherit;cursor:pointer;\
background:transparent;color:var(--rh-text);border:none;padding:.35rem 0}\
.rh-dm-peer:hover{color:var(--rh-accent)}\
.rh-dm-peer.active{color:var(--rh-accent);font-weight:600}\
.rh-member-link{flex-direction:row;align-items:center;gap:.5rem}\
.rh-member-name{font-weight:600}\
.rh-members{max-width:24rem;border-right:1px solid var(--rh-surface);\
display:flex;flex-direction:column;gap:.6rem}\
.rh-card{background:var(--rh-surface);border-radius:.6rem;padding:1.2rem}\
.rh-card-name{margin:0;color:var(--rh-accent)}\
.rh-card-handle,.rh-card-status{margin:.2rem 0;color:var(--rh-muted);\
font-size:.85rem}\
.rh-card-bio{margin:.8rem 0 0}\
";
