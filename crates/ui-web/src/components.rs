//! Leptos function components: the view layer.
//!
//! These are intentionally thin — all non-trivial logic lives in
//! [`crate::state`] (the DOM-free reducer) and [`crate::client`] (the seam),
//! both host-testable. Components only wire reactive signals to markup and
//! forward user intent to [`AppState::dispatch`].
//!
//! ## Accessibility contract
//!
//! Every routed view renders one `<main id="rh-main">` (the skip-link
//! target) containing exactly one `<h1 id="rh-view-title" tabindex="-1">`
//! (the route-change focus target — visible where the design has a title,
//! `.rh-visually-hidden` otherwise), with headings descending without skips
//! beneath it. The full checklist — landmarks, labels, live regions, tables,
//! keyboard, focus — lives in [`crate::a11y`].

use leptos::*;
use leptos_router::{use_navigate, use_params_map, A};
use rabbithole_core::api::Command;
use rabbithole_core::theme::{Mode, ThemePack};

use crate::a11y;
use crate::app::AppState;
use crate::files::{human_size, node_kind_label, TransferStatus, KIND_FOLDER};
use crate::syndication_admin::FeedsStatus;
use crate::theme_css::{mode_label, pack_label};
use crate::theme_editor::{contrast_warnings, EditorAction, EditorState};

/// Appearance picker: a pack button cycling Clean → Retro → High Contrast and
/// a mode button cycling System → Light → Dark. Together they cover the full
/// pack × mode grid; the combined choice is persisted to `localStorage` and
/// re-themes the whole app via the root CSS variables.
#[component]
pub fn ThemeToggle() -> impl IntoView {
    let app = expect_context::<AppState>();
    let pack = move || pack_label(app.theme.get().pack);
    let mode = move || mode_label(app.theme.get().mode);
    // The server-theming opt-out only appears when the connected burrow ships
    // a theme (PLAN §9.11: server theming is on by default, user can disable).
    let server_name = move || {
        app.server_theme
            .with(|s| s.as_ref().map(|o| o.name.clone()))
    };
    let disabled = move || app.server_theme_disabled.get();
    let server_title = move || match (server_name(), disabled()) {
        (Some(name), false) => {
            format!("Server theme \u{201c}{name}\u{201d} on \u{2014} click to use your own")
        }
        (Some(name), true) => {
            format!("Server theme \u{201c}{name}\u{201d} off \u{2014} click to apply it")
        }
        (None, _) => String::new(),
    };
    // Solid when the server theme is applied (its accent tints the button
    // itself), ghost when the user has switched it off.
    let server_class = move || {
        if disabled() {
            "rh-btn ghost small"
        } else {
            "rh-btn small"
        }
    };
    view! {
        <span class="rh-theme-menu">
            <Show when=move || server_name().is_some() fallback=|| ()>
                <button
                    class=server_class
                    aria-pressed=move || (!disabled()).to_string()
                    title=server_title
                    on:click=move |_| {
                        app.set_server_theme_disabled(!app.server_theme_disabled.get_untracked())
                    }
                >
                    "\u{25C6} Server"
                </button>
            </Show>
            <button
                class="rh-btn ghost"
                title="Cycle theme pack: Clean / Retro / High Contrast"
                on:click=move |_| app.cycle_pack()
            >
                {pack}
            </button>
            <button
                class="rh-btn ghost"
                title="Cycle appearance: Auto / Light / Dark"
                on:click=move |_| app.cycle_theme()
            >
                {mode}
            </button>
        </span>
    }
}

/// The primary section links. Rendered inside the [`StatusBar`], they preserve
/// the shared session (context) while switching routes. The router's
/// [`A`] stamps `aria-current="page"` on the active link, and the stylesheet
/// keys the active style off that attribute.
#[component]
pub fn Nav() -> impl IntoView {
    let app = expect_context::<AppState>();
    let is_admin = app.is_admin;
    view! {
        <nav class="rh-nav" aria-label="Primary">
            <A href="/lobby">"Lobby"</A>
            <A href="/boards">"Boards"</A>
            <A href="/dms">"DMs"</A>
            <A href="/directory">"Directory"</A>
            <A href="/files">"Files"</A>
            <A href="/radio">"Radio"</A>
            <A href="/servers">"Servers"</A>
            <A href="/art">"Art"</A>
            <Show when=move || is_admin.get() fallback=|| ()>
                <A href="/admin">"Admin"</A>
            </Show>
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
    let conn_label = move || state.with(|s| s.conn.label());
    let dot_class = move || {
        if state.with(|s| s.conn.is_live()) {
            "rh-dot on"
        } else if state.with(|s| s.conn.is_pending()) {
            "rh-dot pending"
        } else {
            "rh-dot off"
        }
    };
    let radio = app.radio;
    let now_playing = move || radio.with(crate::radio::status_segment).unwrap_or_default();
    // The connection label and status line are polite live regions so
    // transient states ("Connecting…", "Reconnecting…", command errors)
    // announce without stealing focus; the now-playing segment sits in an
    // always-mounted `role="status"` slot (collapsed via CSS when empty) so
    // track changes announce too. The dot is decorative — the label beside
    // it carries the state as text.
    view! {
        <header class="rh-header">
            <span class=dot_class aria-hidden="true"></span>
            <span class="rh-conn" role="status">{conn_label}</span>
            <span class="rh-title">{title}</span>
            <span class="rh-status" role="status">{status}</span>
            <span class="rh-spacer"></span>
            <span class="rh-live-slot" role="status">
                <Show when=move || radio.with(|r| r.on_air().is_some()) fallback=|| ()>
                    <A href="/radio" class="rh-radio-now">{now_playing}</A>
                </Show>
            </span>
            <Nav/>
            <button
                type="button"
                class="rh-kbd-jump"
                on:click=move |_| app.palette_open.set(true)
                aria-haspopup="dialog"
                aria-label="Jump to a section (Command-K)"
                title="Jump to a section (\u{2318}K)"
            >
                <span aria-hidden="true">"\u{2318}K"</span>
            </button>
            <ThemeToggle/>
        </header>
    }
}

/// The ⌘K command palette: a modal overlay to jump between sections by
/// keyboard. This is the SPA's first dialog, so it carries the contract the
/// [`crate::a11y`] notes reserved for the first overlay: `role="dialog"` +
/// `aria-modal`, Escape to close, the input autofocused on open, arrow-key
/// selection, and click-outside to dismiss. Matching is the host-tested
/// [`crate::palette`]; this only wires it to the DOM and the router.
#[component]
pub fn CommandPalette() -> impl IntoView {
    let app = expect_context::<AppState>();
    let open = app.palette_open;
    let navigate = use_navigate();
    let query = create_rw_signal(String::new());
    let selected = create_rw_signal(0usize);
    let input_ref = create_node_ref::<leptos::html::Input>();

    let matches = move || crate::palette::palette_matches(&query.get());
    // Hoisted out of `view!`: the `::<Vec<_>>` turbofish confuses the macro's
    // tag parser (the `<` reads as an open tag).
    let items = move || {
        matches()
            .into_iter()
            .enumerate()
            .collect::<Vec<(usize, crate::palette::Section)>>()
    };

    // Reset the query + focus the input each time the palette opens.
    create_effect(move |_| {
        if open.get() {
            query.set(String::new());
            selected.set(0);
            #[cfg(target_arch = "wasm32")]
            if let Some(el) = input_ref.get() {
                let _ = el.focus();
            }
        }
    });

    // Global ⌘K / Ctrl-K toggles the palette from anywhere (wasm only).
    #[cfg(target_arch = "wasm32")]
    {
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if (ev.meta_key() || ev.ctrl_key()) && ev.key().eq_ignore_ascii_case("k") {
                ev.prevent_default();
                open.update(|o| *o = !*o);
            }
        });
        on_cleanup(move || handle.remove());
    }

    // Navigation as a `Callback` (which is `Copy`), so every handler inside the
    // re-rendered `<Show>` can use it without move/`Fn` friction.
    let go = Callback::new(move |route: &'static str| {
        open.set(false);
        navigate(route, Default::default());
    });

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div class="rh-palette-backdrop" on:click=move |_| open.set(false)>
                <div
                    class="rh-palette"
                    role="dialog"
                    aria-modal="true"
                    aria-label="Jump to a section"
                    on:click=|ev| ev.stop_propagation()
                >
                    <input
                        node_ref=input_ref
                        class="rh-input rh-palette-input"
                        type="text"
                        placeholder="Jump to a section…"
                        aria-label="Jump to a section"
                        prop:value=query
                        on:input=move |ev| {
                            query.set(event_target_value(&ev));
                            selected.set(0);
                        }
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                            match ev.key().as_str() {
                                "ArrowDown" => {
                                    ev.prevent_default();
                                    let n = matches().len();
                                    if n > 0 {
                                        selected.update(|s| *s = (*s + 1) % n);
                                    }
                                }
                                "ArrowUp" => {
                                    ev.prevent_default();
                                    let n = matches().len();
                                    if n > 0 {
                                        selected.update(|s| *s = (*s + n - 1) % n);
                                    }
                                }
                                "Enter" => {
                                    ev.prevent_default();
                                    if let Some(sec) = matches().get(selected.get()).copied() {
                                        go.call(sec.route);
                                    }
                                }
                                "Escape" => {
                                    ev.prevent_default();
                                    open.set(false);
                                }
                                _ => {}
                            }
                        }
                    />
                    <ul class="rh-palette-list" role="listbox" aria-label="Sections">
                        <For
                            each=items
                            key=|(_, s)| s.route
                            children=move |(i, s)| {
                                view! {
                                    <li
                                        class="rh-palette-item"
                                        class:selected=move || selected.get() == i
                                        role="option"
                                        aria-selected=move || {
                                            if selected.get() == i { "true" } else { "false" }
                                        }
                                        on:click=move |_| go.call(s.route)
                                        on:mouseenter=move |_| selected.set(i)
                                    >
                                        <span class="rh-palette-label">{s.label}</span>
                                        <span class="rh-palette-hint">{s.hint}</span>
                                    </li>
                                }
                            }
                        />
                    </ul>
                </div>
            </div>
        </Show>
    }
}

