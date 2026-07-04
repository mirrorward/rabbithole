//! RabbitHole Tauri v2 shell — shared desktop + mobile entry point.
//!
//! `run()` is the single entry used on every platform. On mobile the
//! `mobile_entry_point` macro exports it for the iOS/Android host frameworks;
//! on desktop `main.rs` calls it directly. For the scaffold slice it stands up
//! the default window, which loads the `rabbithole-ui-web` Leptos SPA verbatim
//! (the trunk dev server in `dev`, the bundled `crates/ui-web/dist` in release).
//!
//! Later slices wire the native chrome here — tray/menubar quick-status, native
//! notifications, `rabbit://` deep links, auto-update, and eventually the
//! in-process Rust core — behind `.plugin()` / `.setup()`.

/// Source discovery + multi-source swarm download orchestration (Tauri-free).
pub mod swarm;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| Ok(()))
        .run(tauri::generate_context!())
        .expect("error while running the RabbitHole desktop application");
}
