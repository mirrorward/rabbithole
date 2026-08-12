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
  // Layout + build forensics: report what the webview is ACTUALLY rendering.
  // This is the only reliable window into the webview from the outside — the
  // desktop app has no remote devtools, and screenshots keep lying (stale
  // caches, capture offsets). Logged by `ping` to stderr.
  window.addEventListener('load', function () {
    setTimeout(function () {
      try {
        var d = document.documentElement;
        var app = document.querySelector('.rh-app');
        var r = app && app.getBoundingClientRect();
        var hr = d.getBoundingClientRect();
        var scripts = Array.prototype.map.call(document.scripts, function (s) {
          return (s.src || '').split('/').pop();
        }).filter(Boolean);
        var diag = {
          v: d.getAttribute('data-rh-version') || 'UNSTAMPED (pre-0.178 or stale cache)',
          iw: window.innerWidth, ih: window.innerHeight,
          dpr: window.devicePixelRatio,
          vv: window.visualViewport ? {
            w: window.visualViewport.width, h: window.visualViewport.height,
            scale: window.visualViewport.scale
          } : null,
          html: { w: hr.width, h: hr.height, x: hr.x, y: hr.y },
          app: r ? { w: r.width, h: r.height, x: r.x, y: r.y } : null,
          native_class: !!(app && app.classList.contains('native')),
          scripts: scripts,
          href: location.href
        };
        window.__RH_NATIVE__.invoke('ping', { name: 'diag ' + JSON.stringify(diag) });
      } catch (err) {
        window.__RH_NATIVE__.invoke('ping', { name: 'diag FAILED ' + String(err) });
      }
    }, 2500);
  });
  // No browser context menu over app chrome: "Reload Page" floating over a
  // sidebar is the loudest possible web-page tell. Editable fields and the
  // selectable content regions keep their menus — Copy and Look Up on a
  // message are features, not tells.
  window.addEventListener('contextmenu', function (e) {
    var t = e.target;
    if (t && t.closest &&
        t.closest('input, textarea, [contenteditable], .rh-rich, .rh-line, .rh-post, pre, code')) {
      return;
    }
    e.preventDefault();
  }, { capture: true });
  window.__RH_NATIVE__.listen('rh://navigate', function (e) {
    var to = e && e.payload;
    if (typeof to === 'string' && to.charAt(0) === '/') {
      // The SPA owns routing; dispatching popstate after pushState is how a
      // history-router hears about a navigation it didn't initiate.
      window.history.pushState({}, '', to);
      window.dispatchEvent(new PopStateEvent('popstate'));
    }
  });
  window.__RH_NATIVE__.listen('rh://fullscreen', function (e) {
    document.documentElement.classList.toggle('rh-fullscreen', !!(e && e.payload));
  });
  // Transitions come from events; the STARTING state has to be asked for —
  // events are not replayed, and a reload while fullscreen resets the class.
  window.__RH_NATIVE__.invoke('fullscreen_state')
    .then(function (fs) {
      document.documentElement.classList.toggle('rh-fullscreen', !!fs);
    })
    .catch(function () {});
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
/// The webview asks for the CURRENT fullscreen state at startup. The
/// `rh://fullscreen` events only fire on transitions (inside the Resized
/// handler), so a window restored fullscreen at launch — or a webview reload
/// while fullscreen, which resets <html> classes — would otherwise keep the
/// title strip's dead band until the user happened to resize.
#[tauri::command]
fn fullscreen_state(window: tauri::WebviewWindow) -> bool {
    window.is_fullscreen().unwrap_or(false)
}

#[tauri::command]
fn tick_ack(payload: String) {
    eprintln!("[rh-bridge] tick_ack from webview: event payload={payload:?} — Rust→JS event delivery works");
}

