//! Slice 4b: the Tauri command + event surface wrapping the swarm core.
//!
//! The wasm SPA (Slice 5) invokes these over the `window.__RH_NATIVE__` bridge
//! and listens for `swarm://event` to drive its multi-source Transfers UI. The
//! command *bodies* are the already-tested [`crate::swarm::run_swarm_download`];
//! this layer is Tauri glue (managed state + serialization + event emission),
//! best exercised end-to-end with `cargo tauri dev` (see the design doc).

#![cfg_attr(rustfmt, rustfmt_skip)]

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use crate::downloads::{self, Destination, DownloadPrefs};
#[cfg(test)]
use crate::downloads::sanitize_name;

use rabbithole_core::Client;

use crate::swarm::{
    other_burrows, run_download_sharing, BurrowLink, SourceMode, SwarmEvent, Wanted,
    OTHER_BURROWS_MAX, SESSION_WAIT,
};

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
    /// `Client` is `!Sync`, so each session lives behind its own async
    /// mutex, and the map's lock is held only long enough to find one. A
    /// download holds *its* burrow's session for as long as it runs, so two
    /// downloads from one burrow still take turns — but another burrow's
    /// download, the re-announce pass, and the Settings screen no longer
    /// wait behind it.
    clients: Mutex<std::collections::HashMap<String, Session>>,
    /// What this machine offers to each burrow's swarm, when the person has
    /// opted in. Per burrow, like the sessions. Lock order is always
    /// `clients` then `seeders`.
    seeders: Mutex<std::collections::HashMap<String, crate::seeding::Seeder>>,
    /// The last reason sharing did not work, for the Settings screen. Sharing
    /// never fails a download, so this is the only place it is said.
    seeding_note: std::sync::Mutex<Option<String>>,
    /// Downloads running now, by the transfer the webview knows them as, so
    /// the person can stop one.
    running: Mutex<std::collections::HashMap<u64, Arc<Stopper>>>,
}

/// A running download's stop switch: set once, and whoever is waiting on it
/// gives up at the next moment it can.
#[derive(Default)]
pub struct Stopper {
    stopped: std::sync::atomic::AtomicBool,
    tell: tokio::sync::Notify,
}

impl Stopper {
    fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tell.notify_waiters();
    }

    /// Resolves once the download has been told to stop.
    async fn stopped(&self) {
        loop {
            if self.stopped.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            self.tell.notified().await;
        }
    }
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
    app: AppHandle,
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
    if !authed {
        state.clients.lock().await.remove(&endpoint);
        return Ok(());
    }
    let session: Session = Arc::new(Mutex::new(client));
    state.clients.lock().await.insert(endpoint.clone(), session.clone());
    // Adverts die with a session. A new one announces what is on offer.
    {
        let mut client = session.lock().await;
        let mut seeders = state.seeders.lock().await;
        if let Some(seeder) = seeders.get_mut(&endpoint) {
            if let Err(why) = seeder.announce(&mut client).await {
                *state.seeding_note.lock().expect("not poisoned") = Some(why.to_string());
            }
        }
    }
    // What this machine was offering this burrow when the app last ran is
    // offered again, in the background: each file is read to check it is
    // still the file that was shared, which is not something a sign-in
    // should wait for.
    if downloads::load(&prefs_path(&app)?).seed {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            reoffer(app, endpoint, session).await;
        });
    }
    Ok(())
}

pub use crate::swarm::Session;

