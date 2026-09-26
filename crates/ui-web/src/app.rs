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
use crate::admin_view::Admin;
use crate::client::{MockClient, UiClient, LOBBY};
use crate::components::{
    About, ArtGallery, BoardView, Boards, CommandPalette, ConfirmDialog, Directory, Dms, Files,
    Lobby, Login, Nav, People, PersonPage, Radio, ServerBrowser, Settings, Toasts, Transfers,
    WelcomeSheet, WishingWell, You,
};
use crate::files::{join_path, FilesState};
use crate::packs::PackTokens;
use crate::radio::{clamp_volume, RadioPrefs, RadioState};
use crate::server_theme::ServerOverlay;
use crate::state::UiState;
use crate::syndication_admin::SynAdminState;
use crate::theme_css::{next_mode, next_pack, ThemeChoice, STYLESHEET};
use crate::wire::{AdminCommand, AdminEvent, FileCommand, FileEvent, NoticeRoute};

/// Identifies one connected burrow (a live server session). For now the initial
/// session is [`ServerId::local`]; live sessions will key on their normalized
/// dial endpoint (see `docs/design/client-experience.md`, the WarrenState refactor).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ServerId(pub String);

impl ServerId {
    /// The id of the initial (offline/mock, or first-connected) session.
    pub fn local() -> Self {
        ServerId("local".to_string())
    }

    /// Is this the app's **placeholder** session — the one that exists before
    /// you join anywhere, so there is always something to focus? It is not a
    /// place: it never appears in the rail, and a burrow route can't be shown
    /// against it ([`crate::palette::needs_a_burrow`]).
    pub fn is_placeholder(&self) -> bool {
        self.0 == "local"
    }
}

/// One server session's per-connection reactive state. `Copy` (a bundle of
/// signal handles), so [`AppState::focused`] can hand it out by value. The
/// warren shell (identity, People, Transfers, toasts, palette, theme choice)
/// lives above this on [`AppState`]; each burrow you're connected to is a
/// `Session`.
#[derive(Clone, Copy)]
pub struct Session {
    /// The flat UI model for this server, folded from its events.
    pub state: RwSignal<UiState>,
    /// This server's file-library model.
    pub files: RwSignal<FilesState>,
    /// Whether this session holds an admin capability on its server.
    pub is_admin: RwSignal<bool>,
    /// The role this session holds on its server (the `Role` ordinal), its
    /// effective capability mask, and the handle it signed in as. The burrow
    /// enforces what an operator may do; the console reads these so it never
    /// offers what would be refused ([`crate::admin_people`]).
    pub role: RwSignal<u8>,
    pub caps: RwSignal<u64>,
    pub handle: RwSignal<String>,
    /// How many times this session has signed in. A socket coming back is
    /// not the same as being signed in on it, and a pane that asked this
    /// burrow something before the drop has no answer coming: panes track
    /// this and ask again.
    pub ready: RwSignal<u64>,
    /// Authenticated on the current socket, cleared while reconnecting.
    pub authenticated: RwSignal<bool>,
    /// Whether this session is a guest: a handle with no account behind it.
    /// The server refuses a guest everything that is between accounts, so
    /// the DM view offers sign-in instead of asking and failing.
    pub is_guest: RwSignal<bool>,
    /// Whether this session is **live** (a real RHP-over-WebSocket transport)
    /// rather than the seeded [`MockClient`] demo.
    pub live: RwSignal<bool>,
    /// This server's published theme overlay (PLAN §9.11), if any.
    pub server_theme: RwSignal<Option<ServerOverlay>>,
    /// Stable original login, when authentication context retained it.
    pub theme_owner: RwSignal<Option<crate::theme_preferences::Owner>>,
    pub theme_choice: RwSignal<Option<crate::theme_preferences::ThemeMode>>,
    pub theme_sync: RwSignal<crate::theme_preferences::SyncState>,
    pub theme_revision: RwSignal<u64>,
    /// The burrow's display name, learned from the `Connected` handshake. `None`
    /// until connected (the rail tile falls back to the endpoint host).
    pub name: RwSignal<Option<String>>,
    /// How many lobby lines the user has seen in this burrow — the message
    /// count at the moment they last focused it (or focused away from it).
    /// `messages.len() - seen` on an unfocused session is its rail-tile
    /// unread count; the focused session always reads as caught up.
    pub seen: RwSignal<usize>,
    /// This server's live browser WebSocket transport. wasm-only.
    #[cfg(target_arch = "wasm32")]
    pub(crate) ws: StoredValue<crate::ws::WsClient>,
    /// This session's command seam. `MockClient` today; a real transport later.
    client: StoredValue<MockClient>,
}

impl Session {
    /// The transport captures this session at registration time. Receiving a
    /// theme while another burrow is focused must only change its owner.
    fn set_server_theme(self, overlay: Option<ServerOverlay>) {
        self.server_theme.set(overlay);
    }
}

/// How a live session authenticates: a fresh password sign-in, or resuming a
/// prior session with a persisted bearer token (auto-reconnect on load).
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
enum AuthMethod {
    Password { login: String, password: String },
    Resume { token: String },
}

#[cfg(target_arch = "wasm32")]
struct SignInMemory {
    login: Option<String>,
    keep_recent: bool,
    save_bookmark: bool,
    bookmark: Option<crate::bookmarks::Bookmark>,
    saved_once: bool,
}

/// How many accounts one look at the People pane brings back. The burrow
/// clamps to 200; searching asks it again rather than paging by hand.
const ACCOUNT_PAGE: u32 = 100;

/// Run `f` once the current call has unwound. A reply sink runs inside the
/// transport's own borrow, so anything in it that wants to send again has to
/// wait a tick. Off the browser there is no such borrow, and no event loop.
pub(crate) fn defer(f: impl FnOnce() + 'static) {
    #[cfg(target_arch = "wasm32")]
    leptos::set_timeout(f, std::time::Duration::ZERO);
    #[cfg(not(target_arch = "wasm32"))]
    f();
}

/// Reactive, `Copy` handle bundle shared through context.
#[derive(Clone, Copy)]
pub struct AppState {
    /// Every connected burrow, insertion-ordered, keyed by [`ServerId`]. The
    /// burrow rail renders this list; a reactive signal so adding/removing a
    /// session re-renders the rail.
    sessions: RwSignal<Vec<(ServerId, Session)>>,
    /// Which session's "place" is currently in the main pane.
    focused_id: RwSignal<ServerId>,
    /// The user's presence status, a warren-layer choice fanned to **every**
    /// connected burrow (set once, applies everywhere).
    pub presence: RwSignal<rabbithole_proto::presence::PresenceState>,
    /// The portable identity (public face), set once at launch. `None` until
    /// loaded (and always `None` in host tests, which have no browser storage).
    pub you: RwSignal<Option<crate::identity::You>>,
    /// Chimes on/off (on by default; a saved choice takes precedence).
    pub sound_on: RwSignal<bool>,
    /// The full signing identity (holds the secret seed), loaded at launch.
    /// Needed to *sign* friendship attestations; `you` is the public face.
    /// `None` in host tests and until the browser seed loads.
    pub identity: StoredValue<Option<crate::identity::Identity>>,
    /// The cross-burrow sightings ledger — where you know each person from,
    /// persisted. Fed by every roster that arrives ([`crate::sightings`]).
    pub sightings: RwSignal<Vec<crate::sightings::Sighting>>,
    /// The picture being looked at: which file it is, its name, and its
    /// bytes. A file the gallery asked for arrives here instead of being
    /// saved; everything else a person downloads still goes to disk.
    pub art_open: RwSignal<Option<crate::state::ArtOpen>>,
    /// Your chosen warren mark, if you've picked one instead of the mark your
    /// key derives ([`crate::avatar::ChosenMark`]). Local-only: the wire has
    /// no mark field, so this changes what *you* see.
    pub my_mark: RwSignal<Option<crate::avatar::ChosenMark>>,
    /// App settings: trackers and the handful of choices that are yours rather
    /// than a burrow's ([`crate::settings`]). Persisted.
    pub settings: RwSignal<crate::settings::Settings>,
    /// Unpublished profile edits, kept only for this app session and scoped
    /// to the burrow plus the server-confirmed persona.
    pub profile_drafts: StoredValue<crate::profile_edit::DraftCache>,
    /// Signed friendships and half-offers, persisted ([`crate::friend`]).
    pub friends: RwSignal<Vec<crate::friend::Friendship>>,
    /// The web-admin model, folded from admin events.
    pub admin: RwSignal<AdminState>,
    /// The focused burrow's settings: what it described, what is staged, and
    /// what came of the last save ([`crate::admin_settings`]).
    pub admin_settings: RwSignal<crate::admin_settings::SettingsState>,
    /// Which burrow [`Self::admin_settings`] belongs to. Drafts are edits to
    /// one burrow; they must never be saved into another because the focus
    /// moved while they were staged.
    pub admin_settings_owner: RwSignal<Option<ServerId>>,
    /// The People pane's invitations and the outcome of its last action
    /// ([`crate::admin_people`]).
    pub people: RwSignal<crate::admin_people::PeopleState>,
    /// The Moderation pane's queue, deny list and audit log
    /// ([`crate::admin_moderation`]).
    pub moderation: RwSignal<crate::admin_moderation::ModerationState>,
    /// The Peers and Backups panes' lists ([`crate::admin_federation`]).
    pub federation: RwSignal<crate::admin_federation::FederationState>,
    /// The Syndication & Gateways panel model, folded from paired config
    /// get/set replies ([`crate::syndication_admin`]).
    pub syndication: RwSignal<SynAdminState>,
    /// Whether the ⌘K command palette overlay is open. Shared so both the
    /// header affordance and the global key binding drive the one overlay.
    pub palette_open: RwSignal<bool>,
    /// Whether the phone-width **warren sheet** is open — the bottom sheet
    /// that stands in for the burrow rail where there is no room for one
    /// ([`WarrenSheet`]).
    pub switcher_open: RwSignal<bool>,
    /// The Looking Glass server-browser directory ([`crate::servers`]).
    pub servers: RwSignal<Vec<crate::servers::DirectoryServer>>,
    /// Where the current listing came from — shown, because "who told you
    /// this" is part of the answer to "who is out there".
    pub directory_source: RwSignal<crate::servers::DirectorySource>,
    /// Whether a directory refresh is in flight.
    pub directory_loading: RwSignal<bool>,
    /// The burrows you chose to keep ([`crate::bookmarks`]), persisted.
    pub bookmarks: RwSignal<Vec<crate::bookmarks::Bookmark>>,
    /// What knocking on unlisted burrows found ([`crate::probe`]).
    pub probes: RwSignal<crate::connect::Probes>,
    /// An endpoint chosen in the server browser, handed to the login screen to
    /// prefill on its next mount (then cleared).
    pub pending_endpoint: RwSignal<Option<String>>,
    pub pending_login: RwSignal<Option<String>>,
    pub pending_bookmark: RwSignal<Option<String>>,
    /// Why the connect form is open, when it is open because something went
    /// wrong ("Your session on Wonderland expired. Sign in again."). Shown
    /// once by the form, then cleared.
    pub pending_notice: RwSignal<Option<String>>,
    /// A question the app is asking before it does something that can't be
    /// taken back (leaving a burrow). `None` = no dialog.
    pub confirm: RwSignal<Option<ConfirmAsk>>,
    /// The "Send to another burrow" dialog, while it is open.
    pub sending: RwSignal<Option<SendAsk>>,
    /// Transient toast notifications — humanized-event moments
    /// ([`crate::toasts`]).
    pub toasts: RwSignal<crate::toasts::ToastQueue>,
    /// The user's appearance choice: theme pack (Clean/Retro/HighContrast)
    /// plus mode policy (System/Light/Dark). The effective [`Mode`] is
    /// derived from this plus the OS hint via [`AppState::mode`].
    pub theme: RwSignal<ThemeChoice>,
    /// Live operating-system appearance, so System follows changes without a reload.
    pub system_dark: RwSignal<bool>,
    /// The theme editor's **custom pack override slot**: when set, these
    /// tokens replace the built-in pack for this session (mode resolution
    /// still applies). Session-local and unpersisted — Apply is a preview,
    /// not a save.
    pub custom_pack: RwSignal<Option<PackTokens>>,
    /// Device default plus overrides bound to original account logins.
    pub theme_preferences: RwSignal<crate::theme_preferences::ThemePreferences>,
    pub theme_storage_ok: RwSignal<bool>,
    /// Radio now-playing per station, folded from routed `[radio]` notices.
    pub radio: RwSignal<RadioState>,
    /// The user's radio player preferences (enable/volume/mute/station plus
    /// the Icecast delivery address), persisted to `localStorage`.
    pub radio_prefs: RwSignal<RadioPrefs>,
    /// What the audio element actually did, distinct from saved listening intent.
    pub radio_playback: RwSignal<crate::playback::PlaybackStatus>,
    /// Where downloads go, in the desktop shell (which owns the preference).
    /// `None` in a browser tab, where the browser decides.
    pub download_prefs: RwSignal<Option<crate::save::DownloadPrefs>>,
    /// Cover art fetched for the radio, as `data:` URLs keyed by blob hex.
    pub radio_covers: RwSignal<std::collections::HashMap<String, String>>,
    /// The wasm-only `<audio>` element wrapper the preference setters keep in
    /// sync ([`crate::player`]). Absent on the host, where there is no DOM.
    #[cfg(target_arch = "wasm32")]
    player: StoredValue<crate::player::RadioPlayer>,
}

impl AppState {
    /// Create the shared state for a fresh session.
    pub fn new() -> Self {
        let session = Session {
            state: create_rw_signal(UiState::default()),
            files: create_rw_signal(FilesState::default()),
            is_admin: create_rw_signal(false),
            is_guest: create_rw_signal(false),
            role: create_rw_signal(0),
            caps: create_rw_signal(0),
            handle: create_rw_signal(String::new()),
            ready: create_rw_signal(0),
            authenticated: create_rw_signal(false),
            live: create_rw_signal(false),
            server_theme: create_rw_signal(None),
            theme_owner: create_rw_signal(None),
            theme_choice: create_rw_signal(None),
            theme_sync: create_rw_signal(Default::default()),
            theme_revision: create_rw_signal(0),
            name: create_rw_signal(None),
            seen: create_rw_signal(0),
            #[cfg(target_arch = "wasm32")]
            ws: store_value(crate::ws::WsClient::new()),
            client: store_value(MockClient::new()),
        };
        let radio_playback = create_rw_signal(crate::playback::PlaybackStatus::Idle);
        Self {
            sessions: create_rw_signal(vec![(ServerId::local(), session)]),
            focused_id: create_rw_signal(ServerId::local()),
            presence: create_rw_signal(rabbithole_proto::presence::PresenceState::Online),
            you: create_rw_signal(None),
            identity: store_value(None),
            sightings: create_rw_signal(Vec::new()),
            art_open: create_rw_signal(None),
            friends: create_rw_signal(Vec::new()),
            settings: create_rw_signal(crate::settings::Settings::default()),
            profile_drafts: store_value(crate::profile_edit::DraftCache::default()),
            my_mark: create_rw_signal(None),
            #[cfg(target_arch = "wasm32")]
            sound_on: create_rw_signal(crate::sound::enabled()),
            #[cfg(not(target_arch = "wasm32"))]
            sound_on: create_rw_signal(true),
            admin: create_rw_signal(AdminState::default()),
            admin_settings: create_rw_signal(Default::default()),
            admin_settings_owner: create_rw_signal(None),
            people: create_rw_signal(Default::default()),
            moderation: create_rw_signal(Default::default()),
            federation: create_rw_signal(Default::default()),
            syndication: create_rw_signal(SynAdminState::default()),
            palette_open: create_rw_signal(false),
            switcher_open: create_rw_signal(false),
            servers: create_rw_signal(crate::servers::sample_directory()),
            directory_source: create_rw_signal(crate::servers::DirectorySource::Seeded),
            directory_loading: create_rw_signal(false),
            bookmarks: create_rw_signal({
                #[cfg(target_arch = "wasm32")]
                {
                    crate::bookmarks::load()
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    Vec::new()
                }
            }),
            probes: create_rw_signal(Default::default()),
            pending_endpoint: create_rw_signal(None),
            pending_login: create_rw_signal(None),
            pending_bookmark: create_rw_signal(None),
            pending_notice: create_rw_signal(None),
            confirm: create_rw_signal(None),
            sending: create_rw_signal(None),
            toasts: create_rw_signal(crate::toasts::ToastQueue::default()),
            theme: create_rw_signal(initial_theme_choice()),
            system_dark: create_rw_signal(os_prefers_dark()),
            custom_pack: create_rw_signal(None),
            theme_preferences: create_rw_signal({
                #[cfg(target_arch = "wasm32")]
                {
                    crate::theme_preferences::storage::load()
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    Default::default()
                }
            }),
            theme_storage_ok: create_rw_signal(true),
            radio: create_rw_signal(RadioState::default()),
            radio_prefs: create_rw_signal(initial_radio_prefs()),
            radio_playback,
            radio_covers: create_rw_signal(Default::default()),
            download_prefs: create_rw_signal(None),
            #[cfg(target_arch = "wasm32")]
            player: store_value(crate::player::RadioPlayer::with_status(move |status| {
                radio_playback.try_set(status);
            })),
        }
    }

    /// The session whose "place" is currently in the main pane. `Copy`, so
    /// callers use `app.focused().state`, `.files`, `.live`, etc. exactly where
    /// they used the old flat `app.state` fields. For Wave A there is one
    /// session and focus never changes; Wave B makes focus reactive + switchable.
    /// The focused session, but as a **reactive** read — re-runs the calling
    /// reactive scope when focus changes (unlike [`focused`](Self::focused),
    /// which reads untracked). Use in views that must follow the focused burrow.
    pub fn focused_tracked(&self) -> Session {
        let _ = self.focused_id.get();
        self.focused()
    }

    /// Ask a pane again after each sign-in. A live socket opening is not yet
    /// a session: wait for AuthOk before the first read and after reconnect.
    /// Defer live reads because authentication is announced inside the socket
    /// callback; demo reads remain immediate and need no authentication.
    pub fn each_sign_in(&self, ask: impl Fn() + 'static) {
        let app = *self;
        let ask = std::rc::Rc::new(ask);
        create_effect(move |was: Option<Option<u64>>| {
            let session = app.focused_tracked();
            let live = session.live.get();
            if live && !session.authenticated.get() {
                return None;
            }
            let now = session.ready.get();
            if was != Some(Some(now)) {
                if live {
                    let ask = ask.clone();
                    defer(move || ask());
                } else {
                    ask();
                }
            }
            Some(now)
        });
    }

    /// The `files` signal of the session whose Transfers currently hold
    /// `transfer_id`, if any. Native swarm-progress events must route to the
    /// session that *started* the download — which may not be the focused one if
    /// the user switched burrows mid-transfer — so we resolve by transfer id, not
    /// by focus.
    pub fn transfer_session_files(&self, transfer_id: u64) -> Option<RwSignal<FilesState>> {
        self.sessions.with_untracked(|list| {
            list.iter()
                .find(|(_, s)| {
                    s.files
                        .with_untracked(|fs| fs.transfers.iter().any(|t| t.id == transfer_id))
                })
                .map(|(_, s)| s.files)
        })
    }

    pub fn focused(&self) -> Session {
        let id = self.focused_id.get_untracked();
        self.sessions.with_untracked(|list| {
            list.iter()
                .find(|(sid, _)| *sid == id)
                .map(|(_, session)| *session)
                .expect("the focused session is always present")
        })
    }

