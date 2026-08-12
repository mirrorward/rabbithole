//! Native (Tauri desktop) bridge — Slice 5b of the swarm backend.
//!
//! Detects the desktop shell and routes file downloads to the **in-process swarm
//! core** over the `window.__RH_NATIVE__` IPC bridge (`rabbithole-desktop`,
//! Slice 3) instead of the WebSocket transport — so a download pulls chunks from
//! many peers at once. Progress arrives as `swarm://event` and folds into the
//! *same* [`crate::files::FilesState`] reducer the WS path uses (via
//! [`crate::wire::swarm_event_to_file_events`]), so the Transfers UI is identical.
//!
//! Wasm-only. On the plain web build [`native_available`] is `false` and none of
//! this runs; the download falls through to the WebSocket path.

use leptos::{SignalUpdate, SignalWithUntracked};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use crate::app::AppState;
use crate::wire::{swarm_event_to_file_events, SwarmWireEvent};

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
    let _ = js_sys::Reflect::set(&args, &JsValue::from_str("token"), &JsValue::from_str(token));
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

/// Invoke the native `swarm_start_download` command: fetch content `root_hex`
/// (`size` bytes) named `name` from the swarm. Fire-and-forget — progress is
/// delivered to the [`install_swarm_listener`] callback.
pub fn start_swarm_download(
    app: AppState,
    transfer_id: u64,
    root_hex: &str,
    size: u64,
    name: &str,
    max_sources: u32,
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
                            f.apply(&crate::wire::FileEvent::TransferFailed {
                                transfer_id,
                                detail,
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
    let tid = match ev {
        SwarmWireEvent::Opened { transfer_id, .. }
        | SwarmWireEvent::Chunk { transfer_id, .. }
        | SwarmWireEvent::Done { transfer_id, .. }
        | SwarmWireEvent::Failed { transfer_id, .. } => *transfer_id,
    };
    // The download's own session (seeded the Transfer at start). If it's gone
    // (session closed), drop the event rather than leak a phantom into whatever
    // burrow happens to be focused.
    let Some(files) = app.transfer_session_files(tid) else {
        return;
    };
    let size = files
        .with_untracked(|f| f.transfers.iter().find(|t| t.id == tid).map(|t| t.total))
        .unwrap_or(0);
    let events = swarm_event_to_file_events(ev, size);
    if !events.is_empty() {
        files.update(|f| {
            for fe in &events {
                f.apply(fe);
            }
        });
    }
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
