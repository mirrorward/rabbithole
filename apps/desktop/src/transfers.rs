//! Slice 4b: the Tauri command + event surface wrapping the swarm core.
//!
//! The wasm SPA (Slice 5) invokes these over the `window.__RH_NATIVE__` bridge
//! and listens for `swarm://event` to drive its multi-source Transfers UI. The
//! command *bodies* are the already-tested [`crate::swarm::run_swarm_download`];
//! this layer is Tauri glue (managed state + serialization + event emission),
//! best exercised end-to-end with `cargo tauri dev` (see the design doc).

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
/// `wss://` don't), stored for subsequent swarm downloads.
#[tauri::command]
pub async fn connect_native(
    state: State<'_, TransfersManager>,
    endpoint: String,
    fingerprint: Option<String>,
) -> Result<(), String> {
    let client = Client::connect(
        &endpoint,
        None,
        fingerprint.as_deref(),
        "rabbithole-desktop",
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .map_err(|e| e.to_string())?;
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
) -> Result<(), String> {
    let root = parse_root(&root_hex)?;
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    let dest = dir.join(sanitize_name(&name));
    let mut guard = state.client.lock().await;
    let client = guard.as_mut().ok_or("not connected to a burrow")?;
    run_swarm_download(client, root, size, &dest, move |event| {
        let _ = app.emit("swarm://event", TransferEvent { transfer_id, event });
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
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
