//! Slice 4b: the Tauri command + event surface wrapping the swarm core.
//!
//! The wasm SPA (Slice 5) invokes these over the `window.__RH_NATIVE__` bridge
//! and listens for `swarm://event` to drive its multi-source Transfers UI. The
//! command *bodies* are the already-tested [`crate::swarm::run_swarm_download`];
//! this layer is Tauri glue (managed state + serialization + event emission),
//! best exercised end-to-end with `cargo tauri dev` (see the design doc).

#![cfg_attr(rustfmt, rustfmt_skip)]

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use crate::downloads::{self, Destination, DownloadPrefs};
#[cfg(test)]
use crate::downloads::sanitize_name;

use rabbithole_core::Client;

use crate::swarm::{run_download, SourceMode, SwarmEvent, Wanted};

/// App-managed state: the native RHP sessions, one per burrow.
#[derive(Default)]
pub struct TransfersManager {
    /// The authenticated native session for each burrow, keyed by the endpoint
    /// the webview dialled. One per burrow, not one for the app: a file's node
    /// id means something only on its own burrow, and a single shared session
    /// (what this used to be) would ask whichever burrow connected last, which
    /// with two burrows joined is a coin toss, and with the origin fallback
    /// could fetch a *different file* that happens to share the id.
    ///
    /// `Client` is `!Sync`, so they live behind an async mutex; a fetch runs
    /// while the lock is held, so downloads serialize (concurrent downloads +
    /// mid-fetch abort are a later refinement, unblocked by splitting the
    /// find/ticket phase from the lock-free fetch).
    clients: Mutex<std::collections::HashMap<String, Client>>,
}

/// Authoritative "am I running inside the native shell?" signal. The wasm SPA
/// also runtime-checks `window.__RH_IS_NATIVE__`, but this command confirms the
/// backend is actually wired.
#[tauri::command]
pub fn native_available() -> bool {
    true
}

/// Open a native RHP session to `endpoint` (QUIC needs `fingerprint`; `ws://` /
/// `wss://` don't) and authenticate it, stored for subsequent swarm downloads.
///
/// The webview's WebSocket session and this in-process session are separate
/// connections; the swarm core needs its own authenticated session because
/// `swarm_find` / `swarm_ticket` are privileged. The caller passes the resume
/// `token` it received from its own `AuthOk`, so the native session lands on the
/// same account without re-prompting for a password.
#[tauri::command]
pub async fn connect_native(
    state: State<'_, TransfersManager>,
    endpoint: String,
    fingerprint: Option<String>,
    token: Option<String>,
) -> Result<(), String> {
    let mut client = Client::connect(
        &endpoint,
        None,
        fingerprint.as_deref(),
        "rabbithole-desktop",
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .map_err(|e| e.to_string())?;
    // Resume the caller's session so this client is authenticated. Without a
    // token (guest sessions aren't resumable) the swarm commands will fail
    // later with a clear error rather than silently misbehaving here.
    let authed = if let Some(token) = token.as_deref().filter(|t| !t.is_empty()) {
        client
            .auth_resume(token, 0)
            .await
            .map_err(|e| format!("native session resume failed: {e}"))?;
        client
            .expect_welcome()
            .await
            .map_err(|e| format!("native session welcome failed: {e}"))?;
        true
    } else {
        false
    };
    eprintln!(
        "[rh-swarm] native core connected to {endpoint} (authenticated: {authed}) — swarm downloads {}",
        if authed { "ready" } else { "unavailable (no resume token; guest?)" }
    );
    // Only a session that can actually ask for things is kept. A guest has no
    // resume token; the webview knows that too and downloads over its own
    // socket instead.
    let mut clients = state.clients.lock().await;
    if authed {
        clients.insert(endpoint, client);
    } else {
        clients.remove(&endpoint);
    }
    Ok(())
}

/// A `SwarmEvent` tagged with which transfer it belongs to — the `swarm://event`
/// payload the ui-web Transfers manager routes by `transfer_id`.
#[derive(Clone, serde::Serialize)]
struct TransferEvent {
    transfer_id: u64,
    #[serde(flatten)]
    event: SwarmEvent,
}

/// Fetch content `root_hex` (`size` bytes; `0` = derive from the source list)
/// from the swarm into the OS downloads directory as `name`, emitting
/// `swarm://event` progress tagged with `transfer_id` as each unit lands.
// Two of these are injected by Tauri; the rest are the command's wire
// shape, which the webview calls by name. Bundling them into a struct would
// change that call for no reader's benefit.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn swarm_start_download(
    app: AppHandle,
    state: State<'_, TransfersManager>,
    endpoint: String,
    transfer_id: u64,
    root_hex: String,
    size: u64,
    name: String,
    max_sources: u32,
    burrow: Option<String>,
    node_id: Option<i64>,
    mode: Option<String>,
) -> Result<(), String> {
    let root = parse_root(&root_hex)?;
    // Where it goes is settled before a byte moves: asking after the fetch
    // would download a file the person then declines to keep.
    let Some(dest) = resolve_destination(&app, burrow.as_deref().unwrap_or_default(), &name, true).await? else {
        return Err("Save cancelled.".to_string());
    };
    eprintln!("[rh-swarm] swarm_start_download: transfer={transfer_id} root={root_hex} size={size} name={name:?}");
    let mut guard = state.clients.lock().await;
    let client = guard
        .get_mut(&endpoint)
        .ok_or("the app has no signed-in session to that burrow")?;
    // A failure is reported to the webview as an event, not just returned:
    // the UI's transfer row is driven by the event stream, and an error that
    // only comes back through the invoke promise leaves that row saying
    // nothing about what happened.
    let emit_app = app.clone();
    // Peers when anyone has it, else the burrow itself, unless told otherwise.
    let mode = SourceMode::parse(mode.as_deref().unwrap_or("auto"));
    let want = Wanted { root, size, node_id, max_sources: max_sources as usize, mode };
    let result = run_download(client, &want, &dest, move |event| {
        let _ = emit_app.emit("swarm://event", TransferEvent { transfer_id, event });
    })
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            let reason = e.to_string();
            let _ = app.emit(
                "swarm://event",
                TransferEvent {
                    transfer_id,
                    event: crate::swarm::SwarmEvent::Failed {
                        reason: reason.clone(),
                        sources_tried: 0,
                    },
                },
            );
            Err(reason)
        }
    }
}

