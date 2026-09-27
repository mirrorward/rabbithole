//! Native (Tauri desktop) bridge — Slice 5b of the swarm backend.
//!
//! Detects the desktop shell and routes file downloads to the **in-process swarm
//! core** over the `window.__RH_NATIVE__` IPC bridge (`rabbithole-desktop`,
//! Slice 3) instead of the WebSocket transport — so a download pulls chunks from
//! many peers at once. Progress arrives as `swarm://event` and folds into the
//! [`crate::files::FilesState::apply_swarm_event`] reducer, preserving native
//! attempt identity and diagnostics in the shared Transfers UI.
//!
//! Wasm-only. On the plain web build [`native_available`] is `false` and none of
//! this runs; the download falls through to the WebSocket path.

use leptos::SignalUpdate;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use crate::app::AppState;
use crate::wire::SwarmWireEvent;

/// The `window.__RH_NATIVE__` bridge object, if the shell injected it.
fn bridge() -> Option<js_sys::Object> {
    let win = web_sys::window()?;
    js_sys::Reflect::get(&win, &JsValue::from_str("__RH_NATIVE__"))
        .ok()?
        .dyn_into::<js_sys::Object>()
        .ok()
}

/// A named function on the bridge object.
fn method(obj: &js_sys::Object, name: &str) -> Option<js_sys::Function> {
    js_sys::Reflect::get(obj, &JsValue::from_str(name))
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()
}

/// True only inside the native shell — the plain web build lacks the flag the
/// init script sets, so this is the runtime switch between transports.
pub fn native_available() -> bool {
    web_sys::window()
        .and_then(|w| js_sys::Reflect::get(&w, &JsValue::from_str("__RH_IS_NATIVE__")).ok())
        .map(|v| v.is_truthy())
        .unwrap_or(false)
}

/// Open the native core's own session to `endpoint` (the in-process RHP client
/// that swarm downloads run on). The webview's WebSocket session is separate —
/// the native core needs its own connection to ask "who has this blob?" and to
/// get a capability ticket. Called once per burrow after the SPA authenticates;
/// failures are non-fatal (downloads then fall back to the WS inline path).
pub fn connect_native(endpoint: &str, token: &str) {
    let Some(b) = bridge() else { return };
    let Some(invoke) = method(&b, "invoke") else {
        return;
    };
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("endpoint"),
        &JsValue::from_str(endpoint),
    );
    // The SPA dials over ws://, which needs no pinned cert fingerprint.
    let _ = js_sys::Reflect::set(&args, &JsValue::from_str("fingerprint"), &JsValue::NULL);
    // The resume token authenticates the native session onto the same account.
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("token"),
        &JsValue::from_str(token),
    );
    if let Ok(ret) = invoke.call2(&b, &JsValue::from_str("connect_native"), &args) {
        if let Ok(promise) = ret.dyn_into::<js_sys::Promise>() {
            spawn_local(async move {
                // Settle the promise; a failure just means swarm downloads are
                // unavailable for this burrow (the WS path still works).
                let _ = JsFuture::from(promise).await;
            });
        }
    }
}

/// Invoke the native `swarm_cancel_download` command: stop a download the
/// shell is running. Fire-and-forget — the row is marked by the failure
/// event the shell sends back, like any other ending.
pub fn cancel_swarm_download(transfer_id: u64) {
    let Some(b) = bridge() else { return };
    let Some(invoke) = method(&b, "invoke") else {
        return;
    };
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("transferId"),
        &JsValue::from_f64(transfer_id as f64),
    );
    if let Ok(ret) = invoke.call2(&b, &JsValue::from_str("swarm_cancel_download"), &args) {
        if let Ok(promise) = ret.dyn_into::<js_sys::Promise>() {
            spawn_local(async move {
                let _ = JsFuture::from(promise).await;
            });
        }
    }
}

