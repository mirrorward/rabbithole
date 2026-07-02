//! Leptos function components: the view layer.
//!
//! These are intentionally thin — all non-trivial logic lives in
//! [`crate::state`] (the DOM-free reducer) and [`crate::client`] (the seam),
//! both host-testable. Components only wire reactive signals to markup and
//! forward user intent to [`AppState::dispatch`].

use leptos::*;
use leptos_router::use_navigate;
use rabbithole_core::api::Command;

use crate::app::AppState;
use crate::client::LOBBY;
use crate::theme_css::toggle;
use rabbithole_core::theme::Mode;

/// Light/dark switch. Persists the choice in the shared `mode` signal and
/// re-themes the whole app via the root CSS variables.
#[component]
pub fn ThemeToggle() -> impl IntoView {
    let app = expect_context::<AppState>();
    let label = move || match app.mode.get() {
        Mode::Dark => "\u{2600} Light",
        Mode::Light => "\u{263D} Dark",
    };
    view! {
        <button
            class="rh-btn ghost"
            on:click=move |_| app.mode.update(|m| *m = toggle(*m))
        >
            {label}
        </button>
    }
}

/// The header/status bar: server name, connection state, theme toggle.
#[component]
pub fn StatusBar() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    let title = move || {
        let name = state.with(|s| s.server_name.clone());
        if name.is_empty() {
            "RabbitHole".to_string()
        } else {
            name
        }
    };
    let status = move || state.with(|s| s.status.clone());
    let dot_class = move || {
        if state.with(|s| s.connected) {
            "rh-dot on"
        } else {
            "rh-dot off"
        }
    };
    view! {
        <header class="rh-header">
            <span class=dot_class></span>
            <span class="rh-title">{title}</span>
            <span class="rh-status">{status}</span>
            <span class="rh-spacer"></span>
            <ThemeToggle/>
        </header>
    }
}

/// Connect screen: server URL + handle + connect button.
#[component]
pub fn Login() -> impl IntoView {
    let app = expect_context::<AppState>();
    let navigate = use_navigate();
    let endpoint = create_rw_signal("ws://localhost:9000".to_string());
    let handle = create_rw_signal(String::new());

    let connect = move |_| {
        let who = handle.get();
        if who.trim().is_empty() {
            return;
        }
        app.dispatch(Command::Connect {
            endpoint: endpoint.get(),
            pinned_fingerprint: None,
        });
        app.dispatch(Command::SignIn {
            login: who,
            password: String::new(),
        });
        app.refresh_who();
        navigate("/lobby", Default::default());
    };

    view! {
        <div class="rh-login">
            <h1>"RabbitHole"</h1>
            <label>"Server"</label>
            <input
                class="rh-input"
                prop:value=endpoint
                on:input=move |ev| endpoint.set(event_target_value(&ev))
            />
            <label>"Handle"</label>
            <input
                class="rh-input"
                placeholder="your handle"
                prop:value=handle
                on:input=move |ev| handle.set(event_target_value(&ev))
            />
            <button class="rh-btn" on:click=connect>"Connect"</button>
        </div>
    }
}

/// Sidebar listing the handles present in the room.
#[component]
pub fn WhoList() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    view! {
        <aside class="rh-who">
            <h2>"Present"</h2>
            <ul>
                <For
                    each=move || state.with(|s| s.who.clone())
                    key=|handle| handle.clone()
                    children=move |handle| view! { <li>{handle}</li> }
                />
            </ul>
        </aside>
    }
}

/// The main view: header, chat scrollback, compose box, and who-list.
#[component]
pub fn Lobby() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    let draft = create_rw_signal(String::new());

    let send = move || {
        let text = draft.get();
        if text.trim().is_empty() {
            return;
        }
        app.dispatch(Command::SendChat {
            room: LOBBY.to_string(),
            text,
        });
        draft.set(String::new());
    };

    view! {
        <StatusBar/>
        <div class="rh-body">
            <section class="rh-chat">
                <div class="rh-scroll">
                    <For
                        each=move || {
                            state.with(|s| s.messages.clone().into_iter().enumerate().collect::<Vec<_>>())
                        }
                        key=|(i, _)| *i
                        children=move |(_, line)| view! {
                            <div class="rh-line">
                                <span class="rh-from">{line.from}</span>
                                {line.text}
                            </div>
                        }
                    />
                </div>
                <form
                    class="rh-compose"
                    on:submit=move |ev| {
                        ev.prevent_default();
                        send();
                    }
                >
                    <input
                        class="rh-input"
                        placeholder="Message the lobby\u{2026}"
                        prop:value=draft
                        on:input=move |ev| draft.set(event_target_value(&ev))
                    />
                    <button class="rh-btn" type="submit">"Send"</button>
                </form>
            </section>
            <WhoList/>
        </div>
    }
}