/// The toast notification region: renders [`AppState`]'s toast queue into an
/// `aria-live="polite"` stack, each toast dismissible and (in the browser)
/// auto-expiring after a few seconds.
#[component]
pub fn Toasts() -> impl IntoView {
    let app = expect_context::<AppState>();
    let toasts = app.toasts;
    view! {
        <div class="rh-toasts" aria-live="polite" aria-label="Notifications">
            <For
                each=move || toasts.with(|q| q.items().to_vec())
                key=|t| t.id
                children=move |t| {
                    let id = t.id;
                    let cls = format!("rh-toast {}", t.kind.class());
                    // Auto-dismiss after a few seconds (browser only).
                    #[cfg(target_arch = "wasm32")]
                    leptos::set_timeout(
                        move || app.dismiss_toast(id),
                        std::time::Duration::from_secs(5),
                    );
                    view! {
                        <div class=cls role="status">
                            <span class="rh-toast-glyph" aria-hidden="true">
                                {t.kind.glyph()}
                            </span>
                            <span class="rh-toast-text">{t.text.clone()}</span>
                            <button
                                class="rh-toast-close"
                                aria-label="Dismiss notification"
                                on:click=move |_| app.dismiss_toast(id)
                            >
                                "\u{00d7}"
                            </button>
                        </div>
                    }
                }
            />
        </div>
    }
}

