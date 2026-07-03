//! The application root, shared reactive state, and the command seam wiring.
//!
//! [`AppState`] is a `Copy` bundle of reactive handles plus the [`MockClient`]
//! (held in a `StoredValue`). It is provided as Leptos context so every route
//! and component shares one session. [`AppState::dispatch`] is the single
//! choke point: it drives a [`Command`] through the [`UiClient`] and folds the
//! resulting [`Event`]s into the reactive [`UiState`].

use leptos::*;
use leptos_router::*;
use rabbithole_core::api::Command;
use rabbithole_core::theme::Mode;

use crate::admin::AdminState;
use crate::client::{MockClient, UiClient, LOBBY};
use crate::components::{
    Admin, ArtGallery, BoardView, Boards, Directory, Dms, Files, Lobby, Login, Radio,
};
use crate::files::{join_path, FilesState};
use crate::packs::PackTokens;
use crate::radio::{clamp_volume, RadioPrefs, RadioState};
use crate::state::UiState;
use crate::theme_css::{next_mode, next_pack, resolve_root_style, ThemeChoice, STYLESHEET};
use crate::wire::{AdminCommand, AdminEvent, FileCommand, FileEvent, NoticeRoute};

/// Reactive, `Copy` handle bundle shared through context.
#[derive(Clone, Copy)]
pub struct AppState {
    /// The flat UI model, folded from events.
    pub state: RwSignal<UiState>,
    /// The file-library model, folded from file events.
    pub files: RwSignal<FilesState>,
    /// The web-admin model, folded from admin events.
    pub admin: RwSignal<AdminState>,
    /// Whether the signed-in session holds an admin capability. Gates the admin
    /// nav entry and routes.
    pub is_admin: RwSignal<bool>,
    /// The user's appearance choice: theme pack (Clean/Retro/HighContrast)
    /// plus mode policy (System/Light/Dark). The effective [`Mode`] is
    /// derived from this plus the OS hint via [`AppState::mode`].
    pub theme: RwSignal<ThemeChoice>,
    /// The theme editor's **custom pack override slot**: when set, these
    /// tokens replace the built-in pack for this session (mode resolution
    /// still applies). Session-local and unpersisted — Apply is a preview,
    /// not a save.
    pub custom_pack: RwSignal<Option<PackTokens>>,
    /// Radio now-playing per station, folded from routed `[radio]` notices.
    pub radio: RwSignal<RadioState>,
    /// The user's radio player preferences (enable/volume/mute/station plus
    /// the Icecast delivery address), persisted to `localStorage`.
    pub radio_prefs: RwSignal<RadioPrefs>,
    /// The command seam. `MockClient` today; a real transport later.
    pub client: StoredValue<MockClient>,
    /// The wasm-only `<audio>` element wrapper the preference setters keep in
    /// sync ([`crate::player`]). Absent on the host, where there is no DOM.
    #[cfg(target_arch = "wasm32")]
    player: StoredValue<crate::player::RadioPlayer>,
}

impl AppState {
    /// Create the shared state for a fresh session.
    pub fn new() -> Self {
        Self {
            state: create_rw_signal(UiState::default()),
            files: create_rw_signal(FilesState::default()),
            admin: create_rw_signal(AdminState::default()),
            is_admin: create_rw_signal(false),
            theme: create_rw_signal(initial_theme_choice()),
            custom_pack: create_rw_signal(None),
            radio: create_rw_signal(RadioState::default()),
            radio_prefs: create_rw_signal(initial_radio_prefs()),
            client: store_value(MockClient::new()),
            #[cfg(target_arch = "wasm32")]
            player: store_value(crate::player::RadioPlayer::new()),
        }
    }

    /// The effective appearance [`Mode`], resolved from the user's
    /// [`ThemeChoice`] and the OS `prefers-color-scheme` hint. Reactive on the
    /// theme signal.
    pub fn mode(&self) -> Mode {
        crate::theme_css::effective_mode(self.theme.get().mode, os_prefers_dark())
    }

    /// Advance the mode choice (System → Light → Dark → …) and persist it.
    pub fn cycle_theme(&self) {
        self.theme.update(|c| c.mode = next_mode(c.mode));
        self.persist_theme();
    }

