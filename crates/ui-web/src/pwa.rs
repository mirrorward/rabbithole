//! PWA install-readiness: the service-worker registration edge plus the
//! host-tested shape of the shell assets `trunk build` ships.
//!
//! The installable story is three checked-in pieces under `assets/`, copied
//! to the web root by `index.html`'s `data-trunk rel="copy-file"` links (the
//! embedded server then serves them at `/`; see `apps/server/src/http.rs` —
//! it generates `/manifest.webmanifest` only when the web root ships none,
//! so our checked-in manifest wins):
//!
//! - **`sw.js`** — the app-shell service worker: one version-stamped cache
//!   (`CACHE_VERSION`), runtime fetch-then-cache for trunk's content-hashed
//!   bundles (no precache list to drift), network-first navigations falling
//!   back to the cached shell document, a hard `/files/` bypass (downloads
//!   are never cached, so never stale), and explicit update activation.
//!   Plain JS, no external code; its load-bearing markers are asserted
//!   textually by the shape tests below — crude, but drift is visible.
//! - **`manifest.webmanifest`** — name/short_name "RabbitHole", standalone
//!   display, `start_url`/`scope` `/`, colours from the Clean-dark pack
//!   tokens ([`crate::packs::PackTokens`]), and two maskable icons.
//! - **`icon-192.png` / `icon-512.png`** — rendered by [`icon_rgba`] (a
//!   rabbit-hole disc on the accent field) and written once by the
//!   `#[ignore]`d `regenerate_icons` test; a normal test run decodes the
//!   checked-in bytes and compares them against the generator, so the PNGs
//!   cannot drift from the code that describes them.
//!
//! Registration itself lives in [`PwaNotice`] (wasm only): it
//! feature-detects `navigator.serviceWorker` (absent on insecure contexts
//! and older browsers) and logs — never throws — on failure, so the app
//! boots identically with or without a worker.

/// URL the service worker is registered under. It must sit at the web-root
/// top level: a worker's default scope is its own directory, and ours has
/// to cover the whole app (`/`).
pub const SW_URL: &str = "/sw.js";

/// URL of the shipped manifest — matches both the `<link rel="manifest">`
/// in `index.html` and the embedded server's generated-fallback path.
pub const MANIFEST_URL: &str = "/manifest.webmanifest";

/// The icon field colour: Clean-dark `--rh-accent`. Cross-checked against
/// the pack tokens by `icon_and_manifest_colours_match_the_default_pack`.
const ICON_FIELD: [u8; 3] = [0x6c, 0x9c, 0xff];

/// The rabbit hole itself: Clean-dark `--rh-bg`.
const ICON_HOLE: [u8; 3] = [0x14, 0x16, 0x1b];

/// The hole's radius as a fraction of the icon edge. Maskable icons must
/// keep their motif inside the safe zone — a centred circle of 40% radius —
/// so 30% survives every platform mask (circle, squircle, rounded square).
const HOLE_RADIUS: f64 = 0.30;

/// Render the install icon: `size` × `size` fully opaque RGBA pixels — a
/// dark rabbit-hole disc centred on the accent field, with a one-pixel
/// antialiased rim. Pure and deterministic, so the checked-in PNGs are
/// reproducible from this function alone (see the module notes).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(size as usize * size as usize * 4);
    let centre = (f64::from(size) - 1.0) / 2.0;
    let radius = f64::from(size) * HOLE_RADIUS;
    for y in 0..size {
        for x in 0..size {
            let dx = f64::from(x) - centre;
            let dy = f64::from(y) - centre;
            let dist = (dx * dx + dy * dy).sqrt();
            // 0 inside the hole, 1 on the field, blended across one pixel.
            let t = (dist - radius + 0.5).clamp(0.0, 1.0);
            for channel in 0..3 {
                let hole = f64::from(ICON_HOLE[channel]);
                let field = f64::from(ICON_FIELD[channel]);
                out.push((hole + (field - hole) * t).round() as u8);
            }
            out.push(0xff); // maskable icons must not rely on alpha
        }
    }
    out
}