/// The other burrows a download may ask: every session the app holds but
/// the one it is downloading from, deduped by the burrow's own key so one
/// reached two ways is one burrow, and only those new enough to answer.
/// The rule itself is [`crate::swarm::other_burrows`]; this only fetches
/// what it needs from the live sessions.
async fn other_links(
    state: &TransfersManager,
    mode: SourceMode,
    origin_endpoint: &str,
    origin: &Session,
) -> Vec<BurrowLink> {
    // The person may have said where a download comes from: then no other
    // burrow is asked, and none is even told what is being looked for.
    if mode != SourceMode::Auto {
        return Vec::new();
    }
    let sessions: Vec<(String, Session)> = {
        let map = state.clients.lock().await;
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    let origin_key = match tokio::time::timeout(SESSION_WAIT, origin.lock()).await {
        Ok(client) => client.server.server_key,
        Err(_) => return Vec::new(),
    };
    let mut cards = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (endpoint, session) in &sessions {
        // A burrow busy with something else is not held up for this: it
        // simply does not join this download.
        let Ok(client) = tokio::time::timeout(SESSION_WAIT, session.lock()).await else {
            continue;
        };
        cards.push((
            endpoint.clone(),
            client.server.server_key,
            client.server.server_version.clone(),
        ));
        names.push(if client.server.server_name.is_empty() {
            endpoint.clone()
        } else {
            client.server.server_name.clone()
        });
    }
    let picked = other_burrows(mode, &cards, origin_endpoint, origin_key, OTHER_BURROWS_MAX);
    picked
        .into_iter()
        .map(|i| BurrowLink {
            label: names[i].clone(),
            session: sessions
                .iter()
                .find(|(e, _)| *e == cards[i].0)
                .map(|(_, s)| s.clone())
                .expect("from the same list"),
            server_key: cards[i].1,
            version: cards[i].2.clone(),
        })
        .collect()
}

/// Offer this burrow again what was on offer to it when the app last ran.
/// A file that has gone, moved, or changed since is quietly forgotten: the
/// note said it was shared, the file says whether it still can be.
async fn reoffer(app: AppHandle, endpoint: String, session: Session) {
    let Ok(path) = shared_path(&app) else { return };
    let known = downloads::load_shared(&path);
    let mine: Vec<downloads::SharedFile> = known
        .iter()
        .filter(|f| f.burrow == endpoint)
        .cloned()
        .collect();
    if mine.is_empty() {
        return;
    }
    let mut gone = Vec::new();
    for file in &mine {
        // Switched off while this was working through the list: stop, and
        // do not make a seeder for a burrow that is no longer sharing.
        if !prefs_path(&app).map(|p| downloads::load(&p).seed).unwrap_or(false) {
            return;
        }
        let Some(root) = parse_root(&file.root).ok() else {
            gone.push(file.root.clone());
            continue;
        };
        let state = app.state::<TransfersManager>();
        // Reading the file through to check it is still what was shared
        // happens off the runtime and without holding anything: it is
        // seconds of work per file, and nobody is waiting for it.
        let store = {
            let mut seeders = state.seeders.lock().await;
            seeders.entry(endpoint.clone()).or_default().store()
        };
        let path = file.path.clone();
        let read = tokio::task::spawn_blocking(move || store.add(root, &path))
            .await
            .map_err(|_| ())
            .and_then(|r| r.map_err(|_| ()));
        if read.is_err() {
            gone.push(file.root.clone());
            continue;
        }
        // Already in the store now, so this only advertises it.
        let mut client = session.lock().await;
        let mut seeders = state.seeders.lock().await;
        let offered = seeders
            .entry(endpoint.clone())
            .or_default()
            .share(&mut client, root, file.size, &file.name, &file.path)
            .await;
        drop(seeders);
        drop(client);
        if offered.is_err() {
            gone.push(file.root.clone());
        }
        // A file at a time, and a breath between them: reading each one
        // through is work nobody asked for right now.
        tokio::task::yield_now().await;
    }
    if gone.is_empty() {
        return;
    }
    // Written back once, so a file that cannot be offered any more is not
    // read again at every sign-in.
    let mut known = downloads::load_shared(&path);
    known.retain(|f| !(f.burrow == endpoint && gone.contains(&f.root)));
    let _ = downloads::store_shared(&path, &known);
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
    // The map is held only long enough to find the session; the download
    // then holds that one burrow's session, and no other.
    let session = state
        .clients
        .lock()
        .await
        .get(&endpoint)
        .cloned()
        .ok_or("the app has no signed-in session to that burrow")?;
    // Peers when anyone has it, else the burrow itself, unless told
    // otherwise — settled before anything is asked of anyone.
    let mode = SourceMode::parse(mode.as_deref().unwrap_or("auto"));
    // The other burrows this person is on, which may hold the same content
    // and let them download it there: they are asked, and the ones that say
    // yes carry units beside the peers.
    let others = other_links(&state, mode, &endpoint, &session).await;
    // A failure is reported to the webview as an event, not just returned:
    // the UI's transfer row is driven by the event stream, and an error that
    // only comes back through the invoke promise leaves that row saying
    // nothing about what happened.
    let emit_app = app.clone();
    let want = Wanted { root, size, node_id, max_sources: max_sources as usize, mode };
    // Opted in, and the size is known: offer the file from the start, so
    // what lands is fetched from here while the rest is still coming.
    let seeding = downloads::load(&prefs_path(&app)?).seed;
    let share = if seeding && size > 0 && mode != SourceMode::OriginOnly {
        let mut client = session.lock().await;
        let mut seeders = state.seeders.lock().await;
        match seeders.entry(endpoint.clone()).or_default().begin(&mut client, root, size, &name).await {
            Ok(seeds) => Some(seeds),
            Err(e) => {
                *state.seeding_note.lock().expect("not poisoned") = Some(e.to_string());
                None
            }
        }
    } else {
        None
    };
    let sharing = share.is_some();
    // Registered before a byte moves, so Cancel works from the first moment
    // the row appears, and taken away however this ends.
    let stopper = Arc::new(Stopper::default());
    {
        // One download of a file at a time: a second start would write the
        // same file from two fetches, count two downloads, and leave the
        // first with no stop switch of its own.
        let mut running = state.running.lock().await;
        if running.contains_key(&transfer_id) {
            return Err("that download is already running".to_string());
        }
        running.insert(transfer_id, stopper.clone());
    }
    // What the fetch said it had to work with, so a row that ends badly can
    // say whether it had three sources or none.
    let sources_tried = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = sources_tried.clone();
    let result = {
        let fetch = run_download_sharing(&session, &want, &dest, share, &others, move |event| {
            if let crate::swarm::SwarmEvent::Opened { source_count, .. } = &event {
                counted.store(*source_count, std::sync::atomic::Ordering::Relaxed);
            }
            let _ = emit_app.emit("swarm://event", TransferEvent { transfer_id, event });
        });
        tokio::pin!(fetch);
        // Dropping the fetch stops its workers where they are: what has been
        // verified stays on disk for a retry, and nothing else is written.
        tokio::select! {
            done = &mut fetch => done,
            _ = stopper.stopped() => Err(crate::swarm::SwarmError::Cancelled),
        }
    };
    {
        // Only this download's switch: a later start for the same transfer
        // has its own, and must keep it.
        let mut running = state.running.lock().await;
        if running
            .get(&transfer_id)
            .is_some_and(|live| Arc::ptr_eq(live, &stopper))
        {
            running.remove(&transfer_id);
        }
    }
    // Nothing landed, or what landed is not this burrow's to be offered:
    // take back what was offered in part.
    if sharing && !matches!(result, Ok(done) if done.may_share) {
        let mut client = session.lock().await;
        if let Some(seeder) = state.seeders.lock().await.get_mut(&endpoint) {
            seeder.abandon(&mut client, root).await;
        }
    }
    match result {
        Ok(done) => {
            // Opted in: what was just downloaded from this burrow is offered
            // to this burrow's swarm. A courtesy on top of the download, so a
            // failure to share is noted for Settings and never fails it.
            // Opted in now (it may have been switched on while this ran).
            // Never what another burrow carried: that was lent to this
            // person, not given to this burrow.
            // As it is now, not as it was when this started: sharing
            // switched off mid-download means this file is not offered.
            if done.may_share && downloads::load(&prefs_path(&app)?).seed {
                let mut client = session.lock().await;
                let mut seeders = state.seeders.lock().await;
                let shared = seeders
                    .entry(endpoint.clone())
                    .or_default()
                    .share(&mut client, root, size, &name, &dest)
                    .await;
                if shared.is_ok() {
                    remember_shared(
                        &app,
                        downloads::SharedFile {
                            burrow: endpoint.clone(),
                            root: root_hex.clone(),
                            size,
                            name: name.clone(),
                            path: dest.clone(),
                        },
                    );
                }
                *state.seeding_note.lock().expect("not poisoned") = shared.err().map(|e| e.to_string());
            }
            Ok(())
        }
        // Stopped by the person: the row says so, and what was verified
        // stays on disk for a retry. Not a failure to report back.
        Err(crate::swarm::SwarmError::Cancelled) => {
            let _ = app.emit(
                "swarm://event",
                TransferEvent {
                    transfer_id,
                    event: crate::swarm::SwarmEvent::Failed {
                        reason: "Stopped.".to_string(),
                        sources_tried: sources_tried.load(std::sync::atomic::Ordering::Relaxed),
                        retryable: true,
                    },
                },
            );
            Ok(())
        }
        Err(e) => {
            let reason = e.to_string();
            let _ = app.emit(
                "swarm://event",
                TransferEvent {
                    transfer_id,
                    event: crate::swarm::SwarmEvent::Failed {
                        reason: reason.clone(),
                        sources_tried: sources_tried.load(std::sync::atomic::Ordering::Relaxed),
                        retryable: !matches!(
                            e,
                            crate::swarm::SwarmError::NoPeerSources { server_has: false }
                        ),
                    },
                },
            );
            Err(reason)
        }
    }
}

/// Stop a download that is still going. Its row says so, and what has
/// already been verified stays on disk, so starting it again picks up where
/// it stopped rather than from the beginning.
#[tauri::command]
pub async fn swarm_cancel_download(
    state: State<'_, TransfersManager>,
    transfer_id: u64,
) -> Result<bool, String> {
    let stopper = state.running.lock().await.get(&transfer_id).cloned();
    match stopper {
        Some(stopper) => {
            stopper.stop();
            Ok(true)
        }
        // Already finished, or never ours: nothing to stop, and saying so is
        // not an error.
        None => Ok(false),
    }
}

/// The person's download preferences, as the Settings screen shows them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPrefsView {
    /// The folder downloads go to without asking. `None`: ask each time.
    folder: Option<String>,
    per_burrow: bool,
    /// Whether downloads are offered to other people on the same burrow.
    seed: bool,
    /// How many files are on offer right now, across every burrow.
    seeding_files: usize,
    /// Why sharing last did not work, when it did not.
    seeding_note: Option<String>,
    /// The system downloads folder, which is where a save panel opens.
    system_folder: String,
}