/// The person's download preferences, as the Settings screen shows them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPrefsView {
    /// The folder downloads go to without asking. `None`: ask each time.
    folder: Option<String>,
    per_burrow: bool,
    /// The system downloads folder, which is where a save panel opens.
    system_folder: String,
}

fn prefs_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("downloads.json"))
}

fn view_of(app: &AppHandle, prefs: &DownloadPrefs) -> Result<DownloadPrefsView, String> {
    Ok(DownloadPrefsView {
        folder: prefs.folder.as_ref().map(|p| p.display().to_string()),
        per_burrow: prefs.per_burrow,
        system_folder: app
            .path()
            .download_dir()
            .map_err(|e| e.to_string())?
            .display()
            .to_string(),
    })
}

/// The current download preferences.
#[tauri::command]
pub fn download_prefs(app: AppHandle) -> Result<DownloadPrefsView, String> {
    view_of(&app, &downloads::load(&prefs_path(&app)?))
}

/// Choose the download folder in a native folder panel. The webview asks for
/// the panel; it never names the folder. `None` when the panel was cancelled.
#[tauri::command]
pub async fn choose_download_folder(app: AppHandle) -> Result<Option<DownloadPrefsView>, String> {
    let path = prefs_path(&app)?;
    let mut prefs = downloads::load(&path);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut panel = app.dialog().file().set_title("Save downloads to");
    if let Some(start) = prefs.folder.clone().or_else(|| app.path().download_dir().ok()) {
        panel = panel.set_directory(start);
    }
    panel.pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let Some(picked) = rx.await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    prefs.folder = Some(picked.into_path().map_err(|e| e.to_string())?);
    downloads::store(&path, &prefs)?;
    view_of(&app, &prefs).map(Some)
}

/// Go back to asking where each download goes.
#[tauri::command]
pub fn clear_download_folder(app: AppHandle) -> Result<DownloadPrefsView, String> {
    let path = prefs_path(&app)?;
    let mut prefs = downloads::load(&path);
    prefs.folder = None;
    downloads::store(&path, &prefs)?;
    view_of(&app, &prefs)
}

/// Turn a folder per burrow on or off.
#[tauri::command]
pub fn set_per_burrow_folders(app: AppHandle, on: bool) -> Result<DownloadPrefsView, String> {
    let path = prefs_path(&app)?;
    let mut prefs = downloads::load(&path);
    prefs.per_burrow = on;
    downloads::store(&path, &prefs)?;
    view_of(&app, &prefs)
}

/// Settle where one download goes: the set folder without asking, else a
/// native save panel. `None` means the person cancelled the panel.
///
/// `resumable`: a swarm download resumes from a `.rhstate` beside its
/// destination, so with a folder set it must land on the same path as last
/// time rather than a fresh numbered one.
async fn resolve_destination(
    app: &AppHandle,
    burrow: &str,
    name: &str,
    resumable: bool,
) -> Result<Option<PathBuf>, String> {
    let prefs = downloads::load(&prefs_path(app)?);
    let system = app.path().download_dir().map_err(|e| e.to_string())?;
    let dest = match downloads::plan(&prefs, &system, burrow, name) {
        Destination::Write(path) => {
            let path = if resumable {
                // Same name as last time, so an interrupted fetch picks up.
                path.parent()
                    .map(|dir| dir.join(downloads::sanitize_name(name)))
                    .unwrap_or(path)
            } else {
                path
            };
            Some(path)
        }
        Destination::Ask { dir, name } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            // Open on the burrow's folder when it exists, else its parent: a
            // panel should not create folders just by being shown.
            let start = if dir.is_dir() { dir.clone() } else { system.clone() };
            app.dialog()
                .file()
                .set_title("Save download")
                .set_directory(start)
                .set_file_name(&name)
                .save_file(move |picked| {
                    let _ = tx.send(picked);
                });
            match rx.await.map_err(|e| e.to_string())? {
                Some(picked) => Some(picked.into_path().map_err(|e| e.to_string())?),
                None => None,
            }
        }
    };
    if let Some(path) = &dest {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
    }
    Ok(dest)
}