    /// The connected burrows for the rail: `(id, label, is_focused, conn,
    /// unread)`, reactive over the session list, the focus, each session's
    /// name, connection health, and scrollback growth. The label is the
    /// server's display name once known, else a short form of its id; unread
    /// is the lobby lines that landed since the user last had that burrow
    /// focused (always 0 for the focused one).
    pub fn burrow_tiles(&self) -> Vec<(ServerId, String, bool, crate::conn::ConnState, usize)> {
        let focused = self.focused_id.get();
        self.sessions.with(|list| {
            list.iter()
                // The placeholder is not a burrow. It used to render as a
                // "Demo — Offline" tile beside every live burrow, in shipped
                // builds too, where there is no demo to open.
                .filter(|(id, _)| !id.is_placeholder())
                .map(|(id, session)| {
                    // Prefer the burrow's handshake name, then a published theme
                    // name, then the endpoint host.
                    let name = session
                        .name
                        .get()
                        .or_else(|| {
                            session
                                .server_theme
                                .with(|t| t.as_ref().map(|o| o.name.clone()))
                        })
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| server_label(id));
                    let conn = session.state.with(|s| s.conn);
                    let unread = if *id == focused {
                        0
                    } else {
                        session
                            .state
                            .with(|s| s.messages.len())
                            .saturating_sub(session.seen.get())
                    };
                    (id.clone(), name, *id == focused, conn, unread)
                })
                .collect()
        })
    }

    /// Is any real burrow joined (live, or a seeded demo)? Reactive. False
    /// while only the placeholder exists.
    pub fn has_burrows(&self) -> bool {
        self.sessions
            .with(|list| list.iter().any(|(id, _)| !id.is_placeholder()))
    }

    /// Total unread lobby lines across every burrow you aren't currently viewing
    /// — reactive. Drives the browser-tab title so a backgrounded warren still
    /// tells you someone's talking.
    pub fn total_unread(&self) -> usize {
        self.burrow_tiles().iter().map(|(_, _, _, _, u)| u).sum()
    }

    /// The aggregated cross-server People list: everyone present on any
    /// connected burrow, coalesced by screen name (reactive over every session's
    /// roster).
    pub fn people(&self) -> Vec<crate::state::Person> {
        let rosters: Vec<(String, Vec<crate::state::Presence>)> = self.sessions.with(|list| {
            list.iter()
                .map(|(id, session)| {
                    let name = session
                        .name
                        .get()
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| server_label(id));
                    (name, session.state.with(|s| s.who.clone()))
                })
                .collect()
        });
        crate::state::merge_people(&rosters)
    }

    /// Every transfer across every connected burrow, tagged with the burrow it
    /// belongs to — the unified Transfers manager's list (reactive).
    pub fn all_transfers(&self) -> Vec<(String, crate::files::Transfer)> {
        self.sessions.with(|list| {
            list.iter()
                .flat_map(|(id, session)| {
                    let name = session
                        .name
                        .get()
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| server_label(id));
                    session.files.with(|f| {
                        f.transfers
                            .iter()
                            .map(|t| (name.clone(), t.clone()))
                            .collect::<Vec<_>>()
                    })
                })
                .collect()
        })
    }

    /// Focus a connected burrow (switch which place is in the main pane).
    ///
    /// Both ends of the switch mark their scrollback read: leaving a place
    /// means you saw everything in it, and arriving shows you everything —
    /// so unread badges only ever count lines that landed while you were
    /// somewhere else.
    pub fn focus(&self, id: &ServerId) {
        self.mark_read(&self.focused_id.get_untracked());
        self.set_focus(id.clone());
        self.mark_read(id);
    }

    /// Record that the user is caught up on a session's scrollback (its
    /// current message count becomes the "seen" watermark).
    fn mark_read(&self, id: &ServerId) {
        self.sessions.with_untracked(|list| {
            if let Some((_, session)) = list.iter().find(|(sid, _)| sid == id) {
                let len = session.state.with_untracked(|s| s.messages.len());
                session.seen.set(len);
            }
        });
    }

    /// Set the user's presence status and **fan it to every connected burrow** —
    /// one control, applied everywhere. A newly-joined burrow inherits the
    /// current status from [`connect_live`].
    pub fn set_presence(&self, state: rabbithole_proto::presence::PresenceState) {
        self.presence.set(state);
        #[cfg(target_arch = "wasm32")]
        self.sessions.with_untracked(|list| {
            for (_, session) in list {
                if session.live.get_untracked() {
                    session.ws.update_value(|c| c.set_presence(state, None));
                }
            }
        });
    }

    /// Ensure a distinct session exists for the live server at `endpoint` and
    /// focus it, keeping the offline "local" demo session (and any other
    /// burrows) intact. The rail then shows a tile for it; the reactive remount
    /// in [`App`] swaps the place to this session's signals.
    #[cfg(target_arch = "wasm32")]
    fn ensure_session(&self, endpoint: &str) {
        let id = ServerId(endpoint.to_string());
        let exists = self
            .sessions
            .with_untracked(|list| list.iter().any(|(sid, _)| *sid == id));
        if !exists {
            let session = Session {
                state: create_rw_signal(UiState::default()),
                files: create_rw_signal(FilesState::default()),
                is_admin: create_rw_signal(false),
                is_guest: create_rw_signal(false),
                role: create_rw_signal(0),
                caps: create_rw_signal(0),
                handle: create_rw_signal(String::new()),
                ready: create_rw_signal(0),
                authenticated: create_rw_signal(false),
                live: create_rw_signal(false),
                server_theme: create_rw_signal(None),
                theme_owner: create_rw_signal(None),
                theme_choice: create_rw_signal(None),
                theme_sync: create_rw_signal(Default::default()),
                theme_revision: create_rw_signal(0),
                name: create_rw_signal(None),
                seen: create_rw_signal(0),
                ws: store_value(crate::ws::WsClient::new()),
                client: store_value(MockClient::new()),
            };
            self.sessions
                .update(|list| list.push((id.clone(), session)));
        }
        self.set_focus(id);
    }

    /// Focus `id`, but only actually write the signal when it changes.
    ///
    /// `focused_id` drives the shell's remount: writing it rebuilds the whole
    /// routed tree. `RwSignal::set` notifies unconditionally — it does not
    /// compare — so re-focusing the burrow you are already on used to tear the
    /// live view down and stand a new one up. Anything mid-flight in the old
    /// tree (a board's thread request, here) then resolved against a disposed
    /// owner and panicked, which is why opening a board could leave its
    /// skeleton up forever.
    fn set_focus(&self, id: ServerId) {
        if self.focused_id.get_untracked() != id {
            self.focused_id.set(id);
        }
    }

    /// The effective appearance [`Mode`], resolved from the user's
    /// [`ThemeChoice`] and the OS `prefers-color-scheme` hint. Reactive on the
    /// theme signal.
    pub fn mode(&self) -> Mode {
        crate::theme_css::effective_mode(self.theme.get().mode, self.system_dark.get())
    }

    /// Choose a theme pack outright (Settings) and persist it.
    pub fn set_pack(&self, pack: rabbithole_core::theme::ThemePack) {
        self.custom_pack.set(None);
        self.theme.update(|c| c.pack = pack);
        self.persist_theme();
    }

    /// Choose light, dark or follow-the-system outright (Settings) and
    /// persist it.
    pub fn set_mode(&self, mode: crate::theme_css::ModeChoice) {
        self.theme.update(|c| c.mode = mode);
        self.persist_theme();
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
        self.focused()
            .set_server_theme((!overlay.is_empty()).then_some(overlay));
    }

    /// Drop the current server theme (e.g. on disconnect).
    pub fn clear_server_theme(&self) {
        self.focused().set_server_theme(None);
    }

    /// Global defaults are local to this device. Explicit account choices win.
    pub fn set_theme_default(&self, mode: crate::theme_preferences::ThemeMode) {
        self.theme_preferences.update(|prefs| prefs.default = mode);
        self.persist_theme_preferences();
    }

    pub fn set_burrow_theme(&self, choice: Option<crate::theme_preferences::ThemeMode>) {
        let session = self.focused();
        session.theme_choice.set(choice);
        if let Some(owner) = session.theme_owner.get_untracked() {
            self.theme_preferences
                .update(|prefs| prefs.set(owner, choice));
            self.persist_theme_preferences();
        }
        #[cfg(target_arch = "wasm32")]
        self.sync_theme_preference(session, true);
    }

    fn persist_theme_preferences(&self) {
        #[cfg(target_arch = "wasm32")]
        self.theme_storage_ok.set(
            self.theme_preferences
                .with_untracked(crate::theme_preferences::storage::save),
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn sync_theme_preference(&self, session: Session, write: bool) {
        use crate::theme_preferences::{SyncState, ThemeMode};
        use rabbithole_proto::welcome::{ThemePrefGet, ThemePrefSet, ThemePrefState};
        if !session.authenticated.get_untracked() || session.is_guest.get_untracked() {
            session.theme_sync.set(SyncState::Local);
            return;
        }
        session
            .theme_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        let revision = session.theme_revision.get_untracked();
        let ready = session.ready.get_untracked();
        let choice = session.theme_choice.get_untracked();
        let desired = choice.unwrap_or(ThemeMode::Full);
        let app = *self;
        session.theme_sync.set(if write {
            SyncState::Loading
        } else {
            SyncState::Checking
        });
        wasm_bindgen_futures::spawn_local(async move {
            let current = || {
                session.theme_revision.try_get_untracked() == Some(revision)
                    && session.ready.try_get_untracked() == Some(ready)
                    && session.authenticated.try_get_untracked() == Some(true)
                    && app.sessions.with_untracked(|sessions| {
                        sessions
                            .iter()
                            .any(|(_, active)| active.state == session.state)
                    })
            };
            if !current() {
                return;
            }
            // This microtask runs outside the transport's immutable event borrow.
            let reply = session
                .ws
                .with_value(|ws| {
                    if write {
                        ws.call_with_timeout(&ThemePrefSet::new(desired == ThemeMode::Off), 3000)
                    } else {
                        ws.call_with_timeout(&ThemePrefGet, 3000)
                    }
                })
                .await;
            if !current() {
                return;
            }
            let remote = reply
                .filter(|frame| frame.error.is_none())
                .and_then(|frame| frame.decode::<ThemePrefState>().and_then(Result::ok));
            let Some(remote) = remote else {
                session.theme_sync.set(SyncState::Unavailable);
                return;
            };
            if write && remote.disable_server_theme != (desired == ThemeMode::Off) {
                session.theme_sync.set(SyncState::Unavailable);
                return;
            }
            if !write {
                if choice.is_some() && remote.disable_server_theme != (desired == ThemeMode::Off) {
                    // An explicit saved local override is user intent. A default
                    // must never silently turn on an account's existing opt-out.
                    app.sync_theme_preference(session, true);
                    return;
                }
                if choice.is_none() && remote.disable_server_theme {
                    session.theme_choice.set(Some(ThemeMode::Off));
                    if let Some(owner) = session.theme_owner.get_untracked() {
                        app.theme_preferences
                            .update(|prefs| prefs.set(owner, Some(ThemeMode::Off)));
                        app.persist_theme_preferences();
                    }
                }
            }
            session.theme_sync.set(SyncState::Saved);
        });
    }

    /// The connected server's theme name, if it ships one — labels the opt-out
    /// control in [`crate::components::ThemeToggle`].
    pub fn server_theme_name(&self) -> Option<String> {
        self.focused()
            .server_theme
            .with(|s| s.as_ref().map(|o| o.name.clone()))
    }

    /// Load the mock's seeded server theme bundle so the overlay + opt-out are
    /// demonstrable in dev. Live transports fetch and verify their own signed
    /// theme after authentication. Mirrors [`AppState::load_radio`].
    pub fn load_server_theme(&self) {
        if self.focused().live.get_untracked() {
            return;
        }
        let bundle = self
            .focused()
            .client
            .with_value(|c| c.server_theme_bundle());
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
    /// Open a live session to `endpoint`, authenticating with a fresh password.
    #[cfg(target_arch = "wasm32")]
    pub fn connect_live(&self, endpoint: String, login: String, password: String) {
        self.connect_live_bookmarked(endpoint, login, password, false, None);
    }

    pub fn connect_live_bookmarked(
        &self,
        endpoint: String,
        login: String,
        password: String,
        save: bool,
        bookmark_id: Option<String>,
    ) {
        #[cfg(target_arch = "wasm32")]
        {
            let login = login.trim().to_string();
            let bookmark = self.bookmarks.with_untracked(|list| {
                bookmark_id
                    .as_deref()
                    .and_then(|id| crate::bookmarks::by_id(list, id))
                    .filter(|b| {
                        crate::bookmarks::credential_endpoint(&b.endpoint)
                            == crate::bookmarks::credential_endpoint(&endpoint)
                            && b.login
                                .as_ref()
                                .is_none_or(|saved| saved.eq_ignore_ascii_case(&login))
                    })
                    .or_else(|| crate::bookmarks::find_account(list, &endpoint, &login))
                    .or_else(|| {
                        list.iter().find(|b| {
                            b.login.is_none()
                                && crate::bookmarks::credential_endpoint(&b.endpoint)
                                    == crate::bookmarks::credential_endpoint(&endpoint)
                        })
                    })
                    .cloned()
            });
            let account_login = (!password.is_empty()).then(|| login.clone());
            self.connect_with(
                endpoint,
                AuthMethod::Password { login, password },
                SignInMemory {
                    login: account_login,
                    keep_recent: save,
                    save_bookmark: save,
                    bookmark,
                    saved_once: false,
                },
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (endpoint, login, password, save, bookmark_id);
            self.focused().live.set(true);
        }
    }

    pub fn connect_bookmark(&self, id: &str, keep: bool) {
        #[cfg(target_arch = "wasm32")]
        if let Some(bookmark) = self
            .bookmarks
            .with_untracked(|list| crate::bookmarks::by_id(list, id).cloned())
        {
            if let Some(token) = bookmark.token.clone() {
                self.connect_with(
                    bookmark.endpoint.clone(),
                    AuthMethod::Resume { token },
                    SignInMemory {
                        login: bookmark.login.clone(),
                        keep_recent: keep,
                        save_bookmark: keep,
                        bookmark: Some(bookmark),
                        saved_once: false,
                    },
                );
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (id, keep);
    }

    /// Auto-reconnect to `endpoint` by resuming a persisted session `token` — no
    /// password needed. Used on launch to restore your burrows.
    #[cfg(target_arch = "wasm32")]
    pub fn reconnect_live(&self, endpoint: String, token: String) {
        let bookmark = self.bookmarks.with_untracked(|list| {
            list.iter()
                .find(|b| {
                    b.token.as_ref() == Some(&token)
                        && crate::bookmarks::credential_endpoint(&b.endpoint)
                            == crate::bookmarks::credential_endpoint(&endpoint)
                })
                .cloned()
        });
        let login = bookmark.as_ref().and_then(|b| b.login.clone()).or_else(|| {
            crate::recent::load()
                .into_iter()
                .find(|b| {
                    b.token.as_ref() == Some(&token)
                        && crate::bookmarks::credential_endpoint(&b.endpoint)
                            == crate::bookmarks::credential_endpoint(&endpoint)
                })
                .map(|b| b.handle)
        });
        self.connect_with(
            endpoint,
            AuthMethod::Resume { token },
            SignInMemory {
                login,
                keep_recent: true,
                save_bookmark: bookmark.is_some(),
                bookmark,
                saved_once: false,
            },
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn connect_with(&self, endpoint: String, auth: AuthMethod, memory: SignInMemory) {
        use crate::wire::EventClient;
        use rabbithole_core::api::{Command, Event};
        let endpoint = crate::bookmarks::credential_endpoint(&endpoint).unwrap_or(endpoint);
        let memory = std::rc::Rc::new(std::cell::RefCell::new(memory));
        // Accounts share one active connection per burrow. Switching accounts
        // must drop the old account's data and permissions before opening it.
        if self
            .sessions
            .with_untracked(|list| list.iter().any(|(sid, _)| sid.0 == endpoint))
        {
            self.disconnect(&ServerId(endpoint.clone()));
        }
        // Give this live server its own session (keyed by endpoint) + focus it,
        // so the offline demo and any other burrows stay put. Everything below
        // then binds to the new session via `self.focused()`.
        self.ensure_session(&endpoint);
        let theme_session = self.focused();
        let state = self.focused().state;
        let toasts = self.toasts;
        let radio = self.radio;
        let files = self.focused().files;
        let session_name = self.focused().name;
        let is_admin = self.focused().is_admin;
        let is_guest = self.focused().is_guest;
        let my_role = self.focused().role;
        let my_caps = self.focused().caps;
        let my_login = self.focused().handle;
        let session_ready = self.focused().ready;
        let session_authenticated = self.focused().authenticated;
        let session_seen = self.focused().seen;
        let presence = self.presence;
        let ws_sv = self.focused().ws;
        // Endpoint captured for both the "connected" toast/label and, on a
        // successful auth, persisting the resume token + handle for next launch.
        let ep = endpoint.clone();
        let resuming = matches!(auth, AuthMethod::Resume { .. });
        let so_app = *self;
        let so_name = self.focused().name;
        // Our own handle on this burrow (from AuthOk), so a chat line echoed back
        // to us never raises a notification about ourselves.
        let my_handle = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let notify_handle = my_handle.clone();
        let dm_handle = my_handle.clone();
        let sound_on = self.sound_on;
        let dm_sound_on = self.sound_on;
        #[cfg(target_arch = "wasm32")]
        let chime_app = *self;
        let notify_name = self.focused().name;
        // Sightings + friendship need `self` inside the sinks; `AppState` is
        // Copy, so clone the handle rather than borrowing across the closures.
        let app_for_sightings = *self;
        let fr_app = *self;
        let who_endpoint = endpoint.clone();
        let who_endpoint_label = endpoint.clone();
        let who_name = self.focused().name;
        let who_burrow = move || {
            who_name
                .get_untracked()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| server_label(&ServerId(who_endpoint_label.clone())))
        };
        self.focused().ws.update_value(|ws| {
            ws.on_theme(std::rc::Rc::new(move |overlay| {
                theme_session.set_server_theme(overlay);
            }));
            let failure_endpoint = endpoint.clone();
            let failure_memory = memory.clone();
            ws.on_auth_failure(std::rc::Rc::new(move |failure| {
                let id = ServerId(failure_endpoint.clone());
                let failure_memory = failure_memory.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    // The rejected attempt may already have been replaced or
                    // disconnected by the time this deferred callback runs.
                    let same_session = so_app.sessions.with_untracked(|list| {
                        list.iter()
                            .any(|(sid, session)| *sid == id && session.ws == ws_sv)
                    });
                    if !same_session
                        || !ws_sv
                            .try_with_value(|c| c.auth_failure_is_current(failure))
                            .unwrap_or(false)
                    {
                        return;
                    }
                    let burrow = so_name
                        .get_untracked()
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| server_label(&id));
                    let notice = if resuming && failure.code == rabbithole_proto::ErrorCode::SessionExpired {
                        if let Some(login) = &failure_memory.borrow().login {
                            crate::recent::remember_account_token(&id.0, login, "");
                        }
                        if let Some(bookmark) = &failure_memory.borrow().bookmark {
                            so_app.forget_bookmark_signin(&bookmark.id);
                        }
                        format!("Your session on {burrow} expired. Sign in again.")
                    } else if resuming {
                        format!("{burrow} could not resume this sign-in ({:?}). Your saved sign-in has been kept; try again.", failure.code)
                    } else if failure.code == rabbithole_proto::ErrorCode::Unauthenticated {
                        format!("{burrow} didn’t accept that handle and password.")
                    } else {
                        format!("{burrow} didn’t accept that sign-in ({:?}).", failure.code)
                    };
                    so_app.pending_login.set(failure_memory.borrow().login.clone());
                    so_app.pending_bookmark.set(failure_memory.borrow().bookmark.as_ref().map(|b| b.id.clone()));
                    so_app.sign_out(&id, notice);
                });
            }));
            ws.on_event(std::rc::Rc::new(move |event| {
                match &event {
                    Event::Connected { server_name, .. } => {
                        let name = server_name.clone();
                        // Label this session's rail tile with the burrow's name.
                        if !name.is_empty() {
                            session_name.set(Some(name.clone()));
                        }
                        toasts.update(|q| {
                            q.push(
                                crate::toasts::ToastKind::Success,
                                format!("Connected to {name}"),
                            );
                        });
                        // The transport defers this under its own borrow and
                        // captures the socket generation before doing so.
                        let cmd = match &auth {
                            AuthMethod::Password { login, password } if !login.is_empty() => {
                                Some(Command::SignIn {
                                    login: login.clone(),
                                    password: password.clone(),
                                })
                            }
                            AuthMethod::Resume { token } if !token.is_empty() => {
                                Some(Command::Resume {
                                    token: token.clone(),
                                })
                            }
                            _ => None,
                        };
                        if let Some(cmd) = cmd {
                            ws_sv.with_value(|c| c.authenticate(cmd));
                        }
                    }
                    Event::Authenticated {
                        token,
                        screen_name,
                        role,
                        caps,
                    } => {
                        // Authentication automatically joins the lobby. Other
                        // rooms must accept a fresh join on this socket before
                        // the transport requests their private scrollback.
                        wasm_bindgen_futures::spawn_local(async move {
                            ws_sv.with_value(|c| {
                                c.request_who();
                                c.request_front_page();
                                c.request_radio_stations();
                                c.dispatch_room(&crate::wire::RoomCommand::List);
                                c.set_presence(presence.get_untracked(), None);
                                c.request_chat_history(crate::client::LOBBY);
                                let here = state.with_untracked(|s| s.room().to_string());
                                if here != crate::client::LOBBY {
                                    c.dispatch_room(&crate::wire::RoomCommand::Join {
                                        room: here.clone(),
                                    });
                                }
                                c.dispatch_room(&crate::wire::RoomCommand::Keeping { room: here });
                            });
                        });
                        // A pane that is already open asked this burrow
                        // before; whatever it asked for died with the old
                        // socket, so say plainly that this is a new one.
                        session_ready.update(|n| *n += 1);
                        my_role.set(*role);
                        my_caps.set(*caps);
                        my_login.set(screen_name.clone());
                        *my_handle.borrow_mut() = screen_name.clone();
                        // Operators get the console. The server already told
                        // us the role; the sidebar used to ignore it, so a
                        // live admin never saw an Admin entry at all (only the
                        // demo's seeded "rabbit" handle did).
                        is_admin.set(crate::state::role_is_operator(*role));
                        // And whether there is an account behind the handle
                        // at all: the DM view used to ask a guest's
                        // conversation list, get Forbidden, and offer a
                        // "Try again" that could never work.
                        is_guest.set(crate::state::role_is_guest(*role));
                        session_authenticated.set(true);
                        let owner = memory.borrow().login.as_deref().filter(|_| !is_guest.get_untracked())
                            .and_then(|login| crate::theme_preferences::Owner::new(&ep, login));
                        let previous_owner = theme_session.theme_owner.get_untracked();
                        if previous_owner != owner || is_guest.get_untracked() {
                            theme_session.theme_choice.set(None);
                        }
                        theme_session.theme_owner.set(owner.clone());
                        if let Some(owner) = owner {
                            theme_session.theme_choice.set(so_app.theme_preferences.with_untracked(|prefs| prefs.choice(&owner)));
                        }
                        so_app.sync_theme_preference(theme_session, false);
                        so_app.remember_authenticated(&ep, screen_name, token, &mut memory.borrow_mut());
                        // In the desktop shell, give the in-process swarm core its
                        // own session to this burrow so downloads can resolve
                        // sources + tickets and fetch multi-source. No-op on web.
                        if crate::native::native_available() {
                            crate::native::connect_native(&ep, token);
                        }
                    }
                    Event::ChatMessage {
                        room,
                        from,
                        text,
                        at_unix_ms,
                    } => {
                        let mut fresh = false;
                        state.update(|s| {
                            fresh = s.push_chat(crate::state::ChatLine {
                                room: room.clone(),
                                from: from.clone(),
                                text: text.clone(),
                                at_unix_ms: *at_unix_ms,
                            });
                        });
                        if !fresh {
                            return;
                        }
                        // Someone spoke. If the window isn't focused (and it
                        // wasn't us), raise an OS notification — the loudest
                        // level of the unread story, above the rail badge and
                        // the tab title. Permission is asked here, in context.
                        let me = notify_handle.borrow().clone();
                        let focused = crate::notify::window_focused();
                        if crate::notify::should_notify(focused, from, &me) {
                            let burrow = notify_name.get_untracked().unwrap_or_default();
                            crate::notify::notify(
                                crate::notify::notification_title(from, &burrow),
                                crate::notify::notification_body(text),
                                crate::notify::TAG_CHAT,
                            );
                        }
                        if crate::sound::should_chime(sound_on.get_untracked(), focused, from, &me)
                        {
                            chime_app.play_chime(crate::sound::Chime::Chat);
                        }
                    }
                    _ => {}
                }
                if !matches!(event, Event::ChatMessage { .. }) {
                    state.update(|s| s.apply(&event));
                }
            }));
            ws.on_conn(std::rc::Rc::new(move |c| {
                if c != crate::conn::ConnState::Online {
                    session_authenticated.set(false);
                }
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
                state.update(|s| {
                    if c != crate::conn::ConnState::Online {
                        s.chat_history.reset();
                        s.radio_requests = Default::default();
                    }
                    s.set_conn(c);
                });
            }));
            ws.on_front_page(std::rc::Rc::new(move |widgets| {
                state.update(|s| s.front_page = widgets)
            }));
            ws.on_who(std::rc::Rc::new(move |roster| {
                state.update(|s| s.who = roster);
                // Leave a trace of everyone seen here, so the person page can
                // say where you know someone *from* even after you disconnect.
                app_for_sightings.note_roster_sighting(&who_endpoint, &who_burrow());
            }));
            ws.on_presence(std::rc::Rc::new(move |delta| {
                state.update(|s| match delta {
                    crate::wire::PresenceDelta::Joined(p) => {
                        if !s.who.iter().any(|x| x.screen_name == p.screen_name) {
                            s.who.push(p);
                        }
                    }
                    // One session leaving is not the person leaving: they may
                    // still be here from another device. Ask again, and let
                    // the fresh roster say.
                    crate::wire::PresenceDelta::Left(_) => {
                        if let Some(app) = current() {
                            defer(move || app.refresh_who());
                        }
                    }
                })
            }));
            ws.on_sessions(std::rc::Rc::new(move |sessions| {
                state.update(|s| s.sessions = sessions)
            }));
            ws.on_file_bytes(std::rc::Rc::new(move |file| {
                if let Some(app) = current() {
                    app.show_art(file);
                }
            }));
            let requests_endpoint = endpoint.clone();
            ws.on_radio_requests(std::rc::Rc::new(move |answer| {
                if let Some(app) = current() {
                    if let Some(session) = app.session_at(&requests_endpoint) {
                        if session.state == state {
                            app.requests_answered(session, answer);
                        }
                    }
                }
            }));
            ws.on_chat_history(std::rc::Rc::new(move |(room, lines)| {
                let mut added = 0;
                state.update(|s| added = s.merge_chat_history(&room, lines));
                // Backfilled lines do not become unread notifications on an
                // unfocused burrow. Preserve any genuinely new live count.
                let total = state.with_untracked(|s| s.messages.len());
                session_seen.update(|seen| *seen = seen.saturating_add(added).min(total));
            }));
            ws.on_rooms(std::rc::Rc::new(move |rooms| {
                state.update(|s| {
                    s.rooms = rooms;
                    // A room that is no longer there (emptied and gone while
                    // the connection was down) is not somewhere to be.
                    let here = s.room().to_string();
                    if here != crate::client::LOBBY
                        && !s.rooms.iter().any(|r| r.name.eq_ignore_ascii_case(&here))
                    {
                        s.room = String::new();
                    }
                })
            }));
            let keeping_endpoint = endpoint.clone();
            ws.on_room_keeping(std::rc::Rc::new(move |answer| {
                if let Some(app) = current() {
                    if let Some(session) = app.session_at(&keeping_endpoint) {
                        app.room_kept(session, answer);
                    }
                }
            }));
            ws.on_room(std::rc::Rc::new(move |room| {
                state.update(|s| match s.rooms.iter_mut().find(|r| r.name == room.name) {
                    Some(slot) => *slot = room,
                    None => s.rooms.push(room),
                })
            }));
            ws.on_wishes(std::rc::Rc::new(move |wishes| {
                state.update(|s| s.wishes.wishes = wishes)
            }));
            ws.on_wish(std::rc::Rc::new(move |wish| {
                state.update(|s| s.wishes.changed(wish))
            }));
            ws.on_boards(std::rc::Rc::new(move |boards| {
                state.update(|s| s.set_boards(boards))
            }));
            ws.on_board_tree(std::rc::Rc::new(move |tree| {
                state.update(|s| s.set_board_tree(tree))
            }));
            ws.on_threads(std::rc::Rc::new(move |threads| {
                state.update(|s| s.set_threads(threads))
            }));
            ws.on_posts(std::rc::Rc::new(move |posts| {
                state.update(|s| s.set_posts(posts))
            }));
            ws.on_dm_threads(std::rc::Rc::new(move |threads| {
                state.update(|s| s.set_dm_threads(threads))
            }));
            ws.on_dm_history(std::rc::Rc::new(move |(peer, msgs)| {
                // Apply only if this history is still for the open conversation
                // — a late reply from a previous selection is dropped (it would
                // otherwise briefly render another peer's private messages).
                state.update(|s| {
                    if s.selected_dm.as_deref() == Some(peer.as_str()) {
                        s.set_dm_messages(&peer, msgs);
                    }
                })
            }));
            ws.on_dm_received(std::rc::Rc::new(move |(peer, msg)| {
                // A friendship attestation rides in as a DM. Verify it binds
                // OUR key (a relayed offer for someone else fails and is
                // dropped), store their half, and never show the raw payload.
                #[cfg(target_arch = "wasm32")]
                if let Some((their_pub, sig)) = crate::friend::parse_offer(&msg.text) {
                    if let Some(me) = fr_app.you.get_untracked().map(|y| y.public_hex) {
                        if crate::friend::verify_half(&their_pub, &me, &sig) {
                            fr_app.friends.update(|list| {
                                crate::friend::record_their_offer(
                                    list, &their_pub, &msg.from, &sig,
                                );
                            });
                            crate::friend::storage::save(&fr_app.friends.get_untracked());
                        }
                    }
                    return;
                }
                // A DM is addressed to you personally — notify when away, with
                // its own title and tag so it never reads (or collapses) as room
                // chatter. Same policy as the lobby: silent while you're looking.
                let me = dm_handle.borrow().clone();
                let focused = crate::notify::window_focused();
                if crate::notify::should_notify(focused, &msg.from, &me) {
                    crate::notify::notify(
                        crate::notify::dm_notification_title(&msg.from),
                        crate::notify::notification_body(&msg.text),
                        crate::notify::TAG_DM,
                    );
                }
                if crate::sound::should_chime(dm_sound_on.get_untracked(), focused, &msg.from, &me)
                {
                    chime_app.play_chime(crate::sound::Chime::Dm);
                }
                state.update(|s| s.receive_dm(&peer, msg))
            }));
            let app = *self;
            ws.on_file_event(std::rc::Rc::new(move |event| {
                // A pull's ending is worth a toast once: a reconnect replays
                // it, and the row already says so by then.
                let ending = match &event {
                    FileEvent::PullStatus(status)
                        if status.state != rabbithole_proto::filelib::pull_state::RUNNING =>
                    {
                        let settled = files.with_untracked(|f| {
                            f.pulls
                                .get(&status.pull_id)
                                .and_then(|k| f.transfers.iter().find(|t| t.id == *k))
                                .is_some_and(|t| {
                                    matches!(
                                        t.status,
                                        crate::files::TransferStatus::Done
                                            | crate::files::TransferStatus::Failed
                                    )
                                })
                        });
                        (!settled).then(|| status.clone())
                    }
                    _ => None,
                };
                files.update(|f| f.apply(&event));
                if let Some(status) = ending {
                    use rabbithole_proto::filelib::{pull_reason, pull_state};
                    let what = files.with_untracked(|f| {
                        f.pulls
                            .get(&status.pull_id)
                            .and_then(|k| f.transfers.iter().find(|t| t.id == *k))
                            .map(|t| t.name.clone())
                    });
                    let what = what.unwrap_or_else(|| status.landed.clone());
                    let dest = session_name
                        .get_untracked()
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| "this burrow".to_string());
                    let kind = match (status.state, status.reason) {
                        (pull_state::DONE, _) if status.missing == 0 => {
                            crate::toasts::ToastKind::Success
                        }
                        (_, pull_reason::CANCELLED) => crate::toasts::ToastKind::Info,
                        _ => crate::toasts::ToastKind::Warn,
                    };
                    app.notify(kind, crate::send::ended(&status, &what, &dest));
                }
            }));
            let admin_sig = self.admin;
            let syn_sig = self.syndication;
            ws.on_admin(std::rc::Rc::new(move |(key, events)| {
                admin_sig.update(|a| {
                    for event in &events {
                        a.apply(event);
                    }
                });
                // The feed pane reads config keys; a `*` marker is not one.
                if !key.as_deref().is_some_and(|k| k.starts_with('*')) {
                    syn_sig.update(|s| s.apply_live(key.as_deref(), &events));
                }
                if let Some(app) = current() {
                    app.fold_admin_reply(key.as_deref(), &events);
                }
            }));
            ws.on_members(std::rc::Rc::new(move |members| {
                // `online` is recomputed from the live roster at render time
                // (UiState::matching_members), so presence deltas keep the
                // directory badges fresh — no need to bake it in here.
                state.update(|s| s.set_members(members))
            }));
            ws.on_profile(std::rc::Rc::new(move |profile| {
                let avatar_hex = profile.avatar_hex.clone();
                state.update(|s| s.set_profile(profile));
                // Fetch the avatar blob if any. Deferred (spawn_local) because
                // this sink runs inside the transport's own borrow — a sync
                // request_blob would re-enter the RefCell.
                #[cfg(target_arch = "wasm32")]
                if let Some(hex) = avatar_hex {
                    wasm_bindgen_futures::spawn_local(async move {
                        ws_sv.update_value(|c| c.request_blob(&hex));
                    });
                }
                #[cfg(not(target_arch = "wasm32"))]
                let _ = avatar_hex;
            }));
            let radio_covers = self.radio_covers;
            ws.on_avatar(std::rc::Rc::new(move |(hex, data_url)| {
                // A blob fetched for a station's cover goes to the radio; the
                // same reply channel carries both, told apart by who asked.
                let is_cover = radio.with_untracked(|r| {
                    r.stations()
                        .any(|s| s.cover.is_some_and(|c| crate::wire::id_to_hex(&c) == hex))
                });
                if is_cover {
                    radio_covers.update(|m| {
                        m.insert(hex.clone(), data_url.clone());
                    });
                }
                // Only attach if the fetched blob still belongs to the selected
                // profile — a late reply from a previous selection is dropped.
                state.update(|s| s.set_avatar_src(&hex, data_url))
            }));
            let listing_app = *self;
            let listing_endpoint = endpoint.clone();
            ws.on_radio_listing(std::rc::Rc::new(move |listing| {
                let wanted: Vec<String> = listing
                    .stations
                    .iter()
                    .filter_map(|s| s.cover.map(|c| crate::wire::id_to_hex(&c)))
                    .filter(|hex| radio_covers.with_untracked(|m| !m.contains_key(hex)))
                    .collect();
                radio.update(|r| {
                    r.apply_listing(
                        crate::radio::Tuning {
                            endpoint: listing_endpoint.clone(),
                            stream_base: listing.stream_base,
                            port: listing.port,
                        },
                        listing.stations,
                    )
                });
                // Deferred: this sink runs inside the transport's own borrow,
                // so fetching covers (and re-syncing the player, which may now
                // have an address to play) waits a tick.
                wasm_bindgen_futures::spawn_local(async move {
                    for hex in wanted {
                        ws_sv.update_value(|c| c.request_blob(&hex));
                    }
                    listing_app.radio_prefs_changed();
                });
            }));
            let notice_endpoint = endpoint.clone();
            ws.on_notice(std::rc::Rc::new(move |route| match route {
                crate::wire::NoticeRoute::Radio(u) => {
                    if let crate::radio::RadioUpdate::Off(station) = &u {
                        state.update(|s| s.radio_requests.forget(station));
                    }
                    // A push from another connected burrow cannot alter the
                    // station model whose listing currently owns the player.
                    if radio.with_untracked(|r| r.endpoint().is_some_and(|ep| ep != notice_endpoint)) {
                        return;
                    }
                    let mut track_changed = false;
                    radio.update(|r| track_changed = r.apply_update(u));
                    // A new track has a new cover: ask for the picture again.
                    if track_changed {
                        wasm_bindgen_futures::spawn_local(async move {
                            ws_sv.update_value(|c| c.request_radio_stations());
                        });
                    }
                }
                crate::wire::NoticeRoute::Chat { from, text } => {
                    state.update(|s| s.push_notice(&from, &text))
                }
            }));
            ws.dispatch(Command::Connect {
                endpoint: endpoint.clone(),
                pinned_fingerprint: None,
            });
        });
        self.focused().live.set(true);
    }

    /// Host stub: no socket off-target, so this only flips the live flag.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect_live(&self, _endpoint: String, _login: String, _password: String) {
        self.focused().live.set(true);
    }

    /// Leave a burrow: close its socket, drop its resume token, and remove its
    /// session (and rail tile). Focus falls back to another connected burrow,
    /// or to the connect screen when that was the last one.
    pub fn disconnect(&self, id: &ServerId) {
        let id = id.clone();
        #[cfg(target_arch = "wasm32")]
        {
            use crate::wire::EventClient;
            // Tell the transport this close is deliberate, so it doesn't
            // schedule a reconnect for a burrow the user just left.
            self.sessions.with_untracked(|list| {
                if let Some((_, session)) = list.iter().find(|(sid, _)| *sid == id) {
                    session
                        .theme_revision
                        .update(|revision| *revision = revision.wrapping_add(1));
                    session.authenticated.set(false);
                    session
                        .ws
                        .update_value(|c| c.dispatch(rabbithole_core::api::Command::Disconnect));
                }
            });
            // Leaving drops the session token, not the burrow: the handle
            // stays in the recent list so rejoining is a password, not a
            // form. Forgetting the whole entry meant every Leave cost the
            // user their handle too.
            crate::recent::remember_token(&id.0, "");
        }
        self.drop_session(&id);
    }

    /// Remove a session (and its rail tile); focus falls back to the first
    /// remaining one. The placeholder is the app's floor and never goes.
    fn drop_session(&self, id: &ServerId) {
        if id.is_placeholder() {
            return;
        }
        // Theme and account controls react to both signals. No observer may
        // see a focused id after its session was removed.
        batch(|| {
            self.sessions
                .update(|list| list.retain(|(sid, _)| sid != id));
            if self.focused_id.get_untracked() == *id {
                let next = self
                    .sessions
                    .with_untracked(|list| list.first().map(|(sid, _)| sid.clone()));
                if let Some(next) = next {
                    self.set_focus(next);
                }
            }
        });
    }

    /// Sign a burrow out after a refused sign-in or a dead resume token: the
    /// socket closes for good (no reconnect loop retrying a token the server
    /// already refused), the session and its tile go, the saved handle stays
    /// so the connect form comes up prefilled, and the form carries `notice`
    /// saying why. Focus falls back to the placeholder and the route guard
    /// takes the user to the connect screen.
    #[cfg(target_arch = "wasm32")]
    pub fn sign_out(&self, id: &ServerId, notice: String) {
        use crate::wire::EventClient;
        self.sessions.with_untracked(|list| {
            if let Some((_, session)) = list.iter().find(|(sid, _)| sid == id) {
                session
                    .ws
                    .update_value(|c| c.dispatch(rabbithole_core::api::Command::Disconnect));
            }
        });
        self.pending_endpoint.set(Some(id.0.clone()));
        self.pending_notice.set(Some(notice));
        // "Connected to Wonderland" beside "your session expired" is two
        // stories; the form's notice is the one that is still true.
        self.toasts.update(|q| q.clear());
        self.drop_session(id);
    }

    /// A guest who wants to be someone: bring this burrow's connect form back,
    /// prefilled with the guest handle, with a line saying why it is there.
    /// The guest session goes (there is nothing in it to keep); a member
    /// session replaces it once the form is submitted.
    pub fn sign_in_as_member(&self) {
        let id = self.focused_id.get_untracked();
        let burrow = self
            .focused()
            .name
            .get_untracked()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| server_label(&id));
        let notice = format!("Direct messages on {burrow} need an account. Sign in with yours.");
        #[cfg(target_arch = "wasm32")]
        self.sign_out(&id, notice);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pending_endpoint.set(Some(id.0.clone()));
            self.pending_notice.set(Some(notice));
            self.drop_session(&id);
        }
    }

    /// What to call the focused burrow: its name, else its address. Used as a
    /// folder name for "a folder per burrow" downloads; the shell makes it safe.
    pub fn focused_burrow_label(&self) -> String {
        self.focused()
            .name
            .get_untracked()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| server_label(&self.focused_id.get_untracked()))
    }

    /// Ask the shell where downloads go (desktop only; a no-op in a tab).
    pub fn load_download_prefs(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let prefs = self.download_prefs;
            crate::native::download_prefs(move |answer| {
                if let Ok(Some(p)) = answer {
                    prefs.set(Some(p));
                }
            });
        }
    }

    /// Fold a preference command's answer in, or say why it failed.
    #[cfg(target_arch = "wasm32")]
    fn download_prefs_answer(
        &self,
    ) -> impl FnOnce(Result<Option<crate::save::DownloadPrefs>, String>) + 'static {
        let app = *self;
        move |answer| match answer {
            Ok(Some(p)) => app.download_prefs.set(Some(p)),
            Ok(None) => {} // the panel was cancelled; nothing changed
            Err(why) => {
                app.notify(crate::toasts::ToastKind::Warn, why);
            }
        }
    }

    /// Choose the download folder in a native folder panel.
    pub fn choose_download_folder(&self) {
        #[cfg(target_arch = "wasm32")]
        crate::native::choose_download_folder(self.download_prefs_answer());
    }

    /// Go back to asking where each download goes.
    pub fn clear_download_folder(&self) {
        #[cfg(target_arch = "wasm32")]
        crate::native::clear_download_folder(self.download_prefs_answer());
    }

    /// Turn a folder per burrow on or off.
    pub fn set_per_burrow_folders(&self, on: bool) {
        #[cfg(target_arch = "wasm32")]
        crate::native::set_per_burrow_folders(on, self.download_prefs_answer());
        #[cfg(not(target_arch = "wasm32"))]
        let _ = on;
    }

    /// Turn sharing downloads with a burrow's swarm on or off (desktop only).
    pub fn set_seeding(&self, on: bool) {
        #[cfg(target_arch = "wasm32")]
        crate::native::set_seeding(on, self.download_prefs_answer());
        #[cfg(not(target_arch = "wasm32"))]
        let _ = on;
    }

    #[cfg(target_arch = "wasm32")]
    fn remember_authenticated(
        &self,
        endpoint: &str,
        screen_name: &str,
        token: &str,
        memory: &mut SignInMemory,
    ) {
        let Some(login) = memory.login.as_deref() else {
            crate::recent::remember(endpoint, screen_name);
            crate::recent::remember_token(endpoint, "");
            return;
        };
        crate::recent::remember_login(endpoint, login);
        // A removed bookmark or deliberately forgotten credential stays gone,
        // even if an earlier sign-in/reconnect completes afterward. Renames are
        // harmless and are kept by upsert rather than overwritten.
        let still_current = memory
            .bookmark
            .as_ref()
            .map(|before| {
                self.bookmarks.with_untracked(|list| {
                    crate::bookmarks::by_id(list, &before.id).is_some_and(|now| {
                        now.endpoint == before.endpoint
                            && now.login == before.login
                            && now.token == before.token
                    })
                })
            })
            .unwrap_or(!memory.saved_once || !memory.save_bookmark);
        if !still_current {
            return;
        }
        let mut keep_recent = memory.keep_recent;
        if memory.save_bookmark && !token.is_empty() {
            let name = if memory.bookmark.is_some() {
                String::new()
            } else {
                self.session_at(endpoint)
                    .and_then(|session| session.name.get_untracked())
                    .unwrap_or_default()
            };
            match crate::bookmarks::upsert_account(
                self.bookmarks.get_untracked(),
                endpoint,
                login,
                &name,
                token,
            ) {
                Ok((list, id)) => {
                    let saved = crate::bookmarks::by_id(&list, &id).cloned();
                    if self.store_bookmarks(list) {
                        memory.bookmark = saved;
                    } else {
                        keep_recent = false;
                    }
                }
                Err(error) => {
                    keep_recent = false;
                    self.notify(
                        crate::toasts::ToastKind::Warn,
                        format!(
                            "Signed in, but the bookmark was not saved. {}",
                            error.message()
                        ),
                    );
                }
            }
        } else if !memory.keep_recent {
            if let Some(bookmark) = &memory.bookmark {
                self.forget_bookmark_signin(&bookmark.id);
            }
        }
        memory.saved_once = true;
        if keep_recent {
            crate::recent::remember_account_token(endpoint, login, token);
        }
    }

    /// Keep a burrow. `Err` says why not, in words for the person.
    pub fn add_bookmark(
        &self,
        endpoint: &str,
        name: &str,
    ) -> Result<(), crate::bookmarks::AddError> {
        let list = crate::bookmarks::add(self.bookmarks.get_untracked(), endpoint, name)?;
        self.store_bookmarks(list);
        // A new bookmark's status is unknown until someone knocks.
        self.knock_on(vec![endpoint.trim().to_string()]);
        Ok(())
    }

    /// Stop keeping a burrow.
    pub fn remove_bookmark(&self, id: &str) {
        self.forget_bookmark_signin(id);
        let list = crate::bookmarks::remove(self.bookmarks.get_untracked(), id);
        self.store_bookmarks(list);
    }

    pub fn forget_bookmark_signin(&self, id: &str) {
        #[cfg(target_arch = "wasm32")]
        if let Some(bookmark) = self
            .bookmarks
            .with_untracked(|list| crate::bookmarks::by_id(list, id).cloned())
        {
            if let Some(login) = bookmark.login.as_deref() {
                crate::recent::remember_account_token(&bookmark.endpoint, login, "");
            }
        }
        self.store_bookmarks(crate::bookmarks::clear_token(
            self.bookmarks.get_untracked(),
            id,
        ));
    }

    /// Call a kept burrow something else.
    pub fn rename_bookmark(&self, id: &str, name: &str) {
        let list = crate::bookmarks::rename(self.bookmarks.get_untracked(), id, name);
        self.store_bookmarks(list);
    }

    fn store_bookmarks(&self, list: Vec<crate::bookmarks::Bookmark>) -> bool {
        #[cfg(target_arch = "wasm32")]
        if let Err(error) = crate::bookmarks::save(&list) {
            self.notify(crate::toasts::ToastKind::Warn, error);
            return false;
        }
        self.bookmarks.set(list);
        true
    }

    /// Knock on these burrows and record who answered. A place reads
    /// "checking" until its knock settles, never "down" by default.
    pub fn knock_on(&self, endpoints: Vec<String>) {
        if endpoints.is_empty() {
            return;
        }
        self.probes.update(|p| {
            for e in &endpoints {
                p.insert(crate::connect::probe_key(e), crate::probe::Probe::Checking);
            }
        });
        #[cfg(target_arch = "wasm32")]
        for endpoint in endpoints {
            let probes = self.probes;
            let key = crate::connect::probe_key(&endpoint);
            crate::probe::knock(&endpoint, move |up| {
                // `try_update`: the app outlives every view, but be exact.
                let _ = probes.try_update(|p| {
                    p.insert(
                        key,
                        if up {
                            crate::probe::Probe::Up
                        } else {
                            crate::probe::Probe::Down
                        },
                    );
                });
            });
        }
    }

    /// Ask before leaving the focused burrow.
    pub fn ask_leave(&self) {
        let id = ServerId(self.focused_endpoint());
        let name = self.focused().name.get_untracked().unwrap_or_default();
        self.confirm.set(Some(ConfirmAsk::leave(id, &name)));
    }

    /// The person answered the open question. `true` carries out its intent.
    pub fn answer_confirm(&self, yes: bool) {
        let Some(ask) = self.confirm.get_untracked() else {
            return;
        };
        self.confirm.set(None);
        if !yes {
            return;
        }
        match ask.intent {
            ConfirmIntent::Leave(id) => self.disconnect(&id),
            ConfirmIntent::DisableAccount(login) => self.set_account_disabled(&login, true),
            ConfirmIntent::RemoveAccount(login) => self.remove_account(&login),
            ConfirmIntent::ResetTotp(login) => self.reset_account_totp(&login),
            ConfirmIntent::RevokeInvite(code) => self.revoke_invite(&code),
            ConfirmIntent::DeleteBoard(slug) => self.delete_board(&slug),
            ConfirmIntent::DeletePost(id) => self.delete_post(&id),
            ConfirmIntent::DeleteArea(slug) => self.delete_area(&slug),
            ConfirmIntent::DeleteNode(id, name) => self.delete_node(id, &name),
            ConfirmIntent::KickSession(id, _) => self.kick_session(id),
            ConfirmIntent::RevokePeer(key) => self.revoke_peer(key),
            ConfirmIntent::DeleteBackup(name) => self.delete_backup(&name),
        }
    }

    /// Manually redial the live socket now (the reconnect banner's button).
    pub fn reconnect(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.update_value(|c| c.redial());
        }
    }

    /// Whether this session can send commands. An open transport still needs
    /// AuthOk before a live composer may submit and clear its draft.
    pub fn online(&self) -> bool {
        let session = self.focused();
        session.state.with(|s| s.conn.is_live())
            && (!session.live.get() || session.authenticated.get())
    }

    /// Send a lobby chat line — over the live socket when connected, else
    /// through the seeded mock seam.
    pub fn send_chat(&self, text: String) {
        // Whichever room is being read: a burrow has more than the lobby,
        // and what somebody types belongs where they are looking.
        let room = self
            .focused()
            .state
            .with_untracked(|s| s.room().to_string());
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            use crate::wire::EventClient;
            self.focused().ws.update_value(|c| {
                c.dispatch(rabbithole_core::api::Command::SendChat {
                    room: room.clone(),
                    text: text.clone(),
                });
            });
            return;
        }
        self.dispatch(rabbithole_core::api::Command::SendChat { room, text });
    }

    /// Tell the burrow its agreement is accepted — over the live socket
    /// when connected, else through the seeded mock seam. Until it is
    /// told, it refuses everything the agreement gates, and asks again at
    /// the next sign-in.
    pub fn accept_agreement(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            use crate::wire::EventClient;
            self.focused().ws.update_value(|c| {
                c.dispatch(rabbithole_core::api::Command::AcceptAgreement);
            });
            return;
        }
        self.dispatch(rabbithole_core::api::Command::AcceptAgreement);
    }

    /// Drive one command through the seam and fold its events into state.
    pub fn dispatch(&self, command: Command) {
        let state = self.focused().state;
        self.focused().client.update_value(|client| {
            let events = client.send(command);
            state.update(|s| {
                for event in &events {
                    s.apply(event);
                }
            });
        });
    }

    /// Join a seeded demo burrow: give it its own session (keyed by the demo
    /// endpoint, so two of them coexist in the rail exactly like two live
    /// burrows), label it, and connect its mock client.
    ///
    /// Dev-only. Demo data is compiled out of shipping builds — see the
    /// `demo` feature in `Cargo.toml`.
    #[cfg(feature = "demo")]
    pub fn join_demo(&self, demo: &crate::client::DemoBurrow, handle: &str) {
        let id = ServerId(demo.endpoint.to_string());
        let exists = self
            .sessions
            .with_untracked(|list| list.iter().any(|(sid, _)| *sid == id));
        if !exists {
            let session = Session {
                state: create_rw_signal(UiState::default()),
                files: create_rw_signal(FilesState::default()),
                is_admin: create_rw_signal(false),
                is_guest: create_rw_signal(false),
                role: create_rw_signal(0),
                caps: create_rw_signal(0),
                handle: create_rw_signal(String::new()),
                ready: create_rw_signal(0),
                authenticated: create_rw_signal(false),
                live: create_rw_signal(false),
                server_theme: create_rw_signal(None),
                theme_owner: create_rw_signal(None),
                theme_choice: create_rw_signal(None),
                theme_sync: create_rw_signal(Default::default()),
                theme_revision: create_rw_signal(0),
                name: create_rw_signal(Some(demo.name.to_string())),
                seen: create_rw_signal(0),
                #[cfg(target_arch = "wasm32")]
                ws: store_value(crate::ws::WsClient::new()),
                client: store_value(crate::client::MockClient::named(demo)),
            };
            self.sessions
                .update(|list| list.push((id.clone(), session)));
        }
        self.set_focus(id);
        self.focused().name.set(Some(demo.name.to_string()));
        self.dispatch(Command::Connect {
            endpoint: demo.endpoint.to_string(),
            pinned_fingerprint: None,
        });
        self.dispatch(Command::SignIn {
            login: handle.to_string(),
            password: String::new(),
        });
        // The seeded host handle carries the admin capability.
        self.set_admin(handle == "rabbit");
        let me = self.focused();
        me.handle.set(handle.to_string());
        me.role.set(if handle == "rabbit" { 3 } else { 1 });
        me.caps.set(if handle == "rabbit" { u64::MAX } else { 0 });
        self.refresh_who();
        self.load_dms();
        self.load_radio();
        self.load_server_theme();
        self.load_demo_front_page();
    }

    /// Seed the focused (demo) session's news panel from its burrow identity.
    /// A real burrow sends a `WelcomeScreen` on connect; the mock never did,
    /// so the demo had no news at all.
    pub fn load_demo_front_page(&self) {
        let widgets = self
            .focused()
            .client
            .with_value(|c| c.demo_welcome_widgets());
        if widgets.is_empty() {
            return;
        }
        self.focused().state.update(|s| {
            s.front_page = widgets;
        });
    }

    /// Ask again who is here. On a live burrow that is the server's to say,
    /// and the answer arrives on `on_who` (and, being the same reply, on
    /// `on_sessions`). Only the demo's roster is ours to make up.
    ///
    /// Making it up over a live one is how a real lobby came to show three
    /// people who were never in it: the console's Moderation pane refreshes
    /// the roster when it opens, and this used to hand it the demo's.
    pub fn refresh_who(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.with_value(|client| client.request_who());
            return;
        }
        let who: Vec<crate::state::Presence> = self
            .focused()
            .client
            .with_value(|client| client.who(LOBBY))
            .into_iter()
            .map(|screen_name| {
                // The demo seeds one user with a portable identity key so the People
                // mark is live-visible before the wire carries real keys: "rabbit"
                // stands in as our own portable identity (you, in the demo).
                let key = (screen_name == "rabbit")
                    .then(|| self.you.get_untracked().map(|y| y.public_hex))
                    .flatten();
                crate::state::Presence {
                    screen_name,
                    state: rabbithole_proto::presence::PresenceState::Online,
                    transport: "mock".to_string(),
                    key,
                }
            })
            .collect();
        self.focused().state.update(|s| s.who = who);
    }

    /// Load the board tree snapshot into state.
    pub fn load_boards(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            // Live: request over the socket; the reply folds through the sink.
            // Mark it in flight so the view shows a skeleton, not "no boards yet".
            self.focused().state.update(|s| s.loading.boards = true);
            self.focused().ws.update_value(|c| c.request_boards());
            return;
        }
        let boards = self.focused().client.with_value(|c| c.boards());
        let tree = self.focused().client.with_value(|c| c.board_tree());
        self.focused().state.update(|s| {
            s.set_boards(boards);
            s.set_board_tree(tree);
        });
    }

    /// Select a board and load its threads into state.
    pub fn select_board(&self, slug: &str) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            // Reset the board view; the thread list arrives via the sink.
            // (select_board clears the flag, so raise it after.)
            self.focused().state.update(|s| {
                s.select_board(slug, Vec::new());
                s.loading.threads = true;
            });
            self.focused().ws.update_value(|c| c.request_threads(slug));
            return;
        }
        let threads = self.focused().client.with_value(|c| c.threads(slug));
        self.focused()
            .state
            .update(|s| s.select_board(slug, threads));
    }

    /// Open a thread and load its posts into state.
    pub fn open_thread(&self, id: String) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            // Open the thread immediately; its posts stream in via the sink.
            // The live thread id is the root post's hex id.
            let root = crate::wire::hex_to_id(&id);
            self.focused().state.update(|s| {
                s.open_thread(id, Vec::new());
                s.loading.posts = true;
            });
            if let Some(root) = root {
                self.focused().ws.update_value(|c| c.request_posts(root));
            }
            return;
        }
        let posts = self.focused().client.with_value(|c| c.posts(&id));
        self.focused().state.update(|s| s.open_thread(id, posts));
    }

    /// Start a new thread on `board`. Live: post it, then re-request the thread
    /// list (the connection is ordered, so the new thread is included). Mock:
    /// prepend it locally so the demo composer stays interactive.
    pub fn post_thread(&self, board: &str, subject: &str, body: &str) {
        if subject.trim().is_empty() {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.update_value(|c| {
                c.send_post(board, subject, body);
                c.request_threads(board);
            });
            return;
        }
        // The mock models a thread by its subject; the first-post body is only
        // sent over a live transport.
        let _ = body;
        let (board, subject) = (board.to_string(), subject.to_string());
        self.focused().state.update(|s| {
            let id = format!("tnew{}", s.threads.len());
            s.threads.insert(
                0,
                crate::state::Thread {
                    id,
                    board,
                    title: subject,
                    author: "you".to_string(),
                    replies: 0,
                    last_activity_unix_ms: crate::clock::now_ms(),
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
        let Some(thread_id) = self
            .focused()
            .state
            .with_untracked(|s| s.selected_thread.clone())
        else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            let board = self
                .focused()
                .state
                .with_untracked(|s| s.selected_board.clone())
                .unwrap_or_default();
            if let Some(root) = crate::wire::hex_to_id(&thread_id) {
                self.focused().ws.update_value(|c| {
                    c.send_reply(&board, root, body);
                    c.request_posts(root);
                    // The thread list shows a reply count and last-activity
                    // time; refresh it so the row you just replied to agrees
                    // with the reader.
                    c.request_threads(&board);
                });
            }
            return;
        }
        let body = body.to_string();
        self.focused().state.update(|s| {
            let id = format!("pnew{}", s.posts.len());
            s.posts.push(crate::state::Post {
                id,
                thread: thread_id,
                author: "you".to_string(),
                body,
                at_unix_ms: crate::clock::now_ms(),
                removed: false,
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
            self.focused().live.get_untracked()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    }

    /// Load the DM conversation snapshots into state. Live: request the
    /// conversation list over the socket (the reply folds through the sink).
    pub fn load_dms(&self) {
        // A guest has no conversations to list and the server says so with
        // Forbidden; the DM view shows the sign-in gate instead of asking.
        if self.focused().is_guest.get_untracked() {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().state.update(|s| s.loading.dms = true);
            self.focused().ws.update_value(|c| c.request_dm_threads());
            return;
        }
        let threads = self.focused().client.with_value(|c| c.dm_threads());
        self.focused().state.update(|s| s.set_dm_threads(threads));
    }

    /// Select a DM conversation with `peer`. Live: request its history (the
    /// reply folds into the selected thread via the sink).
    pub fn select_dm(&self, peer: &str) {
        self.focused().state.update(|s| s.select_dm(peer));
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.request_dm_history(peer));
        }
    }

    /// Send a DM into the selected conversation. Live: send over the socket,
    /// then re-request the history so the sent message appears. Mock: append it
    /// locally.
    pub fn send_dm(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let Some(id) = self
            .focused()
            .state
            .with_untracked(|s| s.selected_dm.clone())
        else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.update_value(|c| {
                c.send_dm(&id, text);
                c.request_dm_history(&id);
            });
            return;
        }
        let state = self.focused().state;
        self.focused().client.update_value(|c| {
            if let Some(msg) = c.send_dm(&id, text) {
                state.update(|s| s.append_dm(&id, msg));
            }
        });
    }

    /// Load the member directory snapshot into state. Live: request the
    /// directory over the socket (empty query = list all; reply folds via sink).
    pub fn load_members(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().state.update(|s| s.loading.members = true);
            self.focused().ws.update_value(|c| c.request_directory(""));
            return;
        }
        let members = self.focused().client.with_value(|c| c.members());
        self.focused().state.update(|s| s.set_members(members));
    }

    /// Select a member and (live) fetch their full profile card.
    pub fn select_member(&self, handle: &str) {
        self.focused().state.update(|s| s.select_member(handle));
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.request_profile(handle));
        }
    }

    /// Drive one [`FileCommand`] through the seam and fold its file events into
    /// the [`FilesState`].
    fn dispatch_file(&self, command: FileCommand) {
        // Live: send over the socket; replies fold in through the file sink
        // registered in `connect_live`. Mock: drive the seam synchronously.
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.dispatch_file(&command));
            return;
        }
        let files = self.focused().files;
        self.focused().client.update_value(|client| {
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
        self.focused().files.update(|f| {
            f.current_area = Some(slug.to_string());
            f.path.clear();
            f.selected = None;
        });
        self.refresh_files();
        self.load_upload_limits();
    }

    /// Descend into a child folder of the current location and list it.
    pub fn open_subfolder(&self, name: &str) {
        self.focused().files.update(|f| {
            f.path.push(name.to_string());
            f.selected = None;
        });
        self.refresh_files();
    }

    /// Jump to a breadcrumb path (`None` = area root) and list it.
    pub fn go_to_path(&self, path: Option<String>) {
        self.focused().files.update(|f| {
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
            .focused()
            .files
            .with(|f| (f.current_area.clone(), join_path(&f.path)));
        let Some(area) = area else {
            return;
        };
        self.dispatch_file(FileCommand::ListFolder { area, path });
    }

    /// Persist the sightings ledger and mirror it into the signal. wasm-only
    /// (host tests have no storage); the signal drives the person page.
    pub fn note_roster_sighting(&self, endpoint: &str, burrow_name: &str) {
        #[cfg(target_arch = "wasm32")]
        {
            let roster = self.focused().state.with_untracked(|s| s.who.clone());
            crate::sightings::storage::note_roster(
                endpoint,
                burrow_name,
                &roster,
                crate::clock::now_ms(),
            );
            self.sightings.set(crate::sightings::storage::load());
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (endpoint, burrow_name);
    }

    /// Resolve a person page's `:seed` to the live [`Person`] if they're
    /// currently on any connected burrow (their presence + which burrows).
    pub fn person_by_seed(&self, seed: &str) -> Option<crate::state::Person> {
        self.people()
            .into_iter()
            .find(|p| crate::sightings::seed_of(p.key.as_deref(), &p.screen_name) == seed)
    }

    /// Files on the focused burrow uploaded by a person, matched by the handle
    /// they use *there*. The person page's "shared files" — offered by them,
    /// linkable at this burrow.
    pub fn files_by(&self, handle: &str) -> Vec<rabbithole_proto::filelib::FileNodeView> {
        if handle.is_empty() {
            return Vec::new();
        }
        let h = handle.to_lowercase();
        self.focused().files.with_untracked(|f| {
            f.nodes
                .iter()
                .filter(|n| n.uploader.to_lowercase() == h)
                .cloned()
                .collect()
        })
    }

    /// The DM thread with `handle` on the focused burrow, if one exists.
    pub fn dm_with(&self, handle: &str) -> Option<crate::state::DmThread> {
        if handle.is_empty() {
            return None;
        }
        self.focused()
            .state
            .with_untracked(|s| s.dm_threads.iter().find(|t| t.peer == handle).cloned())
    }

    /// Refresh the Looking Glass from the network.
    ///
    /// Three sources, in preference order:
    ///
    /// 1. **rabbithole.directory** over HTTPS+JSON. CORS is open, so this
    ///    works in a browser tab and in the shell alike.
    /// 2. **The standard Looking Glass** (`tracker.rabbit.direct`) over HTTPS
    ///    JSON — a different shape from the directory's (nested endpoints),
    ///    also CORS-open, so a plain tab can use it.
    /// 3. **A Looking Glass status port** — line-oriented TCP (`INDEX`). A
    ///    browser cannot dial it; this leg exists only in the native shell.
    ///
    /// A live source that answers empty is not exclusive: the next source is
    /// asked, so an empty directory cannot hide a glass that has listings.
    /// Nothing reachable leaves the seeded sample in place, labelled as such:
    /// an empty browser would look like "nobody is out there", which is a
    /// different and wrong claim.
    pub fn load_directory(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let servers = self.servers;
            let source = self.directory_source;
            let loading = self.directory_loading;
            let app = *self;
            loading.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                let mut answers = Vec::new();
                match crate::net::fetch_text(crate::servers::DIRECTORY_URL).await {
                    Some(text) => answers.push(
                        crate::servers::parse_directory_json(&text)
                            .map(|rows| (rows, crate::servers::DirectorySource::Directory)),
                    ),
                    None => answers.push(Err("rabbithole.directory did not answer".into())),
                }
                // Non-empty directory is enough. Empty or failed: ask the glass.
                if answers
                    .last()
                    .and_then(|a| a.as_ref().ok())
                    .is_some_and(|(rows, _)| !rows.is_empty())
                {
                    let listing = crate::servers::pick_live_listing(answers).expect("rows");
                    servers.set(listing.servers);
                    source.set(listing.source);
                    loading.set(false);
                    return;
                }
                // The standard Looking Glass, over HTTPS with CORS open — so
                // this fallback works in a plain browser tab, not only in the
                // native shell. Its reply is a *different shape* from the
                // directory's (a glass relays announced descriptors, so the
                // endpoints are nested), hence a different parser.
                match crate::net::fetch_text(crate::servers::TRACKER_URL).await {
                    Some(text) => answers.push(
                        crate::servers::parse_glass_json(&text, &["ws"])
                            .map(|rows| (rows, crate::servers::DirectorySource::standard_glass())),
                    ),
                    None => answers.push(Err("the looking glass did not answer".into())),
                }
                if answers
                    .last()
                    .and_then(|a| a.as_ref().ok())
                    .is_some_and(|(rows, _)| !rows.is_empty())
                {
                    let listing = crate::servers::pick_live_listing(answers).expect("rows");
                    servers.set(listing.servers);
                    source.set(listing.source);
                    loading.set(false);
                    return;
                }
                // Last: the shell's TCP status-port INDEX. `None` in a browser
                // tab, where there is no shell to ask.
                match crate::native::tracker_index().await {
                    Some(text) => answers.push(
                        crate::servers::parse_tracker_index(&text)
                            .map(|rows| (rows, crate::servers::DirectorySource::standard_glass())),
                    ),
                    None => answers.push(Err("the status port did not answer".into())),
                }
                if let Some(listing) = crate::servers::pick_live_listing(answers) {
                    servers.set(listing.servers);
                    source.set(listing.source);
                    loading.set(false);
                    return;
                }
                // Nobody answered. A listing already on screen stays there,
                // under the name of whoever gave it: relabelling real rows as
                // the built-in sample (what this used to do on a failed
                // refresh) is wrong twice over, and the connect window, which
                // never shows the sample, would drop a good list for nothing.
                let held_live =
                    source.with_untracked(|s| *s != crate::servers::DirectorySource::Seeded);
                app.notify(
                    crate::toasts::ToastKind::Warn,
                    if held_live {
                        "Couldn\u{2019}t refresh the list. Showing the last one.".to_string()
                    } else {
                        "Couldn\u{2019}t reach a directory.".to_string()
                    },
                );
                loading.set(false);
            });
        }
    }

    /// A recovery document for the current identity, or `None` when there
    /// isn't one to back up.
    pub fn identity_backup(&self) -> Option<String> {
        self.identity
            .with_value(|id| id.as_ref().map(crate::identity::backup_json))
    }

    /// Replace the local identity from a recovery document. Returns the new
    /// fingerprint, or a message explaining why the file was refused.
    pub fn restore_identity(&self, text: &str) -> Result<String, String> {
        let id = crate::identity::restore_from_backup(text)?;
        let fp = id.fingerprint();
        #[cfg(target_arch = "wasm32")]
        {
            crate::identity::adopt(&id);
            self.you.set(Some(id.you()));
            self.identity.set_value(Some(id));
            // Friendships were signed by the OLD key and mean nothing under
            // the new one; keeping them would show a badge no signature backs.
            self.friends.set(Vec::new());
            crate::friend::storage::save(&[]);
        }
        Ok(fp)
    }

    /// Pick (or clear) your own mark. Persisted immediately, like settings.
    pub fn set_my_mark(&self, mark: Option<crate::avatar::ChosenMark>) {
        self.my_mark.set(mark);
        #[cfg(target_arch = "wasm32")]
        crate::avatar::chosen::save(mark);
    }

    /// The SVG for your own mark at `size`: your pick when you have one, else
    /// the mark your identity key derives.
    pub fn my_mark_svg(&self, size: u32) -> String {
        match self.my_mark.get() {
            Some(m) => crate::avatar::glyph_svg(m.glyph, m.color, size),
            None => match self.you.get() {
                Some(you) => crate::avatar::mark_svg(&you.public_hex, size),
                None => crate::avatar::glyph_svg(0, 0, size),
            },
        }
    }

    /// Persist settings after any change. Callers mutate `settings` then call
    /// this; keeping the write in one place means no edit path can forget it.
    pub fn save_settings(&self) {
        #[cfg(target_arch = "wasm32")]
        crate::settings::storage::save(&self.settings.get_untracked());
    }

    /// Can the focused burrow be left? Everything except the app's floor
    /// session (the one that exists before you join anywhere).
    pub fn can_leave(&self) -> bool {
        self.focused_id.get() != ServerId::local()
    }

    /// The focused burrow's endpoint — the id the sightings ledger files a
    /// person's per-burrow handle under.
    pub fn focused_endpoint(&self) -> String {
        self.focused_id.get().0
    }

    /// The friendship status with a peer identity key (hex).
    pub fn friendship(&self, peer_pub: &str) -> crate::friend::Status {
        self.friends
            .with_untracked(|list| crate::friend::status_of(list, peer_pub))
    }

    /// Send (or accept) a friendship offer to the person with identity key
    /// `peer_pub`, whom we can reach as `handle` on the focused live burrow.
    /// Signs our half, stores it, and DMs the signed offer to their handle.
    pub fn offer_friendship(&self, peer_pub: &str, peer_name: &str, handle: &str) {
        #[cfg(target_arch = "wasm32")]
        {
            let sig = self
                .identity
                .with_value(|id| id.as_ref().map(|id| crate::friend::sign(id, peer_pub)));
            let (Some(sig), Some(me)) = (sig, self.you.get_untracked().map(|y| y.public_hex))
            else {
                self.toasts.update(|q| {
                    q.push(
                        crate::toasts::ToastKind::Warn,
                        "No identity to sign with yet.".to_string(),
                    );
                });
                return;
            };
            self.friends.update(|list| {
                crate::friend::record_my_sig(list, peer_pub, peer_name, &sig);
            });
            crate::friend::storage::save(&self.friends.get_untracked());
            // The offer is a DM to their handle on this burrow — no server
            // involvement beyond delivering a message they can already receive.
            let offer = crate::friend::encode_offer(&me, &sig);
            if self.focused().live.get_untracked() && !handle.is_empty() {
                self.focused()
                    .ws
                    .update_value(|c| c.send_dm(handle, &offer));
            }
            let mutual = matches!(self.friendship(peer_pub), crate::friend::Status::Mutual);
            self.toasts.update(|q| {
                q.push(
                    crate::toasts::ToastKind::Success,
                    if mutual {
                        format!("You and {peer_name} are now friends.")
                    } else {
                        format!("Friendship offer sent to {peer_name}.")
                    },
                );
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (peer_pub, peer_name, handle);
    }

    /// Show a node's metadata card.
    pub fn select_file(&self, id: i64) {
        self.focused().files.update(|f| f.selected = Some(id));
    }

    /// The burrow whose stations the Radio view is showing: the one that
    /// last said what is on the air. What is asked of a station goes to it,
    /// and its answers are what the view shows, whichever burrow is focused.
    pub fn radio_session(&self) -> Option<Session> {
        let endpoint = self
            .radio
            .with_untracked(|r| r.endpoint().map(str::to_string))?;
        self.session_at(&endpoint)
    }

    /// [`session_at`](Self::session_at), for a view that should follow the
    /// burrow list as it changes.
    pub fn session_at_tracked(&self, endpoint: &str) -> Option<Session> {
        self.sessions.with(|list| {
            list.iter()
                .find(|(sid, _)| sid.0 == endpoint)
                .map(|(_, session)| *session)
        })
    }

    /// The connected burrow at `endpoint`, if there is one.
    pub fn session_at(&self, endpoint: &str) -> Option<Session> {
        self.sessions.with_untracked(|list| {
            list.iter()
                .find(|(sid, _)| sid.0 == endpoint)
                .map(|(_, session)| *session)
        })
    }

    /// Ask a station what is waiting, because the song has changed or the
    /// person has come to look: a refusal from before no longer stands.
    pub fn load_requests(&self, station: &str) {
        if let Some(session) = self.radio_session() {
            session.state.update(|s| s.radio_requests.moved_on(station));
        }
        self.radio_ask(crate::wire::RadioAsk::List {
            station: station.to_string(),
        });
    }

    /// The Radio panel watches exactly one station at its owning burrow.
    /// Closing it, changing owners, or disconnecting invalidates old replies.
    pub fn watch_requests(&self, target: Option<(String, String)>) {
        let sessions = self.sessions.get_untracked();
        for (id, session) in sessions {
            let wanted = target
                .as_ref()
                .filter(|(endpoint, _)| *endpoint == id.0)
                .map(|(_, station)| station.clone());
            let mut changed = false;
            session
                .state
                .update(|s| changed = s.radio_requests.watch(wanted.clone()));
            if changed {
                #[cfg(target_arch = "wasm32")]
                if session.live.get_untracked() {
                    let ws = session.ws;
                    let ready = session.ready.get_untracked();
                    let endpoint = id.0.clone();
                    let app = *self;
                    defer(move || {
                        if app
                            .session_at(&endpoint)
                            .is_some_and(|current| current.state == session.state)
                            && session.authenticated.get_untracked()
                            && session.ready.get_untracked() == ready
                            && session.state.with_untracked(|s| {
                                s.radio_requests.watching() == wanted.as_deref()
                            })
                        {
                            ws.with_value(|c| c.watch_radio_requests(wanted));
                        }
                    });
                }
            }
        }
    }

    /// Look through what a station can be asked for.
    pub fn look_for_songs(&self, station: &str, search: &str) {
        if let Some(session) = self.radio_session() {
            session
                .state
                .update(|s| s.radio_requests.looking(station, search));
        }
        self.radio_ask(crate::wire::RadioAsk::Offer {
            station: station.to_string(),
            search: search.to_string(),
        });
    }

    /// Ask a station for a song.
    pub fn ask_for_song(&self, station: &str, track: u64) {
        if let Some(session) = self.radio_session() {
            session.state.update(|s| s.radio_requests.asking(station));
        }
        self.radio_ask(crate::wire::RadioAsk::Request {
            station: station.to_string(),
            track,
        });
    }

    /// Join somebody else's request.
    pub fn join_request(&self, station: &str, track: u64) {
        if let Some(session) = self.radio_session() {
            session.state.update(|s| s.radio_requests.asking(station));
        }
        self.radio_ask(crate::wire::RadioAsk::Vote {
            station: station.to_string(),
            track,
        });
    }

    /// A station's answer, from either seam, into the state of the burrow
    /// that gave it. A request or vote turned down asks for the queue again:
    /// what the person was looking at has usually moved on.
    fn requests_answered(&self, session: Session, answer: crate::wire::RadioAnswer) {
        let again = session
            .state
            .try_update(|s| s.radio_requests.answered(answer))
            .flatten();
        if let Some(station) = again {
            let app = *self;
            defer(move || app.radio_ask_at(session, crate::wire::RadioAsk::List { station }));
        }
    }

    fn radio_ask(&self, ask: crate::wire::RadioAsk) {
        if let Some(session) = self.radio_session() {
            self.radio_ask_at(session, ask);
        }
    }

    fn radio_ask_at(&self, session: Session, ask: crate::wire::RadioAsk) {
        // Sent a tick later: the pane asks again when the song changes, and
        // that news arrives inside the socket's own borrow.
        #[cfg(target_arch = "wasm32")]
        if session.live.get_untracked() {
            let ws = session.ws;
            let ready = session.ready.get_untracked();
            defer(move || {
                if session.authenticated.try_get_untracked() == Some(true)
                    && session.ready.try_get_untracked() == Some(ready)
                    && session
                        .state
                        .try_with_untracked(|s| s.radio_requests.watching() == Some(ask.station()))
                        == Some(true)
                {
                    ws.with_value(|c| c.dispatch_radio_ask(&ask));
                }
            });
            return;
        }
        let me = session.handle.get_untracked();
        let mut answer = None;
        session
            .client
            .update_value(|c| answer = Some(c.radio_ask(&ask, &me)));
        if let Some(answer) = answer {
            self.requests_answered(session, answer);
        }
    }

    /// Ask the burrow what rooms it has.
    pub fn load_rooms(&self) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .with_value(|c| c.dispatch_room(&crate::wire::RoomCommand::List));
            return;
        }
        let rooms = self.focused().client.with_value(|c| c.rooms());
        self.focused().state.update(|s| s.rooms = rooms);
    }

    /// Read a room. Joining it is the burrow's business: ask, and the
    /// answer puts it in the list with you in it.
    pub fn show_room(&self, name: &str) {
        let name = name.to_string();
        self.focused().state.update(|s| s.room = name.clone());
        // Joining is asked every time: the list says how many are in a
        // room, not whether this person is one of them (it used to be read
        // that way, so a room with anybody else in it was never joined and
        // everything said there was refused). A burrow takes a second join
        // as the first, and says nothing to anybody about it.
        if name != crate::client::LOBBY {
            self.room_command(crate::wire::RoomCommand::Join { room: name });
        } else {
            #[cfg(target_arch = "wasm32")]
            if self.focused().live.get_untracked() {
                self.focused()
                    .ws
                    .with_value(|c| c.request_chat_history(&name));
            }
        }
    }

    /// Make a room and go into it.
    pub fn create_room(&self, name: &str, topic: &str, private: bool) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.focused().state.update(|s| s.room = name.clone());
        self.room_command(crate::wire::RoomCommand::Create {
            name,
            topic: topic.trim().to_string(),
            private,
        });
    }

    /// Say what a room is about.
    pub fn set_room_topic(&self, room: &str, topic: &str) {
        self.room_command(crate::wire::RoomCommand::SetTopic {
            room: room.to_string(),
            topic: topic.trim().to_string(),
        });
    }

    /// Ask somebody into a room.
    pub fn invite_to_room(&self, room: &str, who: &str) {
        let who = who.trim().trim_start_matches('@').to_string();
        if who.is_empty() {
            return;
        }
        self.room_command(crate::wire::RoomCommand::Invite {
            room: room.to_string(),
            who,
        });
    }

    /// Come out of a room, and go back to the lobby.
    pub fn leave_room(&self, name: &str) {
        self.room_command(crate::wire::RoomCommand::Leave {
            room: name.to_string(),
        });
        self.focused().state.update(|s| {
            s.rooms.retain(|r| r.name != name);
            s.room = crate::client::LOBBY.to_string();
        });
    }

    /// One ask about rooms, over the live socket or the demo's own.
    fn room_command(&self, command: crate::wire::RoomCommand) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.with_value(|c| c.dispatch_room(&command));
            // A topic and an invitation are answered with a bare ack, so
            // the list is what says what came of them.
            self.load_rooms();
            return;
        }
        let mut answered = None;
        self.focused()
            .client
            .update_value(|c| answered = c.room_command(&command));
        if let Some(room) = answered {
            self.focused().state.update(|s| {
                match s.rooms.iter_mut().find(|r| r.name == room.name) {
                    Some(slot) => *slot = room,
                    None => s.rooms.push(room),
                }
            });
        }
    }

    /// Ask how a room is kept: its pace, who is in it, who is muted.
    pub fn load_keeping(&self, room: &str) {
        self.keeping_command_at(
            self.focused(),
            crate::wire::RoomCommand::Keeping {
                room: room.to_string(),
            },
        );
    }

    /// Stop somebody talking in a room, for `secs` or until lifted.
    pub fn mute_in_room(&self, room: &str, who: &str, secs: Option<u32>) {
        self.keep_room(crate::wire::RoomCommand::Mute {
            room: room.to_string(),
            who: who.to_string(),
            secs,
        });
    }

    /// Let somebody talk in a room again.
    pub fn unmute_in_room(&self, room: &str, who: &str) {
        self.keep_room(crate::wire::RoomCommand::Unmute {
            room: room.to_string(),
            who: who.to_string(),
        });
    }

    /// Take somebody out of a room; `ban` keeps them out until asked back.
    pub fn remove_from_room(&self, room: &str, who: &str, ban: bool) {
        let room = room.to_string();
        self.keep_room(crate::wire::RoomCommand::Remove {
            room: room.clone(),
            who: who.to_string(),
            ban,
        });
        // Nobody else is told somebody was taken out, so look again: on the
        // same socket, after the burrow has done it.
        self.load_keeping(&room);
    }

    /// Set a room's pace: one message each every `secs`, `0` off.
    pub fn set_room_pace(&self, room: &str, secs: u32) {
        self.keep_room(crate::wire::RoomCommand::Pace {
            room: room.to_string(),
            secs,
        });
    }

    /// One act of keeping a room, from the person.
    fn keep_room(&self, command: crate::wire::RoomCommand) {
        self.keeping_command_at(self.focused(), command);
    }

    /// Send something about keeping a room to `session`. Always a tick
    /// later: this is asked from inside a reply's own sink (a mute pushed
    /// by somebody else is a reason to look again), and a send from there
    /// panics inside the socket's borrow and takes every later send with
    /// it.
    fn keeping_command_at(&self, session: Session, command: crate::wire::RoomCommand) {
        #[cfg(target_arch = "wasm32")]
        if session.live.get_untracked() {
            let ws = session.ws;
            defer(move || ws.with_value(|c| c.dispatch_room(&command)));
            return;
        }
        let me = session.handle.get_untracked();
        let mut answer = None;
        session
            .client
            .update_value(|c| answer = c.room_keeping(&command, &me));
        if let Some(answer) = answer {
            self.room_kept(session, answer);
        }
    }

    /// What a burrow said about keeping a room, into that burrow's state:
    /// look again when a room's keeping changed, go back to the lobby when
    /// this person was taken out of the room they were in.
    fn room_kept(&self, session: Session, answer: crate::wire::RoomKeepingAnswer) {
        use crate::room_keeping::Next;
        // Over the wire a refusal is said in the scrollback on its way in;
        // the demo has no wire, so it is said here.
        if let crate::wire::RoomKeepingAnswer::Refused { ask, code } = &answer {
            if !session.live.get_untracked() {
                if let Some(words) = crate::room_keeping::refusal(*ask, *code) {
                    session
                        .state
                        .update(|s| s.push_notice("the burrow", &words));
                }
            }
        }
        let me = session.handle.get_untracked();
        let now = crate::clock::now_ms();
        let next = session
            .state
            .try_update(|s| s.keeping.answered(answer, &me, now))
            .flatten();
        let app = *self;
        let look = move |room: String| {
            defer(move || {
                app.keeping_command_at(session, crate::wire::RoomCommand::Keeping { room })
            })
        };
        match next {
            Some(Next::Look(room)) => look(room),
            // A timed mute runs out without a word from the burrow: look
            // again then, so nobody is told they are muted after they are
            // not. Only the latest answer's time is kept, so a room looked
            // at often does not pile timers up.
            #[cfg(target_arch = "wasm32")]
            Some(Next::LookAt { room, at_ms }) => {
                let wait = u64::try_from(at_ms - now).unwrap_or(0) + 1_000;
                leptos::set_timeout(
                    move || {
                        let due = session
                            .state
                            .try_update(|s| s.keeping.look_due(&room, crate::clock::now_ms()))
                            .unwrap_or(false);
                        if due {
                            app.keeping_command_at(
                                session,
                                crate::wire::RoomCommand::Keeping { room },
                            );
                        }
                    },
                    std::time::Duration::from_millis(wait),
                );
            }
            #[cfg(not(target_arch = "wasm32"))]
            Some(Next::LookAt { .. }) => {}
            Some(Next::LookHere) => look(session.state.with_untracked(|s| s.room().to_string())),
            Some(Next::Leave { room, words }) => {
                // Back to the lobby, and told why there, where they land.
                session.state.update(|s| {
                    if s.room().eq_ignore_ascii_case(&room) {
                        s.room = String::new();
                    }
                    s.push_notice("the burrow", &words);
                });
                defer(move || app.load_rooms());
            }
            None => {}
        }
    }

    /// Ask the Wishing Well what people have asked for. `status` `None` is
    /// all of them.
    pub fn load_wishes(&self, status: Option<u8>) {
        self.focused().state.update(|s| s.wishes.showing = status);
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.with_value(|c| {
                c.dispatch_wish(&crate::wire::WishCommand::List { status, limit: 100 })
            });
            return;
        }
        let wishes = self.focused().client.with_value(|c| c.wishes(status));
        self.focused().state.update(|s| s.wishes.wishes = wishes);
    }

    /// Wish for something.
    pub fn make_wish(&self, kind: u8, title: &str, details: &str) {
        self.wish_command(crate::wire::WishCommand::Make {
            kind,
            title: title.trim().to_string(),
            details: details.trim().to_string(),
        });
    }

    /// Add your vote to a wish, or take it back.
    pub fn vote_wish(&self, id: i64) {
        self.wish_command(crate::wire::WishCommand::Vote { id });
    }

    /// Take a wish on, finish it, or turn it down.
    pub fn set_wish_status(&self, id: i64, status: u8, note: Option<String>) {
        self.wish_command(crate::wire::WishCommand::SetStatus { id, status, note });
    }

    /// One ask of the Wishing Well, over the live socket or the demo's own.
    fn wish_command(&self, command: crate::wire::WishCommand) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused().ws.with_value(|c| c.dispatch_wish(&command));
            return;
        }
        let mut answered = Err("nothing happened".to_string());
        self.focused()
            .client
            .update_value(|c| answered = c.wish_command(&command));
        match answered {
            Ok(wish) => self.focused().state.update(|s| s.wishes.changed(wish)),
            Err(why) => self.focused().state.update(|s| s.wishes.status = why),
        }
    }

    /// Open a picture in the gallery: ask for its bytes and keep them, so
    /// they can be drawn where they are rather than saved and forgotten.
    ///
    /// The ask goes out the same way a download does; the answer is claimed
    /// by [`AppState::art_open`] carrying this id, and anything else a
    /// person downloads still goes to disk.
    pub fn open_art(&self, id: i64, name: &str) {
        self.art_open.set(Some(crate::state::ArtOpen {
            id,
            name: name.to_string(),
            bytes: Vec::new(),
        }));
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.dispatch_file(&FileCommand::Download { id }));
            return;
        }
        // The demo answers out of its own seeded bytes.
        let file = self
            .focused()
            .client
            .with_value(|client| client.download_bytes(id));
        if let Some(file) = file {
            self.show_art(file);
        }
    }

    /// Bytes that came back for a picture: keep them if they are the ones
    /// the gallery is waiting for, and otherwise save them, which is what a
    /// download is.
    pub fn show_art(&self, file: crate::wire::DownloadedFile) {
        let wanted = self
            .art_open
            .with_untracked(|open| open.as_ref().map(|o| o.id) == Some(file.id));
        if !wanted {
            crate::save::save_bytes(&file.name, &file.mime, &file.bytes);
            return;
        }
        self.art_open.set(Some(crate::state::ArtOpen {
            id: file.id,
            name: file.name,
            bytes: file.bytes,
        }));
    }

    /// Close the gallery's picture.
    pub fn close_art(&self) {
        self.art_open.set(None);
    }

    /// Download a file inline; the completed transfer lands in the queue.
    pub fn download(&self, id: i64) {
        // In the native shell, a content-addressed file downloads via the
        // in-process swarm (many peers at once) instead of the WS inline path.
        // A seeded demo burrow has no swarm to ask. In the desktop shell this
        // used to go to the swarm anyway, asking for the all-zero root every
        // seeded node carries, and fail: "downloads don't work in the demo".
        if !self.focused().live.get_untracked() {
            let file = self
                .focused()
                .client
                .with_value(|client| client.download_bytes(id));
            self.dispatch_file(FileCommand::Download { id });
            if let Some(file) = file {
                crate::save::save_bytes(&file.name, &file.mime, &file.bytes);
            }
            return;
        }
        // A guest has no resume token, so the shell has no signed-in session of
        // its own to this burrow. The webview's socket does: download over
        // that, and the bytes are still saved through the shell.
        #[cfg(target_arch = "wasm32")]
        if crate::native::native_available() && !self.focused().is_guest.get_untracked() {
            let info = self.focused().files.with_untracked(|f| {
                f.nodes.iter().find(|n| n.id == id).and_then(|n| {
                    n.blob_id.map(|b| {
                        (
                            crate::wire::id_to_hex(&b),
                            n.size.max(0) as u64,
                            n.name.clone(),
                        )
                    })
                })
            });
            if let Some((root_hex, size, name)) = info {
                let transfer_id = id as u64;
                // Seed the local Transfer so the UI (and the swarm listener, which
                // reads the size back off it) knows the total up front.
                self.focused().files.update(|f| {
                    f.apply(&crate::wire::FileEvent::TransferOpened {
                        transfer_id,
                        size,
                        server_have: 0,
                    })
                });
                // Remember which node this transfer came from so a failure
                // can be retried without hunting for it again.
                self.focused().files.update(|f| {
                    if let Some(t) = f.transfers.iter_mut().find(|t| t.id == transfer_id) {
                        t.node_id = Some(id);
                    }
                });
                let max_sources =
                    crate::settings::clamp_max_sources(self.settings.get_untracked().max_sources);
                crate::native::start_swarm_download(
                    *self,
                    transfer_id,
                    &root_hex,
                    size,
                    &name,
                    max_sources,
                    &self.focused_burrow_label(),
                    id,
                    self.settings.get_untracked().download_from,
                    &self.focused_endpoint(),
                );
                return;
            }
            // No content hash (e.g. a legacy blob): fall through to the WS path.
        }
        self.dispatch_file(FileCommand::Download { id });
    }

    /// Try a failed transfer again.
    ///
    /// Retrying *is* resuming: the swarm engine keeps a `.rhstate` beside the
    /// destination listing the units already verified, so a second attempt
    /// re-fetches only what's missing — and re-runs source discovery, which is
    /// the point when the last attempt failed because a peer went away.
    pub fn retry_transfer(&self, transfer_id: u64) {
        let node = self.focused().files.with_untracked(|f| {
            f.transfers
                .iter()
                .find(|t| t.id == transfer_id)
                .and_then(|t| t.node_id)
        });
        match node {
            Some(id) => {
                self.focused().files.update(|f| {
                    if let Some(t) = f.transfers.iter_mut().find(|t| t.id == transfer_id) {
                        t.status = crate::files::TransferStatus::Queued;
                        t.error = None;
                        t.done = 0;
                    }
                });
                self.download(id);
            }
            // A transfer we didn't start from a known node (a resumed queue
            // entry from a previous run) has nothing to retry against — say so
            // rather than pretending the button did something.
            None => {
                self.notify(
                    crate::toasts::ToastKind::Warn,
                    "That transfer's file isn't in view \u{2014} open its folder and download it again."
                        .to_string(),
                );
            }
        }
    }

    /// Upload a small file inline into the focused burrow's current folder.
    /// The reply's node lands in the listing.
    pub fn upload(&self, name: &str, bytes: Vec<u8>) {
        self.upload_to(self.focused(), name, bytes);
    }

    /// Upload a small file inline into `session`'s current folder: the
    /// burrow the upload was started on, even if focus has moved since.
    pub fn upload_to(&self, session: Session, name: &str, bytes: Vec<u8>) {
        let (area, parent) = session
            .files
            .with_untracked(|f| (f.current_area.clone(), join_path(&f.path)));
        let Some(area) = area else {
            return;
        };
        let command = FileCommand::Upload {
            area,
            parent,
            name: name.to_string(),
            mime: crate::upload::guess_mime(name).to_string(),
            comment: String::new(),
            bytes,
        };
        #[cfg(target_arch = "wasm32")]
        if session.live.get_untracked() {
            session.ws.update_value(|c| c.dispatch_file(&command));
            self.load_upload_limits_for(session);
            return;
        }
        let files = session.files;
        session.client.update_value(|client| {
            let events = client.dispatch_file(command);
            files.update(|f| {
                for event in &events {
                    f.apply(event);
                }
            });
        });
        self.load_upload_limits_for(session);
    }

    /// Ask the focused burrow what this person may upload.
    pub fn load_upload_limits(&self) {
        self.load_upload_limits_for(self.focused());
    }

    /// Ask `session`'s burrow what this person may upload: live, by an
    /// awaited request (an old burrow's `Unsupported` must not land on a
    /// transfer row); in the demo, through the seam.
    pub fn load_upload_limits_for(&self, session: Session) {
        #[cfg(target_arch = "wasm32")]
        if session.live.get_untracked() {
            crate::upload::load_limits(session);
            return;
        }
        let files = session.files;
        session.client.update_value(|client| {
            let events = client.dispatch_file(FileCommand::GetUploadLimits);
            files.update(|f| {
                for event in &events {
                    f.apply(event);
                }
            });
        });
    }

    /// Stop an upload that is still going. Its row says so, and the sender
    /// abandons the transfer before its next chunk.
    pub fn cancel_upload(&self, key: u64) {
        // A pull into a burrow is that burrow's to stop: ask it, and let its
        // closing status say so on the row.
        let pull = self.sessions.with_untracked(|list| {
            list.iter().find_map(|(_, s)| {
                s.files
                    .with_untracked(|f| f.pull_id_of(key))
                    .map(|p| (*s, p))
            })
        });
        if let Some((session, pull_id)) = pull {
            #[cfg(target_arch = "wasm32")]
            {
                let ws = session.ws.get_value();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = ws
                        .call(&rabbithole_proto::filelib::RemotePullCancel::new(pull_id))
                        .await;
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (session, pull_id);
            return;
        }
        // A download the shell is running is the shell's to stop: it drops
        // the fetch, keeps what was verified, and says so on the row.
        #[cfg(target_arch = "wasm32")]
        {
            let native_download = crate::native::native_available()
                && self.focused().files.with_untracked(|f| {
                    f.transfers.iter().any(|t| {
                        t.id == key
                            && t.dir == crate::files::TransferDir::Download
                            && matches!(
                                t.status,
                                crate::files::TransferStatus::Active
                                    | crate::files::TransferStatus::Queued
                            )
                    })
                });
            if native_download {
                crate::native::cancel_swarm_download(key);
                return;
            }
        }
        self.sessions.with_untracked(|list| {
            for (_, session) in list {
                session.files.update(|f| f.cancel_upload(key));
            }
        });
    }

    /// The session for a burrow, if the app is on it.
    pub fn session_of(&self, id: &ServerId) -> Option<Session> {
        self.sessions
            .with_untracked(|list| list.iter().find(|(sid, _)| sid == id).map(|(_, s)| *s))
    }

    /// What a burrow is called here: its handshake name, a published theme
    /// name, or its host.
    pub fn burrow_name(&self, id: &ServerId) -> String {
        self.session_of(id)
            .and_then(|s| {
                s.name.get_untracked().or_else(|| {
                    s.server_theme
                        .with_untracked(|t| t.as_ref().map(|o| o.name.clone()))
                })
            })
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| server_label(id))
    }

    /// The other burrows the person could send to from the focused one: live,
    /// connected, and signed in (a guest cannot receive anything).
    pub fn send_targets(&self) -> Vec<(ServerId, String)> {
        let focused = self.focused_id.get();
        let ids: Vec<ServerId> = self.sessions.with(|list| {
            list.iter()
                .filter(|(id, s)| {
                    *id != focused
                        && !id.is_placeholder()
                        && s.live.get()
                        && !s.is_guest.get()
                        && s.state.with(|st| st.conn == crate::conn::ConnState::Online)
                })
                .map(|(id, _)| id.clone())
                .collect()
        });
        ids.into_iter()
            .map(|id| {
                let name = self.burrow_name(&id);
                (id, name)
            })
            .collect()
    }

    /// Whether "Send to another burrow" belongs on screen: a signed-in, live
    /// session here and at least one other burrow to send to.
    pub fn can_send(&self) -> bool {
        let here = self.focused_tracked();
        here.live.get() && !here.is_guest.get() && !self.send_targets().is_empty()
    }

    /// Open the send dialog for a file or folder of the focused burrow.
    pub fn ask_send(&self, node_id: i64, name: &str, is_folder: bool) {
        self.sending.set(Some(SendAsk {
            node_id,
            name: name.to_string(),
            is_folder,
            source: self.focused_id.get_untracked(),
        }));
    }

    /// Grant or revoke the admin capability for the current session. Gates the
    /// admin nav and routes.
    pub fn set_admin(&self, is_admin: bool) {
        self.focused().is_admin.set(is_admin);
    }

    /// Drive one [`AdminCommand`] through the seam and fold its admin events
    /// into the [`AdminState`]. Live: write over the socket; replies fold
    /// through the admin sink registered in `connect_live`.
    fn dispatch_admin(&self, command: AdminCommand) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.dispatch_admin(&command));
            return;
        }
        let admin = self.admin;
        let syndication = self.syndication;
        self.focused().client.update_value(|client| {
            let events: Vec<AdminEvent> = client.dispatch_admin(command);
            admin.update(|a| {
                for event in &events {
                    a.apply(event);
                }
            });
            syndication.update(|s| s.apply_live(None, &events));
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
            limit: ACCOUNT_PAGE,
        });
    }

    /// Find accounts by part of a login. The console narrows what it has
    /// as the operator types, and asks the burrow as well, because what
    /// they are looking for may not be on the page that is loaded — and
    /// because a narrower search's answer is not an answer to a wider one.
    pub fn find_accounts(&self, find: &str) {
        let find = find.trim().to_string();
        self.admin.update(|a| a.account_find = find.clone());
        if find.is_empty() {
            self.load_accounts();
            return;
        }
        self.dispatch_people(AdminCommand::FindAccounts {
            find,
            offset: 0,
            limit: ACCOUNT_PAGE,
        });
    }

    /// The accounts again, as they were last asked for: the search that is
    /// on, else the first page.
    pub fn reload_accounts(&self) {
        let find = self.admin.with_untracked(|a| a.account_find.clone());
        if find.is_empty() {
            self.load_accounts();
        } else {
            self.dispatch_people(AdminCommand::FindAccounts {
                find,
                offset: 0,
                limit: ACCOUNT_PAGE,
            });
        }
    }

    /// Ask before removing an account: it cannot be undone.
    pub fn ask_remove_account(&self, login: &str) {
        self.confirm.set(Some(ConfirmAsk::remove_account(login)));
    }

    /// Remove an account for good. What to show afterwards is the answer's
    /// to say (`Reload::Accounts`), which keeps the search that is on.
    pub fn remove_account(&self, login: &str) {
        self.dispatch_people(AdminCommand::DeleteAccount {
            login: login.to_string(),
        });
    }

    /// Enable or disable an account. A disabled account is signed out at once.
    pub fn set_account_disabled(&self, login: &str, disabled: bool) {
        self.dispatch_people(AdminCommand::SetAccount {
            login: login.to_string(),
            role: None,
            class: None,
            disabled: Some(disabled),
        });
    }

    /// Ask the focused burrow to describe its settings. Settings staged for
    /// another burrow are dropped first: a draft is an edit to one place.
    pub fn load_config(&self) {
        let here = self.focused_id.get_untracked();
        if self.admin_settings_owner.get_untracked().as_ref() != Some(&here) {
            self.admin_settings.set(Default::default());
            self.admin_settings_owner.set(Some(here));
        }
        self.admin_settings.update(|s| s.loading());
        self.dispatch_settings(
            crate::admin_settings::DESCRIBE,
            AdminCommand::DescribeConfig,
        );
        self.load_surfaces();
    }

    /// Ask what the burrow's optional surfaces are actually doing.
    pub fn load_surfaces(&self) {
        self.dispatch_settings(
            crate::admin_settings::SURFACES,
            AdminCommand::GetSurfaceStatus,
        );
    }

    /// Drive one config command and fold its reply, paired with the key it
    /// was about. Live, the transport does the pairing and the reply arrives
    /// through the admin sink; the demo burrow answers on the spot.
    fn dispatch_settings(&self, key: &str, command: AdminCommand) {
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.dispatch_admin(&command));
            return;
        }
        let mut events = Vec::new();
        self.focused()
            .client
            .update_value(|client| events = client.dispatch_admin(command));
        self.fold_settings_reply(Some(key), &events);
    }

    /// Fold a config reply into the settings model. `key` is what the request
    /// was about: a config key, or [`crate::admin_settings::DESCRIBE`].
    pub fn fold_settings_reply(&self, key: Option<&str>, events: &[AdminEvent]) {
        let Some(key) = key else {
            return;
        };
        let was_saving = self.admin_settings.with_untracked(|s| s.saving());
        let mut fall_back = false;
        self.admin_settings.update(|s| {
            for event in events {
                match event {
                    AdminEvent::ConfigDescribed(entries) => s.described(entries),
                    AdminEvent::SurfacesReported(surfaces) => s.surfaces_reported(surfaces),
                    // An older burrow has no such report. Nothing is shown.
                    AdminEvent::Failed(_) if key == crate::admin_settings::SURFACES => {}
                    AdminEvent::ConfigLoaded { key, value } => s.bare_value(key, value),
                    AdminEvent::ConfigApplied { applied_live } => s.saved(key, *applied_live),
                    AdminEvent::Failed(_) if key == crate::admin_settings::DESCRIBE => {
                        s.describe_refused();
                        fall_back = true;
                    }
                    AdminEvent::Failed(detail) => s.refused(key, detail),
                    _ => {}
                }
            }
        });
        let finished = was_saving && self.admin_settings.with_untracked(|s| !s.saving());
        if finished {
            if let Some((clean, line)) = self.admin_settings.with_untracked(|s| s.save_summary()) {
                let kind = if clean {
                    crate::toasts::ToastKind::Success
                } else {
                    crate::toasts::ToastKind::Warn
                };
                self.notify(kind, line);
            }
        }
        // Anything that talks to the burrow again waits a tick: live, this
        // runs inside the transport's own borrow.
        if fall_back || finished {
            let app = *self;
            crate::app::defer(move || {
                if fall_back {
                    // An older burrow: read the well-known keys one at a time.
                    for key in crate::admin::OPERATOR_KEYS {
                        app.dispatch_settings(
                            key,
                            AdminCommand::GetConfig {
                                key: key.to_string(),
                            },
                        );
                    }
                } else {
                    // The burrow may have tidied what it was given (a role
                    // name, a trailing slash): show what it actually holds.
                    app.dispatch_settings(
                        crate::admin_settings::DESCRIBE,
                        AdminCommand::DescribeConfig,
                    );
                    // A saved switch may have started or stopped something.
                    app.load_surfaces();
                    // The feed pane reads a few of these keys on its own.
                    app.load_syndication();
                }
            });
        }
    }

    /// Save every staged burrow setting: one `ConfigSet` per changed key.
    /// Refuses to run if the focus moved to another burrow since the edits
    /// were made.
    pub fn save_admin_settings(&self) {
        let here = self.focused_id.get_untracked();
        if self.admin_settings_owner.get_untracked().as_ref() != Some(&here) {
            return;
        }
        let mut batch = Vec::new();
        self.admin_settings.update(|s| batch = s.begin_save());
        for (key, value) in batch {
            self.dispatch_settings(
                &key,
                AdminCommand::SetConfig {
                    key: key.clone(),
                    value,
                },
            );
        }
    }

    /// Route a tagged admin reply to the model that is waiting for it: the
    /// settings model for config keys and its two markers, the People model
    /// for account, class and invitation actions.
    pub fn fold_admin_reply(&self, tag: Option<&str>, events: &[AdminEvent]) {
        let is_people = tag.is_some_and(|t| !crate::admin_settings::is_settings_tag(t));
        if !is_people {
            self.fold_settings_reply(tag, events);
            return;
        }
        let tag = tag.unwrap_or_default().to_string();
        // A tag that names a burrow is only for that burrow. Sessions run
        // side by side, each with its own socket, and the console models
        // are the app's, not a session's: an answer that arrives after the
        // operator has moved on would otherwise be read as this burrow's.
        if let Some(at) = tag.strip_prefix("*board-keeping:") {
            if at != self.focused_endpoint() {
                return;
            }
        }
        // A search for an account is tagged (so a burrow too old to answer
        // it can say so), and its answer is the same account list the
        // plain listing gives.
        for event in events {
            match event {
                AdminEvent::AccountsListed { accounts, total } => {
                    let (accounts, total) = (accounts.clone(), *total);
                    self.admin.update(|a| {
                        a.accounts = accounts;
                        a.account_total = total;
                    });
                }
                // What each board keeps is asked for with a tag, so a
                // burrow too old to say can say that instead. This is the
                // only place the map is written; the guard above has
                // already established the answer is this burrow's.
                AdminEvent::BoardKeepingListed(boards) => {
                    let said = crate::admin::Keeping::said(boards);
                    self.admin.update(|a| a.board_keeping = said);
                }
                // Said plainly rather than left looking like a burrow that
                // keeps nothing.
                AdminEvent::Failed(detail)
                    if tag.starts_with("*board-keeping:") && detail.contains("Unsupported") =>
                {
                    self.admin
                        .update(|a| a.board_keeping = crate::admin::Keeping::Cannot);
                }
                _ => {}
            }
        }
        // Listings land in the moderation model as they are.
        self.moderation.update(|m| {
            for event in events {
                match event {
                    AdminEvent::ReportsListed(reports, total) => {
                        m.reports = reports.clone();
                        m.total = *total;
                    }
                    AdminEvent::HeldListed(list, total) => {
                        m.held = list.clone();
                        m.held_total = *total;
                        m.held_unknown = false;
                    }
                    // A burrow from before the list existed holds content
                    // and cannot say what: not the same as holding none.
                    AdminEvent::Failed(detail)
                        if tag == "*held-list" && detail.contains("Unsupported") =>
                    {
                        m.held_unknown = true;
                    }
                    AdminEvent::DenyHashesListed(list) => m.deny = list.clone(),
                    AdminEvent::AuditListed(list) => m.audit = list.clone(),
                    _ => {}
                }
            }
        });
        self.federation.update(|f| {
            for event in events {
                match event {
                    AdminEvent::PeersListed(list) => f.peers = list.clone(),
                    AdminEvent::OriginsListed(list) => f.origins = list.clone(),
                    AdminEvent::BackupsListed(dir, list) => {
                        f.listed_backups(dir.clone(), list.clone())
                    }
                    AdminEvent::BackupChecked(result) => f.checked(result.clone()),
                    AdminEvent::StationsListed(list) => f.stations = list.clone(),
                    _ => {}
                }
            }
        });
        let mut reload = crate::admin_people::Reload::Nothing;
        let mut said = None;
        self.people.update(|p| {
            reload = p.apply(&tag, events);
            said = p.notice.take();
        });
        // A toast, not a line at the top of the pane: the operator may be a
        // screen further down, where the thing they just did is.
        if let Some((ok, text)) = said {
            let kind = if ok {
                crate::toasts::ToastKind::Success
            } else {
                crate::toasts::ToastKind::Warn
            };
            self.notify(kind, text);
        }
        if reload == crate::admin_people::Reload::Nothing {
            return;
        }
        // Live, this runs inside the transport's borrow: reload a tick later.
        let app = *self;
        defer(move || match reload {
            crate::admin_people::Reload::Accounts => app.reload_accounts(),
            crate::admin_people::Reload::Classes => app.load_classes(),
            crate::admin_people::Reload::Invites => app.load_invites(),
            crate::admin_people::Reload::Boards => {
                app.load_boards();
                // What a board keeps is its own listing, and saving a board
                // is usually how it changes.
                app.load_board_keeping();
            }
            crate::admin_people::Reload::Reports => app.load_reports(None),
            crate::admin_people::Reload::DenyHashes => app.load_deny_hashes(),
            crate::admin_people::Reload::Sessions => app.refresh_who(),
            crate::admin_people::Reload::Peers => app.load_peers(),
            crate::admin_people::Reload::Origins => app.load_origins(),
            crate::admin_people::Reload::Backups => app.load_backups(),
            crate::admin_people::Reload::Areas => app.load_areas(),
            crate::admin_people::Reload::Folder => {
                app.focused().files.update(|f| f.selected = None);
                app.refresh_files();
            }
            crate::admin_people::Reload::Thread => {
                if let Some(id) = app
                    .focused()
                    .state
                    .with_untracked(|s| s.selected_thread.clone())
                {
                    app.open_thread(id);
                }
            }
            crate::admin_people::Reload::Nothing => {}
        });
    }

    /// Drive one People action and fold its reply under its own tag, so the
    /// pane can say what came of *that* action.
    fn dispatch_people(&self, command: AdminCommand) {
        let Some(tag) = command.tag() else {
            return;
        };
        self.people.update(|p| p.sent(&tag));
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.dispatch_admin(&command));
            return;
        }
        let mut events = Vec::new();
        self.focused()
            .client
            .update_value(|client| events = client.dispatch_admin(command));
        // The demo burrow answers a list request with the list itself.
        let admin = self.admin;
        admin.update(|a| {
            for event in &events {
                a.apply(event);
            }
        });
        self.fold_admin_reply(Some(&tag), &events);
    }

    /// Make an account with `role` (a `Role` ordinal).
    pub fn create_account(&self, login: &str, password: &str, role: u8) {
        self.dispatch_people(AdminCommand::CreateAccount {
            login: login.trim().to_string(),
            password: password.into(),
            role,
        });
    }

    /// Give an account a new password. It is signed out everywhere.
    pub fn set_account_password(&self, login: &str, password: &str) {
        self.dispatch_people(AdminCommand::SetAccountPassword {
            login: login.to_string(),
            password: password.into(),
        });
    }

    /// Remove an account's two-factor enrolment.
    pub fn reset_account_totp(&self, login: &str) {
        self.dispatch_people(AdminCommand::ResetAccountTotp {
            login: login.to_string(),
        });
    }

    /// Change an account's role.
    pub fn set_account_role(&self, login: &str, role: u8) {
        self.dispatch_people(AdminCommand::SetAccount {
            login: login.to_string(),
            role: Some(role),
            class: None,
            disabled: None,
        });
    }

    /// Put an account in a class (`""` takes it out of any).
    pub fn set_account_class(&self, login: &str, class: &str) {
        self.dispatch_people(AdminCommand::SetAccount {
            login: login.to_string(),
            role: None,
            class: Some(class.to_string()),
            disabled: None,
        });
    }

    /// Create a class, or change what one allows.
    pub fn save_class(&self, name: &str, base_mask: u64) {
        self.dispatch_people(AdminCommand::SetClass {
            name: name.trim().to_string(),
            base_mask,
        });
    }

    /// Load the invitations.
    pub fn load_invites(&self) {
        self.dispatch_people(AdminCommand::ListInvites);
    }

    /// Withdraw an invitation nobody has used.
    pub fn revoke_invite(&self, code: &str) {
        self.dispatch_people(AdminCommand::RevokeInvite {
            code: code.to_string(),
        });
    }

    /// The report queue, for one state or all of them.
    pub fn load_reports(&self, state: Option<u8>) {
        self.dispatch_people(AdminCommand::ListReports { state });
    }

    /// Claim, resolve or dismiss a report.
    pub fn resolve_report(&self, id: i64, action: u8, note: &str) {
        self.dispatch_people(AdminCommand::ResolveReport {
            id,
            action,
            note: note.trim().to_string(),
        });
    }

    /// What is being held back for review, from the top.
    pub fn load_held(&self) {
        self.dispatch_people(AdminCommand::ListHeld { offset: 0 });
    }

    /// Hold something back: it reads as absent everywhere until it is let
    /// through. The list is asked for again, since nothing announces it.
    pub fn hold_back(&self, kind: u8, subject: Vec<u8>, reason: &str) {
        self.dispatch_people(AdminCommand::Hold {
            kind,
            subject,
            reason: reason.trim().to_string(),
        });
        self.load_held();
    }

    /// Let held content through again.
    pub fn let_through(&self, kind: u8, subject: Vec<u8>) {
        self.dispatch_people(AdminCommand::LetThrough { kind, subject });
        self.load_held();
    }

    /// The hash-deny list.
    pub fn load_deny_hashes(&self) {
        self.dispatch_people(AdminCommand::ListDenyHashes);
    }

    /// Refuse a file by its content hash, wherever it is uploaded.
    pub fn add_deny_hash(&self, hash: [u8; 32], reason: &str) {
        self.dispatch_people(AdminCommand::AddDenyHash {
            hash,
            reason: reason.trim().to_string(),
        });
    }

    /// Allow a denied hash again.
    pub fn remove_deny_hash(&self, hash: [u8; 32]) {
        self.dispatch_people(AdminCommand::RemoveDenyHash { hash });
    }

    /// The audit log's newest lines.
    pub fn load_audit(&self) {
        self.dispatch_people(AdminCommand::ListAudit { limit: 200 });
    }

    /// The burrows this one has met over federation.
    pub fn load_peers(&self) {
        self.dispatch_people(AdminCommand::ListPeers);
    }

    /// Approve a peer, bound to `origin` or to the one it announced.
    pub fn approve_peer(&self, key: [u8; 32], origin: Option<String>) {
        self.dispatch_people(AdminCommand::ApprovePeer {
            key,
            origin: origin
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty()),
        });
    }

    /// Withdraw a peer's approval; its session closes.
    pub fn revoke_peer(&self, key: [u8; 32]) {
        self.dispatch_people(AdminCommand::RevokePeer { key });
    }

    /// The origins whose signing keys are believed.
    pub fn load_origins(&self) {
        self.dispatch_people(AdminCommand::ListOrigins);
    }

    /// Pin an origin's key by hand.
    pub fn pin_origin(&self, origin: &str, key: [u8; 32]) {
        self.dispatch_people(AdminCommand::PinOrigin {
            origin: origin.trim().to_string(),
            key,
        });
    }

    /// The snapshots in the backup folder.
    pub fn load_backups(&self) {
        self.dispatch_people(AdminCommand::ListBackups);
    }

    /// Ask what each station is doing, for the Radio pane.
    pub fn load_stations(&self) {
        self.dispatch_people(AdminCommand::ListStations);
    }

    /// Make a snapshot now.
    pub fn make_backup(&self) {
        self.dispatch_people(AdminCommand::MakeBackup);
    }

    /// Check a snapshot against its manifest and the database's own check.
    pub fn verify_backup(&self, name: &str) {
        self.dispatch_people(AdminCommand::VerifyBackup {
            name: name.to_string(),
        });
    }

    /// Remove a snapshot.
    pub fn delete_backup(&self, name: &str) {
        self.dispatch_people(AdminCommand::DeleteBackup {
            name: name.to_string(),
        });
    }

    /// Disconnect a session, and hear how it went.
    pub fn kick_session(&self, session_id: u64) {
        self.dispatch_people(AdminCommand::Kick { session_id });
    }

    /// Say something to everyone connected, and hear how it went.
    pub fn broadcast_notice(&self, text: &str) {
        self.dispatch_people(AdminCommand::Broadcast {
            text: text.trim().to_string(),
        });
    }

    /// Make a file area.
    pub fn create_area(&self, slug: &str, title: &str, description: &str) {
        self.dispatch_people(AdminCommand::CreateArea {
            slug: slug.trim().to_string(),
            title: title.trim().to_string(),
            description: description.trim().to_string(),
        });
    }

    /// Change what a file area is called and says about itself.
    pub fn update_area(&self, slug: &str, title: &str, description: &str) {
        self.dispatch_people(AdminCommand::UpdateArea {
            slug: slug.to_string(),
            title: title.trim().to_string(),
            description: description.trim().to_string(),
        });
    }

    /// Remove an empty file area.
    pub fn delete_area(&self, slug: &str) {
        self.dispatch_people(AdminCommand::DeleteArea {
            slug: slug.to_string(),
        });
    }

    /// Make a folder where Files is looking.
    pub fn create_folder_here(&self, name: &str, is_dropbox: bool) {
        let (area, path) = self
            .focused()
            .files
            .with_untracked(|f| (f.current_area.clone(), join_path(&f.path)));
        let Some(area) = area else {
            return;
        };
        self.dispatch_people(AdminCommand::CreateFolder {
            area,
            parent: path,
            name: name.trim().to_string(),
            is_dropbox,
        });
    }

    /// Remove a file, or a folder with everything in it.
    pub fn delete_node(&self, id: i64, name: &str) {
        self.dispatch_people(AdminCommand::DeleteNode {
            id,
            name: name.to_string(),
        });
    }

    /// Change a file's description.
    pub fn describe_node(&self, id: i64, name: &str, icon: &str, comment: &str) {
        self.dispatch_people(AdminCommand::DescribeNode {
            id,
            name: name.to_string(),
            icon: icon.to_string(),
            comment: comment.trim().to_string(),
        });
    }

    /// Give a file or folder a new name.
    pub fn rename_node(&self, id: i64, name: &str, new_name: &str) {
        self.dispatch_people(AdminCommand::RenameNode {
            id,
            name: name.to_string(),
            new_name: new_name.trim().to_string(),
        });
    }

    /// Pick a file or folder up, to put down in another folder of its area.
    pub fn pick_up_node(&self, id: i64, name: &str, is_folder: bool) {
        let files = self.focused().files;
        let (area, from) = files.with_untracked(|f| (f.current_area.clone(), join_path(&f.path)));
        let Some(area) = area else {
            return;
        };
        files.update(|f| {
            f.carrying = Some(crate::files::Carried {
                id,
                name: name.to_string(),
                area,
                from,
                is_folder,
            });
            f.selected = None;
        });
    }

    /// Put what is carried back where it was: nothing moves.
    pub fn put_down_node(&self) {
        self.focused().files.update(|f| f.carrying = None);
    }

    /// Put what is carried down in the folder Files is looking at.
    pub fn move_node_here(&self) {
        let files = self.focused().files;
        let (carried, why_not, here) = files.with_untracked(|f| {
            (
                f.carrying.clone(),
                f.cannot_put_down_here(),
                join_path(&f.path),
            )
        });
        let Some(carried) = carried else {
            return;
        };
        if let Some(why) = why_not {
            self.notify(crate::toasts::ToastKind::Warn, why.to_string());
            return;
        }
        files.update(|f| f.carrying = None);
        self.dispatch_people(AdminCommand::MoveNode {
            id: carried.id,
            name: carried.name,
            folder: here,
        });
    }

    /// Take a post down (its author, or a board moderator).
    pub fn delete_post(&self, id: &str) {
        self.dispatch_people(AdminCommand::DeletePost { id: id.to_string() });
    }

    /// Make a board (`kind` 2) or a category (`kind` 0).
    pub fn create_board(
        &self,
        slug: &str,
        title: &str,
        description: &str,
        kind: u8,
        parent: Option<String>,
    ) {
        self.dispatch_people(AdminCommand::CreateBoard {
            slug: slug.trim().to_string(),
            title: title.trim().to_string(),
            description: description.trim().to_string(),
            kind,
            parent,
        });
    }

    /// Change what a board is called, says about itself, and keeps.
    pub fn update_board(&self, slug: &str, title: &str, description: &str, keep: Option<u32>) {
        self.dispatch_people(AdminCommand::UpdateBoard {
            slug: slug.to_string(),
            title: title.trim().to_string(),
            description: description.trim().to_string(),
            keep,
        });
    }

    /// What every board keeps: the listing carries neither the limit nor
    /// how many threads there are. What another burrow said about a board
    /// of the same name is not this one's, so it goes first.
    pub fn load_board_keeping(&self) {
        let at = self.focused_endpoint();
        self.admin
            .update(|a| a.board_keeping = crate::admin::Keeping::Asking);
        self.dispatch_people(AdminCommand::ListBoardKeeping { at });
    }

    /// Put a board where it should be read: straight after `after`, or
    /// first when there is none.
    pub fn move_board(&self, slug: &str, after: Option<String>) {
        self.dispatch_people(AdminCommand::MoveBoard {
            slug: slug.to_string(),
            after,
        });
    }

    /// Remove an empty board.
    pub fn delete_board(&self, slug: &str) {
        self.dispatch_people(AdminCommand::DeleteBoard {
            slug: slug.to_string(),
        });
    }

    /// A strong password to hand to someone, from the browser's randomness.
    pub fn generate_password(&self) -> String {
        #[cfg(target_arch = "wasm32")]
        let bytes = {
            let mut bytes = [0u8; 15];
            if let Some(crypto) = web_sys::window().and_then(|w| w.crypto().ok()) {
                let _ = crypto.get_random_values_with_u8_array(&mut bytes);
            }
            bytes
        };
        // Off the browser there is no randomness to draw on, and no one to
        // hand a password to.
        #[cfg(not(target_arch = "wasm32"))]
        let bytes = [0u8; 15];
        crate::admin_people::password_from(&bytes)
    }

    /// Mint an invite code with the given time-to-live in seconds.
    pub fn create_invite(&self, ttl_secs: i64) {
        self.dispatch_people(AdminCommand::CreateInvite { ttl_secs });
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

    /// Publish a postcard theme bundle to the focused burrow (empty
    /// signature — the server signs at serve time).
    pub fn publish_theme(&self, bundle: Vec<u8>) {
        self.dispatch_admin(AdminCommand::SetThemeBundle { bundle });
    }

    /// Drive one `GetConfig` for the Syndication & Gateways panel and fold
    /// its replies — paired with the requested `key` so the reducer knows
    /// which read failed (the wire's `Failed` carries no key).
    fn dispatch_syn_get(&self, key: &str) {
        let command = AdminCommand::GetConfig {
            key: key.to_string(),
        };
        #[cfg(target_arch = "wasm32")]
        if self.focused().live.get_untracked() {
            self.focused()
                .ws
                .update_value(|c| c.dispatch_admin(&command));
            return;
        }
        let syndication = self.syndication;
        self.focused().client.update_value(|client| {
            let events = client.dispatch_admin(command);
            syndication.update(|s| s.apply_get_reply(key, &events));
        });
    }

    /// Load what the feeds pane reads: the syndication knobs, the TOML-only
    /// `syndication_feeds` attempt, and the live gateway-stats snapshot.
    pub fn load_syndication(&self) {
        for key in crate::syndication_admin::LOAD_KEYS {
            self.dispatch_syn_get(key);
        }
        self.dispatch_admin(AdminCommand::GetGatewayStats);
    }

    /// Fold one routed notice: `[radio]` bridge updates feed the radio
    /// reducer silently; ordinary operator notices land in the chat log.
    /// Mirrors the TUI's routing split — the transport slice calls this from
    /// its notice sink.
    pub fn apply_notice(&self, route: NoticeRoute) {
        match route {
            NoticeRoute::Radio(update) => self.radio.update(|r| {
                r.apply_update(update);
            }),
            NoticeRoute::Chat { from, text } => {
                self.focused().state.update(|s| s.push_notice(&from, &text));
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
        // In a live session the burrow is asked: what is on, and where. Never
        // mix seeded mock stations in.
        if self.skip_mock_load() {
            #[cfg(target_arch = "wasm32")]
            self.focused()
                .ws
                .update_value(|c| c.request_radio_stations());
            return;
        }
        // The demo: the seeded listing, decoded the way a live one is.
        if let Some(listing) = self.focused().client.with_value(|c| c.radio_listing()) {
            let endpoint = self.focused_endpoint();
            self.radio.update(|r| {
                r.apply_listing(
                    crate::radio::Tuning {
                        endpoint,
                        stream_base: listing.stream_base,
                        port: listing.port,
                    },
                    listing.stations,
                )
            });
        }
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

    pub fn set_radio_ducking(&self, enabled: bool) {
        self.radio_prefs.update(|p| p.ducking = enabled);
        self.radio_prefs_changed();
    }

    #[cfg(target_arch = "wasm32")]
    fn play_chime(&self, chime: crate::sound::Chime) {
        let player = self.player;
        crate::sound::play_with(chime, move |duration| {
            player.try_update_value(|p| p.duck_for_chime(duration));
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub fn preview_chime(&self) -> impl std::future::Future<Output = Result<(), &'static str>> {
        let player = self.player;
        crate::sound::preview_with(crate::sound::Chime::Dm, move |duration| {
            player.try_update_value(|p| p.duck_for_chime(duration));
        })
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

    /// Retry a refused or failed stream directly from a user gesture.
    pub fn retry_radio(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let prefs = self.radio_prefs.get_untracked();
            self.player.update_value(|player| player.retry(&prefs));
        }
    }

    /// Persist the preferences and reconcile the audio element (both are
    /// browser-only edges; no-ops on the host).
    fn radio_prefs_changed(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let prefs = self.radio_prefs.get_untracked();
            crate::radio::storage::save_prefs(&prefs);
            // Where to tune in is the burrow's to say (`RadioState::stream_url`).
            let url = prefs
                .station
                .as_deref()
                .and_then(|station| self.radio.with_untracked(|r| r.stream_url(station)));
            self.player.update_value(|p| p.sync(&prefs, url));
        }
    }
}

/// The theme choice a fresh session starts with: the persisted choice on the
/// browser, else the default (follow-OS).
/// A short human label for a burrow rail tile before its server name is known.
/// The offline demo session reads as "Demo"; a live session falls back to the
/// host of its dial endpoint.
/// The send dialog: it speaks to live burrows, so the browser build has it
/// and the host build (tests) has nothing in its place.
fn send_dialog() -> View {
    #[cfg(target_arch = "wasm32")]
    return view! { <crate::send_view::SendDialog/> }.into_view();
    #[cfg(not(target_arch = "wasm32"))]
    ().into_view()
}

fn server_label(id: &ServerId) -> String {
    if id.0 == "local" {
        return "Demo".to_string();
    }
    let host =
        id.0.trim_start_matches("ws://")
            .trim_start_matches("wss://");
    host.split(['/', ':']).next().unwrap_or(host).to_string()
}

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
    set_current(app);
    #[cfg(target_arch = "wasm32")]
    crate::appearance::watch_system_mode(app.system_dark);

    // In the native shell, listen for swarm download progress and fold it into
    // the Transfers reducer. No-op on the web build.
    #[cfg(target_arch = "wasm32")]
    crate::native::install_swarm_listener(app);
    app.load_download_prefs();

    // Load (or mint) the portable identity that names you across every burrow.
    // Keep the full signing identity (for friendship attestations) in a
    // StoredValue, and expose only its public face in the reactive `you`.
    #[cfg(target_arch = "wasm32")]
    {
        let id = crate::identity::load_or_create();
        app.you.set(Some(id.you()));
        app.identity.set_value(Some(id));
        // Restore the persisted sightings ledger and friendships.
        app.sightings.set(crate::sightings::storage::load());
        app.friends.set(crate::friend::storage::load());
        app.settings.set(crate::settings::storage::load());
        app.my_mark.set(crate::avatar::chosen::load());
    }

    // Reflect cross-burrow unread in the browser tab title, so a backgrounded
    // warren still signals activity: "(3) RabbitHole".
    #[cfg(target_arch = "wasm32")]
    create_effect(move |_| {
        let n = app.total_unread();
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let title = if n == 0 {
                "RabbitHole".to_string()
            } else if n > 99 {
                "(99+) RabbitHole".to_string()
            } else {
                format!("({n}) RabbitHole")
            };
            doc.set_title(&title);
        }
    });

    // Persist connections across loads: auto-reconnect to every burrow that left
    // a resume token, and land in the lobby instead of the login screen. Runs
    // before <Router> mounts, so the URL is already `/lobby` when it reads it.
    #[cfg(target_arch = "wasm32")]
    {
        let recent = crate::recent::load();
        let (resumable, blocked) = crate::recent::secure_resumable(&recent);
        if !blocked.is_empty() {
            app.toasts.update(|queue| {
                queue.push(
                    crate::toasts::ToastKind::Warn,
                    format!(
                        "Skipped {} saved plaintext remote connection{}; replace ws:// with wss:// to reconnect safely.",
                        blocked.len(),
                        if blocked.len() == 1 { "" } else { "s" }
                    ),
                );
            });
        }
        if !resumable.is_empty() {
            for (endpoint, token) in resumable {
                app.reconnect_live(endpoint, token);
            }
            if let Some(hist) = web_sys::window().and_then(|w| w.history().ok()) {
                let _ =
                    hist.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/lobby"));
            }
        }
    }

    let style = move || {
        let (pack, mode) = (app.theme.get().pack, app.mode());
        let mut appearance = app.settings.get().appearance;
        // The old boolean is migration input; the new scoped choice owns policy.
        appearance.use_burrow_theme = true;
        let session = app.focused_tracked();
        let mode_choice = session
            .theme_choice
            .get()
            .unwrap_or(app.theme_preferences.get().default);
        app.custom_pack.with(|custom| {
            session.server_theme.with(|server| {
                let overlay = server
                    .as_ref()
                    .and_then(|server| mode_choice.overlay(server));
                crate::appearance::resolve_style(
                    &appearance,
                    custom.as_ref(),
                    overlay.as_ref(),
                    pack,
                    mode,
                )
            })
        })
    };

    view! {
        <style>{STYLESHEET}</style>
        <style>{crate::appearance::STYLES}</style>
        // Re-paint the page itself with the theme's background. index.html
        // paints `html` with a fixed dark pre-boot backdrop (no white flash on
        // first frame); once the theme is resolved, anywhere the app fails to
        // cover — a viewport gap, overscroll, a webview quirk — must show the
        // theme's colour, not that backdrop. The black frame around the whole
        // app that shipped for weeks was exactly this: an unpainted 8px body
        // margin rendering the pre-boot dark.
        <style>{move || {
            format!(
                "html,body{{background:{}}}",
                crate::theme_css::background_of(&style())
            )
        }}</style>
        <Router>
            // `native` marks the desktop shell: the window's own title bar is
            // hidden there, so the app's header takes over that job (drag
            // region, room for the traffic lights). In a browser tab none of
            // that applies and the class is absent.
            <div class="rh-app" class:native=is_native() style=style
                class:rh-compact=move || app.settings.get().appearance.density == crate::appearance::Density::Compact
                class:rh-times-always=move || app.settings.get().appearance.timestamps == crate::appearance::Timestamps::Always
                class:rh-times-hidden=move || app.settings.get().appearance.timestamps == crate::appearance::Timestamps::Hidden
                class:rh-no-chat-icons=move || !app.settings.get().appearance.show_avatars
                class:rh-reduce-motion=move || app.settings.get().appearance.reduce_motion>
                // The desktop title bar. A real element carrying
                // `data-tauri-drag-region`, because that attribute is the only
                // thing Tauri's WKWebView drag handler looks for —
                // `-webkit-app-region:drag`, which this used to rely on, is a
                // Chromium extension and a silent no-op here. With the system
                // title bar hidden that no-op meant the window couldn't be
                // moved by its own chrome at all.
                <Show when=is_native fallback=|| ()>
                    <div class="rh-drag-strip" data-tauri-drag-region="true"></div>
                </Show>
                <a class="rh-skip" href=crate::a11y::SKIP_HREF rel="external">
                    "Skip to main content"
                </a>
                <RouteFocus/>
                <PlaceGuard/>
                <CommandPalette/>
                <ConfirmDialog/>
                {send_dialog()}
                <WarrenSheet/>
                <Toasts/>
                <crate::pwa::PwaNotice/>
                <div class="rh-shell">
                    <BurrowRail/>
                    <SideNav/>
                    <PhoneTabBar/>
                    <div class="rh-shell-main">
                        <WelcomeSheet/>
                        // Remount the place when the focused burrow changes,
                        // so each view re-binds the newly-focused session's
                        // signals. The URL is unchanged (it lives on <Router>),
                        // so the same route re-renders against the new session.
                        //
                        // A keyed <For>, not a `move ||` child: a dynamic
                        // child wrapping <Routes> re-ran on *every* navigation
                        // (leptos_router warned "only render <Routes/> once" on
                        // each click, even with nothing tracked), rebuilding the
                        // whole routed tree twice per click and panicking any
                        // effect the first build had queued (OwnerDisposed). A
                        // keyed list only rebuilds its child when the key — the
                        // focused burrow — actually changes.
                        <For
                            each=move || vec![app.focused_id.get()]
                            key=|id| id.clone()
                            children=move |_| {
                                view! {
                                    <Routes>
                                        <Route path="/" view=Login/>
                                        <Route path="/about" view=About/>
                                        <Route path="/settings" view=Settings/>
                                        <Route path="/people" view=People/>
                                        <Route path="/people/:seed" view=PersonPage/>
                                        <Route path="/transfers" view=Transfers/>
                                        <Route path="/you" view=You/>
                                        <Route path="/lobby" view=Lobby/>
                                        <Route path="/boards" view=Boards/>
                                        <Route path="/boards/:slug" view=BoardView/>
                                        <Route path="/dms" view=Dms/>
                                        <Route path="/directory" view=Directory/>
                                        <Route path="/files" view=Files/>
                                        <Route path="/radio" view=Radio/>
                                        <Route path="/servers" view=ServerBrowser/>
                                        <Route path="/art" view=ArtGallery/>
                                        <Route path="/wishing-well" view=WishingWell/>
                                        <Route path="/admin" view=Admin/>
                                        <Route path="/admin/:section" view=Admin/>
                                    </Routes>
                                }
                            }
                        />
                    </div>
                </div>
            </div>
        </Router>
    }
}

thread_local! {
    /// The running app, for the few callbacks that fire outside any component
    /// (a shell command resolving, a socket frame arriving) and still need to
    /// tell the person something. `AppState` is a bundle of signal handles owned
    /// by the root scope, which lives as long as the page does.
    static CURRENT: std::cell::Cell<Option<AppState>> = const { std::cell::Cell::new(None) };
}

/// Record the running app. Called once, by [`App`].
pub fn set_current(app: AppState) {
    CURRENT.with(|c| c.set(Some(app)));
}

/// The running app, if one is mounted.
pub fn current() -> Option<AppState> {
    CURRENT.with(|c| c.get())
}

/// What a confirmed question does. An intent the app carries out itself, not
/// a closure: the view that asked can be gone by the time the answer comes (a
/// route change, a burrow switch), and a callback owned by a disposed scope
/// panics when called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmIntent {
    /// Leave this burrow: close its socket, drop its session and its token.
    Leave(ServerId),
    /// Disable an account (by login). It is signed out at once.
    DisableAccount(String),
    /// Remove an account for good.
    RemoveAccount(String),
    /// Remove an account's two-factor enrolment.
    ResetTotp(String),
    /// Withdraw an unused invitation (by code).
    RevokeInvite(String),
    /// Remove an empty board (by slug).
    DeleteBoard(String),
    /// Take a post down (by id).
    DeletePost(String),
    /// Remove an empty file area (by slug).
    DeleteArea(String),
    /// Remove a file or folder: its id and its name.
    DeleteNode(i64, String),
    /// Disconnect a session (by id); the name is for the question.
    KickSession(u64, String),
    /// Withdraw a federation peer's approval (by key).
    RevokePeer([u8; 32]),
    /// Remove a snapshot (by name).
    DeleteBackup(String),
}

/// What the send dialog is sending: a file or folder of one burrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendAsk {
    pub node_id: i64,
    pub name: String,
    pub is_folder: bool,
    /// The burrow it is on.
    pub source: ServerId,
}