/// Should the service worker exist in this context?
///
/// **Never in the native shell.** The worker is cache-first over the hashed
/// bundles — exactly right for a web deployment's offline shell, and exactly
/// wrong inside the desktop webview, where it pinned weeks-stale builds: the
/// app kept rendering an old bundle from CacheStorage across rebuilds and
/// relaunches, which surfaced to the user as long-fixed bugs (an 8px black
/// margin) "still there". The desktop app's assets are local; it has nothing
/// to be offline *from*.
pub fn sw_allowed(native: bool) -> bool {
    !native
}

/// Web-shell reachability is independent from every burrow's WebSocket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellStatus {
    #[default]
    Online,
    CachedOffline,
    Unreachable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PwaState {
    pub shell: ShellStatus,
    pub update_available: bool,
    pub applying: bool,
    pub error: bool,
}

impl PwaState {
    pub fn checked(&mut self, reachable: bool, update: bool, loaded_from_cache: bool) {
        self.shell = if reachable {
            ShellStatus::Online
        } else if loaded_from_cache {
            ShellStatus::CachedOffline
        } else {
            ShellStatus::Unreachable
        };
        self.update_available |= update;
    }

    pub fn begin_reload(&mut self) -> bool {
        if !self.update_available || self.applying {
            return false;
        }
        self.applying = true;
        self.error = false;
        true
    }
}

/// The build actually executing in this document, derived from Trunk's
/// content-hashed module URL. Package versions alone do not identify builds.
pub fn document_build(module: &str) -> Option<String> {
    let start = module.find("/rabbithole-ui-web-")?;
    let tail = &module[start..];
    let end = tail.find(".js")? + 3;
    Some(tail[..end].to_string())
}

