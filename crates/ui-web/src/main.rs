//! The trunk wasm entry point.
//!
//! `trunk build` (run in `crates/ui-web/`) compiles this bin to
//! `wasm32-unknown-unknown` via the `data-trunk rel="rust"` link in
//! `index.html`; the generated JS glue calls `main` on page load. All it
//! does is hand off to [`rabbithole_ui_web::mount`], which registers the
//! service worker (browser only, never fatal) and mounts the Leptos app
//! into `document.body`.
//!
//! On the host this binary compiles (so `--all-targets` covers it) but is
//! never run — there is no DOM to mount into.

fn main() {
    rabbithole_ui_web::mount();
}