/// A question put to the person before something irreversible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmAsk {
    pub title: String,
    /// What happens, and what is kept: the question a person actually has.
    pub body: String,
    /// The confirming button's label: the verb, not "OK".
    pub action: String,
    pub intent: ConfirmIntent,
}

impl ConfirmAsk {
    /// "Disable alice?"
    pub fn disable_account(login: &str) -> Self {
        ConfirmAsk {
            title: format!("Disable {login}?"),
            body: "They are signed out now, everywhere, and cannot sign in until you enable \
                   the account again. Nothing of theirs is removed."
                .to_string(),
            action: "Disable".to_string(),
            intent: ConfirmIntent::DisableAccount(login.to_string()),
        }
    }

    /// "Remove alice for good?"
    pub fn remove_account(login: &str) -> Self {
        ConfirmAsk {
            title: format!("Remove {login} for good?"),
            body: "They cannot sign in again, and their personas, saved sign-ins, two-factor, \
                   buddies and any invitations nobody has used go with them. What they wrote \
                   stays, under the name they wrote it with \u{2014} so their names are kept out \
                   of use, and nobody can take the byline. This cannot be undone: disabling an \
                   account is the way back."
                .to_string(),
            action: "Remove".to_string(),
            intent: ConfirmIntent::RemoveAccount(login.to_string()),
        }
    }

