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
//!   the browser transport.
//! - [`ws`] (wasm only) is the browser [`WsClient`](ws::WsClient) WebSocket
//!   transport that speaks RHP over `ws://`/`wss://`.
//! - [`state`] holds the DOM-free [`UiState`](state::UiState) reducer, unit
//!   tested on the host.
//! - [`files`] holds the DOM-free file-library reducer and transfer-queue model
//!   (the FILE-family `command`/`event` mapping lives in [`wire`]).
//! - [`art`] turns parsed CP437/ANSI art (via `rabbithole-art`) into pure,
//!   host-tested canvas draw ops; only the paint call is wasm-gated.
//! - [`theme_css`] maps [`rabbithole_core::theme`] design tokens to CSS custom
//!   properties and resolves light/dark from `(choice, os_pref)`.
//! - [`app`] and [`components`] are the Leptos view layer.
//!
//! ## wasm hygiene
//!
//! This crate depends on `rabbithole-core` **without** its `native` feature and
//! never pulls in tokio, `std::fs`, or `std::net`, so it stays wasm-clean.

#![forbid(unsafe_code)]

pub mod app;
pub mod art;
pub mod client;
pub mod components;
pub mod files;
pub mod state;
pub mod theme_css;
pub mod wire;

/// Browser WebSocket transport (`wasm32-unknown-unknown` only).
#[cfg(target_arch = "wasm32")]
pub mod ws;

pub use app::{mount, App, AppState};
pub use client::{MockClient, UiClient};
pub use files::{FilesState, Transfer, TransferDir, TransferStatus};
pub use state::{Board, ChatLine, DmMessage, DmThread, Member, Post, Thread, UiState};
pub use wire::{
    command_to_frame, file_command_to_frame, frame_to_events, frame_to_file_events, hello_request,
    normalize_ws_url, ping_request, who_request, EventClient, EventSink, FileCommand, FileEvent,
};

#[cfg(target_arch = "wasm32")]
pub use ws::WsClient;