/// Connect screen: server URL + handle + connect button. A real `<form>`
/// (Enter submits from either field) with `<label for=…>` on both inputs.
#[component]
pub fn Login() -> impl IntoView {
    let app = expect_context::<AppState>();
    let navigate = use_navigate();
    // Prefill the endpoint if the server browser picked one, then clear it.
    let endpoint = create_rw_signal(
        app.pending_endpoint
            .get_untracked()
            .unwrap_or_else(|| "ws://localhost:9000".to_string()),
    );
    app.pending_endpoint.set(None);
    let handle = create_rw_signal(String::new());
    let password = create_rw_signal(String::new());
    // Opt in to a real RHP-over-WebSocket session instead of the seeded demo.
    let go_live = create_rw_signal(false);

    let connect = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if go_live.get() {
            // Live requires a handle to authenticate — without one the session
            // connects but never signs in (a silent dead session).
            let who = handle.get();
            if who.trim().is_empty() {
                return;
            }
            // Live: open a real socket + authenticate; state fills from
            // transport events (the handshake sets the header to Online, and
            // the lobby fills with live chat once signed in).
            app.connect_live(endpoint.get(), who, password.get());
            navigate("/lobby", Default::default());
            return;
        }
        let who = handle.get();
        if who.trim().is_empty() {
            return;
        }
        let name = who.clone();
        app.dispatch(Command::Connect {
            endpoint: endpoint.get(),
            pinned_fingerprint: None,
        });
        // The seeded host handle carries the admin capability; everyone else
        // signs in as a regular member. A real transport would derive this from
        // the session's capability set in the `HelloAck`.
        let is_admin = who == "rabbit";
        app.dispatch(Command::SignIn {
            login: who,
            password: String::new(),
        });
        app.set_admin(is_admin);
        app.refresh_who();
        app.load_dms();
        // Seeded now-playing notices, so the status segment shows on arrival.
        app.load_radio();
        // The server's published theme bundle (welcome frame in the real
        // transport), so the overlay + opt-out are live on arrival.
        app.load_server_theme();
        // Humanized arrival: a welcome toast, plus a "you've got mail" moment
        // when the inbox has conversations waiting.
        app.notify(
            crate::toasts::ToastKind::Success,
            format!("Signed in as {name}"),
        );
        let waiting = app.state.with(|s| s.dm_threads.len());
        if waiting > 0 {
            app.notify(
                crate::toasts::ToastKind::Mail,
                format!(
                    "You\u{2019}ve got mail \u{2014} {waiting} conversation{} waiting",
                    if waiting == 1 { "" } else { "s" }
                ),
            );
        }
        navigate("/lobby", Default::default());
    };

    view! {
        <main id=a11y::MAIN_ID tabindex="-1">
            <form class="rh-login" on:submit=connect>
                <h1 id=a11y::VIEW_TITLE_ID tabindex="-1">"RabbitHole"</h1>
                <label for=a11y::LOGIN_SERVER_ID>"Server"</label>
                <input
                    id=a11y::LOGIN_SERVER_ID
                    class="rh-input"
                    prop:value=endpoint
                    on:input=move |ev| endpoint.set(event_target_value(&ev))
                />
                <label for=a11y::LOGIN_HANDLE_ID>"Handle"</label>
                <input
                    id=a11y::LOGIN_HANDLE_ID
                    class="rh-input"
                    placeholder="your handle"
                    prop:value=handle
                    on:input=move |ev| handle.set(event_target_value(&ev))
                />
                <label class="rh-live-toggle">
                    <input
                        type="checkbox"
                        prop:checked=go_live
                        on:change=move |ev| go_live.set(event_target_checked(&ev))
                    />
                    "Live connection (connect to a real server)"
                </label>
                <Show when=move || go_live.get() fallback=|| ()>
                    <label for="rh-login-password">"Password"</label>
                    <input
                        id="rh-login-password"
                        class="rh-input"
                        type="password"
                        placeholder="password"
                        prop:value=password
                        on:input=move |ev| password.set(event_target_value(&ev))
                    />
                </Show>
                <button class="rh-btn" type="submit">"Connect"</button>
            </form>
        </main>
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
        // Routes over the live socket when connected, else the mock seam.
        app.send_chat(text);
        draft.set(String::new());
    };

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Lobby"</h1>
            <section class="rh-chat" aria-label="Lobby chat">
                // role=log: an implicitly polite live region — new messages
                // are announced without moving focus off the compose box.
                <div class="rh-scroll" role="log" aria-label="Chat messages">
                    <ul class="rh-lines">
                        <For
                            each=move || {
                                state.with(|s| s.messages.clone().into_iter().enumerate().collect::<Vec<_>>())
                            }
                            key=|(i, _)| *i
                            children=move |(_, line)| view! {
                                <li class="rh-line">
                                    <span class="rh-from">{line.from}</span>
                                    {line.text}
                                </li>
                            }
                        />
                    </ul>
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
                        aria-label="Message the lobby"
                        placeholder="Message the lobby\u{2026}"
                        prop:value=draft
                        on:input=move |ev| draft.set(event_target_value(&ev))
                    />
                    <button class="rh-btn" type="submit">"Send"</button>
                </form>
            </section>
            <WhoList/>
        </main>
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
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-panel">
                <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">"Boards"</h1>
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
        </main>
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

    let new_subject = create_rw_signal(String::new());
    let new_body = create_rw_signal(String::new());
    let post = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let slug = state.with(|s| s.selected_board.clone()).unwrap_or_default();
        app.post_thread(&slug, &new_subject.get(), &new_body.get());
        new_subject.set(String::new());
        new_body.set(String::new());
    };

    let reply_body = create_rw_signal(String::new());
    let reply = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if reply_body.with(|b| b.trim().is_empty()) {
            return;
        }
        app.post_reply(&reply_body.get());
        reply_body.set(String::new());
    };

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
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-panel rh-threads" aria-label="Threads">
                <A href="/boards" class="rh-back">"\u{2190} All boards"</A>
                <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">{board_name}</h1>
                <ul class="rh-tree">
                    <For
                        each=move || state.with(|s| s.threads.clone())
                        key=|t| t.id.clone()
                        children=move |t| {
                            let id = t.id.clone();
                            let sel_id = id.clone();
                            // A Memo (Copy) so both `class` and `aria-current`
                            // can read the selection.
                            let selected = create_memo(move |_| {
                                state.with(|s| s.selected_thread.as_deref() == Some(sel_id.as_str()))
                            });
                            let class = move || {
                                if selected.get() {
                                    "rh-thread-link active"
                                } else {
                                    "rh-thread-link"
                                }
                            };
                            view! {
                                <li class="rh-tree-item">
                                    <button
                                        class=class
                                        aria-current=move || selected.get().then_some("true")
                                        on:click=move |_| app.open_thread(id.clone())
                                    >
                                        <span class="rh-thread-title">{t.title}</span>
                                        <span class="rh-thread-author">"by "{t.author}</span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
                <form class="rh-newthread" on:submit=post>
                    <input
                        class="rh-input"
                        placeholder="New thread subject\u{2026}"
                        aria-label="New thread subject"
                        prop:value=new_subject
                        on:input=move |ev| new_subject.set(event_target_value(&ev))
                    />
                    <textarea
                        class="rh-input"
                        placeholder="Write the first post\u{2026}"
                        aria-label="First post body"
                        prop:value=new_body
                        on:input=move |ev| new_body.set(event_target_value(&ev))
                    ></textarea>
                    <button class="rh-btn" type="submit">"Post thread"</button>
                </form>
            </section>
            <section class="rh-panel rh-reader" aria-label="Thread posts">
                <Show
                    when=move || state.with(|s| s.selected_thread.is_some())
                    fallback=|| view! {
                        <p class="rh-empty">"Select a thread to read."</p>
                    }
                >
                    <div class="rh-posts">
                        <For
                            each=move || state.with(|s| s.posts.clone())
                            key=|p| p.id.clone()
                            children=move |p| view! {
                                <article class="rh-post">
                                    <span class="rh-from">{p.author}</span>
                                    <p class="rh-post-body">{p.body}</p>
                                </article>
                            }
                        />
                    </div>
                    <form class="rh-reply" on:submit=reply>
                        <textarea
                            class="rh-input"
                            placeholder="Write a reply\u{2026}"
                            aria-label="Reply body"
                            prop:value=reply_body
                            on:input=move |ev| reply_body.set(event_target_value(&ev))
                        ></textarea>
                        <button class="rh-btn" type="submit">"Reply"</button>
                    </form>
                </Show>
            </section>
        </main>
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
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">
                "Direct messages"
            </h1>
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
                            let current = selected.clone();
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
                                        aria-current=move || current().then_some("true")
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
            <section class="rh-chat" aria-label="Conversation">
                <Show
                    when=move || state.with(|s| s.selected_dm.is_some())
                    fallback=|| view! {
                        <p class="rh-empty">"Select a conversation."</p>
                    }
                >
                    <div class="rh-scroll" role="log" aria-label="Conversation messages">
                        <ul class="rh-lines">
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
                                    <li class="rh-line">
                                        <span class="rh-from">{m.from}</span>
                                        {m.text}
                                    </li>
                                }
                            />
                        </ul>
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
                            aria-label="Write a direct message"
                            placeholder="Write a message\u{2026}"
                            prop:value=draft
                            on:input=move |ev| draft.set(event_target_value(&ev))
                        />
                        <button class="rh-btn" type="submit">"Send"</button>
                    </form>
                </Show>
            </section>
        </main>
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
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-panel rh-members">
                <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">"Members"</h1>
                <input
                    class="rh-input"
                    type="search"
                    aria-label="Search members"
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
                            // The dot alone carried presence; keep it
                            // decorative and speak the state as hidden text.
                            let presence = if m.online { "Online:" } else { "Offline:" };
                            view! {
                                <li class="rh-tree-item">
                                    <button
                                        class="rh-member-link"
                                        on:click=move |_| {
                                            state.update(|s| s.select_member(&handle))
                                        }
                                    >
                                        <span class=dot aria-hidden="true"></span>
                                        <span class="rh-visually-hidden">{presence}</span>
                                        <span class="rh-member-name">{m.display_name}</span>
                                        <span class="rh-member-handle">"@"{m.handle}</span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
            </section>
            <section class="rh-panel rh-profile" aria-label="Member profile">
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
                                    <h2 class="rh-card-name">{m.display_name.clone()}</h2>
                                    <p class="rh-card-handle">"@"{m.handle.clone()}</p>
                                    <p class="rh-card-status">{status}</p>
                                    <p class="rh-card-bio">{m.bio.clone()}</p>
                                </div>
                            }
                        })
                    })}
                </Show>
            </section>
        </main>
    }
}

/// The Looking Glass **server browser**: search + a ranked list of public
/// servers, each with a Connect action that hands its endpoint to the login
/// screen (which prefills on its next mount). Directory data is the host-tested
/// [`crate::servers`] model, seeded in dev until a tracker transport lands.
#[component]
pub fn ServerBrowser() -> impl IntoView {
    let app = expect_context::<AppState>();
    let servers = app.servers;
    let navigate = use_navigate();
    let query = create_rw_signal(String::new());
    let rows = move || crate::servers::browse(&servers.get(), &query.get());

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-panel rh-servers" aria-label="Server directory">
                <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">"Looking Glass"</h1>
                <input
                    class="rh-input"
                    type="search"
                    aria-label="Search servers"
                    placeholder="Search servers\u{2026}"
                    prop:value=move || query.get()
                    on:input=move |ev| query.set(event_target_value(&ev))
                />
                <ul class="rh-server-list">
                    <For
                        each=rows
                        key=|s| s.endpoint.clone()
                        children=move |s| {
                            let navigate = navigate.clone();
                            let endpoint = s.endpoint.clone();
                            let dot = if s.reachable { "rh-dot on" } else { "rh-dot off" };
                            let presence = if s.reachable { "Online:" } else { "Offline:" };
                            let uptime = crate::servers::uptime_label(s.uptime_pct);
                            view! {
                                <li class="rh-server-card">
                                    <div class="rh-server-head">
                                        <span class=dot aria-hidden="true"></span>
                                        <span class="rh-visually-hidden">{presence}</span>
                                        <span class="rh-server-name">{s.name.clone()}</span>
                                        <span class="rh-server-users">
                                            {s.users_online}" online"
                                        </span>
                                    </div>
                                    <p class="rh-server-desc">{s.description.clone()}</p>
                                    <div class="rh-server-foot">
                                        <span class="rh-server-uptime">{uptime}</span>
                                        <code class="rh-server-endpoint">{s.endpoint.clone()}</code>
                                        <button
                                            class="rh-btn"
                                            on:click=move |_| {
                                                app.pending_endpoint.set(Some(endpoint.clone()));
                                                navigate("/", Default::default());
                                            }
                                        >
                                            "Connect"
                                        </button>
                                    </div>
                                </li>
                            }
                        }
                    />
                </ul>
            </section>
        </main>
    }
}