/// Allocate an attempt in the native process, then seed the captured session
/// before its first progress event. Allocating in the shell prevents ID reuse
/// after a webview reload while an older native download is still running.
#[allow(clippy::too_many_arguments)]
pub fn queue_swarm_download(
    app: AppState,
    session_id: crate::app::ServerId,
    files: leptos::RwSignal<crate::files::FilesState>,
    root_hex: String,
    size: u64,
    name: String,
    max_sources: u32,
    burrow: String,
    node_id: i64,
    from: crate::settings::DownloadFrom,
) {
    let Some(b) = bridge() else { return };
    let Some(invoke) = method(&b, "invoke") else {
        return;
    };
    spawn_local(async move {
        let allocated = async {
            let returned = invoke.call2(
                &b,
                &JsValue::from_str("swarm_next_transfer_id"),
                &js_sys::Object::new(),
            )?;
            let value = JsFuture::from(returned.dyn_into::<js_sys::Promise>()?).await?;
            let id = value
                .as_f64()
                .filter(|n| {
                    n.is_finite()
                        && n.fract() == 0.0
                        && *n >= (1u64 << 52) as f64
                        && *n <= ((1u64 << 53) - 1) as f64
                })
                .ok_or_else(|| {
                    JsValue::from_str("The desktop app returned an invalid download ID.")
                })?;
            Ok::<u64, JsValue>(id as u64)
        }
        .await;
        // Closing and reconnecting to the same burrow creates another session;
        // this pending click belongs only to the signal captured above.
        if !app.owns_files_session(&session_id, files) {
            return;
        }
        match allocated {
            Ok(transfer_id) => {
                files.update(|f| {
                    f.start_native_download(
                        transfer_id,
                        node_id,
                        name.clone(),
                        size,
                        root_hex.clone(),
                    )
                });
                start_swarm_download(
                    app,
                    transfer_id,
                    &root_hex,
                    size,
                    &name,
                    max_sources,
                    &burrow,
                    node_id,
                    from,
                    &session_id.0,
                );
            }
            Err(error) => {
                app.notify(
                    crate::toasts::ToastKind::Warn,
                    error.as_string().unwrap_or_else(|| {
                        "The desktop app could not start the download. Try again.".to_string()
                    }),
                );
            }
        }
    });
}

/// Invoke the native `swarm_start_download` command: fetch content `root_hex`
/// (`size` bytes) named `name` from the swarm. Fire-and-forget — progress is
/// delivered to the [`install_swarm_listener`] callback.
// The arguments mirror the native IPC command.
#[allow(clippy::too_many_arguments)]
pub fn start_swarm_download(
    app: AppState,
    transfer_id: u64,
    root_hex: &str,
    size: u64,
    name: &str,
    max_sources: u32,
    burrow: &str,
    node_id: i64,
    from: crate::settings::DownloadFrom,
    endpoint: &str,
) {
    let Some(b) = bridge() else { return };
    let Some(invoke) = method(&b, "invoke") else {
        return;
    };
    // Tauri maps camelCase JS keys to the command's snake_case params.
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("transferId"),
        &JsValue::from_f64(transfer_id as f64),
    );
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("rootHex"),
        &JsValue::from_str(root_hex),
    );
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("size"),
        &JsValue::from_f64(size as f64),
    );
    let _ = js_sys::Reflect::set(&args, &JsValue::from_str("name"), &JsValue::from_str(name));
    // How many sources this fetch may use at once — the engine runs one worker
    // per source, so this is the parallelism the user chose in Settings.
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("maxSources"),
        &JsValue::from_f64(max_sources as f64),
    );
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("burrow"),
        &JsValue::from_str(burrow),
    );
    // Which burrow: the shell keeps a native session per burrow, and a node id
    // means something only on its own.
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("endpoint"),
        &JsValue::from_str(endpoint),
    );
    // The file's node, so the burrow itself can be asked when no peer has it,
    // and where the person wants it from (peers, the burrow, or best available).
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("nodeId"),
        &JsValue::from_f64(node_id as f64),
    );
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("mode"),
        &JsValue::from_str(from.wire()),
    );
    if let Ok(ret) = invoke.call2(&b, &JsValue::from_str("swarm_start_download"), &args) {
        if let Ok(promise) = ret.dyn_into::<js_sys::Promise>() {
            // Await the command; if it rejects (undeterminable size, no source,
            // connect failure) mark the seeded Transfer Failed so it doesn't hang
            // at 0% — routing to the session that owns it, not the focused one.
            spawn_local(async move {
                if let Err(err) = JsFuture::from(promise).await {
                    // The shell's own words, not a constant: the command
                    // rejects with the reason ("not connected to a burrow",
                    // "root must be 64 hex chars"…), and replacing it with
                    // "swarm download failed" was the last place the cause
                    // got thrown away.
                    let detail = err
                        .as_string()
                        .or_else(|| {
                            js_sys::Reflect::get(&err, &JsValue::from_str("message"))
                                .ok()
                                .and_then(|m| m.as_string())
                        })
                        .unwrap_or_else(|| "the download could not be started".to_string());
                    if let Some(files) = app.transfer_session_files(transfer_id) {
                        files.update(|f| {
                            f.apply_swarm_event(&SwarmWireEvent::Failed {
                                transfer_id,
                                reason: detail,
                                sources_tried: 0,
                                retryable: true,
                            })
                        });
                    }
                }
            });
        }
    }
}

