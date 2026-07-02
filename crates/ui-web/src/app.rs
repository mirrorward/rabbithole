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
use crate::components::{Lobby, Login};
use crate::state::UiState;
use crate::theme_css::{root_vars, STYLESHEET};

/// Reactive, `Copy` handle bundle shared through context.
#[derive(Clone, Copy)]
pub struct AppState {
    /// The flat UI model, folded from events.
    pub state: RwSignal<UiState>,
    /// The active appearance mode (light/dark).
    pub mode: RwSignal<Mode>,
    /// The command seam. `MockClient` today; a real transport later.
    pub client: StoredValue<MockClient>,
}

impl AppState {
    /// Create the shared state for a fresh session.
    pub fn new() -> Self {
        Self {
            state: create_rw_signal(UiState::default()),
            mode: create_rw_signal(Mode::Dark),
            client: store_value(MockClient::new()),
        }
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

    let style = move || root_vars(app.mode.get());

    view! {
        <style>{STYLESHEET}</style>
        <Router>
            <div class="rh-app" style=style>
                <Routes>
                    <Route path="/" view=Login/>
                    <Route path="/lobby" view=Lobby/>
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