    /// "Remove two-factor from alice?"
    pub fn reset_totp(login: &str) -> Self {
        ConfirmAsk {
            title: format!("Remove two-factor from {login}?"),
            body: "Only do this for someone you are sure of: until they set it up again, \
                   their password alone lets them in."
                .to_string(),
            action: "Remove two-factor".to_string(),
            intent: ConfirmIntent::ResetTotp(login.to_string()),
        }
    }

    /// "Disconnect dormouse?"
    pub fn kick(session_id: u64, who: &str, transport: &str) -> Self {
        ConfirmAsk {
            title: format!("Disconnect {who}?"),
            body: format!(
                "Their {transport} session is closed now. They can sign in again at once; to \
                 keep them out, disable the account under People."
            ),
            action: "Disconnect".to_string(),
            intent: ConfirmIntent::KickSession(session_id, who.to_string()),
        }
    }

    /// "Remove the Music area?"
    /// "Revoke grove.example?"
    pub fn revoke_peer(key: [u8; 32], title: &str) -> Self {
        ConfirmAsk {
            title: format!("Revoke {title}?"),
            body: "Its session with this burrow is closed now, and it waits for approval again \
                   before anything more is exchanged. Posts already received stay."
                .to_string(),
            action: "Revoke".to_string(),
            intent: ConfirmIntent::RevokePeer(key),
        }
    }