/// The macOS application menu.
///
/// Tauri's default menu names the app submenu and its About item after the
/// *binary* — "About rabbithole-desktop" — which is a build artifact's name,
/// not the app's. Building the menu by hand fixes that and buys two things
/// the default can't: a **Settings…** item on ⌘, (the macOS convention, which
/// every Mac user reaches for), and an About panel with something in it.
///
/// Building a custom menu replaces the whole default, so Edit and Window are
/// re-created here in full. Dropping them would silently cost ⌘C/⌘V in an app
/// full of text fields — a much worse regression than the wrong app name.
#[cfg(target_os = "macos")]
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};

    let about = AboutMetadataBuilder::new()
        .name(Some("RabbitHole"))
        .version(Some(env!("CARGO_PKG_VERSION")))
        .copyright(Some("© Mirrorward"))
        // `credits` is the one rich field macOS renders here, so it carries
        // what the panel is for: what this program *is*. One paragraph per
        // entry — the panel wraps text itself, and manual breaks fight it.
        .credits(Some(
            [
                "A warren client for RabbitHole.",
                "",
                "Chat, message boards, file libraries and swarmed transfers across many burrows at once \u{2014} with a portable identity that is yours, not any server's.",
                "",
                "rabbit.direct",
            ]
            .join("\n"),
        ))
        // An unbundled run (cargo run / tauri dev) has no .app for macOS to
        // take an icon from, so the panel falls back to a generic folder.
        // Embedding the already-rounded artwork makes it right either way.
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/about.png")).ok())
        .build();

    let settings = MenuItemBuilder::with_id("settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;

    let app_menu = SubmenuBuilder::new(app, "RabbitHole")
        .item(&PredefinedMenuItem::about(app, Some("About RabbitHole"), Some(about))?)
        .separator()
        .item(&settings)
        .separator()
        .item(&PredefinedMenuItem::services(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, Some("Hide RabbitHole"))?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .item(&PredefinedMenuItem::show_all(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit RabbitHole"))?)
        .build()?;

    // Re-created, not inherited: a custom menu replaces the default wholesale.
    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .item(&PredefinedMenuItem::undo(app, None)?)
        .item(&PredefinedMenuItem::redo(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, None)?)
        .item(&PredefinedMenuItem::copy(app, None)?)
        .item(&PredefinedMenuItem::paste(app, None)?)
        .item(&PredefinedMenuItem::select_all(app, None)?)
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .item(&PredefinedMenuItem::fullscreen(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, None)?)
        .build()?;

    MenuBuilder::new(app)
        .item(&app_menu)
        .item(&edit_menu)
        .item(&window_menu)
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::{Emitter, WebviewUrl, WebviewWindowBuilder};

    tauri::Builder::default()
        // Remember the window frame across launches. A window that reopens
        // wherever the user last put it is table stakes for a desktop app;
        // one that snaps back to a hardcoded 1100x760 every launch reads as a
        // browser tab in a wrapper. The builder's sizes below become
        // first-launch defaults only.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(transfers::TransfersManager::default())
        .invoke_handler(tauri::generate_handler![
            ping,
            tick_ack,
            fullscreen_state,
            transfers::native_available,
            transfers::connect_native,
            transfers::swarm_start_download,
        ])
        .setup(|app| {
            // Name the app after itself, not after its binary.
            #[cfg(target_os = "macos")]
            {
                let menu = build_menu(app.handle())?;
                app.set_menu(menu)?;
            }
            // Build the main window in Rust so it carries the native-bridge init
            // script (config `app.windows` is empty so this is the only window).
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("RabbitHole")
                .inner_size(1100.0, 760.0)
                .min_inner_size(720.0, 480.0)
                .initialization_script(NATIVE_SHIM);
            // On macOS the app's own header becomes the title bar: the system
            // bar is drawn as a transparent overlay, so the traffic lights float
            // over our chrome instead of sitting in a separate grey strip above
            // it. This is what every Mac app of this shape does, and the strip
            // is the single loudest "this is a web page in a box" tell.
            //
            // The SPA holds up its end: `.rh-app.native` reserves room for the
            // lights and marks the header as a drag region.
            #[cfg(target_os = "macos")]
            let win = win
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            let window = win.build()?;
            // Fullscreen reclaims the title strip: the traffic lights slide
            // away, so the 1.75rem clearance would become a dead band. There
            // is no fullscreen event as such — Resized fires on the
            // transition, and is_fullscreen() is a cheap attribute read.
            {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::Resized(_)) {
                        let fs = w.is_fullscreen().unwrap_or(false);
                        let _ = w.emit("rh://fullscreen", fs);
                    }
                });
            }

            // Settings… (⌘,) is a *menu* action with a web destination: the
            // shell tells the SPA where to go rather than owning a second
            // settings surface that would drift from it.
            {
                let handle = app.handle().clone();
                app.on_menu_event(move |_app, event| {
                    if event.id() == "settings" {
                        let _ = handle.emit("rh://navigate", "/settings");
                    }
                });
            }

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