/// A tiny built-in ANSI sample so the art gallery renders something without a
/// live file transfer. Real art will come from the file library once download
/// bytes flow through the transport.
const SAMPLE_ANSI: &[u8] =
    b"\x1b[1;36m  RabbitHole \x1b[0;35mANSI\x1b[0m\r\n\x1b[1;33m  \xDB\xDB\xB2\xB1\xB0\x1b[0;32m warren art \x1b[1;31m\xDB\xDB\x1b[0m\r\n\x1b[0;44;37m  press any key  \x1b[0m\r\n";

/// The file library: browse areas → folders, inspect metadata, download/upload,
/// and watch the transfer queue. Mirrors the boards/directory component style.
#[component]
pub fn Files() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.files;
    app.load_areas();

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Files"</h1>
            <section class="rh-panel rh-files" aria-label="File browser">
                <Show
                    when=move || files.with(|f| f.current_area.is_some())
                    fallback=move || view! { <AreaList/> }
                >
                    <FolderBrowser/>
                </Show>
            </section>
            <section class="rh-panel" aria-label="File details and transfers">
                <FileDetail/>
                <TransferQueue/>
            </section>
        </main>
    }
}

/// The list of file areas shown before one is opened.
#[component]
fn AreaList() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.files;
    view! {
        <h2 class="rh-panel-title">"File areas"</h2>
        <ul class="rh-tree">
            <For
                each=move || files.with(|f| f.areas.clone())
                key=|a| a.slug.clone()
                children=move |a| {
                    let slug = a.slug.clone();
                    view! {
                        <li class="rh-tree-item">
                            <button
                                class="rh-board-link"
                                on:click=move |_| app.open_area(&slug)
                            >
                                <span class="rh-board-name">{a.title}</span>
                                <span class="rh-board-desc">{a.description}</span>
                            </button>
                        </li>
                    }
                }
            />
        </ul>
    }
}

/// The folder browser for an open area: breadcrumbs, an upload action, and the
/// child-node list.
#[component]
fn FolderBrowser() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.files;

    let leave = move |_| {
        files.update(|f| {
            f.current_area = None;
            f.path.clear();
            f.nodes.clear();
            f.selected = None;
        });
    };

    view! {
        <button class="rh-back" on:click=leave>"\u{2190} All areas"</button>
        <nav class="rh-crumbs" aria-label="Folder path">
            <For
                each=move || {
                    files.with(|f| f.breadcrumbs().into_iter().enumerate().collect::<Vec<_>>())
                }
                key=|(i, (label, _))| format!("{i}:{label}")
                children=move |(i, (label, path))| {
                    view! {
                        {(i > 0).then(|| view! {
                            <span class="rh-crumb sep" aria-hidden="true">"/"</span>
                        })}
                        <button
                            class="rh-crumb"
                            on:click=move |_| app.go_to_path(path.clone())
                        >
                            {label}
                        </button>
                    }
                }
            />
        </nav>
        <div class="rh-toolbar">
            <button
                class="rh-btn small"
                on:click=move |_| app.upload("note.txt", b"hello from the web client".to_vec())
            >
                "Upload sample"
            </button>
        </div>
        <h2 class="rh-visually-hidden">"Folder contents"</h2>
        <ul class="rh-tree">
            <For
                each=move || files.with(|f| f.nodes.clone())
                key=|n| n.id
                children=move |n| {
                    let id = n.id;
                    let is_folder = n.kind == KIND_FOLDER;
                    let name = n.name.clone();
                    let icon = if is_folder { "\u{1F4C1}" } else { "\u{1F4C4}" };
                    // The kind emoji is decorative: folders already say
                    // "Folder" in their meta text, files show a size.
                    let meta = if is_folder {
                        node_kind_label(n.kind).to_string()
                    } else {
                        human_size(n.size)
                    };
                    let selected = move || files.with(|f| f.selected == Some(id));
                    let class = move || {
                        if selected() {
                            "rh-file-link active"
                        } else {
                            "rh-file-link"
                        }
                    };
                    let on_click = move |_| {
                        if is_folder {
                            app.open_subfolder(&name);
                        } else {
                            app.select_file(id);
                        }
                    };
                    view! {
                        <li class="rh-tree-item">
                            <button
                                class=class
                                aria-current=move || selected().then_some("true")
                                on:click=on_click
                            >
                                <span class="rh-file-icon" aria-hidden="true">{icon}</span>
                                <span class="rh-file-name">{n.name.clone()}</span>
                                <span class="rh-file-meta">{meta}</span>
                            </button>
                        </li>
                    }
                }
            />
        </ul>
    }
}

/// The metadata card and download action for the selected file.
#[component]
fn FileDetail() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.files;
    view! {
        <Show
            when=move || files.with(|f| f.selected_node().is_some())
            fallback=|| view! { <p class="rh-empty">"Select a file to see its details."</p> }
        >
            {move || {
                files.with(|f| {
                    f.selected_node().map(|n| {
                        let id = n.id;
                        view! {
                            <div class="rh-card">
                                <h2 class="rh-card-name">{n.name.clone()}</h2>
                                <dl class="rh-meta-grid">
                                    <dt>"Type"</dt>
                                    <dd>{n.mime.clone()}</dd>
                                    <dt>"Size"</dt>
                                    <dd>{human_size(n.size)}</dd>
                                    <dt>"Uploader"</dt>
                                    <dd>{n.uploader.clone()}</dd>
                                    <dt>"Downloads"</dt>
                                    <dd>{n.downloads.to_string()}</dd>
                                    <dt>"Comment"</dt>
                                    <dd>{n.comment.clone()}</dd>
                                </dl>
                                <button class="rh-btn" on:click=move |_| app.download(id)>
                                    "Download"
                                </button>
                            </div>
                        }
                    })
                })
            }}
        </Show>
    }
}

/// The transfer queue: queued / active / done / failed with progress bars.
#[component]
fn TransferQueue() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.files;
    view! {
        <Show when=move || files.with(|f| !f.transfers.is_empty()) fallback=|| ()>
            <h2 class="rh-panel-title">"Transfers"</h2>
            <ul class="rh-queue">
                <For
                    each=move || files.with(|f| f.transfers.clone())
                    key=|t| format!("{}:{}:{:?}", t.id, t.percent(), t.status)
                    children=move |t| {
                        let pct = t.percent();
                        let (badge, bar) = match t.status {
                            TransferStatus::Queued => ("rh-badge", "rh-bar-fill"),
                            TransferStatus::Active => ("rh-badge active", "rh-bar-fill"),
                            TransferStatus::Done => ("rh-badge done", "rh-bar-fill"),
                            TransferStatus::Failed => ("rh-badge failed", "rh-bar-fill failed"),
                        };
                        let status = match t.status {
                            TransferStatus::Queued => "queued",
                            TransferStatus::Active => "active",
                            TransferStatus::Done => "done",
                            TransferStatus::Failed => "failed",
                        };
                        let width = format!("width:{pct}%");
                        let bar_label = format!("{} transfer progress", t.name);
                        view! {
                            <li class="rh-queue-item">
                                <div class="rh-queue-head">
                                    <span class="rh-queue-name">{t.name.clone()}</span>
                                    <span class=badge>{status}</span>
                                    <span class="rh-queue-pct">{format!("{pct}%")}</span>
                                </div>
                                <div
                                    class="rh-bar"
                                    role="progressbar"
                                    aria-label=bar_label
                                    aria-valuemin="0"
                                    aria-valuemax="100"
                                    aria-valuenow=pct.to_string()
                                >
                                    <div class=bar style=width></div>
                                </div>
                            </li>
                        }
                    }
                />
            </ul>
        </Show>
    }
}