    /// "Remove snapshot-20260918-224103?"
    pub fn delete_backup(name: &str) -> Self {
        ConfirmAsk {
            title: format!("Remove {name}?"),
            body: "The snapshot is deleted from the backup folder. The burrow itself is not \
                   touched. This cannot be undone."
                .to_string(),
            action: "Remove".to_string(),
            intent: ConfirmIntent::DeleteBackup(name.to_string()),
        }
    }

    pub fn delete_area(slug: &str, title: &str) -> Self {
        ConfirmAsk {
            title: format!("Remove the {title} area?"),
            body: "Only an empty area can be removed: one with files or folders in it stays \
                   where it is."
                .to_string(),
            action: "Remove".to_string(),
            intent: ConfirmIntent::DeleteArea(slug.to_string()),
        }
    }

    /// "Remove mix.mp3?"
    pub fn delete_node(id: i64, name: &str, is_folder: bool) -> Self {
        ConfirmAsk {
            title: format!("Remove {name}?"),
            body: if is_folder {
                "The folder goes, with everything inside it. This cannot be undone.".to_string()
            } else {
                "The file is taken out of the library. This cannot be undone.".to_string()
            },
            action: "Remove".to_string(),
            intent: ConfirmIntent::DeleteNode(id, name.to_string()),
        }
    }