/// Hand bytes the webview already holds to the shell, which writes them into
/// the downloads folder (a webview has no download manager to click into).
/// `done` gets the path it landed at, or the shell's own words for why not.
pub fn save_file(
    name: &str,
    bytes: &[u8],
    burrow: &str,
    done: impl FnOnce(Result<Option<String>, String>) + 'static,
) {
    let (Some(b), true) = (bridge(), native_available()) else {
        done(Err("the desktop shell is not available".to_string()));
        return;
    };
    let Some(invoke) = method(&b, "invoke") else {
        done(Err("the desktop shell is not available".to_string()));
        return;
    };
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &JsValue::from_str("name"), &JsValue::from_str(name));
    // Which burrow it came from, for the "a folder per burrow" preference. A
    // label only: the shell turns it into one safe folder name itself.
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("burrow"),
        &JsValue::from_str(burrow),
    );
    // Base64 rather than a JSON number array: a third the size on the bridge.
    let _ = js_sys::Reflect::set(
        &args,
        &JsValue::from_str("dataBase64"),
        &JsValue::from_str(&crate::wire::base64_encode(bytes)),
    );
    let Ok(ret) = invoke.call2(&b, &JsValue::from_str("save_file"), &args) else {
        done(Err("the save could not be started".to_string()));
        return;
    };
    let Ok(promise) = ret.dyn_into::<js_sys::Promise>() else {
        done(Err("the save could not be started".to_string()));
        return;
    };
    spawn_local(async move {
        match JsFuture::from(promise).await {
            // `null`: the person cancelled the save panel. Not a failure.
            Ok(path) => done(Ok(path.as_string())),
            Err(err) => done(Err(err
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&err, &JsValue::from_str("message"))
                        .ok()
                        .and_then(|m| m.as_string())
                })
                .unwrap_or_else(|| "the file could not be written".to_string()))),
        }
    });
}

use crate::save::DownloadPrefs;

fn prefs_from_js(v: &JsValue) -> Option<DownloadPrefs> {
    if v.is_null() || v.is_undefined() {
        return None;
    }
    let get = |k: &str| js_sys::Reflect::get(v, &JsValue::from_str(k)).ok();
    Some(DownloadPrefs {
        folder: get("folder").and_then(|f| f.as_string()),
        per_burrow: get("perBurrow").and_then(|b| b.as_bool()).unwrap_or(false),
        system_folder: get("systemFolder")
            .and_then(|f| f.as_string())
            .unwrap_or_default(),
        seed: get("seed").and_then(|b| b.as_bool()).unwrap_or(false),
        seeding_files: get("seedingFiles")
            .and_then(|n| n.as_f64())
            .map_or(0, |n| n as u32),
        seeding_note: get("seedingNote").and_then(|s| s.as_string()),
    })
}

/// Run one of the shell's download-preference commands and hand back the
/// preferences it answers with (`None` from "choose" means the panel was
/// cancelled and nothing changed).
fn prefs_command(
    command: &'static str,
    args: js_sys::Object,
    done: impl FnOnce(Result<Option<DownloadPrefs>, String>) + 'static,
) {
    let (Some(b), true) = (bridge(), native_available()) else {
        return;
    };
    let Some(invoke) = method(&b, "invoke") else {
        return;
    };
    let Ok(ret) = invoke.call2(&b, &JsValue::from_str(command), &args) else {
        return;
    };
    let Ok(promise) = ret.dyn_into::<js_sys::Promise>() else {
        return;
    };
    spawn_local(async move {
        match JsFuture::from(promise).await {
            Ok(v) => done(Ok(prefs_from_js(&v))),
            Err(err) => done(Err(err
                .as_string()
                .unwrap_or_else(|| "the setting could not be changed".to_string()))),
        }
    });
}

