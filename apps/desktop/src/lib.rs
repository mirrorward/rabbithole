//! RabbitHole Tauri v2 shell — shared desktop + mobile entry point.
//!
//! `run()` is the single entry used on every platform. On mobile the
//! `mobile_entry_point` macro exports it for the iOS/Android host frameworks;
//! on desktop `main.rs` calls it directly. The window loads the `rabbithole-ui-web`
//! Leptos SPA verbatim (trunk dev server in `dev`, bundled `crates/ui-web/dist`
//! in release).
//!
//! ## Native bridge (Slice 3 of the swarm backend)
//!
//! The window is built in Rust so it can carry an [`NATIVE_SHIM`] init script
//! that exposes a tiny `window.__RH_NATIVE__ = { invoke, listen }` over Tauri's
//! always-present `window.__TAURI_INTERNALS__` — **without** re-enabling the
//! global `window.__TAURI__` (`withGlobalTauri` stays `false`, per the security
//! review). The wasm SPA detects `window.__RH_IS_NATIVE__` at runtime (true only
//! inside Tauri; the plain web build has neither) and, when native, routes
//! downloads to the in-process swarm core instead of the WebSocket transport.
//!
//! This slice is the IPC *hello-world*: a `ping` command + a `test://tick` event,
//! self-tested by the init script so `cargo tauri dev` + the webview devtools
//! console prove the round-trip end-to-end. The real swarm command/event surface
//! (wrapping [`swarm::run_swarm_download`]) is the next slice.

/// Source discovery + multi-source swarm download orchestration (Tauri-free).
pub mod swarm;
/// The Tauri command + event surface wrapping the swarm core.
pub mod transfers;

/// Injected before the SPA loads: expose a minimal native bridge over Tauri's
/// low-level internals (present regardless of `withGlobalTauri`), then self-test
/// invoke + listen so the round-trip is visible in the devtools console.
const NATIVE_SHIM: &str = r#"
(function () {
  var I = window.__TAURI_INTERNALS__;
  window.__RH_IS_NATIVE__ = !!I;
  if (!I) { return; }
  window.__RH_NATIVE__ = {
    invoke: function (cmd, args) { return I.invoke(cmd, args || {}); },
    listen: function (event, cb) {
      return I.invoke('plugin:event|listen', {
        event: event,
        target: { kind: 'Any' },
        handler: I.transformCallback(function (e) { cb(e); })
      });
    }
  };
  // Self-test — visible in the Tauri webview devtools console.
  window.__RH_NATIVE__.invoke('ping', { name: 'slice-3' })
    .then(function (r) { console.log('[rh-native] invoke ping ->', r); })
    .catch(function (e) { console.error('[rh-native] invoke ping FAILED', e); });
  window.__RH_NATIVE__.listen('test://tick', function (e) {
    console.log('[rh-native] event test://tick ->', e && e.payload);
    // Invoke a Rust callback so the event (Rust->JS) round-trip is observable
    // from the `cargo tauri dev` terminal, not just the webview console.
    window.__RH_NATIVE__.invoke('tick_ack', { payload: String(e && e.payload) });
  })
    .then(function () { console.log('[rh-native] listening for test://tick'); })
    .catch(function (e) { console.error('[rh-native] listen FAILED', e); });
})();
"#;

/// A trivial command proving JS→Rust invoke works. Logs on the Rust side so the
/// round-trip is observable from the `cargo tauri dev` terminal (not just the
/// webview devtools console).
#[tauri::command]
fn ping(name: String) -> String {
    eprintln!("[rh-bridge] ping received from webview: name={name:?} — JS→Rust invoke works");
    format!("pong: {name}")
}

/// The webview calls this from its `test://tick` listener, so the Rust→JS event
/// delivery (and the `listen` subscription over `core:event`) is confirmed from
/// the terminal, closing the bridge round-trip in both directions.
#[tauri::command]
fn tick_ack(payload: String) {
    eprintln!("[rh-bridge] tick_ack from webview: event payload={payload:?} — Rust→JS event delivery works");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::{Emitter, WebviewUrl, WebviewWindowBuilder};

    tauri::Builder::default()
        .manage(transfers::TransfersManager::default())
        .invoke_handler(tauri::generate_handler![
            ping,
            tick_ack,
            transfers::native_available,
            transfers::connect_native,
            transfers::swarm_start_download,
        ])
        .setup(|app| {
            // Build the main window in Rust so it carries the native-bridge init
            // script (config `app.windows` is empty so this is the only window).
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("RabbitHole")
                .inner_size(1100.0, 760.0)
                .min_inner_size(720.0, 480.0)
                .initialization_script(NATIVE_SHIM)
                .build()?;

            // Emit a test event a beat after launch so the init-script listener
            // proves Rust→JS event delivery end-to-end.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                eprintln!("[rh-bridge] emitting test://tick — watch the webview console for receipt");
                let _ = handle.emit("test://tick", "hello from the native core");
            });
            eprintln!("[rh-bridge] window built with native shim; app starting");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the RabbitHole desktop application");
}