    /// "Remove this post?"
    pub fn delete_post(id: &str, author: &str) -> Self {
        ConfirmAsk {
            title: format!("Remove this post by {author}?"),
            body: "It keeps its place in the thread and says it was removed. This cannot \
                   be undone."
                .to_string(),
            action: "Remove".to_string(),
            intent: ConfirmIntent::DeletePost(id.to_string()),
        }
    }

    /// "Remove the board Tea Party?"
    pub fn delete_board(slug: &str, title: &str) -> Self {
        ConfirmAsk {
            title: format!("Remove {title}?"),
            body: "Only an empty board can be removed: one with posts in it, or boards \
                   inside it, stays where it is."
                .to_string(),
            action: "Remove".to_string(),
            intent: ConfirmIntent::DeleteBoard(slug.to_string()),
        }
    }

    /// "Withdraw this invitation?"
    pub fn revoke_invite(code: &str) -> Self {
        ConfirmAsk {
            title: "Withdraw this invitation?".to_string(),
            body: format!("Nobody will be able to register with {code}."),
            action: "Withdraw".to_string(),
            intent: ConfirmIntent::RevokeInvite(code.to_string()),
        }
    }

    /// "Leave {name}?" This used to be `window.confirm()`, which a desktop
    /// webview answers `false` to without showing anything, so in the app the
    /// Leave button did nothing at all.
    pub fn leave(id: ServerId, name: &str) -> Self {
        let name = if name.trim().is_empty() {
            "this burrow"
        } else {
            name.trim()
        };
        ConfirmAsk {
            title: format!("Leave {name}?"),
            body: "You will be signed out of it here. Your handle is remembered, and you \
                   can rejoin from your burrows on the connect window."
                .to_string(),
            action: "Leave".to_string(),
            intent: ConfirmIntent::Leave(id),
        }
    }
}