/// The radio view: the station list (live/auto badges + listener counts) and
/// the stream player. All state and logic live in the host-tested
/// [`crate::radio`] (reducer, prefs, URL derivation); the wasm-only `<audio>`
/// element behind the controls is [`crate::player`], driven through the
/// [`AppState`] preference setters.
#[component]
pub fn Radio() -> impl IntoView {
    let app = expect_context::<AppState>();
    let radio = app.radio;
    let prefs = app.radio_prefs;
    app.load_radio();

    let stations = move || radio.with(|r| r.stations().cloned().collect::<Vec<_>>());

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Radio"</h1>
            <section class="rh-panel rh-stations">
                <h2 class="rh-panel-title">"On the air"</h2>
                <Show
                    when=move || radio.with(|r| !r.is_empty())
                    fallback=|| view! { <p class="rh-empty">"(off the air)"</p> }
                >
                    <ul class="rh-tree">
                        <For
                            each=stations
                            key=|s| format!("{}:{}:{}:{}", s.station, s.live, s.listeners, s.title)
                            children=move |s| {
                                let slug = s.station.clone();
                                let selected = {
                                    let slug = slug.clone();
                                    move || prefs.with(|p| p.station.as_deref() == Some(slug.as_str()))
                                };
                                let current = selected.clone();
                                let class = move || {
                                    if selected() {
                                        "rh-station-link active"
                                    } else {
                                        "rh-station-link"
                                    }
                                };
                                let badge = if s.live { "rh-badge live" } else { "rh-badge" };
                                let badge_text = if s.live { "LIVE" } else { "auto" };
                                let dj = if s.live {
                                    format!("DJ {}", s.dj)
                                } else {
                                    s.dj.clone()
                                };
                                let track = crate::radio::track_line(&s);
                                view! {
                                    <li class="rh-tree-item">
                                        <button
                                            class=class
                                            aria-current=move || current().then_some("true")
                                            on:click=move |_| app.select_station(&slug)
                                        >
                                            <span class="rh-station-head">
                                                <span class="rh-station-name">{s.station.clone()}</span>
                                                <span class=badge>{badge_text}</span>
                                                <span class="rh-file-meta">
                                                    {dj}" \u{b7} "{s.listeners}" listening"
                                                </span>
                                            </span>
                                            <span class="rh-station-track">{track}</span>
                                        </button>
                                    </li>
                                }
                            }
                        />
                    </ul>
                </Show>
            </section>
            <section class="rh-panel" aria-label="Radio player">
                <RadioPlayerPanel/>
            </section>
        </main>
    }
}

/// The player controls: the Icecast delivery address, enable/mute toggles,
/// and the volume slider. Controls are disabled (with a hint) until a valid
/// delivery address is set.
#[component]
fn RadioPlayerPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let prefs = app.radio_prefs;

    let base_ok = move || prefs.with(|p| crate::radio::base_is_valid(&p.base));
    let has_station = move || prefs.with(|p| p.station.is_some());
    let ready = move || base_ok() && has_station();
    let enabled = move || prefs.with(|p| p.enabled);
    let muted = move || prefs.with(|p| p.muted);
    let volume_pct = move || (prefs.with(|p| p.volume) * 100.0).round() as i32;

    let tuned = move || {
        prefs.with(|p| {
            p.station
                .as_deref()
                .and_then(|s| crate::radio::stream_url(&p.base, s))
        })
    };

    view! {
        <h2 class="rh-panel-title">"Player"</h2>
        <label class="rh-hint" for="rh-radio-base">
            "Your server's Icecast delivery address, e.g. http://host:8000"
        </label>
        <div class="rh-toolbar">
            <input
                id="rh-radio-base"
                class="rh-input"
                placeholder="http://host:8000"
                prop:value=move || prefs.with(|p| p.base.clone())
                on:change=move |ev| app.set_radio_base(&event_target_value(&ev))
            />
        </div>
        <Show when=move || !base_ok() fallback=|| ()>
            <p class="rh-hint">
                "Set a valid http:// or https:// delivery address to enable the player."
            </p>
        </Show>
        <Show when=move || base_ok() && !has_station() fallback=|| ()>
            <p class="rh-hint">"Pick a station from the list to tune in."</p>
        </Show>
        <fieldset class="rh-fieldset rh-toolbar">
            <legend class="rh-visually-hidden">"Playback controls"</legend>
            <button
                class="rh-btn small"
                disabled=move || !ready()
                on:click=move |_| app.set_radio_enabled(!prefs.get_untracked().enabled)
            >
                {move || if enabled() { "\u{25a0} Stop" } else { "\u{25b6} Listen" }}
            </button>
            <button
                class="rh-btn small ghost"
                disabled=move || !ready()
                on:click=move |_| app.set_radio_muted(!prefs.get_untracked().muted)
            >
                {move || if muted() { "Unmute" } else { "Mute" }}
            </button>
            <input
                class="rh-slider"
                type="range"
                min="0"
                max="100"
                aria-label="Volume"
                disabled=move || !ready()
                prop:value=move || volume_pct().to_string()
                on:input=move |ev| {
                    if let Ok(v) = event_target_value(&ev).parse::<f32>() {
                        app.set_radio_volume(v / 100.0);
                    }
                }
            />
            <span class="rh-file-meta" aria-hidden="true">
                {move || format!("{}%", volume_pct())}
            </span>
        </fieldset>
        <Show when=move || tuned().is_some() fallback=|| ()>
            <p class="rh-hint">
                {move || {
                    let url = tuned().unwrap_or_default();
                    if enabled() { format!("Playing {url}") } else { format!("Ready: {url}") }
                }}
            </p>
        </Show>
    }
}

/// Render CP437/ANSI `bytes` to an HTML `<canvas>`. Parsing and the
/// cells→draw-ops transform are pure ([`crate::art`]); only the paint call is
/// wasm-gated. The canvas is exposed as an image with `label` as its
/// alternative text — canvas content is otherwise invisible to assistive
/// technology.
#[component]
pub fn ArtCanvas(
    #[prop(into)] bytes: Vec<u8>,
    /// Alternative text for the rendered artwork.
    #[prop(into, default = String::from("ANSI artwork"))]
    label: String,
) -> impl IntoView {
    let canvas = crate::art::parse_art(&bytes);
    let (w, h) = crate::art::pixel_size(&canvas);
    let node = create_node_ref::<leptos::html::Canvas>();

    #[cfg(target_arch = "wasm32")]
    {
        let canvas = canvas.clone();
        create_effect(move |_| {
            if let Some(el) = node.get() {
                crate::art::paint(&el, &canvas);
            }
        });
    }

    view! {
        <canvas
            node_ref=node
            width=w
            height=h
            class="rh-art"
            role="img"
            aria-label=label
        ></canvas>
    }
}