/// Where the note of what is on offer lives, beside the preferences.
fn shared_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("shared.json"))
}

/// Note that a file is on offer to a burrow, so the offer can be made again
/// when the app runs next. Never fails a download: if the note cannot be
/// written, the sharing simply does not outlive the app.
fn remember_shared(app: &AppHandle, entry: downloads::SharedFile) {
    let Ok(path) = shared_path(app) else { return };
    let mut known = downloads::load_shared(&path);
    downloads::remember(&mut known, entry);
    let _ = downloads::store_shared(&path, &known);
}

fn prefs_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("downloads.json"))
}

fn view_of(app: &AppHandle, prefs: &DownloadPrefs) -> Result<DownloadPrefsView, String> {
    let state = app.state::<TransfersManager>();
    // `try_lock`: this is a status line, and a download in progress holds the
    // lock for its duration. Better a count of zero than a frozen Settings.
    let seeding_files = state
        .seeders
        .try_lock()
        .map(|s| s.values().map(crate::seeding::Seeder::files).sum())
        .unwrap_or(0);
    let seeding_note = state.seeding_note.lock().expect("not poisoned").clone();
    Ok(DownloadPrefsView {
        folder: prefs.folder.as_ref().map(|p| p.display().to_string()),
        per_burrow: prefs.per_burrow,
        seed: prefs.seed,
        seeding_files,
        seeding_note,
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

/// Turn sharing downloads on or off. Off means off now: every advert is
/// withdrawn and every peer endpoint closed, not merely "no new ones".
#[tauri::command]
pub async fn set_seeding(app: AppHandle, state: State<'_, TransfersManager>, on: bool) -> Result<DownloadPrefsView, String> {
    let path = prefs_path(&app)?;
    let mut prefs = downloads::load(&path);
    prefs.seed = on;
    downloads::store(&path, &prefs)?;
    if !on {
        let sessions = state.clients.lock().await.clone();
        let mut seeders = state.seeders.lock().await;
        for (endpoint, seeder) in seeders.iter_mut() {
            // A burrow busy with a download is not told: closing the
            // endpoint stops the sharing either way, and an advert nobody
            // can dial lapses on its own.
            let mut held = sessions.get(endpoint).and_then(|s| s.try_lock().ok());
            seeder.stop(held.as_deref_mut()).await;
        }
        seeders.clear();
        // Nothing is on offer, and nothing is offered again next time.
        if let Ok(path) = shared_path(&app) {
            let _ = downloads::store_shared(&path, &[]);
        }
        *state.seeding_note.lock().expect("not poisoned") = None;
    }
    view_of(&app, &prefs)
}

/// Keep adverts alive: a burrow forgets an advert that is not renewed. Runs
/// for the life of the app and does nothing while nothing is on offer. A
/// burrow busy with a download is skipped rather than waited for, so one
/// slow download never holds up the others' adverts; that advert can lapse
/// and come back at the next pass: soft state, by design.
pub async fn reannounce_loop(app: AppHandle) {
    loop {
        let wait = {
            let state = app.state::<TransfersManager>();
            let sessions = state.clients.lock().await.clone();
            let mut seeders = state.seeders.lock().await;
            let mut next = 60;
            for (endpoint, seeder) in seeders.iter_mut().filter(|(_, s)| s.files() > 0) {
                if let Some(mut client) = sessions.get(endpoint).and_then(|s| s.try_lock().ok()) {
                    if let Err(why) = seeder.announce(&mut client).await {
                        *state.seeding_note.lock().expect("not poisoned") = Some(why.to_string());
                    }
                }
                next = next.min(seeder.reannounce_after());
            }
            next
        };
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
    }
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
