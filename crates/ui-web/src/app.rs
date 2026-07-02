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

use crate::client::{MockClient, UiClient, LOBBY};
use crate::components::{ArtGallery, BoardView, Boards, Directory, Dms, Files, Lobby, Login};
use crate::files::{join_path, FilesState};
use crate::state::UiState;
use crate::theme_css::{next_choice, root_style, ThemeChoice, DEFAULT_PACK, STYLESHEET};
use crate::wire::{FileCommand, FileEvent};

/// Reactive, `Copy` handle bundle shared through context.
#[derive(Clone, Copy)]
pub struct AppState {
    /// The flat UI model, folded from events.
    pub state: RwSignal<UiState>,
    /// The file-library model, folded from file events.
    pub files: RwSignal<FilesState>,
    /// The user's appearance choice (System/Light/Dark). The effective
    /// [`Mode`] is derived from this plus the OS hint via [`AppState::mode`].
    pub theme: RwSignal<ThemeChoice>,
    /// The command seam. `MockClient` today; a real transport later.
    pub client: StoredValue<MockClient>,
}

impl AppState {
    /// Create the shared state for a fresh session.
    pub fn new() -> Self {
        Self {
            state: create_rw_signal(UiState::default()),
            files: create_rw_signal(FilesState::default()),
            theme: create_rw_signal(initial_theme_choice()),
            client: store_value(MockClient::new()),
        }
    }

    /// The effective appearance [`Mode`], resolved from the user's
    /// [`ThemeChoice`] and the OS `prefers-color-scheme` hint. Reactive on the
    /// theme signal.
    pub fn mode(&self) -> Mode {
        crate::theme_css::effective_mode(self.theme.get(), os_prefers_dark())
    }

    /// Advance the theme choice (System → Light → Dark → …) and persist it.
    pub fn cycle_theme(&self) {
        self.theme.update(|c| *c = next_choice(*c));
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

    let style = move || root_style(DEFAULT_PACK, app.mode());

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
                    <Route path="/art" view=ArtGallery/>
                </Routes>
            </div>
        </Router>
    }
}

/// Mount the app into `document.body`. Called from the wasm entry point (a
/// later trunk/`main.rs` slice); present here so the library is directly
/// runnable in a browser.
pub fn mount() {
    mount_to_body(App);
}