/// The web-admin console: server config, accounts & classes, and a moderation
/// panel. Gated behind the session's admin capability ([`AppState::is_admin`]);
/// the nav entry that reaches it is likewise gated in [`Nav`].
#[component]
pub fn Admin() -> impl IntoView {
    let app = expect_context::<AppState>();
    let is_admin = app.is_admin;
    let admin = app.admin;
    // Load the seeded console data whenever the capability is present.
    create_effect(move |_| {
        if is_admin.get() {
            app.load_classes();
            app.load_accounts();
            app.load_config();
            app.load_syndication();
        }
    });
    let status = move || admin.with(|a| a.status.clone());

    view! {
        <StatusBar/>
        <main class="rh-admin-main" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Admin"</h1>
            <Show
                when=move || is_admin.get()
                fallback=|| view! {
                    <div class="rh-body">
                        <section class="rh-panel">
                            <p class="rh-empty">"You do not have admin access."</p>
                        </section>
                    </div>
                }
            >
                <div class="rh-admin-status" role="status">{status}</div>
                <div class="rh-body rh-admin">
                    <section class="rh-panel">
                        <AdminConfigPanel/>
                        <AdminModerationPanel/>
                    </section>
                    <section class="rh-panel">
                        <AdminAccountsPanel/>
                        <AdminClassesPanel/>
                    </section>
                </div>
                <div class="rh-body">
                    <section class="rh-panel">
                        <SyndicationPanel/>
                    </section>
                </div>
                <div class="rh-body">
                    <section class="rh-panel">
                        <ThemeEditorPanel/>
                    </section>
                </div>
            </Show>
        </main>
    }
}

/// Syndication & Gateways: the per-network gateway matrix (enabled state,
/// listener port, live/restart badge, toggle), the poll-interval editor with
/// inline validation, and the read-only feeds table + monitor. All state and
/// logic live in the host-tested [`crate::syndication_admin`]; the panel
/// rides the **existing** ADMIN config get/set vocabulary — no new wire
/// messages. Feeds are honest about being TOML-only server-side, and live
/// per-feed stats are a clearly-labeled seam for a future server slice.
/// Admin-gated by rendering inside [`Admin`]'s capability guard.
#[component]
fn SyndicationPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let syn = app.syndication;

    let status = move || syn.with(|s| s.status.clone());
    let has_status = move || syn.with(|s| !s.status.is_empty());
    let matrix = move || syn.with(|s| s.gateway_matrix());
    let poll_error = move || syn.with(|s| s.poll_error.clone().unwrap_or_default());
    let has_poll_error = move || syn.with(|s| s.poll_error.is_some());
    let can_save_poll = move || syn.with(|s| s.poll_save_command().is_some());
    let feeds_unavailable = move || syn.with(|s| s.feeds == FeedsStatus::Unavailable);
    let feeds_loaded = move || syn.with(|s| matches!(s.feeds, FeedsStatus::Listed(_)));
    let feed_rows = move || syn.with(|s| s.feed_rows());
    let feed_state = move || syn.with(|s| s.feed_state_line());

    view! {
        <h2 class="rh-panel-title">"Syndication & gateways"</h2>
        <Show when=has_status fallback=|| ()>
            <p class="rh-hint" role="status">{status}</p>
        </Show>

        <h3 class="rh-panel-title">"Gateway matrix"</h3>
        <table class="rh-table">
            <thead>
                <tr>
                    <th scope="col">"State"</th>
                    <th scope="col">"Network"</th>
                    <th scope="col">"Port"</th>
                    <th scope="col">"Applies"</th>
                    <th scope="col"><span class="rh-visually-hidden">"Toggle"</span></th>
                </tr>
            </thead>
            <tbody>
                <For
                    each=matrix
                    key=|r| format!("{}:{:?}:{:?}:{}", r.toggle_key, r.enabled, r.port, r.applies_live)
                    children=move |r| {
                        let (dot, state_text) = match r.enabled {
                            Some(true) => ("rh-dot on", "enabled"),
                            Some(false) => ("rh-dot off", "disabled"),
                            None => ("rh-dot pending", "unknown"),
                        };
                        let port = r
                            .port
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "\u{2014}".to_string());
                        let (badge, badge_text) = if r.applies_live {
                            ("rh-badge done", "live")
                        } else {
                            ("rh-badge", "restart")
                        };
                        let toggle_key = r.toggle_key;
                        let can_toggle = r.enabled.is_some();
                        let label = match r.enabled {
                            Some(true) => "Disable",
                            _ => "Enable",
                        };
                        view! {
                            <tr>
                                <td>
                                    <span class=dot aria-hidden="true"></span>
                                    <span class="rh-visually-hidden">{state_text}</span>
                                </td>
                                <td class="rh-member-name">{r.family}</td>
                                <td class="rh-file-meta">{port}</td>
                                <td><span class=badge>{badge_text}</span></td>
                                <td>
                                    <button
                                        class="rh-btn small"
                                        disabled=!can_toggle
                                        on:click=move |_| app.syn_toggle(toggle_key)
                                    >
                                        {label}
                                        <span class="rh-visually-hidden">" "{r.family}</span>
                                    </button>
                                </td>
                            </tr>
                        }
                    }
                />
            </tbody>
        </table>
        <p class="rh-hint">
            "\"restart\" keys save to burrow.toml but take effect only after a \
             server restart (listeners bind at boot); \"live\" keys apply \
             immediately."
        </p>

        <h3 class="rh-panel-title">"Feed polling"</h3>
        <div class="rh-toolbar">
            <label class="rh-config-key" for="rh-syn-poll-secs">"syndication_poll_secs"</label>
            <input
                id="rh-syn-poll-secs"
                class="rh-input"
                prop:value=move || syn.with(|s| s.poll_draft.clone())
                on:input=move |ev| app.syn_set_poll_draft(&event_target_value(&ev))
            />
            <button
                class="rh-btn small"
                disabled=move || !can_save_poll()
                on:click=move |_| app.syn_save_poll()
            >
                "Save"
            </button>
        </div>
        <Show when=has_poll_error fallback=|| ()>
            <p class="rh-warn" role="alert">{poll_error}</p>
        </Show>
        <p class="rh-hint">
            "Base seconds between feed polls (1\u{2013}604800). The server \
             clamps the effective schedule between 300 s (politeness floor) \
             and 86400 s (backoff ceiling). Restart required \u{2014} the \
             poll task starts at boot."
        </p>

        <h3 class="rh-panel-title">"Feeds (URL \u{2192} board)"</h3>
        <Show when=feeds_unavailable fallback=|| ()>
            <p class="rh-hint">
                "This server does not expose syndication_feeds over the admin \
                 wire \u{2014} the map is TOML-only. Edit the \
                 [syndication_feeds] table in burrow.toml (feed URL = board \
                 slug) and restart."
            </p>
        </Show>
        <Show when=feeds_loaded fallback=|| ()>
            <Show
                when=move || !feed_rows().is_empty()
                fallback=|| view! { <p class="rh-empty">"(no feeds configured)"</p> }
            >
                <table class="rh-table">
                    <thead>
                        <tr>
                            <th scope="col">"Feed URL"</th>
                            <th scope="col">"Board"</th>
                            <th scope="col">"State"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For
                            each=feed_rows
                            key=|f| f.url.clone()
                            children=move |f| view! {
                                <tr>
                                    <td class="rh-member-name">{f.url}</td>
                                    <td class="rh-member-handle">{f.board}</td>
                                    <td class="rh-file-meta">{feed_state}</td>
                                </tr>
                            }
                        />
                    </tbody>
                </table>
            </Show>
            <p class="rh-hint">
                "Read-only here: the mapping itself is TOML-only \u{2014} edit \
                 the [syndication_feeds] table in burrow.toml and restart to \
                 change it."
            </p>
        </Show>

        <h3 class="rh-panel-title">"Feed monitor"</h3>
        <p class="rh-hint">
            "Configured state only. Live per-feed stats (last poll, \
             conditional-GET 304s, dedupe hits) land with a future server \
             slice \u{2014} no feed-stats wire message exists yet, and this \
             panel does not invent one."
        </p>
    }
}