    /// Advance the theme pack (Clean → Retro → HighContrast → …) and persist
    /// it.
    pub fn cycle_pack(&self) {
        self.theme.update(|c| c.pack = next_pack(c.pack));
        self.persist_theme();
    }

    /// Apply the theme editor's working tokens to this session: they fill
    /// the custom override slot and win over the built-in pack until
    /// [`AppState::clear_custom_pack`].
    pub fn apply_custom_pack(&self, tokens: PackTokens) {
        self.custom_pack.set(Some(tokens));
    }

    /// Clear the custom override slot, returning to the chosen built-in pack.
    pub fn clear_custom_pack(&self) {
        self.custom_pack.set(None);
    }

    /// Persist the current theme choice (browser only; no-op on the host).
    fn persist_theme(&self) {
        #[cfg(target_arch = "wasm32")]
        crate::theme_css::storage::save_choice(self.theme.get_untracked());
    }

    /// Drive one command through the seam and fold its events into state.
    pub fn dispatch(&self, command: Command) {
        let state = self.state;
        self.client.update_value(|client| {
            let events = client.send(command);
            state.update(|s| {
                for event in &events {
                    s.apply(event);
                }
            });
        });
    }

    /// Refresh the who-list snapshot from the client.
    pub fn refresh_who(&self) {
        let who = self.client.with_value(|client| client.who(LOBBY));
        self.state.update(|s| s.who = who);
    }

    /// Load the board tree snapshot into state.
    pub fn load_boards(&self) {
        let boards = self.client.with_value(|c| c.boards());
        self.state.update(|s| s.set_boards(boards));
    }

    /// Select a board and load its threads into state.
    pub fn select_board(&self, slug: &str) {
        let threads = self.client.with_value(|c| c.threads(slug));
        self.state.update(|s| s.select_board(slug, threads));
    }

    /// Open a thread and load its posts into state.
    pub fn open_thread(&self, id: u64) {
        let posts = self.client.with_value(|c| c.posts(id));
        self.state.update(|s| s.open_thread(id, posts));
    }

    /// Load the DM conversation snapshots into state.
    pub fn load_dms(&self) {
        let threads = self.client.with_value(|c| c.dm_threads());
        self.state.update(|s| s.set_dm_threads(threads));
    }

    /// Send a DM into the selected conversation, appending it locally. The
    /// real transport will echo a server event instead.
    pub fn send_dm(&self, text: &str) {
        let Some(id) = self.state.with(|s| s.selected_dm.clone()) else {
            return;
        };
        let state = self.state;
        self.client.update_value(|c| {
            if let Some(msg) = c.send_dm(&id, text) {
                state.update(|s| s.append_dm(&id, msg));
            }
        });
    }

    /// Load the member directory snapshot into state.
    pub fn load_members(&self) {
        let members = self.client.with_value(|c| c.members());
        self.state.update(|s| s.set_members(members));
    }

    /// Drive one [`FileCommand`] through the seam and fold its file events into
    /// the [`FilesState`].
    fn dispatch_file(&self, command: FileCommand) {
        let files = self.files;
        self.client.update_value(|client| {
            let events: Vec<FileEvent> = client.dispatch_file(command);
            files.update(|f| {
                for event in &events {
                    f.apply(event);
                }
            });
        });
    }

    /// Load the file-area list into state.
    pub fn load_areas(&self) {
        self.dispatch_file(FileCommand::ListAreas);
    }

    /// Open an area at its root and list it.
    pub fn open_area(&self, slug: &str) {
        self.files.update(|f| {
            f.current_area = Some(slug.to_string());
            f.path.clear();
            f.selected = None;
        });
        self.refresh_files();
    }

    /// Descend into a child folder of the current location and list it.
    pub fn open_subfolder(&self, name: &str) {
        self.files.update(|f| {
            f.path.push(name.to_string());
            f.selected = None;
        });
        self.refresh_files();
    }

    /// Jump to a breadcrumb path (`None` = area root) and list it.
    pub fn go_to_path(&self, path: Option<String>) {
        self.files.update(|f| {
            f.path = match &path {
                Some(p) if !p.is_empty() => p.split('/').map(str::to_string).collect(),
                _ => Vec::new(),
            };
            f.selected = None;
        });
        self.refresh_files();
    }

