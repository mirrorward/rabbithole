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
use rabbithole_proto::welcome::ThemeBundle;

use crate::admin::AdminState;
use crate::client::{MockClient, UiClient, LOBBY};
use crate::components::{
    Admin, ArtGallery, BoardView, Boards, CommandPalette, Directory, Dms, Files, Lobby, Login,
    Radio, ServerBrowser, Toasts,
};
use crate::files::{join_path, FilesState};
use crate::packs::PackTokens;
use crate::radio::{clamp_volume, RadioPrefs, RadioState};
use crate::server_theme::ServerOverlay;
use crate::state::UiState;
use crate::syndication_admin::SynAdminState;
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
    /// The Syndication & Gateways panel model, folded from paired config
    /// get/set replies ([`crate::syndication_admin`]).
    pub syndication: RwSignal<SynAdminState>,
    /// Whether the signed-in session holds an admin capability. Gates the admin
    /// nav entry and routes.
    pub is_admin: RwSignal<bool>,
    /// Whether the ⌘K command palette overlay is open. Shared so both the
    /// header affordance and the global key binding drive the one overlay.
    pub palette_open: RwSignal<bool>,
    /// The Looking Glass server-browser directory ([`crate::servers`]).
    pub servers: RwSignal<Vec<crate::servers::DirectoryServer>>,
    /// An endpoint chosen in the server browser, handed to the login screen to
    /// prefill on its next mount (then cleared).
    pub pending_endpoint: RwSignal<Option<String>>,
    /// Transient toast notifications — humanized-event moments
    /// ([`crate::toasts`]).
    pub toasts: RwSignal<crate::toasts::ToastQueue>,
    /// Whether this session is **live** (a real RHP-over-WebSocket transport)
    /// rather than the seeded [`MockClient`] demo. Set by
    /// [`AppState::connect_live`].
    pub live: RwSignal<bool>,
    /// The live browser WebSocket transport ([`crate::ws::WsClient`]), used
    /// when [`Self::live`] is set. wasm-only — the host has no socket.
    #[cfg(target_arch = "wasm32")]
    ws: StoredValue<crate::ws::WsClient>,
    /// The user's appearance choice: theme pack (Clean/Retro/HighContrast)
    /// plus mode policy (System/Light/Dark). The effective [`Mode`] is
    /// derived from this plus the OS hint via [`AppState::mode`].
    pub theme: RwSignal<ThemeChoice>,
    /// The theme editor's **custom pack override slot**: when set, these
    /// tokens replace the built-in pack for this session (mode resolution
    /// still applies). Session-local and unpersisted — Apply is a preview,
    /// not a save.
    pub custom_pack: RwSignal<Option<PackTokens>>,
    /// The server-published theme overlay (PLAN §9.11), when the connected
    /// burrow ships one. Layered on top of the chosen pack (below the editor's
    /// custom slot) unless the user has disabled server theming.
    pub server_theme: RwSignal<Option<ServerOverlay>>,
    /// The user's opt-out of server theming (persisted). `true` = ignore any
    /// server overlay and render the user's own pack/mode choice only.
    pub server_theme_disabled: RwSignal<bool>,
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
            syndication: create_rw_signal(SynAdminState::default()),
            is_admin: create_rw_signal(false),
            palette_open: create_rw_signal(false),
            servers: create_rw_signal(crate::servers::sample_directory()),
            pending_endpoint: create_rw_signal(None),
            toasts: create_rw_signal(crate::toasts::ToastQueue::default()),
            live: create_rw_signal(false),
            #[cfg(target_arch = "wasm32")]
            ws: store_value(crate::ws::WsClient::new()),
            theme: create_rw_signal(initial_theme_choice()),
            custom_pack: create_rw_signal(None),
            server_theme: create_rw_signal(None),
            server_theme_disabled: create_rw_signal(initial_server_theme_disabled()),
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

    /// Apply a server-published theme bundle to this session (from the welcome
    /// frame / `ThemeGet`). An all-empty bundle clears any prior server theme.
    pub fn apply_server_theme(&self, bundle: &ThemeBundle) {
        let overlay = ServerOverlay::from_bundle(bundle);
        self.server_theme
            .set((!overlay.is_empty()).then_some(overlay));
    }

    /// Drop the current server theme (e.g. on disconnect).
    pub fn clear_server_theme(&self) {
        self.server_theme.set(None);
    }

    /// The connected server's theme name, if it ships one — labels the opt-out
    /// control in [`crate::components::ThemeToggle`].
    pub fn server_theme_name(&self) -> Option<String> {
        self.server_theme
            .with(|s| s.as_ref().map(|o| o.name.clone()))
    }

    /// Turn server theming on/off for this user (persisted), per PLAN §9.11's
    /// "user can disable server theming" rail.
    pub fn set_server_theme_disabled(&self, disabled: bool) {
        self.server_theme_disabled.set(disabled);
        #[cfg(target_arch = "wasm32")]
        crate::server_theme::storage::save_disabled(disabled);
    }

    /// Load the mock's seeded server theme bundle so the overlay + opt-out are
    /// demonstrable in dev (the real transport delivers this in the welcome
    /// frame). Mirrors [`AppState::load_radio`].
    pub fn load_server_theme(&self) {
        let bundle = self.client.with_value(|c| c.server_theme_bundle());
        match bundle {
            Some(b) => self.apply_server_theme(&b),
            None => self.clear_server_theme(),
        }
    }

    /// Persist the current theme choice (browser only; no-op on the host).
    fn persist_theme(&self) {
        #[cfg(target_arch = "wasm32")]
        crate::theme_css::storage::save_choice(self.theme.get_untracked());
    }

    /// Open a **live** RHP session over WebSocket to a real burrow (wasm only),
    /// folding the transport's events into the reactive state: api events
    /// through [`UiState::apply`], connection-lifecycle states through
    /// [`UiState::set_conn`], and routed notices through the radio reducer /
    /// notice log. The default seeded [`MockClient`] path is untouched.
    #[cfg(target_arch = "wasm32")]
    pub fn connect_live(&self, endpoint: String, login: String, password: String) {
        use crate::wire::EventClient;
        use rabbithole_core::api::{Command, Event};
        let state = self.state;
        let toasts = self.toasts;
        let radio = self.radio;
        let files = self.files;
        let ws_sv = self.ws;
        self.ws.update_value(|ws| {
            ws.on_event(std::rc::Rc::new(move |event| {
                if let Event::Connected { server_name, .. } = &event {
                    let name = server_name.clone();
                    toasts.update(|q| {
                        q.push(
                            crate::toasts::ToastKind::Success,
                            format!("Connected to {name}"),
                        );
                    });
                    // Authenticate once the handshake lands. We can't dispatch
                    // from inside the transport's own borrow, so defer to the
                    // next microtask.
                    if !login.is_empty() {
                        let (login, password) = (login.clone(), password.clone());
                        wasm_bindgen_futures::spawn_local(async move {
                            ws_sv.update_value(|c| {
                                c.dispatch(Command::SignIn { login, password });
                                // Pull the initial roster once signed in.
                                c.request_who();
                            });
                        });
                    }
                }
                state.update(|s| s.apply(&event));
            }));
            ws.on_conn(std::rc::Rc::new(move |c| {
                // Toast the drop edge exactly once (Online → Reconnecting);
                // every backoff attempt re-emits Reconnecting, so guard on the
                // transition to avoid spamming.
                let prev = state.with_untracked(|s| s.conn);
                if prev == crate::conn::ConnState::Online
                    && c == crate::conn::ConnState::Reconnecting
                {
                    toasts.update(|q| {
                        q.push(
                            crate::toasts::ToastKind::Warn,
                            "Connection lost \u{2014} reconnecting\u{2026}",
                        );
                    });
                }
                state.update(|s| s.set_conn(c));
            }));
            ws.on_who(std::rc::Rc::new(move |roster| {
                state.update(|s| s.who = roster)
            }));
            ws.on_presence(std::rc::Rc::new(move |delta| {
                state.update(|s| match delta {
                    crate::wire::PresenceDelta::Joined(name) => {
                        if !s.who.contains(&name) {
                            s.who.push(name);
                        }
                    }
                    crate::wire::PresenceDelta::Left(name) => s.who.retain(|h| h != &name),
                })
            }));
            ws.on_boards(std::rc::Rc::new(move |boards| {
                state.update(|s| s.set_boards(boards))
            }));
            ws.on_threads(std::rc::Rc::new(move |threads| {
                state.update(|s| s.threads = threads)
            }));
            ws.on_posts(std::rc::Rc::new(move |posts| {
                state.update(|s| s.posts = posts)
            }));
            ws.on_dm_threads(std::rc::Rc::new(move |threads| {
                state.update(|s| s.set_dm_threads(threads))
            }));
            ws.on_dm_history(std::rc::Rc::new(move |msgs| {
                // Apply to the currently-selected conversation (the reply
                // doesn't name its peer; the single ordered socket keeps this
                // matched to the history we just requested).
                state.update(|s| {
                    if let Some(id) = s.selected_dm.clone() {
                        s.set_dm_messages(&id, msgs);
                    }
                })
            }));
            ws.on_dm_received(std::rc::Rc::new(move |(peer, msg)| {
                state.update(|s| s.receive_dm(&peer, msg))
            }));
            ws.on_file_event(std::rc::Rc::new(move |event| {
                files.update(|f| f.apply(&event))
            }));
            ws.on_members(std::rc::Rc::new(move |members| {
                // The directory list has no presence flag; fill `online` from
                // the live roster.
                state.update(|s| {
                    let present: std::collections::HashSet<String> =
                        s.who.iter().cloned().collect();
                    let mut members = members;
                    for m in &mut members {
                        m.online = present.contains(&m.handle);
                    }
                    s.set_members(members);
                })
            }));
            ws.on_profile(std::rc::Rc::new(move |profile| {
                state.update(|s| s.set_profile(profile))
            }));
            ws.on_notice(std::rc::Rc::new(move |route| match route {
                crate::wire::NoticeRoute::Radio(u) => radio.update(|r| r.apply_update(u)),
                crate::wire::NoticeRoute::Chat { from, text } => {
                    state.update(|s| s.push_notice(&from, &text))
                }
            }));
            ws.dispatch(Command::Connect {
                endpoint: endpoint.clone(),
                pinned_fingerprint: None,
            });
        });
        self.live.set(true);
    }

    /// Host stub: no socket off-target, so this only flips the live flag.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect_live(&self, _endpoint: String, _login: String, _password: String) {
        self.live.set(true);
    }

    /// Manually redial the live socket now (the reconnect banner's button).
    pub fn reconnect(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| c.redial());
        }
    }

    /// Whether the session is currently live-connected. Reactive (reads the
    /// conn signal), so views can gate composers on it.
    pub fn online(&self) -> bool {
        self.state.with(|s| s.conn.is_live())
    }

    /// Send a lobby chat line — over the live socket when connected, else
    /// through the seeded mock seam.
    pub fn send_chat(&self, text: String) {
        let room = crate::client::LOBBY.to_string();
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            use crate::wire::EventClient;
            self.ws.update_value(|c| {
                c.dispatch(rabbithole_core::api::Command::SendChat {
                    room: room.clone(),
                    text: text.clone(),
                });
            });
            return;
        }
        self.dispatch(rabbithole_core::api::Command::SendChat { room, text });
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
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            // Live: request over the socket; the reply folds through the sink.
            self.ws.update_value(|c| c.request_boards());
            return;
        }
        let boards = self.client.with_value(|c| c.boards());
        self.state.update(|s| s.set_boards(boards));
    }

    /// Select a board and load its threads into state.
    pub fn select_board(&self, slug: &str) {
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            // Reset the board view; the thread list arrives via the sink.
            self.state.update(|s| s.select_board(slug, Vec::new()));
            self.ws.update_value(|c| c.request_threads(slug));
            return;
        }
        let threads = self.client.with_value(|c| c.threads(slug));
        self.state.update(|s| s.select_board(slug, threads));
    }

    /// Open a thread and load its posts into state.
    pub fn open_thread(&self, id: String) {
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            // Open the thread immediately; its posts stream in via the sink.
            // The live thread id is the root post's hex id.
            let root = crate::wire::hex_to_id(&id);
            self.state.update(|s| s.open_thread(id, Vec::new()));
            if let Some(root) = root {
                self.ws.update_value(|c| c.request_posts(root));
            }
            return;
        }
        let posts = self.client.with_value(|c| c.posts(&id));
        self.state.update(|s| s.open_thread(id, posts));
    }

    /// Start a new thread on `board`. Live: post it, then re-request the thread
    /// list (the connection is ordered, so the new thread is included). Mock:
    /// prepend it locally so the demo composer stays interactive.
    pub fn post_thread(&self, board: &str, subject: &str, body: &str) {
        if subject.trim().is_empty() {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| {
                c.send_post(board, subject, body);
                c.request_threads(board);
            });
            return;
        }
        // The mock models a thread by its subject; the first-post body is only
        // sent over a live transport.
        let _ = body;
        let (board, subject) = (board.to_string(), subject.to_string());
        self.state.update(|s| {
            let id = format!("tnew{}", s.threads.len());
            s.threads.insert(
                0,
                crate::state::Thread {
                    id,
                    board,
                    title: subject,
                    author: "you".to_string(),
                },
            );
        });
    }

    /// Reply to the currently open thread. Live: post the reply (parent = the
    /// open thread's root id), then re-request the thread's posts so the reply
    /// appears. Mock: append a local post to the open thread.
    pub fn post_reply(&self, body: &str) {
        if body.trim().is_empty() {
            return;
        }
        let Some(thread_id) = self.state.with_untracked(|s| s.selected_thread.clone()) else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            let board = self
                .state
                .with_untracked(|s| s.selected_board.clone())
                .unwrap_or_default();
            if let Some(root) = crate::wire::hex_to_id(&thread_id) {
                self.ws.update_value(|c| {
                    c.send_reply(&board, root, body);
                    c.request_posts(root);
                });
            }
            return;
        }
        let body = body.to_string();
        self.state.update(|s| {
            let id = format!("pnew{}", s.posts.len());
            s.posts.push(crate::state::Post {
                id,
                thread: thread_id,
                author: "you".to_string(),
                body,
            });
        });
    }

    /// Whether the mock seed loaders should no-op: they must not fold seeded
    /// [`MockClient`] data into a **live** session (DMs / members / files /
    /// radio are not wired over the socket yet, so the view stays empty rather
    /// than showing fabricated data). Always `false` off-wasm.
    fn skip_mock_load(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.live.get_untracked()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    }

    /// Load the DM conversation snapshots into state. Live: request the
    /// conversation list over the socket (the reply folds through the sink).
    pub fn load_dms(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| c.request_dm_threads());
            return;
        }
        let threads = self.client.with_value(|c| c.dm_threads());
        self.state.update(|s| s.set_dm_threads(threads));
    }

    /// Select a DM conversation with `peer`. Live: request its history (the
    /// reply folds into the selected thread via the sink).
    pub fn select_dm(&self, peer: &str) {
        self.state.update(|s| s.select_dm(peer));
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| c.request_dm_history(peer));
        }
    }

    /// Send a DM into the selected conversation. Live: send over the socket,
    /// then re-request the history so the sent message appears. Mock: append it
    /// locally.
    pub fn send_dm(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let Some(id) = self.state.with_untracked(|s| s.selected_dm.clone()) else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| {
                c.send_dm(&id, text);
                c.request_dm_history(&id);
            });
            return;
        }
        let state = self.state;
        self.client.update_value(|c| {
            if let Some(msg) = c.send_dm(&id, text) {
                state.update(|s| s.append_dm(&id, msg));
            }
        });
    }

    /// Load the member directory snapshot into state. Live: request the
    /// directory over the socket (empty query = list all; reply folds via sink).
    pub fn load_members(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| c.request_directory(""));
            return;
        }
        let members = self.client.with_value(|c| c.members());
        self.state.update(|s| s.set_members(members));
    }

    /// Select a member and (live) fetch their full profile card.
    pub fn select_member(&self, handle: &str) {
        self.state.update(|s| s.select_member(handle));
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| c.request_profile(handle));
        }
    }

    /// Drive one [`FileCommand`] through the seam and fold its file events into
    /// the [`FilesState`].
    fn dispatch_file(&self, command: FileCommand) {
        // Live: send over the socket; replies fold in through the file sink
        // registered in `connect_live`. Mock: drive the seam synchronously.
        #[cfg(target_arch = "wasm32")]
        if self.live.get_untracked() {
            self.ws.update_value(|c| c.dispatch_file(&command));
            return;
        }
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

    /// Drive one `GetConfig` for the Syndication & Gateways panel and fold
    /// its replies — paired with the requested `key` so the reducer knows
    /// which read failed (the wire's `Failed` carries no key).
    fn dispatch_syn_get(&self, key: &str) {
        let syndication = self.syndication;
        self.client.update_value(|client| {
            let events = client.dispatch_admin(AdminCommand::GetConfig {
                key: key.to_string(),
            });
            syndication.update(|s| s.apply_get_reply(key, &events));
        });
    }

    /// Drive one `SetConfig` for the panel, fold the paired reply, then
    /// re-read the key so the panel shows the authoritative stored value.
    fn dispatch_syn_set(&self, key: &str, command: AdminCommand) {
        let syndication = self.syndication;
        self.client.update_value(|client| {
            let events = client.dispatch_admin(command);
            syndication.update(|s| s.apply_set_reply(key, &events));
        });
        self.dispatch_syn_get(key);
    }

    /// Load every key the Syndication & Gateways panel shows (gateway
    /// toggles, listener addresses, the syndication knobs, and the
    /// TOML-only `syndication_feeds` attempt).
    pub fn load_syndication(&self) {
        for key in crate::syndication_admin::LOAD_KEYS {
            self.dispatch_syn_get(key);
        }
    }

    /// Flip a gateway/syndication boolean key, if it is loaded and parsable.
    pub fn syn_toggle(&self, key: &str) {
        let Some(command) = self.syndication.with_untracked(|s| s.toggle_command(key)) else {
            return;
        };
        self.dispatch_syn_set(key, command);
    }

    /// Update the poll-interval draft (inline validation happens in the
    /// reducer).
    pub fn syn_set_poll_draft(&self, draft: &str) {
        self.syndication.update(|s| s.set_poll_draft(draft));
    }

    /// Save the poll-interval draft, if valid and changed.
    pub fn syn_save_poll(&self) {
        let Some(command) = self.syndication.with_untracked(|s| s.poll_save_command()) else {
            return;
        };
        self.dispatch_syn_set(crate::syndication_admin::KEY_POLL_SECS, command);
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

    /// Raise a toast notification, returning its id (for targeted dismissal).
    pub fn notify(&self, kind: crate::toasts::ToastKind, text: impl Into<String>) -> u64 {
        let mut id = 0;
        self.toasts.update(|q| id = q.push(kind, text));
        id
    }

    /// Dismiss a toast by id.
    pub fn dismiss_toast(&self, id: u64) {
        self.toasts.update(|q| q.dismiss(id));
    }

    /// Load the mock's seeded radio notices into the radio state (each is a
    /// real `ServerNotice` push routed through the host-tested wire mapping),
    /// so the Radio view and status segment render in dev.
    pub fn load_radio(&self) {
        // In a live session the radio reducer is fed by real `[radio]` pushes
        // through the notice sink; never mix seeded mock stations in.
        if self.skip_mock_load() {
            return;
        }
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

/// Whether server theming starts disabled: the persisted opt-out on the
/// browser, else `false` (server themes apply by default, per PLAN §9.11).
fn initial_server_theme_disabled() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::server_theme::storage::load_disabled()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
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
///
/// Accessibility wiring at the root: the **skip link** is the first
/// focusable element on every page (`rel="external"` opts it out of the
/// router's click interception, so the browser performs the native in-page
/// jump to `<main id="rh-main">`), and [`RouteFocus`] moves focus to each
/// view's `<h1>` after client-side navigation.
#[component]
pub fn App() -> impl IntoView {
    let app = AppState::new();
    provide_context(app);

    let style = move || {
        let (pack, mode) = (app.theme.get().pack, app.mode());
        // Server theming layers below the editor's live preview and only when
        // the user hasn't switched it off.
        let show_server = !app.server_theme_disabled.get();
        app.custom_pack.with(|custom| {
            app.server_theme.with(|server| {
                let server = show_server.then_some(server.as_ref()).flatten();
                resolve_root_style(custom.as_ref(), server, pack, mode)
            })
        })
    };

    view! {
        <style>{STYLESHEET}</style>
        <Router>
            <div class="rh-app" style=style>
                <a class="rh-skip" href=crate::a11y::SKIP_HREF rel="external">
                    "Skip to main content"
                </a>
                <RouteFocus/>
                <CommandPalette/>
                <Toasts/>
                <Routes>
                    <Route path="/" view=Login/>
                    <Route path="/lobby" view=Lobby/>
                    <Route path="/boards" view=Boards/>
                    <Route path="/boards/:slug" view=BoardView/>
                    <Route path="/dms" view=Dms/>
                    <Route path="/directory" view=Directory/>
                    <Route path="/files" view=Files/>
                    <Route path="/radio" view=Radio/>
                    <Route path="/servers" view=ServerBrowser/>
                    <Route path="/art" view=ArtGallery/>
                    <Route path="/admin" view=Admin/>
                </Routes>
            </div>
        </Router>
    }
}

/// Focus management for client-side navigation: after every route change
/// (not the initial load, where the browser's own focus handling is right),
/// move focus to the new view's `<h1 id="rh-view-title">` — falling back to
/// `<main id="rh-main">` — via [`crate::a11y::focus_view_title`]. Without
/// this, keyboard and screen-reader users are stranded on the *previous*
/// page's (now unmounted) link and reading order silently resets to `<body>`.
///
/// Renders nothing; it only owns the effect (it must live inside the
/// `<Router>` to reach `use_location`). The DOM call is a wasm-gated
/// no-op on the host, so the effect itself is host-safe.
#[component]
fn RouteFocus() -> impl IntoView {
    let location = use_location();
    create_effect(move |prev: Option<String>| {
        let path = location.pathname.get();
        // Focus only on genuine transitions: `prev` is None on first run.
        if let Some(prev) = prev {
            if prev != path {
                crate::a11y::focus_view_title();
            }
        }
        path
    });
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