/// Are we running inside the desktop shell?
fn is_native() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::native::native_available()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// The section sidebar, mounted once by the shell.
///
/// The rail item you picked dictates whether a sidebar exists at all: a
/// *burrow* is a place with rooms — lobby, boards, files — so it gets the
/// list. People, Transfers, You and Servers are each one screen; a sidebar
/// beside them would be navigation for navigation's sake, so they get the
/// full width instead. Hidden on the connect screen (route `/`) too, same as
/// the rail.
#[component]
fn SideNav() -> impl IntoView {
    let location = leptos_router::use_location();
    let chromeless = move || crate::palette::is_chromeless(&location.pathname.get());
    let warren =
        move || crate::palette::scope_of(&location.pathname.get()) == crate::palette::Scope::Warren;
    // Hidden with CSS, not unmounted: a <Show> would tear the nav down on
    // every warren-scope route and remount it on return, replaying the pips'
    // arrival animation on plain navigation — the exact replay-on-remount
    // class of motion 0.179 removed.
    //
    // Warren scope is a *class*, and the stylesheet decides: on a desktop the
    // sidebar disappears there (People, Transfers, You and Servers are each
    // one screen); on a phone the same element is the bottom tab bar, the
    // only navigation there is, so it stays and lists the warren's sections.
    view! {
        <div class="rh-sidenav-slot" class:rh-hidden=chromeless class:warren-scope=warren>
            <Nav/>
        </div>
    }
}

