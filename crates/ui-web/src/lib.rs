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
//!   [`MockClient`](client::MockClient) in-memory implementation. The real
//!   browser WebSocket transport is a later slice; the seam keeps the UI
//!   buildable and testable now.
//! - [`state`] holds the DOM-free [`UiState`](state::UiState) reducer, unit
//!   tested on the host.
//! - [`theme_css`] maps [`rabbithole_core::theme`] design tokens to CSS custom
//!   properties for light/dark theming.
//! - [`app`] and [`components`] are the Leptos view layer.
//!
//! ## wasm hygiene
//!
//! This crate depends on `rabbithole-core` **without** its `native` feature and
//! never pulls in tokio, `std::fs`, or `std::net`, so it stays wasm-clean.

#![forbid(unsafe_code)]

pub mod app;
pub mod client;
pub mod components;
pub mod state;
pub mod theme_css;

pub use app::{mount, App, AppState};
pub use client::{MockClient, UiClient};
pub use state::{ChatLine, UiState};