/// A compact, non-modal web-only notice, within the app's layout. No update
/// or reachability event reloads a document; that requires the named action.
#[leptos::component]
pub fn PwaNotice() -> impl leptos::IntoView {
    use leptos::*;
    let state = create_rw_signal(PwaState::default());
    #[cfg(target_arch = "wasm32")]
    if let Some(runtime) = browser::start(state) {
        on_cleanup(move || runtime.stop());
    }
    let update = move || state.with(|s| s.update_available);
    let offline = move || state.with(|s| s.shell != ShellStatus::Online);
    let reload = move |_| {
        #[cfg(target_arch = "wasm32")]
        browser::reload();
    };
    let retry = move |_| {
        #[cfg(target_arch = "wasm32")]
        browser::check();
    };
    view! {
        <Show when=move || update() || offline() fallback=|| ()>
            <aside class="rh-pwa-notice" aria-label="Application status">
                <div class="rh-pwa-copy" role="status" aria-live="polite">
                    <Show when=offline fallback=|| ()>
                        <p class="rh-pwa-offline">{move || match state.with(|s| s.shell) {
                            ShellStatus::CachedOffline => "Using a saved copy of RabbitHole. We’ll check for updates when the app is reachable.",
                            _ => "Can’t reach the app for updates. Your burrow connection is separate.",
                        }}</p>
                    </Show>
                    <Show when=update fallback=|| ()>
                        <p><strong>"An app update is ready."</strong>" Finish your work, then reload."</p>
                    </Show>
                    <Show when=move || state.with(|s| s.error) fallback=|| ()>
                        <p>"The update didn’t start. Please try again."</p>
                    </Show>
                </div>
                <div class="rh-pwa-actions">
                    <Show when=update fallback=|| ()>
                        <button class="rh-btn small" type="button" on:click=reload
                            disabled=move || state.with(|s| s.applying)>
                            {move || if state.with(|s| s.applying) { "Reloading…" } else { "Reload to update" }}
                        </button>
                    </Show>
                    <Show when=offline fallback=|| ()>
                        <button class="rh-btn ghost small" type="button" on:click=retry>"Check connection"</button>
                    </Show>
                </div>
            </aside>
        </Show>
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use leptos::{RwSignal, SignalUpdate};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};
    use wasm_bindgen_futures::{spawn_local, JsFuture};
    use web_sys::{
        Event, EventTarget, ServiceWorker, ServiceWorkerContainer, ServiceWorkerRegistration,
    };

    thread_local! {
        static CURRENT: RefCell<Option<std::rc::Weak<Runtime>>> = const { RefCell::new(None) };
    }

    struct Listener {
        target: EventTarget,
        name: String,
        callback: Closure<dyn FnMut(Event)>,
    }

    pub(super) struct Runtime {
        state: RwSignal<PwaState>,
        container: ServiceWorkerContainer,
        registration: RefCell<Option<ServiceWorkerRegistration>>,
        listeners: RefCell<Vec<Listener>>,
        interval: RefCell<Option<gloo_timers::callback::Interval>>,
        build: String,
        cached_document: bool,
        controlled: Cell<bool>,
        reload_requested: Cell<bool>,
        alive: Cell<bool>,
    }

    impl Runtime {
        fn listen(
            self: &Rc<Self>,
            target: &EventTarget,
            name: &str,
            handler: impl Fn(&Rc<Self>, Event) + 'static,
        ) {
            let weak = Rc::downgrade(self);
            let callback = Closure::wrap(Box::new(move |event: Event| {
                if let Some(this) = weak.upgrade().filter(|this| this.alive.get()) {
                    handler(&this, event);
                }
            }) as Box<dyn FnMut(Event)>);
            if target
                .add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())
                .is_ok()
            {
                self.listeners.borrow_mut().push(Listener {
                    target: target.clone(),
                    name: name.into(),
                    callback,
                });
            }
        }

        fn announce_waiting(&self) {
            if self.container.controller().is_some()
                && self
                    .registration
                    .borrow()
                    .as_ref()
                    .is_some_and(|r| r.waiting().is_some())
            {
                self.state.update(|s| s.update_available = true);
            }
        }

        fn watch_installing(self: &Rc<Self>) {
            let worker = self
                .registration
                .borrow()
                .as_ref()
                .and_then(|r| r.installing());
            if let Some(worker) = worker {
                self.listen(worker.as_ref(), "statechange", |this, _| {
                    this.announce_waiting()
                });
            }
            self.announce_waiting();
        }

        fn check(&self) {
            if let Some(controller) = self.container.controller() {
                let message = serde_json::json!({ "type": "RH_PWA_CHECK", "build": self.build });
                let _ = controller.post_message(&JsValue::from_str(&message.to_string()));
            }
            if let Some(registration) = self.registration.borrow().as_ref() {
                if let Ok(promise) = registration.update() {
                    spawn_local(async move {
                        let _ = JsFuture::from(promise).await;
                    });
                }
            }
        }

        pub(super) fn stop(&self) {
            self.alive.set(false);
            self.interval.borrow_mut().take();
            for listener in self.listeners.borrow_mut().drain(..) {
                let _ = listener.target.remove_event_listener_with_callback(
                    &listener.name,
                    listener.callback.as_ref().unchecked_ref(),
                );
            }
        }
    }

    pub(super) fn start(state: RwSignal<PwaState>) -> Option<Rc<Runtime>> {
        let window = web_sys::window()?;
        if !sw_allowed(crate::native::native_available()) {
            purge_service_worker(&window);
            return None;
        }
        let navigator = window.navigator();
        if !js_sys::Reflect::has(navigator.as_ref(), &JsValue::from_str("serviceWorker"))
            .unwrap_or(false)
        {
            return None;
        }
        let container = navigator.service_worker();
        let cached_document =
            js_sys::Reflect::get(window.as_ref(), &JsValue::from_str("__RH_CACHED_SHELL__"))
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let build = window
            .document()
            .and_then(|d| d.query_selector("script[type=module]").ok().flatten())
            .and_then(|script| script.text_content())
            .and_then(|module| document_build(&module))
            .unwrap_or_default();
        if cached_document {
            state.update(|s| s.shell = ShellStatus::CachedOffline);
        }
        let this = Rc::new(Runtime {
            state,
            controlled: Cell::new(container.controller().is_some()),
            container,
            registration: RefCell::new(None),
            listeners: RefCell::new(vec![]),
            interval: RefCell::new(None),
            build,
            cached_document,
            reload_requested: Cell::new(false),
            alive: Cell::new(true),
        });
        CURRENT.with(|current| *current.borrow_mut() = Some(Rc::downgrade(&this)));
        this.listen(this.container.as_ref(), "message", |this, event| {
            let Some(event) = event.dyn_ref::<web_sys::MessageEvent>() else {
                return;
            };
            // Only our service worker may report shell provenance/reachability.
            let source = js_sys::Reflect::get(event.as_ref(), &JsValue::from_str("source"))
                .ok()
                .and_then(|source| source.dyn_into::<ServiceWorker>().ok());
            if !source.is_some_and(|source| source.script_url().ends_with(SW_URL)) {
                return;
            }
            let Some(data) = event.data().as_string() else {
                return;
            };
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&data) else {
                return;
            };
            if message["type"] != "RH_PWA_STATUS" {
                return;
            }
            let Some(reachable) = message["reachable"].as_bool() else {
                return;
            };
            this.state.update(|s| {
                s.checked(
                    reachable,
                    message["update"].as_bool().unwrap_or(false),
                    this.cached_document,
                )
            });
            this.announce_waiting();
        });
        this.listen(this.container.as_ref(), "controllerchange", |this, _| {
            let was_controlled = this.controlled.replace(true);
            if this.reload_requested.replace(false) {
                if let Some(window) = web_sys::window() {
                    if window.location().reload().is_err() {
                        this.state.update(|s| {
                            s.applying = false;
                            s.error = true;
                        });
                    }
                }
            } else {
                // Another tab may accept an update; this tab's draft stays put.
                if was_controlled {
                    this.state.update(|s| s.update_available = true);
                }
                this.check();
            }
        });
        for event in ["online", "offline", "focus"] {
            this.listen(window.as_ref(), event, |this, _| this.check());
        }
        let weak = Rc::downgrade(&this);
        *this.interval.borrow_mut() =
            Some(gloo_timers::callback::Interval::new(60_000, move || {
                if let Some(this) = weak.upgrade().filter(|this| this.alive.get()) {
                    this.check();
                }
            }));
        let weak = Rc::downgrade(&this);
        let promise = this.container.register(SW_URL);
        spawn_local(async move {
            let registration = JsFuture::from(promise)
                .await
                .ok()
                .and_then(|value| value.dyn_into::<ServiceWorkerRegistration>().ok());
            if let (Some(this), Some(registration)) =
                (weak.upgrade().filter(|this| this.alive.get()), registration)
            {
                *this.registration.borrow_mut() = Some(registration.clone());
                this.listen(registration.as_ref(), "updatefound", |this, _| {
                    this.watch_installing()
                });
                this.watch_installing();
                this.check();
            }
        });
        this.check();
        Some(this)
    }

    pub(super) fn check() {
        CURRENT.with(|current| {
            if let Some(this) = current.borrow().as_ref().and_then(|weak| weak.upgrade()) {
                this.check();
            }
        });
    }

    pub(super) fn reload() {
        CURRENT.with(|current| {
            let Some(this) = current.borrow().as_ref().and_then(|weak| weak.upgrade()) else {
                return;
            };
            let mut accepted = false;
            this.state.update(|s| accepted = s.begin_reload());
            if !accepted {
                return;
            }
            let waiting = this
                .registration
                .borrow()
                .as_ref()
                .and_then(|r| r.waiting());
            if let Some(waiting) = waiting {
                this.reload_requested.set(true);
                let _ = waiting.post_message(&JsValue::from_str(r#"{"type":"RH_PWA_ACTIVATE"}"#));
                let weak = Rc::downgrade(&this);
                leptos::set_timeout(
                    move || {
                        if let Some(this) = weak.upgrade().filter(|this| this.alive.get()) {
                            if this.reload_requested.replace(false) {
                                this.state.update(|s| {
                                    s.applying = false;
                                    s.error = true;
                                });
                            }
                        }
                    },
                    std::time::Duration::from_secs(10),
                );
            } else if let Some(window) = web_sys::window() {
                if window.location().reload().is_err() {
                    this.state.update(|s| {
                        s.applying = false;
                        s.error = true;
                    });
                }
            }
        });
    }
}

/// Best-effort teardown: unregister every service worker and delete every
/// cache for this origin. `Reflect`-based so it needs no new `web-sys`
/// features, and every step tolerates absence — a webview with no worker and
/// no caches is already in the desired state.
#[cfg(target_arch = "wasm32")]
fn purge_service_worker(window: &web_sys::Window) {
    use js_sys::{Array, Function, Promise, Reflect};
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    let window = window.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let call0 = |target: &JsValue, name: &str| -> Option<Promise> {
            let f = Reflect::get(target, &JsValue::from_str(name)).ok()?;
            let f: Function = f.dyn_into().ok()?;
            f.call0(target).ok()?.dyn_into().ok()
        };
        use wasm_bindgen::JsCast;
        // navigator.serviceWorker.getRegistrations() -> each .unregister()
        let nav = JsValue::from(window.navigator());
        if let Ok(sw) = Reflect::get(&nav, &JsValue::from_str("serviceWorker")) {
            if !sw.is_undefined() {
                if let Some(p) = call0(&sw, "getRegistrations") {
                    if let Ok(regs) = JsFuture::from(p).await {
                        for reg in Array::from(&regs).iter() {
                            if let Some(p) = call0(&reg, "unregister") {
                                let _ = JsFuture::from(p).await;
                            }
                        }
                    }
                }
            }
        }
        // caches.keys() -> each caches.delete(name)
        if let Ok(caches) = Reflect::get(&window, &JsValue::from_str("caches")) {
            if !caches.is_undefined() {
                if let Some(p) = call0(&caches, "keys") {
                    if let Ok(keys) = JsFuture::from(p).await {
                        for key in Array::from(&keys).iter() {
                            let del = Reflect::get(&caches, &JsValue::from_str("delete"))
                                .ok()
                                .and_then(|f| f.dyn_into::<Function>().ok())
                                .and_then(|f| f.call1(&caches, &key).ok())
                                .and_then(|p| p.dyn_into::<Promise>().ok());
                            if let Some(p) = del {
                                let _ = JsFuture::from(p).await;
                            }
                        }
                    }
                }
            }
        }
        leptos::logging::log!("[rh-pwa] native shell: service worker + caches purged");
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_native_shell_never_gets_a_service_worker() {
        // The worker is cache-first over hashed bundles. In the desktop
        // webview that pinned weeks-stale builds across relaunches — every
        // "this bug is still there" report during the black-margin hunt was
        // this. Web deployments keep it; the shell must not.
        assert!(super::sw_allowed(false), "web keeps its offline shell");
        assert!(
            !super::sw_allowed(true),
            "the native shell must never cache-first itself"
        );
    }

    use super::*;
    use crate::packs::PackTokens;

    #[test]
    fn only_actual_cached_navigation_is_reported_as_cached_offline() {
        let mut state = PwaState::default();
        state.checked(false, false, false);
        assert_eq!(state.shell, ShellStatus::Unreachable);
        state.checked(false, false, true);
        assert_eq!(state.shell, ShellStatus::CachedOffline);
        state.checked(true, false, true);
        assert_eq!(state.shell, ShellStatus::Online);
        assert!(!state.applying, "recovery never reloads a document");
    }

    #[test]
    fn updates_require_an_explicit_single_reload_and_survive_offline_checks() {
        let mut state = PwaState::default();
        assert!(!state.begin_reload());
        state.checked(true, true, false);
        assert!(state.update_available);
        assert!(!state.applying, "update discovery cannot discard a draft");
        state.checked(false, false, false);
        assert!(state.update_available);
        assert!(state.begin_reload());
        assert!(!state.begin_reload(), "double activation is ignored");
    }

    #[test]
    fn build_identity_comes_from_the_executing_bundle_not_package_version() {
        assert_eq!(
            document_build("import init from '/rabbithole-ui-web-abcdef123.js';"),
            Some("/rabbithole-ui-web-abcdef123.js".into())
        );
        assert_eq!(document_build("unrelated script"), None);
    }

    /// The checked-in shell assets, read at compile time so the tests always
    /// see exactly what trunk will copy.
    const SW_JS: &str = include_str!("../assets/sw.js");
    const MANIFEST: &str = include_str!("../assets/manifest.webmanifest");
    const INDEX_HTML: &str = include_str!("../index.html");

    const ICON_SIZES: [u32; 2] = [192, 512];

    fn icon_path(size: u32) -> String {
        format!("{}/assets/icon-{size}.png", env!("CARGO_MANIFEST_DIR"))
    }

    fn hex(c: [u8; 3]) -> String {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    }

    // ---- manifest shape ---------------------------------------------------

    #[test]
    fn manifest_is_installable() {
        let m: serde_json::Value = serde_json::from_str(MANIFEST).expect("valid JSON");
        assert_eq!(m["name"], "RabbitHole");
        assert_eq!(m["short_name"], "RabbitHole");
        assert_eq!(m["display"], "standalone");
        assert_eq!(m["start_url"], "/");
        assert_eq!(m["scope"], "/");
        let icons = m["icons"].as_array().expect("icons array");
        assert!(icons.len() >= 2, "install prompts want at least two icons");
        let mut sizes: Vec<&str> = Vec::new();
        for icon in icons {
            let src = icon["src"].as_str().expect("icon src");
            assert!(src.starts_with('/'), "icon src should be root-relative");
            assert_eq!(icon["type"], "image/png");
            let purpose = icon["purpose"].as_str().expect("icon purpose");
            assert!(purpose.contains("maskable"), "icons must be maskable");
            sizes.push(icon["sizes"].as_str().expect("icon sizes"));
        }
        assert!(sizes.contains(&"192x192") && sizes.contains(&"512x512"));
    }

    #[test]
    fn icon_and_manifest_colours_match_the_default_pack() {
        // The manifest colours and the icon palette are the Clean-dark pack
        // tokens; if the pack ever changes, this pins the drift.
        let clean_dark = PackTokens::default().dark;
        let m: serde_json::Value = serde_json::from_str(MANIFEST).expect("valid JSON");
        assert_eq!(m["background_color"], clean_dark["--rh-bg"].as_str());
        assert_eq!(m["theme_color"], clean_dark["--rh-accent"].as_str());
        assert_eq!(hex(ICON_FIELD), clean_dark["--rh-accent"]);
        assert_eq!(hex(ICON_HOLE), clean_dark["--rh-bg"]);
    }

    /// A bundle the browser refuses — a cached copy that no longer matches
    /// this document's integrity hash is the usual way — leaves a blank
    /// window that says nothing and does not mend itself on a reload,
    /// because the same cached copy comes back. The shell watches for the
    /// failure and clears what is stored, once per visit.
    #[test]
    fn the_shell_mends_itself_when_the_bundle_will_not_load() {
        // The loader is a module, so a refusal is a failed import, not a
        // resource error on a tag: both are watched for, and so is an app
        // that simply never starts.
        for signal in [
            r#"addEventListener("error""#,
            r#"addEventListener("unhandledrejection""#,
            "TrunkApplicationStarted",
            "!document.body.firstChild",
        ] {
            assert!(INDEX_HTML.contains(signal), "the shell must watch {signal}");
        }
        // Scoped to this app's own bundle: a font or an icon that fails is
        // not a reason to throw away everything and reload.
        assert!(INDEX_HTML.contains(r#"url.indexOf("/rabbithole-ui-web-")"#));
        // Once per visit, or a bundle that is genuinely gone would loop.
        assert!(INDEX_HTML.contains("rh-shell-reset"));
        // And what it clears is the worker and its caches, then it reloads.
        for step in [
            "getRegistrations()",
            "r.unregister()",
            "caches.keys()",
            "caches.delete(k)",
            "location.reload()",
        ] {
            assert!(INDEX_HTML.contains(step), "the shell must {step}");
        }
    }

    // ---- sw.js shape (textual, deliberately crude) ------------------------

    #[test]
    fn sw_js_declares_a_version_stamped_cache() {
        let line = SW_JS
            .lines()
            .find(|l| l.starts_with("const CACHE_VERSION"))
            .expect("sw.js must declare CACHE_VERSION");
        let value = line
            .split('"')
            .nth(1)
            .expect("CACHE_VERSION must be a string literal");
        assert!(
            value.starts_with("rabbithole-shell-v"),
            "cache version should be recognisably ours and versioned: {value}"
        );
        assert!(
            value.len() > "rabbithole-shell-v".len(),
            "cache version needs an actual version suffix"
        );
    }

    #[test]
    fn sw_js_bypasses_files_and_retains_assets_for_open_tabs() {
        // The /files/ bypass marker: downloads must never be cached.
        assert!(
            SW_JS.contains(r#"const FILES_PREFIX = "/files/";"#),
            "sw.js lost its /files/ bypass constant"
        );
        assert!(SW_JS.contains("FILES_PREFIX"), "bypass must be used");
        // Old open tabs keep their hashed bundles, including offline.
        assert!(!SW_JS.contains("caches.delete("));
        assert!(SW_JS.contains("clients.claim"));
        assert!(SW_JS.contains("skipWaiting"));
        // The three lifecycle hooks exist.
        for event in ["install", "activate", "fetch"] {
            assert!(
                SW_JS.contains(&format!("addEventListener(\"{event}\"")),
                "sw.js must handle the {event} event"
            );
        }
        // Navigation fallback + same-origin discipline markers.
        assert!(SW_JS.contains(r#"request.mode === "navigate""#));
        assert!(SW_JS.contains("self.location.origin"));
    }

    // ---- index.html trunk wiring -------------------------------------------

    #[test]
    fn index_html_wires_the_pwa_through_trunk() {
        // The rust link builds this crate's bin; the copy-file links land the
        // shell assets at the dist root, where SW_URL/MANIFEST_URL expect them.
        // Matched per attribute rather than as one literal: the tag spans
        // several lines and a reformat is not a regression.
        assert!(INDEX_HTML.contains("data-trunk"));
        assert!(INDEX_HTML.contains(r#"rel="rust""#));
        assert!(INDEX_HTML.contains(r#"data-bin="rabbithole-ui-web""#));
        // Size-first Binaryen pass on `trunk build --release` (debug skips it).
        assert!(
            INDEX_HTML.contains(r#"data-wasm-opt="z""#),
            "release SPA builds should run wasm-opt -Oz"
        );
        // Load-bearing, not cosmetic: wasm-opt validates against the MVP
        // feature set by default and rejects what the toolchain now emits, so
        // without these flags `trunk build --release` fails outright and there
        // is no release SPA at all.
        for feature in [
            "--enable-bulk-memory-opt",
            "--enable-nontrapping-float-to-int",
            "--enable-sign-ext",
            "--enable-multivalue",
            "--enable-reference-types",
            "--enable-mutable-globals",
        ] {
            assert!(
                INDEX_HTML.contains(feature),
                "release builds need wasm-opt {feature}"
            );
        }
        for asset in [
            "assets/sw.js",
            "assets/manifest.webmanifest",
            "assets/icon-192.png",
            "assets/icon-512.png",
        ] {
            assert!(
                INDEX_HTML.contains(&format!(r#"data-trunk rel="copy-file" href="{asset}""#)),
                "index.html must trunk-copy {asset}"
            );
        }
        assert!(INDEX_HTML.contains(&format!(r#"<link rel="manifest" href="{MANIFEST_URL}""#)));
        assert!(INDEX_HTML.contains(r#"<meta name="theme-color""#));
    }

    #[test]
    fn urls_are_root_scoped() {
        // Both URLs live at the web-root top level: the worker so its scope
        // covers "/", the manifest so it shadows the server's generated one.
        for url in [SW_URL, MANIFEST_URL] {
            assert!(url.starts_with('/'));
            assert!(!url.trim_start_matches('/').contains('/'));
        }
        assert_eq!(SW_URL, "/sw.js");
        assert_eq!(MANIFEST_URL, "/manifest.webmanifest");
    }

    // ---- icons --------------------------------------------------------------

    #[test]
    fn icon_rgba_paints_a_hole_on_the_accent_field() {
        for size in ICON_SIZES {
            let px = icon_rgba(size);
            assert_eq!(px.len(), size as usize * size as usize * 4);
            // Every pixel fully opaque: maskable icons must not rely on alpha.
            assert!(px.chunks_exact(4).all(|p| p[3] == 0xff), "{size} opaque");
            // The corner is the untouched accent field...
            assert_eq!(px[..3], ICON_FIELD, "{size} corner");
            // ...and the centre is the hole.
            let centre = ((size / 2) * size + size / 2) as usize * 4;
            assert_eq!(px[centre..centre + 3], ICON_HOLE, "{size} centre");
        }
    }

    #[test]
    fn checked_in_icons_match_the_generator() {
        // Decode (not byte-compare) so a png-crate encoder change can't fail
        // this; only a real pixel drift between assets/ and icon_rgba can.
        for size in ICON_SIZES {
            let path = icon_path(size);
            let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{path}: {e}; regenerate with `cargo test -p rabbithole-ui-web \
                     regenerate_icons -- --ignored`"
                )
            });
            let decoder = png::Decoder::new(bytes.as_slice());
            let mut reader = decoder.read_info().expect("valid PNG");
            let mut buf = vec![0u8; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).expect("decodable PNG");
            assert_eq!((info.width, info.height), (size, size));
            assert_eq!(info.color_type, png::ColorType::Rgba);
            assert_eq!(info.bit_depth, png::BitDepth::Eight);
            buf.truncate(info.buffer_size());
            assert_eq!(buf, icon_rgba(size), "icon-{size}.png drifted");
        }
    }

    /// Regenerate the checked-in icons from [`icon_rgba`]. `#[ignore]`d so a
    /// normal test run never writes into the tree; run explicitly after
    /// changing the generator — `checked_in_icons_match_the_generator` keeps
    /// the outputs honest in every normal run.
    #[test]
    #[ignore = "writes into assets/; run explicitly to regenerate the icons"]
    fn regenerate_icons() {
        for size in ICON_SIZES {
            let mut out = Vec::new();
            let mut encoder = png::Encoder::new(&mut out, size, size);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer.write_image_data(&icon_rgba(size)).expect("PNG data");
            writer.finish().expect("PNG finish");
            std::fs::write(icon_path(size), out).expect("write icon");
        }
    }
}
