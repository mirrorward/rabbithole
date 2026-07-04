//! # rabbithole-ui-web
//!
//! The RabbitHole web SPA: a [Leptos](https://leptos.dev) client-side-rendered
//! (CSR) single-page app that compiles to `wasm32-unknown-unknown`. This is the
//! Wave 8 foundation — a shell that stands up the real component tree, theming,
//! and the command/event seam so later slices can drop in a live transport.
//!
//! ## Architecture
//!
//! Every frontend in the workspace drives the same loop from
//! [`rabbithole_core`]:
//!
//! ```text
//! Component ──Command──▶ UiClient ──Event──▶ UiState ──▶ reactive view
//! ```
//!
//! - [`client`] defines the [`UiClient`](client::UiClient) seam and a
//!   [`MockClient`](client::MockClient) in-memory implementation.
//! - [`wire`] holds the host-tested RHP mapping (Command ↔ Frame ↔ Event) and
//!   the async [`EventClient`](wire::EventClient) seam shared by the mock and
//!   the browser transport. It also carries the FILE-family and ADMIN-family
//!   local vocabularies and their mappings.
//! - [`ws`] (wasm only) is the browser [`WsClient`](ws::WsClient) WebSocket
//!   transport that speaks RHP over `ws://`/`wss://`, with jittered
//!   exponential-backoff reconnect and FILE-family dispatch.
//! - [`conn`] holds the DOM-free connection-lifecycle model
//!   ([`ConnState`](conn::ConnState)) and the pure reconnect
//!   [`backoff_delay`](conn::backoff_delay) schedule, unit tested on the host.
//! - [`state`] holds the DOM-free [`UiState`](state::UiState) reducer, unit
//!   tested on the host.
//! - [`files`] holds the DOM-free file-library reducer and transfer-queue model
//!   (the FILE-family `command`/`event` mapping lives in [`wire`]).
//! - [`admin`] holds the DOM-free web-admin reducer
//!   ([`AdminState`](admin::AdminState)); the ADMIN-family mapping lives in
//!   [`wire`].
//! - [`radio`] holds the DOM-free radio model: the now-playing reducer
//!   ([`RadioState`](radio::RadioState)), player preferences with validation +
//!   persistence resolve logic, and the pure stream-URL join. Inbound RADIO
//!   frames are decoded in [`wire`]
//!   ([`frame_to_notice_route`](wire::frame_to_notice_route)).
//! - [`servers`] is the DOM-free Looking Glass server-browser model: the
//!   [`DirectoryServer`](servers::DirectoryServer) row plus the total
//!   [`browse`](servers::browse) filter/rank.
//! - [`player`] (wasm only) wraps the `<audio>` element the radio streams
//!   through; every decision it acts on is made in [`radio`].
//! - [`art`] turns parsed CP437/ANSI art (via `rabbithole-art`) into pure,
//!   host-tested canvas draw ops; only the paint call is wasm-gated.
//! - [`packs`] defines the theme packs (Clean / Retro / High Contrast) as
//!   complete CSS-variable token sets that round-trip as JSON token files —
//!   the future server-theme-bundle seam.
//! - [`pwa`] is the installable-PWA slice: the wasm-gated service-worker
//!   registration edge plus the host-tested shape of the shell assets
//!   (`assets/sw.js`, `assets/manifest.webmanifest`, the maskable icons
//!   rendered by [`icon_rgba`](pwa::icon_rgba)) that `trunk build` copies
//!   into `dist/` via `index.html`'s `data-trunk` links.
//! - [`theme_css`] holds the app stylesheet, the pack+mode
//!   [`ThemeChoice`](theme_css::ThemeChoice) model, and resolves light/dark
//!   from `(choice, os_pref)` — plus the custom-pack override slot the theme
//!   editor applies through.
//! - [`theme_editor`] is the DOM-free admin theme-editor model: a working
//!   [`PackTokens`](packs::PackTokens) with a validated action reducer, JSON
//!   import/export, and a WCAG contrast checker that warns (never blocks).
//! - [`syndication_admin`] is the DOM-free Syndication & Gateways panel model
//!   ([`SynAdminState`](syndication_admin::SynAdminState)): gateway-matrix
//!   derivation, the poll-interval editor with validation, and a total
//!   `syndication_feeds` parser — all riding the existing ADMIN config
//!   vocabulary in [`wire`].
//! - [`a11y`] is the accessibility layer: the shared landmark/heading id
//!   vocabulary, label/input id pairing helpers, the wasm-gated
//!   route-change focus helpers, and the audit checklist (what is
//!   host-verified vs. what needs a browser).
//! - [`app`] and [`components`] are the Leptos view layer.
//!
//! ## wasm hygiene
//!
//! This crate depends on `rabbithole-core` **without** its `native` feature and
//! never pulls in tokio, `std::fs`, or `std::net`, so it stays wasm-clean.