/// Theme editor: edit a pack's tokens with live scoped preview, WCAG
/// contrast warnings, import/export as shareable JSON token files, and an
/// apply-to-my-session action through the custom-pack override slot. All
/// state and validation live in the host-tested [`crate::theme_editor`]; this
/// component only folds [`EditorAction`]s into an `RwSignal<EditorState>`.
/// Admin-gated by virtue of rendering inside [`Admin`]'s capability guard.
#[component]
fn ThemeEditorPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let editor = create_rw_signal(EditorState::new(ThemePack::Clean));
    let edit_mode = create_rw_signal(Mode::Light);
    let import_text = create_rw_signal(String::new());
    let dispatch = move |action: EditorAction| editor.update(|e| e.apply(action));

    // Base-pack selector. `aria-pressed` mirrors the visual selected state
    // the ghost/solid classes convey.
    let base_buttons = [ThemePack::Clean, ThemePack::Retro, ThemePack::HighContrast].map(|pack| {
        let selected = move || editor.with(|e| e.base == pack);
        let class = move || {
            if selected() {
                "rh-btn small"
            } else {
                "rh-btn small ghost"
            }
        };
        view! {
            <button
                class=class
                aria-pressed=move || selected().to_string()
                on:click=move |_| dispatch(EditorAction::SelectBase(pack))
            >
                {pack_label(pack)}
            </button>
        }
    });

    // Light/dark tabs select which colour map is edited and previewed.
    let mode_tabs = [(Mode::Light, "Light"), (Mode::Dark, "Dark")].map(|(mode, label)| {
        let selected = move || edit_mode.get() == mode;
        let class = move || {
            if selected() {
                "rh-btn small"
            } else {
                "rh-btn small ghost"
            }
        };
        view! {
            <button
                class=class
                aria-pressed=move || selected().to_string()
                on:click=move |_| edit_mode.set(mode)
            >
                {label}
            </button>
        }
    });

    // Rows re-key on (mode, var, value) so committed edits re-render.
    let colour_rows = move || {
        let mode = edit_mode.get();
        editor.with(|e| {
            let map = match mode {
                Mode::Light => &e.working.light,
                Mode::Dark => &e.working.dark,
            };
            map.iter()
                .map(|(var, value)| (mode, var.clone(), value.clone()))
                .collect::<Vec<_>>()
        })
    };
    let shared_rows = move || {
        editor.with(|e| {
            e.working
                .shared
                .iter()
                .map(|(var, value)| (var.clone(), value.clone()))
                .collect::<Vec<_>>()
        })
    };

    let error = move || editor.with(|e| e.error.clone());
    let has_error = move || editor.with(|e| e.error.is_some());
    let warnings = move || {
        editor.with(|e| {
            contrast_warnings(&e.working)
                .into_iter()
                .map(|w| w.message())
                .collect::<Vec<_>>()
        })
    };
    let dirty = move || editor.with(|e| e.dirty);
    let preview_style = move || editor.with(|e| e.working.style_for(edit_mode.get()));

    let apply_session = move |_| app.apply_custom_pack(editor.with(|e| e.working.clone()));
    let revert_session = move |_| app.clear_custom_pack();
    let reset = move |_| dispatch(EditorAction::Reset(editor.with(|e| e.base)));
    let do_import = move |_| dispatch(EditorAction::LoadJson(import_text.get()));

    view! {
        <div class="rh-editor">
            <h2 class="rh-panel-title">
                "Theme editor "
                <Show when=dirty fallback=|| ()>
                    <span class="rh-badge active">"edited"</span>
                </Show>
            </h2>
            <fieldset class="rh-fieldset rh-toolbar">
                <legend class="rh-var-name">"base pack"</legend>
                {base_buttons.to_vec()}
                <button class="rh-btn small ghost" on:click=reset>"Reset"</button>
            </fieldset>
            <fieldset class="rh-fieldset rh-toolbar">
                <legend class="rh-var-name">"mode"</legend>
                {mode_tabs.to_vec()}
            </fieldset>
            <Show when=has_error fallback=|| ()>
                <p class="rh-warn" role="alert">{error}</p>
            </Show>
            <h3 class="rh-panel-title">"Colours"</h3>
            <ul class="rh-tree">
                <For
                    each=colour_rows
                    key=|(mode, var, value)| format!("{mode:?}:{var}:{value}")
                    children=move |(mode, var, value)| {
                        let swatch = format!("background:{value}");
                        let name = var.clone();
                        let scope = match mode {
                            Mode::Light => "light",
                            Mode::Dark => "dark",
                        };
                        let input_id = a11y::token_input_id(scope, &var);
                        let refocus_id = input_id.clone();
                        // Committing re-keys (and so re-creates) this row;
                        // put focus back on the same input so a keyboard
                        // (Enter) commit does not strand focus on <body>.
                        let on_commit = move |ev| {
                            dispatch(EditorAction::SetColor {
                                mode,
                                var: var.clone(),
                                value: event_target_value(&ev),
                            });
                            a11y::focus_id(&refocus_id);
                        };
                        view! {
                            <li class="rh-editor-row">
                                <span class="rh-swatch" style=swatch aria-hidden="true"></span>
                                <label class="rh-var-name" for=input_id.clone()>{name}</label>
                                <input
                                    id=input_id
                                    class="rh-input"
                                    prop:value=value.clone()
                                    on:change=on_commit
                                />
                            </li>
                        }
                    }
                />
            </ul>
            <h3 class="rh-panel-title">"Spacing, radii & type"</h3>
            <ul class="rh-tree">
                <For
                    each=shared_rows
                    key=|(var, value)| format!("{var}:{value}")
                    children=move |(var, value)| {
                        let name = var.clone();
                        let input_id = a11y::token_input_id("shared", &var);
                        let refocus_id = input_id.clone();
                        let on_commit = move |ev| {
                            dispatch(EditorAction::SetShared {
                                var: var.clone(),
                                value: event_target_value(&ev),
                            });
                            a11y::focus_id(&refocus_id);
                        };
                        view! {
                            <li class="rh-editor-row">
                                <label class="rh-var-name" for=input_id.clone()>{name}</label>
                                <input
                                    id=input_id
                                    class="rh-input"
                                    prop:value=value.clone()
                                    on:change=on_commit
                                />
                            </li>
                        }
                    }
                />
            </ul>
            <Show when=move || !warnings().is_empty() fallback=|| ()>
                <h3 class="rh-panel-title">"Contrast warnings"</h3>
                <ul class="rh-tree">
                    <For
                        each=warnings
                        key=|msg| msg.clone()
                        children=|msg| view! { <li class="rh-warn">{msg}</li> }
                    />
                </ul>
            </Show>
            <h3 class="rh-panel-title">"Preview"</h3>
            <ThemeEditorPreview style=Signal::derive(preview_style)/>
            <div class="rh-toolbar">
                <button class="rh-btn small" on:click=apply_session>"Apply to my session"</button>
                <button class="rh-btn small ghost" on:click=revert_session>"Revert session"</button>
                <button
                    class="rh-btn small ghost"
                    disabled=true
                    title="server theme bundles land with the W8 bundle-application slice"
                >
                    "Publish to server"
                </button>
            </div>
            <h3 class="rh-panel-title">"Export (token file)"</h3>
            <textarea
                class="rh-textarea"
                readonly=true
                aria-label="Exported token file JSON"
                prop:value=move || editor.with(|e| e.export_json())
            ></textarea>
            <h3 class="rh-panel-title">"Import (paste a token file)"</h3>
            <textarea
                class="rh-textarea"
                aria-label="Token file JSON to import"
                placeholder="Paste token-file JSON here\u{2026}"
                prop:value=import_text
                on:input=move |ev| import_text.set(event_target_value(&ev))
            ></textarea>
            <div class="rh-toolbar">
                <button class="rh-btn small" on:click=do_import>"Import"</button>
            </div>
        </div>
    }
}