/// Read the download preferences.
pub fn download_prefs(done: impl FnOnce(Result<Option<DownloadPrefs>, String>) + 'static) {
    prefs_command("download_prefs", js_sys::Object::new(), done);
}

/// Open the native folder panel to choose the download folder.
pub fn choose_download_folder(done: impl FnOnce(Result<Option<DownloadPrefs>, String>) + 'static) {
    prefs_command("choose_download_folder", js_sys::Object::new(), done);
}

/// Go back to asking where each download goes.
pub fn clear_download_folder(done: impl FnOnce(Result<Option<DownloadPrefs>, String>) + 'static) {
    prefs_command("clear_download_folder", js_sys::Object::new(), done);
}

/// Turn a folder per burrow on or off.
pub fn set_per_burrow_folders(
    on: bool,
    done: impl FnOnce(Result<Option<DownloadPrefs>, String>) + 'static,
) {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &JsValue::from_str("on"), &JsValue::from_bool(on));
    prefs_command("set_per_burrow_folders", args, done);
}

/// Turn sharing downloads with a burrow's swarm on or off.
pub fn set_seeding(on: bool, done: impl FnOnce(Result<Option<DownloadPrefs>, String>) + 'static) {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &JsValue::from_str("on"), &JsValue::from_bool(on));
    prefs_command("set_seeding", args, done);
}

/// Install the `swarm://event` listener that folds native progress into the
/// focused session's Transfers. The callback `Closure` is `forget()`-leaked so it
/// lives for the app's lifetime (dropping it would silently kill progress).
/// Call once, after `AppState` is provided.
pub fn install_swarm_listener(app: AppState) {
    let Some(b) = bridge() else { return };
    let Some(listen) = method(&b, "listen") else {
        return;
    };
    let cb = Closure::wrap(Box::new(move |event: JsValue| {
        // event = { payload: { transfer_id, kind, ... } }
        let payload =
            js_sys::Reflect::get(&event, &JsValue::from_str("payload")).unwrap_or(JsValue::NULL);
        if let Some(json) = js_sys::JSON::stringify(&payload)
            .ok()
            .and_then(|s| s.as_string())
        {
            if let Ok(ev) = serde_json::from_str::<SwarmWireEvent>(&json) {
                apply_swarm_event(app, &ev);
            }
        }
    }) as Box<dyn FnMut(JsValue)>);
    let _ = listen.call2(
        &b,
        &JsValue::from_str("swarm://event"),
        cb.as_ref().unchecked_ref(),
    );
    cb.forget();
}

/// Fold one native event into the Transfers of the session that *started* this
/// download — resolved by transfer id, NOT the focused session (the user may
/// have switched burrows mid-transfer). The byte `size` comes from the Transfer
/// seeded when the download started (native events carry units, not bytes).
fn apply_swarm_event(app: AppState, ev: &SwarmWireEvent) {
    let Some(files) = app.transfer_session_files(ev.transfer_id()) else {
        return;
    };
    files.update(|f| f.apply_swarm_event(ev));
}

/// Ask the native core for a Looking Glass tracker's `INDEX` listing.
///
/// The tracker's status port is a **line protocol over TCP** — one command
/// line in, tab-separated rows out — which a webview cannot dial. The shell
/// does the socket in Rust and hands back the raw reply for
/// [`crate::servers::parse_tracker_index`]. `None` in a browser tab, where
/// there is no shell to ask: that isn't a failure, it's a capability the web
/// build doesn't have.
#[cfg(target_arch = "wasm32")]
pub async fn tracker_index() -> Option<String> {
    let b = bridge()?;
    let invoke = method(&b, "invoke")?;
    let args = js_sys::Object::new();
    let promise = invoke
        .call2(&b, &JsValue::from_str("tracker_index"), &args.into())
        .ok()?
        .dyn_into::<js_sys::Promise>()
        .ok()?;
    JsFuture::from(promise).await.ok()?.as_string()
}