#![forbid(unsafe_code)]

pub mod a11y;
pub mod admin;
pub mod app;
pub mod art;
pub mod client;
pub mod components;
pub mod conn;
pub mod files;
pub mod packs;
pub mod palette;
pub mod pwa;
pub mod radio;
pub mod server_theme;
pub mod servers;
pub mod state;
pub mod syndication_admin;
pub mod theme_css;
pub mod theme_editor;
pub mod toasts;
pub mod wire;

/// Browser WebSocket transport (`wasm32-unknown-unknown` only).
#[cfg(target_arch = "wasm32")]
pub mod ws;

/// Native (Tauri desktop) swarm-download bridge (`wasm32-unknown-unknown` only).
#[cfg(target_arch = "wasm32")]
pub mod native;

/// Browser `<audio>` playback for the radio (`wasm32-unknown-unknown` only).
#[cfg(target_arch = "wasm32")]
pub mod player;

pub use a11y::{config_input_id, token_input_id, MAIN_ID, SKIP_HREF, VIEW_TITLE_ID};
pub use admin::{AdminState, ConfigEntry};
pub use app::{mount, App, AppState};
pub use client::{MockClient, UiClient};
pub use conn::{backoff_delay, ConnState};
pub use files::{FilesState, Transfer, TransferDir, TransferStatus};
pub use packs::PackTokens;
pub use pwa::{icon_rgba, MANIFEST_URL, SW_URL};
pub use radio::{status_segment, stream_url, RadioPrefs, RadioState, RadioUpdate, StationStatus};
pub use server_theme::ServerOverlay;
pub use state::{
    Board, ChatLine, DmMessage, DmThread, Member, MemberProfile, Post, Presence, Thread, UiState,
};
pub use syndication_admin::{
    expected_applies_live, parse_feeds_value, validate_poll_secs, FeedRow, FeedsStatus, GatewayRow,
    SynAdminState,
};
pub use theme_editor::{contrast_warnings, ContrastWarning, EditorAction, EditorState};
pub use wire::{
    admin_command_to_frame, blob_get_request, blob_to_data_url, board_list_request,
    command_to_frame, directory_search_request, dm_history_request, dm_send, dm_threads_request,
    file_command_to_frame, frame_to_admin_events, frame_to_blob, frame_to_boards,
    frame_to_dm_history, frame_to_dm_received, frame_to_dm_threads, frame_to_events,
    frame_to_file_content, frame_to_file_events, frame_to_members, frame_to_notice_route,
    frame_to_posts, frame_to_presence, frame_to_profile, frame_to_threads, frame_to_who,
    hello_request, hex_to_id, id_to_hex, normalize_ws_url, ping_request, post_create, post_reply,
    profile_get_request, thread_list_request, thread_request, who_request, AdminCommand,
    AdminEvent, EventClient, EventSink, FileCommand, FileEvent, NoticeRoute, PresenceDelta,
};

#[cfg(target_arch = "wasm32")]
pub use player::RadioPlayer;
#[cfg(target_arch = "wasm32")]
pub use ws::WsClient;
