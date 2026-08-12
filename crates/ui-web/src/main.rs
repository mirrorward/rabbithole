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
    // Surface panic locations in the browser console. Without a hook a wasm
    // panic reports only an opaque `RuntimeError: unreachable` — with it the
    // console shows the real message and file:line (this is how the
    // effect-after-dispose panic in the 0.148.0 scroll work was found).
    #[cfg(target_arch = "wasm32")]
    std::panic::set_hook(Box::new(|info| {
        // The message alone names the *library* line that gave up, which for a
        // reactive-graph panic is always somewhere inside leptos and never the
        // code that caused it. Throwing away a JS `Error` at the moment of the
        // panic captures the call stack that got us there, so the wasm frames
        // in between name the real culprit.
        let stack = js_sys::Reflect::get(&js_sys::Error::new("panic"), &"stack".into())
            .unwrap_or_else(|_| "<no stack>".into());
        web_sys::console::error_2(&format!("panic: {info}").into(), &stack);
    }));
    rabbithole_ui_web::mount();
}
