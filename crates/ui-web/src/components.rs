//! Leptos function components: the view layer.
//!
//! These are intentionally thin — all non-trivial logic lives in
//! [`crate::state`] (the DOM-free reducer) and [`crate::client`] (the seam),
//! both host-testable. Components only wire reactive signals to markup and
//! forward user intent to [`AppState::dispatch`].

use leptos::*;
use leptos_router::{use_navigate, use_params_map, A};
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

/// The primary section links. Rendered inside the [`StatusBar`], they preserve
/// the shared session (context) while switching routes.
#[component]
pub fn Nav() -> impl IntoView {
    view! {
        <nav class="rh-nav">
            <A href="/lobby">"Lobby"</A>
            <A href="/boards">"Boards"</A>
            <A href="/dms">"DMs"</A>
            <A href="/directory">"Directory"</A>
        </nav>
    }
}

/// The header/status bar: server name, connection state, nav links, theme
/// toggle.
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
            <Nav/>
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

/// The board tree: every board links to its `/boards/:slug` reading view.
#[component]
pub fn Boards() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    app.load_boards();

    view! {
        <StatusBar/>
        <div class="rh-body">
            <section class="rh-panel">
                <h2 class="rh-panel-title">"Boards"</h2>
                <ul class="rh-tree">
                    <For
                        each=move || state.with(|s| s.boards.clone())
                        key=|b| b.slug.clone()
                        children=move |b| {
                            let href = format!("/boards/{}", b.slug);
                            view! {
                                <li class="rh-tree-item">
                                    <A href=href class="rh-board-link">
                                        <span class="rh-board-name">{b.name}</span>
                                        <span class="rh-board-desc">{b.description}</span>
                                    </A>
                                </li>
                            }
                        }
                    />
                </ul>
            </section>
        </div>
    }
}