/// The scoped live-preview pane: a small mock of nav/status/chat inside a
/// container whose `style` attribute carries the working tokens, so only this
/// subtree re-themes (the app root keeps its own variables).
///
/// The mock is purely decorative, so it is `aria-hidden` and built from
/// non-interactive elements — no `<a>`/`<button>`/`<header>`/`<nav>` that
/// would put fake stops in the tab order or fake landmarks in the outline
/// (the old `href="#"` anchors were even router-interceptable).
#[component]
fn ThemeEditorPreview(style: Signal<String>) -> impl IntoView {
    view! {
        <div class="rh-preview" style=move || style.get() aria-hidden="true">
            <div class="rh-header">
                <span class="rh-dot on"></span>
                <span class="rh-title">"RabbitHole"</span>
                <span class="rh-status">"Connected"</span>
                <span class="rh-spacer"></span>
                <span class="rh-nav">
                    <span class="rh-nav-item active">"Lobby"</span>
                    <span class="rh-nav-item">"Boards"</span>
                </span>
            </div>
            <div class="rh-preview-body">
                <div class="rh-line">
                    <span class="rh-from">"rabbit"</span>
                    "Welcome to the warren."
                </div>
                <div class="rh-line">
                    <span class="rh-from">"carrot"</span>
                    "This theme is looking sharp."
                </div>
                <p class="rh-warn">"A sample error line."</p>
                <span class="rh-btn small">"Send"</span>
            </div>
        </div>
    }
}

/// Server-config editor: one row per known key with a Save action. Each
/// input is labelled by its config key via a real `<label for=…>` pair
/// (ids from [`a11y::config_input_id`]).
#[component]
fn AdminConfigPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let admin = app.admin;
    view! {
        <h2 class="rh-panel-title">"Server config"</h2>
        <ul class="rh-tree">
            <For
                each=move || admin.with(|a| a.config.clone())
                key=|c| c.key.clone()
                children=move |c| {
                    let key = c.key.clone();
                    let input_id = a11y::config_input_id(&key);
                    let draft = create_rw_signal(c.value.clone());
                    let save_key = key.clone();
                    let save = move |_| app.set_config(&save_key, &draft.get());
                    view! {
                        <li class="rh-tree-item rh-config-row">
                            <label class="rh-config-key" for=input_id.clone()>{key}</label>
                            <input
                                id=input_id
                                class="rh-input"
                                prop:value=move || draft.get()
                                on:input=move |ev| draft.set(event_target_value(&ev))
                            />
                            <button class="rh-btn small" on:click=save>"Save"</button>
                        </li>
                    }
                }
            />
        </ul>
    }
}

/// Moderation: broadcast a notice, kick a session, mint an invite.
#[component]
fn AdminModerationPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let notice = create_rw_signal(String::new());
    let session = create_rw_signal(String::new());

    let send_notice = move |_| {
        let text = notice.get();
        if text.trim().is_empty() {
            return;
        }
        app.broadcast(&text);
        notice.set(String::new());
    };
    let do_kick = move |_| {
        if let Ok(id) = session.get().trim().parse::<u64>() {
            app.kick(id);
            session.set(String::new());
        }
    };
    let do_invite = move |_| app.create_invite(86_400);

    view! {
        <h2 class="rh-panel-title">"Moderation"</h2>
        <div class="rh-toolbar">
            <input
                class="rh-input"
                aria-label="Notice to broadcast"
                placeholder="Broadcast a notice\u{2026}"
                prop:value=move || notice.get()
                on:input=move |ev| notice.set(event_target_value(&ev))
            />
            <button class="rh-btn small" on:click=send_notice>"Broadcast"</button>
        </div>
        <div class="rh-toolbar">
            <input
                class="rh-input"
                aria-label="Session id to kick"
                placeholder="Session id to kick\u{2026}"
                prop:value=move || session.get()
                on:input=move |ev| session.set(event_target_value(&ev))
            />
            <button class="rh-btn small" on:click=do_kick>"Kick"</button>
        </div>
        <div class="rh-toolbar">
            <button class="rh-btn small" on:click=do_invite>"Create invite (24h)"</button>
        </div>
    }
}

/// Account directory: role/class/status per account, with an enable/disable
/// toggle. Rendered as a real `<table>` — the data is tabular, and column
/// headers give screen readers the grid context the flex rows lacked.
#[component]
fn AdminAccountsPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let admin = app.admin;
    let total = move || admin.with(|a| a.account_total);
    view! {
        <h2 class="rh-panel-title">"Accounts (" {total} ")"</h2>
        <table class="rh-table">
            <thead>
                <tr>
                    <th scope="col">"State"</th>
                    <th scope="col">"Login"</th>
                    <th scope="col">"Class"</th>
                    <th scope="col">"Role"</th>
                    <th scope="col"><span class="rh-visually-hidden">"Toggle"</span></th>
                </tr>
            </thead>
            <tbody>
                <For
                    each=move || admin.with(|a| a.accounts.clone())
                    key=|a| a.id
                    children=move |a| {
                        let login = a.login.clone();
                        let disabled = a.disabled;
                        let class = a.class.clone().unwrap_or_else(|| "\u{2014}".to_string());
                        let (dot, state_text) = if disabled {
                            ("rh-dot off", "disabled")
                        } else {
                            ("rh-dot on", "enabled")
                        };
                        let toggle_login = login.clone();
                        let toggle = move |_| app.set_account_disabled(&toggle_login, !disabled);
                        let btn_label = if disabled { "Enable" } else { "Disable" };
                        let btn_target = login.clone();
                        view! {
                            <tr>
                                <td>
                                    <span class=dot aria-hidden="true"></span>
                                    <span class="rh-visually-hidden">{state_text}</span>
                                </td>
                                <td class="rh-member-name">{login}</td>
                                <td class="rh-member-handle">{class}</td>
                                <td class="rh-account-role">{a.role.to_string()}</td>
                                <td>
                                    <button class="rh-btn small" on:click=toggle>
                                        {btn_label}
                                        <span class="rh-visually-hidden">" "{btn_target}</span>
                                    </button>
                                </td>
                            </tr>
                        }
                    }
                />
            </tbody>
        </table>
    }
}

/// Permission classes: name, member count, and capability mask (hex), as a
/// table for the same reason as the accounts panel.
#[component]
fn AdminClassesPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let admin = app.admin;
    view! {
        <h2 class="rh-panel-title">"Classes"</h2>
        <table class="rh-table">
            <thead>
                <tr>
                    <th scope="col">"Name"</th>
                    <th scope="col">"Members"</th>
                    <th scope="col">"Capability mask"</th>
                </tr>
            </thead>
            <tbody>
                <For
                    each=move || admin.with(|a| a.classes.clone())
                    key=|c| c.name.clone()
                    children=move |c| {
                        let mask = format!("0x{:016x}", c.base_mask);
                        view! {
                            <tr>
                                <td class="rh-member-name">{c.name}</td>
                                <td class="rh-member-handle">{c.members.to_string()}</td>
                                <td class="rh-file-meta">{mask}</td>
                            </tr>
                        }
                    }
                />
            </tbody>
        </table>
    }
}

/// The ANSI art gallery: renders a built-in sample to a canvas.
#[component]
pub fn ArtGallery() -> impl IntoView {
    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-panel">
                <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">"ANSI Art"</h1>
                <p class="rh-empty">
                    "CP437/ANSI rendered to a canvas through the shared art pipeline."
                </p>
                <div class="rh-art-wrap">
                    <ArtCanvas
                        bytes=SAMPLE_ANSI.to_vec()
                        label="Sample ANSI artwork: RabbitHole warren art in classic CP437 blocks"
                    />
                </div>
            </section>
        </main>
    }
}
