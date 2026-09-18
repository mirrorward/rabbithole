//! Slice 4b: the Tauri command + event surface wrapping the swarm core.
//!
//! The wasm SPA (Slice 5) invokes these over the `window.__RH_NATIVE__` bridge
//! and listens for `swarm://event` to drive its multi-source Transfers UI. The
//! command *bodies* are the already-tested [`crate::swarm::run_swarm_download`];
//! this layer is Tauri glue (managed state + serialization + event emission),
//! best exercised end-to-end with `cargo tauri dev` (see the design doc).

#![cfg_attr(rustfmt, rustfmt_skip)]

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

use rabbithole_core::Client;

use crate::swarm::{run_swarm_download, SwarmEvent};

/// App-managed state: the native RHP session.
#[derive(Default)]
pub struct TransfersManager {
    /// The connected client. `Client` is `!Sync`, so it lives behind an async
    /// mutex; for now the swarm fetch runs while the lock is held, so downloads
    /// serialize (concurrent downloads + mid-fetch abort are a later refinement,
    /// unblocked by splitting the find/ticket phase from the lock-free fetch).
    client: Mutex<Option<Client>>,
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
    *state.client.lock().await = Some(client);
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
#[tauri::command]
pub async fn swarm_start_download(
    app: AppHandle,
    state: State<'_, TransfersManager>,
    transfer_id: u64,
    root_hex: String,
    size: u64,
    name: String,
    max_sources: u32,
) -> Result<(), String> {
    let root = parse_root(&root_hex)?;
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    let dest = dir.join(sanitize_name(&name));
    eprintln!("[rh-swarm] swarm_start_download: transfer={transfer_id} root={root_hex} size={size} name={name:?}");
    let mut guard = state.client.lock().await;
    let client = guard.as_mut().ok_or("not connected to a burrow")?;
    // A failure is reported to the webview as an event, not just returned:
    // the UI's transfer row is driven by the event stream, and an error that
    // only comes back through the invoke promise leaves that row saying
    // nothing about what happened.
    let emit_app = app.clone();
    let result = run_swarm_download(client, root, size, &dest, max_sources as usize, move |event| {
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

/// Write bytes the webview already holds (an inline download over its socket,
/// or a seeded demo file) into the downloads folder, and say where they went.
///
/// A webview has no download manager: the `<a download>` click that saves a
/// file in a browser tab goes nowhere here, which is how an in-app download
/// came to "succeed" and leave nothing on disk. The name is reduced to a safe
/// basename before it touches the filesystem, and an existing file is never
/// overwritten: the newcomer gets a numbered name, the way Finder does it.
#[tauri::command]
pub async fn save_file(
    app: AppHandle,
    name: String,
    data_base64: String,
) -> Result<String, String> {
    let bytes = decode_base64(&data_base64)?;
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("no downloads folder: {e}"))?;
    let dest = unique_path(&dir, &sanitize_name(&name));
    std::fs::write(&dest, &bytes).map_err(|e| format!("could not write {}: {e}", dest.display()))?;
    eprintln!("[rh-save] wrote {} bytes to {}", bytes.len(), dest.display());
    Ok(dest.display().to_string())
}

/// The first path for `name` in `dir` that nothing occupies yet: `name`, then
/// `stem (2).ext`, `stem (3).ext`, and so on.
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let as_path = std::path::Path::new(name);
    let stem = as_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let ext = as_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded counter finds a free name")
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

/// Reduce a server-supplied filename to a bare, safe basename so it can't escape
/// the downloads directory. Strips path separators and rejects `..`, leading
/// dots, and — for Windows — any name containing a `:` (drive-relative prefixes
/// like `C:evil.exe` PATH-resolve off the target dir, and `report.txt:stream`
/// opens an NTFS alternate data stream), falling back to a fixed safe name.
fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let unsafe_name = base.is_empty()
        || base == "."
        || base == ".."
        || base.starts_with('.')
        || base.contains(':');
    if unsafe_name {
        "download.bin".to_string()
    } else {
        base.to_string()
    }
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
        let first = super::unique_path(dir.path(), "readme.txt");
        assert_eq!(first, dir.path().join("readme.txt"));
        std::fs::write(&first, b"one").unwrap();
        let second = super::unique_path(dir.path(), "readme.txt");
        assert_eq!(second, dir.path().join("readme (2).txt"));
        std::fs::write(&second, b"two").unwrap();
        assert_eq!(
            super::unique_path(dir.path(), "readme.txt"),
            dir.path().join("readme (3).txt")
        );
        // No extension, and a name that would climb out of the folder.
        std::fs::write(dir.path().join("LICENSE"), b"x").unwrap();
        assert_eq!(
            super::unique_path(dir.path(), "LICENSE"),
            dir.path().join("LICENSE (2)")
        );
        assert_eq!(super::sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(super::sanitize_name(".."), "download.bin");
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