/// A single board: its thread list plus an inline thread/post reading view.
#[component]
pub fn BoardView() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    let params = use_params_map();

    // Re-select the board whenever the `:slug` route param changes.
    create_effect(move |_| {
        if let Some(slug) = params.with(|p| p.get("slug").cloned()) {
            app.select_board(&slug);
        }
    });

    let board_name = move || {
        state.with(|s| {
            let slug = s.selected_board.clone().unwrap_or_default();
            s.boards
                .iter()
                .find(|b| b.slug == slug)
                .map(|b| b.name.clone())
                .unwrap_or(slug)
        })
    };

    view! {
        <StatusBar/>
        <div class="rh-body">
            <section class="rh-panel rh-threads">
                <A href="/boards" class="rh-back">"\u{2190} All boards"</A>
                <h2 class="rh-panel-title">{board_name}</h2>
                <ul class="rh-tree">
                    <For
                        each=move || state.with(|s| s.threads.clone())
                        key=|t| t.id
                        children=move |t| {
                            let id = t.id;
                            let selected = move || {
                                state.with(|s| s.selected_thread == Some(id))
                            };
                            let class = move || {
                                if selected() {
                                    "rh-thread-link active"
                                } else {
                                    "rh-thread-link"
                                }
                            };
                            view! {
                                <li class="rh-tree-item">
                                    <button
                                        class=class
                                        on:click=move |_| app.open_thread(id)
                                    >
                                        <span class="rh-thread-title">{t.title}</span>
                                        <span class="rh-thread-author">"by "{t.author}</span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </section>
            <section class="rh-panel rh-reader">
                <Show
                    when=move || state.with(|s| s.selected_thread.is_some())
                    fallback=|| view! {
                        <p class="rh-empty">"Select a thread to read."</p>
                    }
                >
                    <div class="rh-posts">
                        <For
                            each=move || state.with(|s| s.posts.clone())
                            key=|p| p.id
                            children=move |p| view! {
                                <article class="rh-post">
                                    <span class="rh-from">{p.author}</span>
                                    <p class="rh-post-body">{p.body}</p>
                                </article>
                            }
                        />
                    </div>
                </Show>
            </section>
        </div>
    }
}

/// Direct messages: a conversation list plus the selected thread and a compose
/// box. Sending appends locally via [`AppState::send_dm`].
#[component]
pub fn Dms() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    let draft = create_rw_signal(String::new());
    app.load_dms();

    let send = move || {
        let text = draft.get();
        if text.trim().is_empty() {
            return;
        }
        app.send_dm(&text);
        draft.set(String::new());
    };

    view! {
        <StatusBar/>
        <div class="rh-body">
            <aside class="rh-who">
                <h2>"Conversations"</h2>
                <ul>
                    <For
                        each=move || state.with(|s| s.dm_threads.clone())
                        key=|t| t.id.clone()
                        children=move |t| {
                            let id = t.id.clone();
                            let selected = {
                                let id = id.clone();
                                move || state.with(|s| s.selected_dm.as_deref() == Some(id.as_str()))
                            };
                            let class = move || {
                                if selected() {
                                    "rh-dm-peer active"
                                } else {
                                    "rh-dm-peer"
                                }
                            };
                            view! {
                                <li>
                                    <button
                                        class=class
                                        on:click=move |_| state.update(|s| s.select_dm(&id))
                                    >
                                        {t.peer}
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </aside>
            <section class="rh-chat">
                <Show
                    when=move || state.with(|s| s.selected_dm.is_some())
                    fallback=|| view! {
                        <p class="rh-empty">"Select a conversation."</p>
                    }
                >
                    <div class="rh-scroll">
                        <For
                            each=move || {
                                state.with(|s| {
                                    s.active_dm()
                                        .map(|t| t.messages.clone())
                                        .unwrap_or_default()
                                        .into_iter()
                                        .enumerate()
                                        .collect::<Vec<_>>()
                                })
                            }
                            key=|(i, _)| *i
                            children=move |(_, m)| view! {
                                <div class="rh-line">
                                    <span class="rh-from">{m.from}</span>
                                    {m.text}
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
                            placeholder="Write a message\u{2026}"
                            prop:value=draft
                            on:input=move |ev| draft.set(event_target_value(&ev))
                        />
                        <button class="rh-btn" type="submit">"Send"</button>
                    </form>
                </Show>
            </section>
        </div>
    }
}

/// Member directory: a searchable list plus a profile card for the selected
/// member.
#[component]
pub fn Directory() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.state;
    app.load_members();

    view! {
        <StatusBar/>
        <div class="rh-body">
            <section class="rh-panel rh-members">
                <h2 class="rh-panel-title">"Members"</h2>
                <input
                    class="rh-input"
                    placeholder="Search members\u{2026}"
                    prop:value=move || state.with(|s| s.directory_query.clone())
                    on:input=move |ev| {
                        let q = event_target_value(&ev);
                        state.update(|s| s.set_directory_query(q));
                    }
                />
                <ul class="rh-tree">
                    <For
                        each=move || state.with(|s| s.matching_members())
                        key=|m| m.handle.clone()
                        children=move |m| {
                            let handle = m.handle.clone();
                            let dot = if m.online { "rh-dot on" } else { "rh-dot off" };
                            view! {
                                <li class="rh-tree-item">
                                    <button
                                        class="rh-member-link"
                                        on:click=move |_| {
                                            state.update(|s| s.select_member(&handle))
                                        }
                                    >
                                        <span class=dot></span>
                                        <span class="rh-member-name">{m.display_name}</span>
                                        <span class="rh-member-handle">"@"{m.handle}</span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </section>
            <section class="rh-panel rh-profile">
                <Show
                    when=move || state.with(|s| s.active_member().is_some())
                    fallback=|| view! {
                        <p class="rh-empty">"Select a member to view their profile."</p>
                    }
                >
                    {move || state.with(|s| {
                        s.active_member().map(|m| {
                            let status = if m.online { "Online" } else { "Offline" };
                            view! {
                                <div class="rh-card">
                                    <h3 class="rh-card-name">{m.display_name.clone()}</h3>
                                    <p class="rh-card-handle">"@"{m.handle.clone()}</p>
                                    <p class="rh-card-status">{status}</p>
                                    <p class="rh-card-bio">{m.bio.clone()}</p>
                                </div>
                            }
                        })
                    })}
                </Show>
            </section>
        </div>
    }
}