    /// List the current area + folder.
    pub fn refresh_files(&self) {
        let (area, path) = self
            .files
            .with(|f| (f.current_area.clone(), join_path(&f.path)));
        let Some(area) = area else {
            return;
        };
        self.dispatch_file(FileCommand::ListFolder { area, path });
    }

    /// Show a node's metadata card.
    pub fn select_file(&self, id: i64) {
        self.files.update(|f| f.selected = Some(id));
    }

    /// Download a file inline; the completed transfer lands in the queue.
    pub fn download(&self, id: i64) {
        self.dispatch_file(FileCommand::Download { id });
    }

    /// Upload a small file into the current area/folder, then refresh the
    /// listing so the new node appears.
    pub fn upload(&self, name: &str, bytes: Vec<u8>) {
        let (area, parent) = self
            .files
            .with(|f| (f.current_area.clone(), join_path(&f.path)));
        let Some(area) = area else {
            return;
        };
        self.dispatch_file(FileCommand::Upload {
            area,
            parent,
            name: name.to_string(),
            mime: "text/plain".to_string(),
            comment: String::new(),
            bytes,
        });
    }

    /// Grant or revoke the admin capability for the current session. Gates the
    /// admin nav and routes.
    pub fn set_admin(&self, is_admin: bool) {
        self.is_admin.set(is_admin);
    }

    /// Drive one [`AdminCommand`] through the seam and fold its admin events
    /// into the [`AdminState`].
    fn dispatch_admin(&self, command: AdminCommand) {
        let admin = self.admin;
        self.client.update_value(|client| {
            let events: Vec<AdminEvent> = client.dispatch_admin(command);
            admin.update(|a| {
                for event in &events {
                    a.apply(event);
                }
            });
        });
    }

    /// Load the permission-class list into admin state.
    pub fn load_classes(&self) {
        self.dispatch_admin(AdminCommand::ListClasses);
    }

    /// Save a permission class's capability mask.
    pub fn set_class(&self, name: &str, base_mask: u64) {
        self.dispatch_admin(AdminCommand::SetClass {
            name: name.to_string(),
            base_mask,
        });
    }

    /// Load a page of accounts into admin state.
    pub fn load_accounts(&self) {
        self.dispatch_admin(AdminCommand::ListAccounts {
            offset: 0,
            limit: 100,
        });
    }

    /// Enable or disable an account.
    pub fn set_account_disabled(&self, login: &str, disabled: bool) {
        self.dispatch_admin(AdminCommand::SetAccount {
            login: login.to_string(),
            role: None,
            class: None,
            disabled: Some(disabled),
        });
        // Reflect the change back into the visible listing.
        self.load_accounts();
    }

    /// Load the seeded config keys the console exposes.
    pub fn load_config(&self) {
        for key in [
            "server.name",
            "server.motd",
            "registration.mode",
            "chat.slowmode_secs",
        ] {
            self.dispatch_admin(AdminCommand::GetConfig {
                key: key.to_string(),
            });
        }
    }

    /// Set a config key/value.
    pub fn set_config(&self, key: &str, value: &str) {
        self.dispatch_admin(AdminCommand::SetConfig {
            key: key.to_string(),
            value: value.to_string(),
        });
    }

    /// Mint an invite code with the given time-to-live in seconds.
    pub fn create_invite(&self, ttl_secs: i64) {
        self.dispatch_admin(AdminCommand::CreateInvite { ttl_secs });
    }

    /// Broadcast a notice to every session.
    pub fn broadcast(&self, text: &str) {
        self.dispatch_admin(AdminCommand::Broadcast {
            text: text.to_string(),
        });
    }

    /// Disconnect a session by id.
    pub fn kick(&self, session_id: u64) {
        self.dispatch_admin(AdminCommand::Kick { session_id });
    }

    /// Fold one routed notice: `[radio]` bridge updates feed the radio
    /// reducer silently; ordinary operator notices land in the chat log.
    /// Mirrors the TUI's routing split — the transport slice calls this from
    /// its notice sink.
    pub fn apply_notice(&self, route: NoticeRoute) {
        match route {
            NoticeRoute::Radio(update) => self.radio.update(|r| r.apply_update(update)),
            NoticeRoute::Chat { from, text } => {
                self.state.update(|s| s.push_notice(&from, &text));
            }
        }
    }