/// The phone's bottom tab bar: what the burrow rail is on a desktop, in the
/// five slots a thumb can reach. Warren (opens the sheet: switch or add a
/// burrow, Settings, Leave), the burrow you're in (its tile; the Looking
/// Glass when you're in none), People, Transfers, You. The burrow's own
/// sections live in a strip under the header, not here: eight of them at a
/// legible size never fit a 390px bar, and they used to render at 9.92px.
/// Hidden wider than a phone by the stylesheet, and on the connect screen.
#[component]
fn PhoneTabBar() -> impl IntoView {
    use crate::palette::{is_chromeless, scope_of, Scope};
    let app = expect_context::<AppState>();
    let location = leptos_router::use_location();
    let navigate = leptos_router::use_navigate();
    let go = Callback::new(move |route: &'static str| navigate(route, Default::default()));
    let hidden = move || is_chromeless(&location.pathname.get());
    let at = move |route: &'static str| location.pathname.get().trim_end_matches('/') == route;
    let in_burrow = move || scope_of(&location.pathname.get()) == Scope::Burrow;
    // The burrow tab shows where you are: the focused burrow's tile and
    // name, or the way to one when nothing is joined.
    let burrow_name = move || {
        app.has_burrows().then(|| {
            app.focused_tracked()
                .name
                .get()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Burrow".into())
        })
    };
    // Lines that landed in burrows you're not looking at: the Warren tab is
    // where you'd go to switch, so it wears the count.
    let elsewhere = move || app.total_unread();
    view! {
        <nav class="rh-tabbar" class:rh-hidden=hidden aria-label="Warren">
            <button
                type="button"
                class="rh-tab"
                aria-haspopup="dialog"
                aria-expanded=move || app.switcher_open.get().to_string()
                on:click=move |_| app.switcher_open.update(|o| *o = !*o)
            >
                <span class="rh-tab-icon">
                    <span inner_html=crate::icons::rail_icon("home")></span>
                    {move || crate::state::unread_badge(elsewhere()).map(|b| view! {
                        <span class="rh-rail-badge" aria-label=format!("{} unread elsewhere", elsewhere())>{b}</span>
                    })}
                </span>
                <span class="rh-tab-label">"Warren"</span>
            </button>
            <button
                type="button"
                class="rh-tab rh-tab-burrow"
                class:active=in_burrow
                aria-current=move || in_burrow().then_some("page")
                on:click=move |_| {
                    if app.has_burrows() { go.call("/lobby") } else { go.call("/servers") }
                }
            >
                {move || match burrow_name() {
                    Some(name) => {
                        let glyph = name.chars().next().unwrap_or('?').to_uppercase().to_string();
                        view! {
                            <span class="rh-tab-icon"><span class="rh-tab-tile">{glyph}</span></span>
                            <span class="rh-tab-label">{name}</span>
                        }.into_view()
                    }
                    None => view! {
                        <span class="rh-tab-icon"><span inner_html=crate::icons::rail_icon("add")></span></span>
                        <span class="rh-tab-label">"Burrows"</span>
                    }.into_view(),
                }}
            </button>
            {[("/people", "People", "people"), ("/transfers", "Transfers", "transfers"), ("/you", "You", "you")]
                .into_iter()
                .map(|(route, label, icon)| view! {
                    <button
                        type="button"
                        class="rh-tab"
                        class:active=move || at(route)
                        aria-current=move || at(route).then_some("page")
                        on:click=move |_| go.call(route)
                    >
                        <span class="rh-tab-icon"><span inner_html=crate::icons::rail_icon(icon)></span></span>
                        <span class="rh-tab-label">{label}</span>
                    </button>
                })
                .collect_view()}
        </nav>
    }
}

/// The phone-width **warren sheet**: what the burrow rail is on a desktop,
/// as a bottom sheet, because a 3.4rem rail beside a 390px screen is a
/// quarter of it gone. Opened from the "Warren" tab the bottom bar grows at
/// that width. Lists your burrows (switch, or add one), the warren's own
/// screens, Settings, and Leave for the burrow you're in — the one header
/// control that doesn't fit a phone's title row.
#[component]
fn WarrenSheet() -> impl IntoView {
    let app = expect_context::<AppState>();
    let open = app.switcher_open;
    let navigate = leptos_router::use_navigate();
    let go = Callback::new(move |route: String| {
        open.set(false);
        navigate(&route, Default::default());
    });
    // Escape closes, like the palette. wasm only: the host has no window.
    #[cfg(target_arch = "wasm32")]
    {
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if ev.key() == "Escape" && open.get_untracked() {
                ev.prevent_default();
                open.set(false);
            }
        });
        on_cleanup(move || handle.remove());
    }
    let warren_rows: [(&str, &str, &str); 4] = [
        ("/people", "People", "people"),
        ("/transfers", "Transfers", "transfers"),
        ("/you", "You", "you"),
        ("/settings", "Settings", "settings"),
    ];
    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="rh-sheet-backdrop" on:click=move |_| open.set(false)>
                <div
                    class="rh-sheet"
                    role="dialog"
                    aria-modal="true"
                    aria-label="Your warren"
                    on:click=|ev| ev.stop_propagation()
                >
                    <h2 class="rh-sheet-title">"Burrows"</h2>
                    <ul class="rh-sheet-list">
                        <For
                            each=move || app.burrow_tiles()
                            key=|(id, name, focused, conn, unread)| {
                                (id.0.clone(), name.clone(), *focused, *conn, *unread)
                            }
                            children=move |(id, name, focused, conn, unread)| {
                                let glyph = name.chars().next().unwrap_or('?').to_uppercase().to_string();
                                let dot = if conn.is_live() {
                                    "rh-rail-dot on"
                                } else if conn.is_pending() {
                                    "rh-rail-dot pending"
                                } else {
                                    "rh-rail-dot off"
                                };
                                let badge = crate::state::unread_badge(unread);
                                let status = conn.label();
                                let click_id = id.clone();
                                view! {
                                    <li>
                                        <button
                                            class="rh-sheet-row"
                                            class:active=move || { focused }
                                            aria-current=move || focused.then_some("true")
                                            on:click=move |_| {
                                                app.focus(&click_id);
                                                go.call("/lobby".to_string());
                                            }
                                        >
                                            <span class="rh-sheet-tile">
                                                {glyph}
                                                <span class=dot aria-hidden="true"></span>
                                            </span>
                                            <span class="rh-sheet-name">{name}</span>
                                            <span class="rh-sheet-meta">{status}</span>
                                            {badge.map(|b| view! {
                                                <span class="rh-pip" aria-label=format!("{unread} unread")>{b}</span>
                                            })}
                                        </button>
                                    </li>
                                }
                            }
                        />
                        <li>
                            <button class="rh-sheet-row" on:click=move |_| go.call("/servers".to_string())>
                                <span class="rh-sheet-tile rh-sheet-add" inner_html=crate::icons::rail_icon("add")></span>
                                <span class="rh-sheet-name">"Add a burrow"</span>
                            </button>
                        </li>
                    </ul>
                    <h2 class="rh-sheet-title">"Warren"</h2>
                    <ul class="rh-sheet-list">
                        {warren_rows
                            .into_iter()
                            .map(|(route, label, icon)| {
                                let svg = if icon == "settings" {
                                    crate::icons::settings_icon()
                                } else {
                                    crate::icons::rail_icon(icon)
                                };
                                view! {
                                    <li>
                                        <button class="rh-sheet-row" on:click=move |_| go.call(route.to_string())>
                                            <span class="rh-sheet-tile rh-sheet-icon" inner_html=svg></span>
                                            <span class="rh-sheet-name">{label}</span>
                                        </button>
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                    <Show when=move || { app.can_leave() } fallback=|| ()>
                        <button
                            class="rh-btn ghost rh-sheet-leave"
                            on:click=move |_| {
                                open.set(false);
                                app.ask_leave();
                            }
                        >
                            {move || format!(
                                "Leave {}",
                                app.focused_tracked().name.get().unwrap_or_else(|| "this burrow".into())
                            )}
                        </button>
                    </Show>
                </div>
            </div>
        </Show>
    }
}

/// The persistent left **burrow rail** — the warren-layer switcher. Renders the
/// unified home mark, the connected burrow tiles (accent-tinted squircles), and
/// an "add a burrow" affordance into the Looking Glass. Hidden on the login /
/// connect screen (route `/`), which is a full-bleed form. This is the shell's
/// server-switcher; Wave B slice 2 makes focus reactive so switching a tile
/// swaps the place in the main pane.
#[component]
fn BurrowRail() -> impl IntoView {
    let app = expect_context::<AppState>();
    let location = leptos_router::use_location();
    let navigate = leptos_router::use_navigate();
    // Hidden on the login/connect screen (route `/`), a full-bleed form.
    let hidden = move || crate::palette::is_chromeless(&location.pathname.get());

    let go_people = {
        let navigate = navigate.clone();
        move |_| navigate("/people", Default::default())
    };
    let go_transfers = {
        let navigate = navigate.clone();
        move |_| navigate("/transfers", Default::default())
    };
    let go_you = {
        let navigate = navigate.clone();
        move |_| navigate("/you", Default::default())
    };
    let go_add = {
        let navigate = navigate.clone();
        move |_| navigate("/servers", Default::default())
    };
    let go_settings = {
        let navigate = navigate.clone();
        move |_| navigate("/settings", Default::default())
    };

    // Which rail destination is current. The warren tiles had no active state
    // at all: standing in Transfers, the only thing lit was the focused burrow
    // tile, so the rail never showed where you actually were.
    let at = move |route: &'static str| {
        let path = location.pathname.get();
        path.trim_end_matches('/') == route
    };
    // Which scope the rail is showing: in a burrow, the focused tile is where
    // you are; in the warren, one of the warren icons is.
    let in_burrow =
        move || crate::palette::scope_of(&location.pathname.get()) == crate::palette::Scope::Burrow;
    view! {
        <nav
            class="rh-rail"
            class:rh-rail-hidden=hidden
            // In warren scope the focused burrow is still *focused* — the header
            // and People still relate to it — but it isn't where you are. It
            // keeps its edge bar and gives up the lit background, so exactly one
            // tile reads as "you are here".
            class:warren=move || { !in_burrow() }
            aria-label="Burrows"
        >
            <button
                class="rh-rail-tile rh-rail-unified"
                class:active=move || at("/people")
                aria-current=move || at("/people").then_some("page")
                title="People"
                aria-label="People"
                on:click=go_people
            >
                <span class="rh-rail-glyph" inner_html=crate::icons::rail_icon("people")></span>
            </button>
            <button
                class="rh-rail-tile rh-rail-unified"
                class:active=move || at("/transfers")
                aria-current=move || at("/transfers").then_some("page")
                title="Transfers"
                aria-label="Transfers"
                on:click=go_transfers
            >
                <span class="rh-rail-glyph" inner_html=crate::icons::rail_icon("transfers")></span>
            </button>
            <button
                class="rh-rail-tile rh-rail-unified rh-rail-you"
                class:active=move || at("/you")
                aria-current=move || at("/you").then_some("page")
                title="You"
                aria-label="You"
                on:click=go_you
            >
                <span class="rh-rail-glyph" inner_html=crate::icons::rail_icon("you")></span>
            </button>
            <div class="rh-rail-sep"></div>
            <For
                each=move || app.burrow_tiles()
                key=|(id, name, focused, conn, unread)| {
                    (id.0.clone(), name.clone(), *focused, *conn, *unread)
                }
                children=move |(id, name, focused, conn, unread)| {
                    let glyph = name.chars().next().unwrap_or('?').to_uppercase().to_string();
                    let cls = if focused {
                        "rh-rail-tile rh-rail-server active"
                    } else {
                        "rh-rail-tile rh-rail-server"
                    };
                    // Connection health: lit when online, pending on a
                    // (re)connect, off otherwise.
                    let dot = if conn.is_live() {
                        "rh-rail-dot on"
                    } else if conn.is_pending() {
                        "rh-rail-dot pending"
                    } else {
                        "rh-rail-dot off"
                    };
                    // Lines that landed while the user was in another burrow.
                    let badge = crate::state::unread_badge(unread);
                    let status = if unread > 0 {
                        format!("{name} — {} — {unread} unread", conn.label())
                    } else {
                        format!("{name} — {}", conn.label())
                    };
                    let nav = navigate.clone();
                    let click_id = id.clone();
                    view! {
                        <button
                            class=cls
                            title=status.clone()
                            aria-label=status
                            aria-current=move || focused.then_some("true")
                            on:click=move |_| {
                                app.focus(&click_id);
                                nav("/lobby", Default::default());
                            }
                        >
                            {glyph}
                            <span class=dot aria-hidden="true"></span>
                            {badge.map(|b| view! {
                                <span class="rh-rail-badge" aria-hidden="true">{b}</span>
                            })}
                        </button>
                    }
                }
            />
            <button
                class="rh-rail-tile rh-rail-add"
                title="Add a burrow"
                aria-label="Add a burrow"
                on:click=go_add
            >
                <span class="rh-rail-glyph" inner_html=crate::icons::rail_icon("add")></span>
            </button>
            // Settings at the rail's foot, where a desktop app keeps its
            // preferences. It had no entry point on a desktop at all: only
            // typing into ⌘K reached it.
            <button
                class="rh-rail-tile rh-rail-unified rh-rail-settings"
                class:active=move || at("/settings")
                aria-current=move || at("/settings").then_some("page")
                title="Settings"
                aria-label="Settings"
                on:click=go_settings
            >
                <span class="rh-rail-glyph" inner_html=crate::icons::settings_icon()></span>
            </button>
        </nav>
    }
}

/// Burrow routes need a burrow. While the focused session is the app's
/// placeholder — nothing joined yet, or the last burrow just left — a
/// burrow-scoped URL (`/lobby`, `/boards`, `/files`…) would render the
/// placeholder's seeded mock as if it were somewhere you'd arrived, in
/// shipped builds too. Those go to the connect screen instead. Warren routes
/// (People, Transfers, You, Servers, Settings, About) stand on their own.
///
/// Renders nothing; it owns one effect and must live inside `<Router>`.
#[component]
fn PlaceGuard() -> impl IntoView {
    let app = expect_context::<AppState>();
    let location = use_location();
    let navigate = use_navigate();
    create_effect(move |_| {
        let path = location.pathname.get();
        let placeholder = app.focused_id.get().is_placeholder();
        if placeholder && crate::palette::needs_a_burrow(&path) {
            navigate(
                "/",
                NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
        }
    });
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
    {
        // Stamp the build onto the document so the outside world can tell
        // WHAT is actually rendering. The desktop shell's diag line reads
        // this; during the black-margin hunt the webview spent hours serving
        // a weeks-old cached bundle and nothing said so.
        if let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = root.set_attribute("data-rh-version", env!("CARGO_PKG_VERSION"));
        }
    }
    mount_to_body(App);
}

#[cfg(test)]
mod theme_session_tests {
    use super::*;

    fn overlay(name: &str) -> ServerOverlay {
        let mut bundle = ThemeBundle::new(name);
        bundle.accent_rgb = Some([40, 80, 120]);
        ServerOverlay::from_bundle(&bundle)
    }

    #[test]
    fn captured_theme_sink_updates_its_burrow_after_focus_changes() {
        let runtime = create_runtime();
        let app = AppState::new();
        let first = app.focused();
        let second = AppState::new().focused();
        let first_id = ServerId("wss://first.example/ws".into());
        let second_id = ServerId("wss://second.example/ws".into());
        app.sessions
            .set(vec![(first_id.clone(), first), (second_id.clone(), second)]);
        let first_sink = move |theme| first.set_server_theme(theme);
        let second_sink = move |theme| second.set_server_theme(theme);
        app.focus(&second_id);
        second_sink(Some(overlay("Second")));
        first_sink(Some(overlay("First")));
        assert_eq!(app.server_theme_name().as_deref(), Some("Second"));
        first_sink(None);
        assert_eq!(app.server_theme_name().as_deref(), Some("Second"));
        app.focus(&first_id);
        assert_eq!(app.server_theme_name(), None);
        first_sink(Some(overlay("First reconnected")));
        assert_eq!(
            app.server_theme_name().as_deref(),
            Some("First reconnected")
        );
        runtime.dispose();
    }

    #[test]
    fn mock_theme_loading_cannot_replace_a_live_verified_overlay() {
        let runtime = create_runtime();
        let app = AppState::new();
        let session = app.focused();
        session.live.set(true);
        session.set_server_theme(Some(overlay("Verified")));
        app.load_server_theme();
        assert_eq!(app.server_theme_name().as_deref(), Some("Verified"));
        runtime.dispose();
    }
}