/// Write bytes the webview already holds (an inline download over its socket,
/// or a seeded demo file) to disk, and say where they went. `None` when the
/// person cancelled the save panel, which is not a failure.
///
/// A webview has no download manager: the `<a download>` click that saves a
/// file in a browser tab goes nowhere here, which is how an in-app download
/// came to "succeed" and leave nothing on disk. Where it goes is
/// [`resolve_destination`]'s call: the set folder, or a native save panel.
/// The name is reduced to a safe basename first, and with a folder set an
/// existing file is never overwritten (the newcomer gets a numbered name).
#[tauri::command]
pub async fn save_file(
    app: AppHandle,
    name: String,
    data_base64: String,
    burrow: Option<String>,
) -> Result<Option<String>, String> {
    let bytes = decode_base64(&data_base64)?;
    let Some(dest) =
        resolve_destination(&app, burrow.as_deref().unwrap_or_default(), &name, false).await?
    else {
        return Ok(None);
    };
    std::fs::write(&dest, &bytes).map_err(|e| format!("could not write {}: {e}", dest.display()))?;
    eprintln!("[rh-save] wrote {} bytes to {}", bytes.len(), dest.display());
    Ok(Some(dest.display().to_string()))
}

/// Decode standard base64 (with or without `=` padding). Hand-rolled so the
/// shell takes no dependency for forty lines; whitespace is tolerated, anything
/// else outside the alphabet is an error, never silently skipped.
fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = value(c).ok_or_else(|| "the file data was not valid base64".to_string())?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Parse a 64-char lowercase-hex blake3 root into bytes.
fn parse_root(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!("root must be 64 hex chars, got {}", hex.len()));
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trips_and_refuses_garbage() {
        assert_eq!(super::decode_base64("").unwrap(), b"");
        assert_eq!(super::decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(super::decode_base64("Zm8=").unwrap(), b"fo");
        assert_eq!(super::decode_base64("Zm9v").unwrap(), b"foo");
        assert_eq!(super::decode_base64("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(super::decode_base64("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(super::decode_base64("/+8=").unwrap(), [0xff, 0xef]);
        assert!(super::decode_base64("Zm9v!").is_err());
    }

    #[test]
    fn a_saved_file_never_overwrites_the_one_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = crate::downloads::unique_path(dir.path(), "readme.txt");
        assert_eq!(first, dir.path().join("readme.txt"));
        std::fs::write(&first, b"one").unwrap();
        let second = crate::downloads::unique_path(dir.path(), "readme.txt");
        assert_eq!(second, dir.path().join("readme (2).txt"));
        std::fs::write(&second, b"two").unwrap();
        assert_eq!(
            crate::downloads::unique_path(dir.path(), "readme.txt"),
            dir.path().join("readme (3).txt")
        );
        // No extension, and a name that would climb out of the folder.
        std::fs::write(dir.path().join("LICENSE"), b"x").unwrap();
        assert_eq!(
            crate::downloads::unique_path(dir.path(), "LICENSE"),
            dir.path().join("LICENSE (2)")
        );
        assert_eq!(crate::downloads::sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(crate::downloads::sanitize_name(".."), "download.bin");
    }

    use super::*;

    #[test]
    fn parse_root_roundtrips_and_rejects_bad_input() {
        let hex = "8d12a2ad".to_string() + &"00".repeat(26) + "212b";
        assert_eq!(hex.len(), 64);
        let bytes = parse_root(&hex).unwrap();
        assert_eq!(bytes[0], 0x8d);
        assert_eq!(&bytes[30..], &[0x21, 0x2b]);
        assert!(parse_root("tooshort").is_err());
        assert!(parse_root(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn sanitize_name_strips_traversal() {
        assert_eq!(sanitize_name("song.mp3"), "song.mp3");
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_name("..\\..\\win.ini"), "win.ini");
        assert_eq!(sanitize_name(".."), "download.bin");
        assert_eq!(sanitize_name(""), "download.bin");
        assert_eq!(sanitize_name(".hidden"), "download.bin");
        // Windows drive-relative prefix + NTFS alternate data stream: reject the ':'.
        assert_eq!(sanitize_name("C:evil.exe"), "download.bin");
        assert_eq!(sanitize_name("report.txt:hidden"), "download.bin");
    }
}