    /// Load the mock's seeded radio notices into the radio state (each is a
    /// real `ServerNotice` push routed through the host-tested wire mapping),
    /// so the Radio view and status segment render in dev.
    pub fn load_radio(&self) {
        for route in self.client.with_value(|c| c.radio_routes()) {
            self.apply_notice(route);
        }
    }

    /// Set the Icecast delivery base address (e.g. `http://host:8000`),
    /// persist it, and re-sync the player.
    pub fn set_radio_base(&self, base: &str) {
        self.radio_prefs
            .update(|p| p.base = base.trim().to_string());
        self.radio_prefs_changed();
    }

    /// Tune the player in or out, persist the choice, and re-sync.
    pub fn set_radio_enabled(&self, enabled: bool) {
        self.radio_prefs.update(|p| p.enabled = enabled);
        self.radio_prefs_changed();
    }

    /// Mute or unmute playback (volume is remembered underneath).
    pub fn set_radio_muted(&self, muted: bool) {
        self.radio_prefs.update(|p| p.muted = muted);
        self.radio_prefs_changed();
    }

    /// Set the playback volume (clamped into `0.0..=1.0`), persist, re-sync.
    pub fn set_radio_volume(&self, volume: f32) {
        self.radio_prefs.update(|p| p.volume = clamp_volume(volume));
        self.radio_prefs_changed();
    }

    /// Select a station: record the slug in the preferences and — when the
    /// player is enabled — start playing its delivery mount.
    pub fn select_station(&self, station: &str) {
        self.radio_prefs
            .update(|p| p.station = Some(station.to_string()));
        self.radio_prefs_changed();
    }

    /// Persist the preferences and reconcile the audio element (both are
    /// browser-only edges; no-ops on the host).
    fn radio_prefs_changed(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let prefs = self.radio_prefs.get_untracked();
            crate::radio::storage::save_prefs(&prefs);
            self.player.update_value(|p| p.sync(&prefs));
        }
    }
}

/// The theme choice a fresh session starts with: the persisted choice on the
/// browser, else the default (follow-OS).
fn initial_theme_choice() -> ThemeChoice {
    #[cfg(target_arch = "wasm32")]
    {
        crate::theme_css::storage::load_choice().unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        ThemeChoice::default()
    }
}

/// The radio preferences a fresh session starts with: the persisted
/// preferences on the browser, else the defaults.
fn initial_radio_prefs() -> RadioPrefs {
    #[cfg(target_arch = "wasm32")]
    {
        crate::radio::storage::load_prefs().unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        RadioPrefs::default()
    }
}

/// Whether the OS prefers dark mode. On the host (tests) this defaults to
/// `true`, preserving the SPA's original dark-first default.
fn os_prefers_dark() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::theme_css::storage::os_prefers_dark()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        true
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Root component: mounts the stylesheet, provides [`AppState`], applies the
/// theme variables to the app root, and routes between login and lobby.
#[component]
pub fn App() -> impl IntoView {
    let app = AppState::new();
    provide_context(app);

    let style = move || {
        let (pack, mode) = (app.theme.get().pack, app.mode());
        app.custom_pack
            .with(|custom| resolve_root_style(custom.as_ref(), pack, mode))
    };

    view! {
        <style>{STYLESHEET}</style>
        <Router>
            <div class="rh-app" style=style>
                <Routes>
                    <Route path="/" view=Login/>
                    <Route path="/lobby" view=Lobby/>
                    <Route path="/boards" view=Boards/>
                    <Route path="/boards/:slug" view=BoardView/>
                    <Route path="/dms" view=Dms/>
                    <Route path="/directory" view=Directory/>
                    <Route path="/files" view=Files/>
                    <Route path="/radio" view=Radio/>
                    <Route path="/art" view=ArtGallery/>
                    <Route path="/admin" view=Admin/>
                </Routes>
            </div>
        </Router>
    }
}

/// Mount the app into `document.body`. Called from the wasm entry point
/// (`src/main.rs`, the trunk build target); present here so the library is
/// directly runnable in a browser. Boot also kicks off the PWA
/// service-worker registration ([`crate::pwa`]) — browser only, and never
/// fatal: the app mounts identically whether or not a worker installs.
pub fn mount() {
    #[cfg(target_arch = "wasm32")]
    crate::pwa::register_service_worker();
    mount_to_body(App);
}
