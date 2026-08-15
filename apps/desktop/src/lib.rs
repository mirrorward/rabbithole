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
  // Stamped at compile time from the workspace manifest + git SHA, so the
  // About window reports the build that is actually running.
  window.__RH_BUILD__ = { version: '__RH_VERSION__', sha: '__RH_SHA__' };
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

/// The init script with the build stamps substituted in. A constant can't
/// carry them, and the About window's whole job is reporting them accurately.
fn native_shim() -> String {
    NATIVE_SHIM
        .replace("__RH_VERSION__", env!("RH_VERSION"))
        .replace("__RH_SHA__", env!("RH_GIT_SHA"))
}

/// Fetch a Looking Glass tracker's `INDEX` listing over its status port.
///
/// The status port is a **line protocol over TCP** (one command line in,
/// tab-separated rows out) — not HTTP, so the webview cannot dial it and the
/// shell does the socket here. The reply is handed back verbatim for
/// `ui_web::servers::parse_tracker_index`; parsing stays in one place, tested
/// against the documented column layout.
///
/// Uses the same client as `rabbit-tui` ([`rabbithole_directory::fetch`]):
/// same host default, same timeouts, same size cap. An empty INDEX is a
/// listing of nobody and is returned as `Some("")`, not `None` — `None` is
/// only "we could not ask".
#[tauri::command]
async fn tracker_index() -> Option<String> {
    // A local `just up` glass first (127.0.0.1 + $RABBIT_TRACKER_STATUS /
    // .rabbithole/looking-glass-status / 5497). If nothing is listening,
    // the public glass — so a shipped app still finds tracker.rabbit.direct.
    match rabbithole_directory::fetch::query_tracker(&local_tracker_status_addr(), "INDEX").await {
        Ok(text) => Some(text),
        Err(_) => rabbithole_directory::fetch::query_tracker(&tracker_status_addr(), "INDEX")
            .await
            .ok(),
    }
}

/// Loopback INDEX — same address the TUI uses for a typed `localhost`.
fn local_tracker_status_addr() -> String {
    rabbithole_directory::status_addr("127.0.0.1")
}

/// Where the shell asks the **public** glass for `INDEX`.
fn tracker_status_addr() -> String {
    rabbithole_directory::fetch::tracker_addr(rabbithole_directory::TRACKER_HOST)
}

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
    use tauri::menu::{
        AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
    };

    let about = AboutMetadataBuilder::new()
        .name(Some("RabbitHole"))
        .version(Some(env!("RH_VERSION")))
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

    // Our own About window, not `PredefinedMenuItem::about`: the system panel
    // accepts an icon, a name, a version and a blob of credits text and
    // nothing else, which is precisely why it can never look like the app it
    // belongs to. The metadata above is still built, so the panel stays one
    // line away if the custom window ever fails to open.
    let _ = &about;
    let about_item = MenuItemBuilder::with_id("about", "About RabbitHole").build(app)?;
    let app_menu = SubmenuBuilder::new(app, "RabbitHole")
        .item(&about_item)
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

/// Open (or focus) the About window: a small, non-resizable webview showing
/// the SPA's `/about` route, so it wears the app's own theme and type.
fn open_about(app: &tauri::AppHandle) {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
    if let Some(w) = app.get_webview_window("about") {
        let _ = w.set_focus();
        return;
    }
    // The SPA is a *history* router, so a fragment (`index.html#/about`) lands
    // it on "/" and the window renders the whole app shell. Rewriting the path
    // in an init script — which runs at document start, before the SPA mounts —
    // means it boots directly into the About route, in dev (where the window
    // loads the dev server) and in a bundle alike.
    let builder = WebviewWindowBuilder::new(app, "about", WebviewUrl::App("index.html".into()))
        .title("About RabbitHole")
        .inner_size(420.0, 640.0)
        .resizable(false)
        .minimizable(false)
        .initialization_script(format!(
            "{}\nhistory.replaceState({{}}, '', '/about');",
            native_shim()
        ));
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Transparent);
    if let Err(e) = builder.build() {
        eprintln!("[rh-menu] about window failed to open: {e}");
    }
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
            tracker_index,
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
                .initialization_script(native_shim());
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
                    match event.id().0.as_str() {
                        "settings" => {
                            let _ = handle.emit("rh://navigate", "/settings");
                        }
                        "about" => open_about(&handle),
                        _ => {}
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

#[cfg(test)]
mod tests {
    fn first_quoted(text: &str, prefix: &str) -> Option<String> {
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix(prefix)
                .and_then(|rest| rest.split('"').nth(1))
                .map(str::to_string)
        })
    }

    #[test]
    fn the_crate_version_matches_the_product() {
        // Isolated workspace: cannot inherit version.workspace. CI also runs
        // scripts/check-desktop-version.sh; this catches `cargo test` here.
        let root = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"));
        let tauri = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json"));
        let workspace = first_quoted(root, "version = ").expect("workspace version");
        assert_eq!(
            env!("CARGO_PKG_VERSION"),
            workspace,
            "apps/desktop/Cargo.toml drifted from the product version"
        );
        let tauri_ver = first_quoted(tauri, "\"version\": ").expect("tauri version");
        assert_eq!(
            tauri_ver, workspace,
            "tauri.conf.json drifted from the product version"
        );
    }

    #[test]
    fn the_shell_asks_the_same_status_port_as_the_tui() {
        // Hardcoding `tracker.rabbit.direct:4655` here is how the two clients
        // drifted. The address comes from the shared crate, same as rabbit-tui.
        assert_eq!(
            super::tracker_status_addr(),
            rabbithole_directory::fetch::tracker_addr(rabbithole_directory::TRACKER_HOST)
        );
        assert_eq!(
            super::tracker_status_addr(),
            format!(
                "{}:{}",
                rabbithole_directory::TRACKER_HOST,
                rabbithole_directory::TRACKER_STATUS_PORT
            )
        );
        assert_eq!(
            super::local_tracker_status_addr(),
            rabbithole_directory::status_addr("127.0.0.1"),
            "loopback uses the same port the TUI appends for localhost"
        );
        assert_ne!(
            super::local_tracker_status_addr(),
            super::tracker_status_addr(),
            "a local just-up glass is not the public glass"
        );
    }
}
