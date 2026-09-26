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
//! Native menus use the same SPA destinations and history as the web client.
//! The bridge also keeps fullscreen chrome and navigation availability in sync.

/// The Tauri command + event surface wrapping the swarm core.
pub mod downloads;
pub mod seeding;
/// Source discovery + multi-source swarm download orchestration (Tauri-free).
pub mod swarm;
pub mod transfers;

/// Injected before the SPA loads: the minimal bridge, native navigation, and
/// fullscreen state. The auxiliary About window only needs build metadata.
const NATIVE_SHIM: &str = include_str!("native-shim.js");

fn native_shim(main_window: bool) -> String {
    NATIVE_SHIM
        .replace("__RH_VERSION__", env!("RH_VERSION"))
        .replace("__RH_SHA__", env!("RH_GIT_SHA"))
        .replace(
            "__RH_MAIN_WINDOW__",
            if main_window { "true" } else { "false" },
        )
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

/// The webview asks for the CURRENT fullscreen state at startup. The
/// `rh://fullscreen` events only fire on transitions (inside the Resized
/// handler), so a window restored fullscreen at launch — or a webview reload
/// while fullscreen, which resets <html> classes — would otherwise keep the
/// title strip's dead band until the user happened to resize.
#[tauri::command]
fn fullscreen_state(window: tauri::WebviewWindow) -> bool {
    window.is_fullscreen().unwrap_or(false)
}

/// Only the main webview owns the native history affordances.
#[tauri::command]
fn navigation_state(window: tauri::WebviewWindow, back: bool, forward: bool) {
    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        if window.label() != "main" {
            return;
        }
        if let Some(menu) = window.app_handle().menu() {
            if let Some(go) = menu.get("go").and_then(|item| item.as_submenu().cloned()) {
                for (id, enabled) in [("back", back), ("forward", forward)] {
                    if let Some(item) = go.get(id).and_then(|item| item.as_menuitem().cloned()) {
                        let _ = item.set_enabled(enabled);
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, back, forward);
}

/// The order and accelerators match the shared SPA's burrow sidebar.
#[cfg(target_os = "macos")]
const BURROW_MENU_ROUTES: &[(&str, &str)] = &[
    ("Lobby", "/lobby"),
    ("Boards", "/boards"),
    ("DMs", "/dms"),
    ("Directory", "/directory"),
    ("Files", "/files"),
    ("Radio", "/radio"),
    ("Art", "/art"),
    ("Wishes", "/wishing-well"),
];

/// The macOS application menu.
///
/// Keep standard editing/window actions alongside visible destinations. Menu
/// accelerators match the SPA shortcuts; no extra global key listener is needed.
#[cfg(target_os = "macos")]
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};

    let settings = MenuItemBuilder::with_id("/settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
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

    let appearance = MenuItemBuilder::with_id("/settings#appearance", "Appearance…").build(app)?;
    let view_menu = SubmenuBuilder::new(app, "View")
        .item(&appearance)
        .separator()
        .item(&PredefinedMenuItem::fullscreen(app, None)?)
        .build()?;

    let back = MenuItemBuilder::with_id("back", "Back")
        .accelerator("CmdOrCtrl+[")
        .enabled(false)
        .build(app)?;
    let forward = MenuItemBuilder::with_id("forward", "Forward")
        .accelerator("CmdOrCtrl+]")
        .enabled(false)
        .build(app)?;
    let mut go_menu = SubmenuBuilder::with_id(app, "go", "Go")
        .item(&back)
        .item(&forward)
        .separator();
    for (index, (label, route)) in BURROW_MENU_ROUTES.iter().enumerate() {
        let item = MenuItemBuilder::with_id(*route, *label)
            .accelerator(format!("CmdOrCtrl+{}", index + 1))
            .build(app)?;
        go_menu = go_menu.item(&item);
    }
    go_menu = go_menu.separator();
    for (label, route) in [
        ("You", "/you"),
        ("People", "/people"),
        ("Transfers", "/transfers"),
        ("Servers", "/servers"),
    ] {
        go_menu = go_menu.item(&MenuItemBuilder::with_id(route, label).build(app)?);
    }
    let go_menu = go_menu.build()?;

    let help_menu = SubmenuBuilder::new(app, "Help")
        .item(&MenuItemBuilder::with_id("help-servers", "Find a Server…").build(app)?)
        .item(&MenuItemBuilder::with_id("help-settings", "Customize RabbitHole…").build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("help-about", "About RabbitHole").build(app)?)
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, None)?)
        .build()?;

    MenuBuilder::new(app)
        .item(&app_menu)
        .item(&edit_menu)
        .item(&view_menu)
        .item(&go_menu)
        .item(&window_menu)
        .item(&help_menu)
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
            native_shim(false)
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
        .plugin(tauri_plugin_window_state::Builder::default().with_denylist(&["about"]).build())
        // Native save and folder panels, called from Rust only.
        .plugin(tauri_plugin_dialog::init())
        .manage(transfers::TransfersManager::default())
        .invoke_handler(tauri::generate_handler![
            navigation_state,
            fullscreen_state,
            tracker_index,
            transfers::native_available,
            transfers::connect_native,
            transfers::swarm_start_download,
            transfers::swarm_cancel_download,
            transfers::save_file,
            transfers::download_prefs,
            transfers::choose_download_folder,
            transfers::clear_download_folder,
            transfers::set_per_burrow_folders,
            transfers::set_seeding,
        ])
        .setup(|app| {
            // Keep swarm adverts alive while anything is on offer (a no-op
            // until the person opts in to sharing their downloads).
            tauri::async_runtime::spawn(transfers::reannounce_loop(app.handle().clone()));
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
                .initialization_script(native_shim(true));
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

            // Navigation belongs to the main window. Broadcasting would also
            // replace the About document with a full app in its small window.
            app.on_menu_event(move |app, event| {
                use tauri::Manager;
                let id = event.id().0.as_str();
                if matches!(id, "about" | "help-about") {
                    open_about(app);
                    return;
                }
                let (event_name, payload) = match id {
                    "back" | "forward" => ("rh://history", id),
                    "help-servers" => ("rh://navigate", "/servers"),
                    "help-settings" => ("rh://navigate", "/settings#appearance"),
                    route if route.starts_with('/') => ("rh://navigate", route),
                    _ => return,
                };
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.show();
                    let _ = main.unminimize();
                    let _ = main.set_focus();
                    let _ = main.emit(event_name, payload);
                }
            });

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
