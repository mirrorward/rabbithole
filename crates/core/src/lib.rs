//! # rabbithole-core
//!
//! The client-side domain core. Every frontend — CLI, TUI, the Leptos wasm
//! SPA (browser or Tauri webview) — is a thin adapter over the same loop:
//!
//! ```text
//! Frontend ──Command──▶ Core (session, state, cache) ──Event──▶ Frontend
//! ```
//!
//! This crate must stay **wasm-compatible**: no tokio, no filesystem, no
//! sockets. Transport and storage are injected by the host application via
//! the traits in `rabbithole-net` / `rabbithole-store-client`.
//!
//! Wave 0 establishes the API shape; Wave 1 fills in the session state
//! machine (hello → auth → steady-state) and Wave 2+ grow the command and
//! event vocabularies with their features.

#![forbid(unsafe_code)]

pub mod api;

pub use api::{Command, Event};
