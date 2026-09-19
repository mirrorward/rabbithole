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
use rabbithole_core::theme::{Mode, ThemePack};

use crate::a11y;
use crate::app::AppState;
use crate::files::{human_size, node_kind_label, TransferStatus, KIND_FOLDER};
use crate::syndication_admin::{feed_stat_line, last_poll_label, FeedsStatus};
use crate::theme_css::{mode_name, pack_label};
use crate::theme_editor::{contrast_warnings, EditorAction, EditorState};
use crate::wire::AdminCommand;

/// The primary section links, as a **left sidebar**.
///
/// This used to be a row of pills in the header, next to the burrow title, the
/// connection state, the now-playing slot, the ⌘K button, presence and the
/// theme control. Nine destinations plus all of that in one 3.5rem strip meant
/// the right end simply ran off the window.
///
/// A vertical column fixes it structurally rather than by shrinking type: rows
/// grow downward, where there is room, so a tenth section costs nothing. It
/// also matches how every desktop app of this shape is laid out — rail, then
/// sidebar, then content — and gives each section an icon and a place for its
/// unread pip that isn't the middle of a sentence.
///
/// Rendered once by the shell (not per route), so switching sections doesn't
/// tear it down and rebuild it.
#[component]
pub fn Nav() -> impl IntoView {
    use crate::palette::{scope_of, sections_for, Scope};
    let app = expect_context::<AppState>();
    let location = leptos_router::use_location();
    // Which scope's sections to list. On a desktop this nav only shows in
    // burrow scope; on a phone it is the bottom tab bar everywhere, so in
    // the warren it lists the warren's screens instead.
    let scope = move || scope_of(&location.pathname.get());
    // The sidebar is mounted once by the shell and outlives every burrow, so
    // it must read the focused session *reactively* (`focused_tracked`), not
    // capture one session's signals at mount. Captured, it stayed bound to
    // whichever burrow was focused when the app started: after leaving that
    // burrow and joining another, the pips and the Admin entry were still
    // that first session's — an admin who reconnected lost the console.
    let is_admin = move || app.focused_tracked().is_admin.get();
    // Only ever rendered inside a burrow (the shell hides the sidebar
    // entirely on warren routes — People, Transfers, You and Servers are each
    // one screen and get the full width). So this lists the burrow's sections,
    // headed by the burrow's name.
    // Unread pips: the counts the wire has always carried, summed per section.
    // A pip says "something happened here" without shouting a number in a nav.
    let dm_unread = move || {
        app.focused_tracked()
            .state
            .with(|s| s.dm_threads.iter().map(|t| t.unread).sum::<u64>())
    };
    let board_unread = move || {
        app.focused_tracked()
            .state
            .with(|s| s.boards.iter().map(|b| b.unread).sum::<u64>())
    };
    let unread_for = move |route: &'static str| match route {
        "/boards" => Some(Signal::derive(board_unread)),
        "/dms" => Some(Signal::derive(dm_unread)),
        _ => None,
    };
    view! {
        <nav class="rh-subnav" aria-label="Primary">
            <span class="rh-subnav-scope">
                // focused_tracked(), not focused(): the latter reads the id
                // untracked, so this label was computed once at mount and
                // never followed a burrow switch.
                {move || match scope() {
                    Scope::Burrow => app
                        .focused_tracked()
                        .name
                        .get()
                        .unwrap_or_else(|| "This burrow".into()),
                    Scope::Warren => "Warren".to_string(),
                }}
            </span>
            <For
                each=move || sections_for(scope()).to_vec()
                key=|s| s.route
                children=move |s| view! {
                    <NavLink path=s.route label=s.label unread=unread_for(s.route)/>
                }
            />
            // The operator console, for operators — a burrow's, so only there.
            <Show when=move || { is_admin() && scope() == Scope::Burrow } fallback=|| ()>
                <div class="rh-subnav-rule" aria-hidden="true"></div>
                <NavLink path="/admin" label="Admin" unread=None/>
            </Show>
        </nav>
    }
}

/// The unread count a section's pip shows, if the section has one: boards
/// and DMs carry counts on the wire; the rest don't.
fn section_unread(app: AppState, route: &'static str) -> Option<Signal<u64>> {
    match route {
        "/boards" => Some(Signal::derive(move || {
            app.focused_tracked()
                .state
                .with(|s| s.boards.iter().map(|b| b.unread).sum::<u64>())
        })),
        "/dms" => Some(Signal::derive(move || {
            app.focused_tracked()
                .state
                .with(|s| s.dm_threads.iter().map(|t| t.unread).sum::<u64>())
        })),
        _ => None,
    }
}

/// The burrow's sections as a scrolling strip of chips under the header.
/// Phones only (the stylesheet hides it wider, where the sidebar lists the
/// same sections): a burrow has eight sections and a phone's bottom bar has
/// room for five, so the bar holds the warren and the strip holds the place.
/// Rendered in burrow scope only; the warren's screens have no sections.
#[component]
fn SectionStrip() -> impl IntoView {
    use crate::palette::{scope_of, sections_for, Scope};
    let app = expect_context::<AppState>();
    let location = leptos_router::use_location();
    let in_burrow = move || scope_of(&location.pathname.get()) == Scope::Burrow;
    let is_admin = move || app.focused_tracked().is_admin.get();
    view! {
        <Show when=in_burrow fallback=|| ()>
            <nav class="rh-section-strip" aria-label="Sections">
                <For
                    each=|| sections_for(Scope::Burrow).to_vec()
                    key=|s| s.route
                    children=move |s| view! {
                        <NavLink path=s.route label=s.label unread=section_unread(app, s.route)/>
                    }
                />
                <Show when=is_admin fallback=|| ()>
                    <NavLink path="/admin" label="Admin" unread=None/>
                </Show>
            </nav>
        </Show>
    }
}

/// One sidebar row: icon, label, and an optional unread pip. Split out so the
/// nav reads as a list of destinations instead of a wall of markup.
#[component]
fn NavLink(
    /// Route to link to; also selects the icon.
    path: &'static str,
    /// Visible text — and, since the icon is `aria-hidden`, the accessible name.
    label: &'static str,
    /// Unread count for this section, if it has one. A `Signal` rather than a
    /// closure because it's `Copy`: the pip's three reactive readers each need
    /// their own handle. Passed explicitly (not an optional prop) so every call
    /// site states whether it tracks unread.
    unread: Option<Signal<u64>>,
) -> impl IntoView {
    let icon = crate::icons::section_icon(path);
    view! {
        <A href=path class="rh-subnav-link">
            <span class="rh-subnav-icon" inner_html=icon></span>
            <span class="rh-subnav-label">{label}</span>
            {unread.map(|n| view! {
                <Show when=move || { n.get() > 0 } fallback=|| ()>
                    <span
                        class="rh-pip"
                        aria-label=move || format!("{} unread", n.get())
                    >{move || crate::state::unread_badge(n.get() as usize)}</span>
                </Show>
            })}
        </A>
    }
}

/// The unified **People** view: everyone present across *all* your connected
/// burrows, coalesced into one list. Each row shows their presence and which
/// burrows they're on — the warren layer's answer to "where are my people?".
#[component]
pub fn People() -> impl IntoView {
    use rabbithole_proto::presence::PresenceState;
    let app = expect_context::<AppState>();
    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"People"</h1>
            <section class="rh-panel">
                <h2 class="rh-panel-title">"People"<span class="rh-panel-sub">"across your burrows"</span></h2>
                <Show
                    when=move || !app.people().is_empty()
                    fallback=|| view! {
                        <EmptyState
                            icon="/people"
                            title="No one's around yet"
                            sub="Your people across every connected burrow gather here."
                        />
                    }
                >
                    <ul class="rh-people">
                        <For
                            each=move || app.people()
                            key=|p| (p.screen_name.clone(), p.key.clone(), p.state, p.servers.clone())
                            children=move |p| {
                                let dot = match p.state {
                                    PresenceState::Online => "rh-pres on",
                                    PresenceState::Away => "rh-pres away",
                                    PresenceState::Idle => "rh-pres idle",
                                    _ => "rh-pres off",
                                };
                                let servers = p.servers.join(" · ");
                                // The person carries a portable identity key whose
                                // possession the burrow proved at handshake (a valid
                                // KeyProof). That stops a passive attacker from just
                                // copying a public key out of a roster — but it is NOT
                                // relay-proof: a malicious burrow you connect to can
                                // relay an honest burrow's challenge (server_key is
                                // self-asserted, not channel-bound). So this is an
                                // identity-coalescing HINT, not an authentication
                                // badge — a key glyph, never a security checkmark.
                                let idkey = p.key.as_ref().map(|k| {
                                    let fp = crate::identity::short_fingerprint(k);
                                    view! {
                                        <span
                                            class="rh-person-idkey"
                                            title=format!("identity key {fp} — possession proven, not relay-proof")
                                         inner_html=crate::icons::key_icon()></span>
                                    }
                                });
                                let mark = crate::avatar::mark_svg(
                                    &crate::avatar::seed_for(p.key.as_deref(), &p.screen_name),
                                    24,
                                );
                                // The row opens the person page, keyed by the
                                // same seed People coalesces on — so the page
                                // survives a handle change and stays distinct
                                // from a same-handle stranger.
                                let href = format!(
                                    "/people/{}",
                                    crate::sightings::seed_of(p.key.as_deref(), &p.screen_name),
                                );
                                view! {
                                    <li class="rh-person">
                                        <A href=href class="rh-person-link">
                                            <span class="rh-mark" inner_html=mark></span>
                                            <span class=dot aria-hidden="true"></span>
                                            <span class="rh-person-name">{p.screen_name}</span>
                                            {idkey}
                                            <span class="rh-person-servers">{servers}</span>
                                        </A>
                                    </li>
                                }
                            }
                        />
                    </ul>
                </Show>
            </section>
        </main>
    }
}

/// The **About window**: what this program is, in the app's own voice.
///
/// macOS's standard About panel takes an icon, a name, a version string and a
/// blob of credits text — and nothing else. That's why it can never look like
/// the app it belongs to. So the desktop shell opens a small webview at
/// `/about` instead, and this renders inside it: the same tokens, the same
/// type, the same theme the rest of the app is wearing.
///
/// Version and build come from the shell (`window.__RH_BUILD__`, stamped at
/// compile time from the workspace manifest plus the git SHA), so the number
/// here is always the number that was actually built.
#[component]
pub fn About() -> impl IntoView {
    let copied = create_rw_signal(false);
    let build = build_info();
    let version = build.0.clone();
    let sha = build.1.clone();
    let sha_copy = sha.clone();
    view! {
        <main class="rh-about" id=a11y::MAIN_ID tabindex="-1">
            // The logo itself, the one the Dock and the connect window wear:
            // an About window is where an app shows its face, and a drawn
            // stand-in in the accent colour was a different face.
            <img
                class="rh-about-logo"
                src="/logo.png"
                alt=""
                width="112"
                height="112"
                draggable="false"
            />
            <h1 class="rh-about-name" id=a11y::VIEW_TITLE_ID tabindex="-1">"RabbitHole"</h1>
            <p class="rh-about-tagline">"A warren client \u{2014} many burrows, one you."</p>
            <p class="rh-about-version">
                <span>
                    {move || {
                        if sha.is_empty() {
                            format!("Version {version}")
                        } else {
                            format!("Version {version} \u{00b7} {sha}")
                        }
                    }}
                </span>
                // Copy belongs next to the thing it copies. It confirms by
                // becoming a tick for a moment — a toast for an action whose
                // result is invisible-but-expected is more interruption than
                // information.
                <button
                    class="rh-copy-btn"
                    class:done=move || copied.get()
                    title="Copy version and build"
                    aria-label="Copy version and build"
                    on:click=move |_| {
                        let v = if sha_copy.is_empty() {
                            build_info().0
                        } else {
                            format!("{} ({})", build_info().0, sha_copy)
                        };
                        copy_text_quiet(&format!("RabbitHole {v}"));
                        copied.set(true);
                        set_timeout(move || copied.set(false), std::time::Duration::from_millis(1400));
                    }
                    inner_html=move || {
                        if copied.get() {
                            crate::icons::check_icon()
                        } else {
                            crate::icons::copy_icon()
                        }
                    }
                ></button>
            </p>

            <ul class="rh-about-points">
                <li>
                    <span class="rh-about-point-k">"Places, not feeds"</span>
                    <span class="rh-about-point-v">
                        "Chat, message boards, file libraries and radio \u{2014} each burrow \
                         its own place, all of them at once."
                    </span>
                </li>
                <li>
                    <span class="rh-about-point-k">"Files that arrive from everywhere"</span>
                    <span class="rh-about-point-v">
                        "Content-addressed transfers, verified end to end, pulled from every \
                         peer that has a piece."
                    </span>
                </li>
                <li>
                    <span class="rh-about-point-k">"An identity that is yours"</span>
                    <span class="rh-about-point-v">
                        "One key names you everywhere. No server owns it, and friendship is \
                         something both sides sign."
                    </span>
                </li>
            </ul>

            <div class="rh-about-foot">
                <a
                    class="rh-about-link"
                    href="https://mirrorward.co"
                    target="_blank"
                    rel="noopener noreferrer"
                >"\u{00a9} Mirrorward"</a>
                <span class="rh-about-dot" aria-hidden="true">"\u{00b7}"</span>
                <a
                    class="rh-about-link"
                    href="https://rabbit.direct"
                    target="_blank"
                    rel="noopener noreferrer"
                >"rabbit.direct"</a>
            </div>
        </main>
    }
}

/// `(version, short_sha)` as stamped by the desktop shell, else the SPA's own
/// crate version — a browser tab has no shell to ask.
fn build_info() -> (String, String) {
    #[cfg(target_arch = "wasm32")]
    {
        use js_sys::Reflect;
        use wasm_bindgen::JsValue;
        if let Some(w) = web_sys::window() {
            if let Ok(b) = Reflect::get(&w, &JsValue::from_str("__RH_BUILD__")) {
                if !b.is_undefined() {
                    let get = |k: &str| {
                        Reflect::get(&b, &JsValue::from_str(k))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_default()
                    };
                    let v = get("version");
                    if !v.is_empty() {
                        return (v, get("sha"));
                    }
                }
            }
        }
    }
    (env!("CARGO_PKG_VERSION").to_string(), String::new())
}

/// **Settings**: the choices that belong to you rather than to any burrow.
///
/// Reached from the app menu (⌘,) on the desktop and from ⌘K anywhere. The
/// centrepiece is the tracker list — the directories the Looking Glass asks
/// when you go looking for burrows. A tracker is a discovery service, not a
/// place you're a member of, so the list is yours: add, disable, remove.
#[component]
pub fn Settings() -> impl IntoView {
    let app = expect_context::<AppState>();
    // The shell owns where downloads go, and it can change outside this
    // screen (a folder deleted, another window): read it fresh on arrival.
    app.load_download_prefs();
    let new_tracker = create_rw_signal(String::new());

    let add = move |_| {
        let input = new_tracker.get();
        let mut added = false;
        app.settings.update(|s| {
            added = crate::settings::add_tracker(&mut s.trackers, &input);
        });
        if added {
            new_tracker.set(String::new());
            app.save_settings();
        } else if !input.trim().is_empty() {
            app.notify(
                crate::toasts::ToastKind::Warn,
                "That tracker is already listed.".to_string(),
            );
        }
    };

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Settings"</h1>
            <section class="rh-panel rh-settings">
                <h2 class="rh-panel-title">"Settings"</h2>

                <h3 class="rh-person-h2">"Appearance"</h3>
                <div class="rh-seg" role="radiogroup" aria-label="Theme">
                    {[
                        rabbithole_core::theme::ThemePack::Clean,
                        rabbithole_core::theme::ThemePack::Retro,
                        rabbithole_core::theme::ThemePack::HighContrast,
                    ]
                    .into_iter()
                    .map(|pack| view! {
                        <button
                            type="button"
                            class="rh-seg-btn"
                            class:on=move || app.theme.get().pack == pack
                            role="radio"
                            aria-checked=move || (app.theme.get().pack == pack).to_string()
                            on:click=move |_| app.set_pack(pack)
                        >
                            {if pack == rabbithole_core::theme::ThemePack::HighContrast { "High contrast" } else { pack_label(pack) }}
                        </button>
                    })
                    .collect_view()}
                </div>
                <div class="rh-seg" role="radiogroup" aria-label="Light or dark">
                    {[
                        crate::theme_css::ModeChoice::System,
                        crate::theme_css::ModeChoice::Light,
                        crate::theme_css::ModeChoice::Dark,
                    ]
                    .into_iter()
                    .map(|mode| view! {
                        <button
                            type="button"
                            class="rh-seg-btn"
                            class:on=move || app.theme.get().mode == mode
                            role="radio"
                            aria-checked=move || (app.theme.get().mode == mode).to_string()
                            on:click=move |_| app.set_mode(mode)
                        >
                            {if mode == crate::theme_css::ModeChoice::System { "Match system" } else { mode_name(mode) }}
                        </button>
                    })
                    .collect_view()}
                </div>

                <h3 class="rh-person-h2">"Trackers"</h3>
                <p class="rh-settings-note">
                    "Directories asked when you look for burrows to join. A tracker only \
                     says who is out there \u{2014} joining is still up to you."
                </p>
                <ul class="rh-tracker-list">
                    <For
                        each=move || app.settings.get().trackers
                        key=|t| t.host.clone()
                        children=move |t| {
                            let host = t.host.clone();
                            let host_toggle = host.clone();
                            let host_remove = host.clone();
                            let is_default = host == crate::settings::DEFAULT_TRACKER;
                            view! {
                                <li class="rh-tracker-row">
                                    <input
                                        type="checkbox"
                                        class="rh-tracker-on"
                                        aria-label=format!("Query {host}")
                                        prop:checked=t.enabled
                                        on:change=move |_| {
                                            app.settings.update(|s| {
                                                if let Some(x) =
                                                    s.trackers.iter_mut().find(|x| x.host == host_toggle)
                                                {
                                                    x.enabled = !x.enabled;
                                                }
                                            });
                                            app.save_settings();
                                        }
                                    />
                                    <span class="rh-tracker-host">{host.clone()}</span>
                                    {is_default.then(|| view! {
                                        <span class="rh-tracker-tag">"default"</span>
                                    })}
                                    <button
                                        class="rh-btn ghost small rh-tracker-remove"
                                        title=format!("Remove {host}")
                                        on:click=move |_| {
                                            app.settings.update(|s| {
                                                s.trackers.retain(|x| x.host != host_remove)
                                            });
                                            app.save_settings();
                                        }
                                    >
                                        "Remove"
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
                <div class="rh-tracker-add">
                    <input
                        class="rh-input"
                        placeholder="tracker.example \u{2014} add a tracker"
                        aria-label="Add a tracker"
                        prop:value=new_tracker
                        on:input=move |ev| new_tracker.set(event_target_value(&ev))
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                add(());
                            }
                        }
                    />
                    <button class="rh-btn" on:click=move |_| add(())>"Add"</button>
                </div>

                <h3 class="rh-person-h2">"Downloads"</h3>
                <p class="rh-settings-note">
                    "A download pulls from every source that has a piece of the file \u{2014} \
                     other burrows and other people \u{2014} in parallel. More sources finish \
                     sooner and survive a peer dropping out; on a metered or narrow link they \
                     also compete for the same pipe."
                </p>
                <label class="rh-settings-range">
                    <span class="rh-settings-range-label">
                        "Sources per download: "
                        <strong>{move || app.settings.get().max_sources}</strong>
                    </span>
                    <input
                        type="range"
                        min="1"
                        max=crate::settings::MAX_SOURCES_CEILING.to_string()
                        prop:value=move || app.settings.get().max_sources.to_string()
                        on:input=move |ev| {
                            let n = event_target_value(&ev).parse::<u32>().unwrap_or(
                                crate::settings::DEFAULT_MAX_SOURCES,
                            );
                            app.settings.update(|s| {
                                s.max_sources = crate::settings::clamp_max_sources(n)
                            });
                            app.save_settings();
                        }
                    />
                </label>

                // Where a download lands. The desktop shell owns this (a folder
                // is only ever chosen in a native panel); in a browser tab the
                // browser decides, so there is nothing here to set.
                <Show
                    when=move || app.download_prefs.with(Option::is_some)
                    fallback=|| view! {
                        <p class="rh-settings-note">
                            "In a browser tab, your browser decides where downloads are saved. \
                             The desktop app can ask each time, or use a folder you choose."
                        </p>
                    }
                >
                    <div class="rh-settings-folder">
                        <p class="rh-settings-folder-line">
                            {move || app.download_prefs.with(|p| {
                                p.as_ref().map(|p| p.summary()).unwrap_or_default()
                            })}
                        </p>
                        <div class="rh-settings-folder-actions">
                            <button
                                type="button"
                                class="rh-btn ghost small"
                                on:click=move |_| app.choose_download_folder()
                            >
                                {move || if app.download_prefs.with(|p| {
                                    p.as_ref().is_some_and(|p| p.folder.is_some())
                                }) {
                                    "Change folder\u{2026}"
                                } else {
                                    "Choose a folder\u{2026}"
                                }}
                            </button>
                            <Show
                                when=move || app.download_prefs.with(|p| {
                                    p.as_ref().is_some_and(|p| p.folder.is_some())
                                })
                                fallback=|| ()
                            >
                                <button
                                    type="button"
                                    class="rh-btn ghost small"
                                    on:click=move |_| app.clear_download_folder()
                                >
                                    "Ask each time"
                                </button>
                            </Show>
                        </div>
                    </div>
                    <label class="rh-settings-check">
                        <input
                            type="checkbox"
                            prop:checked=move || app.download_prefs.with(|p| {
                                p.as_ref().is_some_and(|p| p.per_burrow)
                            })
                            on:change=move |ev| app.set_per_burrow_folders(event_target_checked(&ev))
                        />
                        <span>"Give each burrow a folder of its own"</span>
                    </label>
                    <p class="rh-settings-note">
                        "A download never replaces a file that is already there: it gets a \
                         numbered name instead."
                    </p>
                    <label class="rh-settings-check">
                        <input
                            type="checkbox"
                            prop:checked=move || app.download_prefs.with(|p| {
                                p.as_ref().is_some_and(|p| p.seed)
                            })
                            on:change=move |ev| app.set_seeding(event_target_checked(&ev))
                        />
                        <span>"Help other people\u{2019}s downloads"</span>
                    </label>
                    <p class="rh-settings-note" role="status">
                        {move || app.download_prefs.with(|p| {
                            p.as_ref().map(|p| p.seeding_line()).unwrap_or_default()
                        })}
                    </p>
                    <p class="rh-settings-note">
                        "When this is on, a file you download from a burrow is offered to other \
                         people on that same burrow, from this Mac, while the app is open. The \
                         burrow decides who may fetch it, and only files it already lists are \
                         shared. If that burrow lets people send files to other burrows, a \
                         burrow one of those files is sent to may fetch it from you too, and \
                         sees your address. Turning it off stops it at once."
                    </p>
                </Show>

                <h3 class="rh-person-h2">"On launch"</h3>
                <label class="rh-settings-check">
                    <input
                        type="checkbox"
                        prop:checked=move || app.settings.get().reconnect_on_launch
                        on:change=move |_| {
                            app.settings.update(|s| {
                                s.reconnect_on_launch = !s.reconnect_on_launch
                            });
                            app.save_settings();
                        }
                    />
                    <span>"Reconnect to the burrows I was in"</span>
                </label>

                <h3 class="rh-person-h2">"Notifications"</h3>
                <label class="rh-settings-check">
                    <input
                        type="checkbox"
                        prop:checked=move || app.settings.get().notifications
                        on:change=move |_| {
                            app.settings.update(|s| s.notifications = !s.notifications);
                            app.save_settings();
                        }
                    />
                    <span>"Notify me about direct messages while I'm away"</span>
                </label>
                <label class="rh-settings-check">
                    <input
                        type="checkbox"
                        prop:checked=move || app.sound_on.get()
                        on:change=move |_| {
                            let on = !app.sound_on.get_untracked();
                            app.sound_on.set(on);
                            #[cfg(target_arch = "wasm32")]
                            crate::sound::set_enabled(on);
                        }
                    />
                    <span>"Play a chime for new messages"</span>
                </label>
            </section>
        </main>
    }
}

/// The **person page**: everything you know about one human across your warren.
///
/// Reached from a People row (`/people/:seed`). The seed is the same
/// coalescing id People and the sightings ledger use — an identity key when
/// they have one, else a bare handle — so the page survives a handle change
/// and stays distinct from a same-handle stranger.
///
/// It draws from three sources at once: the live [`crate::state::Person`] (are
/// they on now, and where), the persisted [`crate::sightings`] ledger (where
/// you know them *from*, even offline), and [`crate::friend`] (are you
/// cryptographically friends). Plus the focused burrow's DM thread and their
/// uploads there.
#[component]
pub fn PersonPage() -> impl IntoView {
    use crate::friend::Status;
    let app = expect_context::<AppState>();
    let params = use_params_map();
    let seed = move || params.with(|p| p.get("seed").cloned().unwrap_or_default());

    // Live presence + current burrows, if they're on right now.
    let live = move || app.person_by_seed(&seed());
    // The persisted trail: where you know them from.
    let trail = move || {
        app.sightings
            .with(|l| crate::sightings::burrows_for(l, &seed()).cloned())
    };
    // Their identity key: the live person's, else — for a keyed seed — the
    // seed itself is the key (64 hex chars).
    let peer_key = move || {
        live().and_then(|p| p.key).or_else(|| {
            let s = seed();
            (s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())).then_some(s)
        })
    };
    // Their handle on the FOCUSED burrow (what DMs and uploads there file under).
    let handle_here = move || {
        let focused_ep = app.focused_endpoint();
        app.sightings
            .with(|l| crate::sightings::handle_on(l, &seed(), &focused_ep))
            .or_else(|| live().map(|p| p.screen_name))
            .unwrap_or_default()
    };
    let display_name = move || {
        live()
            .map(|p| p.screen_name)
            .or_else(|| trail().map(|t| t.name))
            .unwrap_or_else(seed)
    };
    let status = move || {
        peer_key()
            .map(|k| app.friendship(&k))
            .unwrap_or(Status::None)
    };
    // You appear in your own People list (you're on those burrows too). The
    // page still reads usefully — where you're known from — but befriending or
    // DMing yourself is nonsense, so those actions go.
    let is_me = move || match (peer_key(), app.you.get()) {
        (Some(k), Some(me)) => k.eq_ignore_ascii_case(&me.public_hex),
        _ => false,
    };
    let navigate = use_navigate();

    // Message: select the conversation with their handle here, then route to
    // it. A Callback (which is Copy) because it's used inside <Show> children,
    // which must be Fn — a plain closure capturing `navigate` is FnOnce.
    let go_dm = Callback::new(move |_: leptos::ev::MouseEvent| {
        let h = handle_here();
        if !h.is_empty() {
            app.select_dm(&h);
            navigate("/dms", Default::default());
        }
    });
    let befriend = move |_| {
        if let Some(k) = peer_key() {
            app.offer_friendship(&k, &display_name(), &handle_here());
        }
    };

    view! {
        <StatusBar/>
        <main class="rh-body rh-person-page" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">
                {move || format!("{} \u{2014} person", display_name())}
            </h1>
            <section class="rh-panel">
                <A href="/people" class="rh-back"><span class="rh-back-icon" inner_html=crate::icons::chevron_left_icon()></span>"People"</A>
                <header class="rh-person-hero">
                    <span
                        class="rh-mark rh-person-hero-mark"
                        inner_html=move || crate::avatar::mark_svg(&seed(), 72)
                    ></span>
                    <div class="rh-person-hero-id">
                        <div class="rh-person-hero-line">
                            <span class="rh-person-hero-name">{display_name}</span>
                            {move || match status() {
                                Status::Mutual => view! {
                                    <span class="rh-friend-badge" title="Mutual signed friendship">
                                        "\u{1f91d} Friends"
                                    </span>
                                }.into_view(),
                                Status::OfferedByThem => view! {
                                    <span class="rh-friend-badge pending" title="They signed \u{2014} accept to confirm">
                                        "wants to be friends"
                                    </span>
                                }.into_view(),
                                Status::OfferedByMe => view! {
                                    <span class="rh-friend-badge pending" title="You signed \u{2014} awaiting their signature">
                                        "offer sent"
                                    </span>
                                }.into_view(),
                                Status::None => ().into_view(),
                            }}
                        </div>
                        {move || peer_key().map(|k| {
                            let fp = crate::identity::short_fingerprint(&k);
                            view! {
                                <div class="rh-person-hero-key" title=k><span class="rh-inline-icon" inner_html=crate::icons::key_icon()></span>{fp}</div>
                            }
                        })}
                        <div class="rh-person-hero-presence">
                            {move || match live() {
                                Some(p) => format!(
                                    "online now \u{00b7} {}",
                                    p.servers.join(" \u{00b7} ")
                                ),
                                None => "not connected right now".to_string(),
                            }}
                        </div>
                    </div>
                    <div class="rh-person-actions">
                        <Show when=move || { is_me() } fallback=|| ()>
                            <span class="rh-friend-badge pending">"this is you"</span>
                        </Show>
                        <Show when=move || { !is_me() } fallback=|| ()>
                            <button
                                class="rh-btn"
                                prop:disabled=move || { handle_here().is_empty() || !app.online() }
                                on:click=move |ev| go_dm.call(ev)
                            >
                                "Message"
                            </button>
                        </Show>
                        {move || peer_key().filter(|_| !is_me()).map(|_| {
                            let (label, disabled) = match status() {
                                Status::Mutual => ("Friends", true),
                                Status::OfferedByThem => ("Accept friendship", false),
                                Status::OfferedByMe => ("Offer sent", true),
                                Status::None => ("Add friend", false),
                            };
                            view! {
                                <button class="rh-btn ghost" prop:disabled=disabled on:click=befriend>
                                    {label}
                                </button>
                            }
                        })}
                    </div>
                </header>

                <h2 class="rh-person-h2">"Known from"</h2>
                <Show
                    when=move || trail().map(|t| !t.burrows.is_empty()).unwrap_or(false)
                    fallback=move || view! {
                        <p class="rh-empty">
                            "No shared burrows on record yet \u{2014} a place appears here once you've both been seen on it."
                        </p>
                    }
                >
                    <ul class="rh-known-from">
                        <For
                            each=move || trail().map(|t| t.burrows).unwrap_or_default()
                            key=|b| b.endpoint.clone()
                            children=move |b| {
                                let ago = crate::files::relative_day(
                                    b.last_seen_unix_ms / 1000,
                                    crate::clock::now_ms() / 1000,
                                );
                                let here_now = live()
                                    .map(|p| p.servers.contains(&b.burrow_name))
                                    .unwrap_or(false);
                                let dot = if here_now { "rh-pres on" } else { "rh-pres off" };
                                view! {
                                    <li class="rh-known-row">
                                        <span class=dot aria-hidden="true"></span>
                                        <span class="rh-known-name">{b.burrow_name}</span>
                                        <span class="rh-known-handle">"as @"{b.handle}</span>
                                        <span class="rh-known-when">
                                            {if here_now { "here now".to_string() } else { ago }}
                                        </span>
                                    </li>
                                }
                            }
                        />
                    </ul>
                </Show>

                <PersonConversation handle=Signal::derive(handle_here)/>
                <PersonFiles handle=Signal::derive(handle_here)/>
            </section>
        </main>
    }
}

/// The recent DM exchange with a person on the focused burrow. A preview: the
/// full conversation lives in DMs, one click away via Message.
#[component]
fn PersonConversation(handle: Signal<String>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let thread = move || app.dm_with(&handle.get());
    view! {
        <Show when=move || thread().map(|t| !t.messages.is_empty()).unwrap_or(false) fallback=|| ()>
            <h2 class="rh-person-h2">"Recent messages"</h2>
            <ul class="rh-person-dm">
                <For
                    each=move || {
                        let msgs = thread().map(|t| t.messages).unwrap_or_default();
                        let start = msgs.len().saturating_sub(4);
                        msgs[start..].iter().cloned().enumerate().collect::<Vec<_>>()
                    }
                    key=|(i, m)| (*i, m.at_unix_ms, m.text.clone())
                    children=move |(_, m)| view! {
                        <li class="rh-person-dm-row">
                            <span class="rh-person-dm-from">{m.from}</span>
                            <span
                                class="rh-rich rh-person-dm-text"
                                inner_html=crate::markdown::inline_to_html(&m.text)
                            ></span>
                        </li>
                    }
                />
            </ul>
        </Show>
    }
}

/// Files a person has uploaded on the focused burrow — what they've offered,
/// each row opening its card in the file browser.
#[component]
fn PersonFiles(handle: Signal<String>) -> impl IntoView {
    use crate::files::{human_size, KIND_FOLDER};
    let app = expect_context::<AppState>();
    let files = move || app.files_by(&handle.get());
    view! {
        <Show when=move || !files().is_empty() fallback=|| ()>
            <h2 class="rh-person-h2">"Shared files"</h2>
            <ul class="rh-tree rh-person-files">
                <For
                    each=files
                    key=|n| (n.id, n.name.clone(), n.size, n.comment.clone())
                    children=move |n| {
                        let is_folder = n.kind == KIND_FOLDER;
                        let size = if is_folder {
                            "\u{2014}".to_string()
                        } else {
                            human_size(n.size)
                        };
                        let id = n.id;
                        let icon = crate::icons::file_icon(is_folder);
                        view! {
                            <li class="rh-tree-item">
                                <button class="rh-file-link" on:click=move |_| app.select_file(id)>
                                    <span class="rh-file-icon" aria-hidden="true" inner_html=icon></span>
                                    <span class="rh-file-name">{n.name}</span>
                                    <span class="rh-fcol-size">{size}</span>
                                </button>
                            </li>
                        }
                    }
                />
            </ul>
        </Show>
    }
}

/// The unified **Transfers** manager: every download and upload across *all*
/// your connected burrows in one place. (The content-addressed swarming source
/// roster is a later slice; this is the aggregated list.)
#[component]
pub fn Transfers() -> impl IntoView {
    use crate::files::{TransferDir, TransferStatus};
    let app = expect_context::<AppState>();
    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Transfers"</h1>
            <section class="rh-panel">
                <h2 class="rh-panel-title">"Transfers"<span class="rh-panel-sub">"across your burrows"</span></h2>
                <Show
                    when=move || !app.all_transfers().is_empty()
                    fallback=|| view! {
                        <EmptyState
                            icon="/transfers"
                            title="No transfers yet"
                            sub="Downloads and uploads from every burrow land here."
                        />
                    }
                >
                    <ul class="rh-xfers">
                        <For
                            each=move || app.all_transfers()
                            key=|(burrow, t)| (burrow.clone(), t.id, t.done, t.status, t.error.clone())
                            children=move |(burrow, t)| {
                                let pct = if let Some(pct) = t.done.min(t.total).saturating_mul(100)
                                    .checked_div(t.total)
                                {
                                    pct as u32
                                } else if matches!(t.status, TransferStatus::Done) {
                                    100
                                } else {
                                    0
                                };
                                let cancelled = t.error.as_deref() == Some(crate::upload::CANCELLED);
                                let (status_cls, status_txt) = match t.status {
                                    TransferStatus::Queued => ("rh-badge", "Queued"),
                                    TransferStatus::Active => ("rh-badge active", "Active"),
                                    TransferStatus::Done => ("rh-badge done", "Done"),
                                    TransferStatus::Failed if cancelled => ("rh-badge", "Cancelled"),
                                    TransferStatus::Failed => ("rh-badge failed", "Failed"),
                                };
                                let fill = if matches!(t.status, TransferStatus::Failed) {
                                    "rh-bar-fill failed"
                                } else {
                                    "rh-bar-fill"
                                };
                                let arrow = match t.dir {
                                    TransferDir::Download => crate::icons::arrow_down_icon(),
                                    TransferDir::Upload => crate::icons::arrow_up_icon(),
                                };
                                // Content id (blake3), the swarm de-dup key, when known.
                                let hash = t.hash.as_ref().map(|h| {
                                    if h.len() >= 12 {
                                        format!("blake3 {}\u{2026}{}", &h[..8], &h[h.len() - 4..])
                                    } else {
                                        format!("blake3 {h}")
                                    }
                                });
                                // The reason, when there is one — the whole
                                // point of the row for a failed transfer.
                                let cancel = (t.dir == TransferDir::Upload
                                    && (t.status == TransferStatus::Queued
                                        || (t.status == TransferStatus::Active && t.done < t.total)))
                                    .then_some(t.id);
                                let upload = t.dir == TransferDir::Upload;
                                let failure = matches!(t.status, TransferStatus::Failed).then(|| {
                                    (
                                        t.error.clone().unwrap_or_else(|| {
                                            "The transfer stopped without saying why.".to_string()
                                        }),
                                        t.retryable && t.node_id.is_some(),
                                        t.id,
                                    )
                                });
                                view! {
                                    <li class="rh-xfer-item">
                                        <div class="rh-xfer-row">
                                            <span class="rh-xfer-dir" aria-hidden="true" inner_html=arrow></span>
                                            <span class="rh-xfer-name">{t.name}</span>
                                            <span class="rh-xfer-burrow">{burrow.clone()}</span>
                                            <div class="rh-bar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow=pct.to_string()>
                                                <div class=fill style=format!("transform:scaleX({})", f64::from(pct) / 100.0)></div>
                                            </div>
                                            <span class="rh-xfer-pct">{format!("{pct}%")}</span>
                                            <span class=status_cls>{status_txt}</span>
                                        </div>
                                        <div class="rh-xfer-detail">
                                            {hash.map(|h| view! { <span class="rh-xfer-hash">{h}</span> })}
                                            // What this transfer actually used.
                                            // A real count when the swarm
                                            // reported one; the honest
                                            // single-source note otherwise.
                                            {match t.sources {
                                                _ if upload => view! {
                                                    <span class="rh-swarmpill" title="Where this upload is going">
                                                        {format!("to {burrow}")}
                                                    </span>
                                                }.into_view(),
                                                Some(n) => view! {
                                                    <span class="rh-swarmpill" title="Sources this download pulled from">
                                                        {format!(
                                                            "{n} source{} \u{00b7} {burrow}",
                                                            if n == 1 { "" } else { "s" },
                                                        )}
                                                    </span>
                                                }.into_view(),
                                                None => view! {
                                                    <span class="rh-swarmpill" title="Multi-source swarming is native-only; this download has one source for now.">
                                                        {format!("1 source \u{00b7} {burrow}")}
                                                    </span>
                                                }.into_view(),
                                            }}
                                            {cancel.map(|key| view! {
                                                <button
                                                    type="button"
                                                    class="rh-btn ghost small"
                                                    on:click=move |_| app.cancel_upload(key)
                                                >
                                                    "Cancel"
                                                </button>
                                            })}
                                        </div>
                                        // A failed transfer says WHY, and
                                        // offers to try again when trying
                                        // again could plausibly work.
                                        {failure.map(|(why, retryable, id)| view! {
                                            <div class="rh-xfer-error">
                                                <span class="rh-xfer-why">{why}</span>
                                                <Show when=move || { retryable } fallback=|| ()>
                                                    <button
                                                        class="rh-btn ghost small"
                                                        on:click=move |_| app.retry_transfer(id)
                                                    >"Try again"</button>
                                                </Show>
                                            </div>
                                        })}
                                    </li>
                                }
                            }
                        />
                    </ul>
                </Show>
            </section>
        </main>
    }
}

/// The burrow's **front page** — the operator-curated welcome screen the server
/// has always been able to send (`WelcomeScreen`) and the client never asked
/// for. Hotline's server news page: who's on, what's featured, the ticker.
#[component]
fn FrontPage() -> impl IntoView {
    use rabbithole_proto::welcome::WelcomeWidget;
    let app = expect_context::<AppState>();
    let widgets = move || app.focused_tracked().state.with(|s| s.front_page.clone());
    view! {
        <Show when=move || !widgets().is_empty() fallback=|| ()>
            <section class="rh-front" aria-label="Burrow news">
                <header class="rh-front-head">
                    <span class="rh-front-eyebrow">"News"</span>
                    <span class="rh-front-where">
                        {move || app.focused().name.get().unwrap_or_else(|| "this burrow".into())}
                    </span>
                </header>
                {move || {
                    widgets()
                        .into_iter()
                        .map(|w| match w {
                            WelcomeWidget::Featured { title, body } => view! {
                                <div class="rh-front-featured">
                                    <span class="rh-front-label">"Featured"</span>
                                    <p class="rh-front-title">{title}</p>
                                    <p class="rh-front-body">{body}</p>
                                </div>
                            }
                            .into_view(),
                            WelcomeWidget::OnlineNow { count, sample } => {
                                let who = sample.join(", ");
                                let line = if who.is_empty() {
                                    format!("{count} here now")
                                } else {
                                    format!("{count} here now \u{2014} {who}")
                                };
                                view! { <p class="rh-front-line"><span class="rh-pres on rh-inline-dot" aria-hidden="true"></span>{line}</p> }.into_view()
                            }
                            WelcomeWidget::UnreadDms(n) => view! {
                                <p class="rh-front-line">
                                    <span class="rh-inline-icon" inner_html=crate::icons::section_icon("/dms")></span>
                                    {format!("{n} conversation{} waiting", if n == 1 { "" } else { "s" })}
                                </p>
                            }
                            .into_view(),
                            WelcomeWidget::Ticker(text) => view! {
                                <p class="rh-front-ticker">{text}</p>
                            }
                            .into_view(),
                            // The sheet shows this too, on arrival — but the
                            // sheet is dismissible and this panel is not, so the
                            // MOTD has to live here to survive the dismissal.
                            WelcomeWidget::Motd(text) => view! {
                                <p class="rh-front-motd">{text}</p>
                            }
                            .into_view(),
                            _ => ().into_view(),
                        })
                        .collect_view()
                }}
            </section>
        </Show>
    }
}

/// A non-modal **welcome sheet**: the focused burrow's message of the day and,
/// when the server gates participation, its agreement to accept. Slides in on
/// connect and dismisses without blocking the rest of the app.
#[component]
pub fn WelcomeSheet() -> impl IntoView {
    let app = expect_context::<AppState>();
    // Reactive over the focused session so switching burrows shows that burrow's
    // welcome (or nothing).
    // Only an *agreement* earns a sheet. The MOTD used to headline it too, but
    // the news panel in the lobby now carries that permanently, so a sheet for
    // it would just say the same thing twice and ask you to dismiss it.
    let welcome = move || {
        app.focused_tracked()
            .state
            .with(|s| s.welcome.clone())
            .filter(|w| w.agreement.is_some())
    };
    let dismiss = move |_| {
        app.focused().state.update(|s| s.dismiss_welcome());
    };
    view! {
        <Show when=move || welcome().is_some() fallback=|| ()>
            {move || welcome().map(|w| {
                let has_agreement = w.agreement.is_some();
                let body = w.agreement.clone().unwrap_or_else(|| w.motd.clone());
                let name = app.focused().name.get().unwrap_or_else(|| "this burrow".into());
                view! {
                    <aside class="rh-welcome" role="region" aria-label="Welcome">
                        <div class="rh-welcome-head">
                            <span class="rh-welcome-title">
                                {if has_agreement { "Before you enter".to_string() }
                                 else { format!("Welcome to {name}") }}
                            </span>
                            <button
                                class="rh-welcome-x"
                                aria-label="Dismiss"
                                on:click=dismiss
                            ><span inner_html=crate::icons::close_icon()></span></button>
                        </div>
                        // Agreement servers show the MOTD above the agreement text.
                        {has_agreement.then(|| {
                            let motd = w.motd.clone();
                            (!motd.trim().is_empty()).then(|| view! {
                                <p class="rh-welcome-motd">{motd}</p>
                            })
                        })}
                        <p class="rh-welcome-body">{body}</p>
                        <div class="rh-welcome-actions">
                            <button class="rh-btn" on:click=dismiss>
                                {if has_agreement { "Accept & enter" } else { "Got it" }}
                            </button>
                        </div>
                    </aside>
                }
            })}
        </Show>
    }
}

/// The **You** hub: your portable Ed25519 identity — the key that names you
/// across every burrow, independent of your per-server handle.
#[component]
pub fn You() -> impl IntoView {
    let app = expect_context::<AppState>();
    let picking = create_rw_signal(false);
    let restoring = create_rw_signal(false);
    let restore_text = create_rw_signal(String::new());
    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"You"</h1>
            <section class="rh-panel">
                <Show
                    when=move || app.you.get().is_some()
                    fallback=|| view! {
                        <p class="rh-empty">"No local identity yet (browser storage unavailable)."</p>
                    }
                >
                    {move || app.you.get().map(|you| {
                        let fp = you.fingerprint.clone();
                        let pk = you.public_hex.clone();
                        let pk_copy = pk.clone();
                        let fp_copy = fp.clone();
                        view! {
                            // The hero: your mark, big, with the two things
                            // that actually identify you underneath.
                            <header class="rh-you-hero">
                                <div class="rh-you-avatar">
                                    <span
                                        class="rh-mark"
                                        inner_html=move || app.my_mark_svg(80)
                                    ></span>
                                    <button
                                        class="rh-btn ghost small"
                                        on:click=move |_| picking.update(|p| *p = !*p)
                                    >
                                        {move || if picking.get() { "Done" } else { "Change mark" }}
                                    </button>
                                </div>
                                <div class="rh-you-ident">
                                    <div class="rh-you-fp-line">
                                        <span class="rh-you-eyebrow">"Fingerprint"</span>
                                        <code class="rh-you-fp">{fp.clone()}</code>
                                        <button
                                            class="rh-btn ghost small"
                                            title="Copy fingerprint"
                                            on:click=move |_| copy_text(&fp_copy, app)
                                        >"Copy"</button>
                                    </div>
                                    <p class="rh-you-lead">
                                        "Read this aloud to a friend to check you're both \
                                         talking about the same key. Sixteen characters is \
                                         short enough to say and long enough that a collision \
                                         isn't a practical worry."
                                    </p>
                                    <details class="rh-you-key">
                                        <summary>"Public key"</summary>
                                        <code class="rh-you-pub">{pk.clone()}</code>
                                        <button
                                            class="rh-btn ghost small"
                                            on:click=move |_| copy_text(&pk_copy, app)
                                        >"Copy"</button>
                                    </details>
                                </div>
                            </header>

                            // The picker, only when asked for.
                            <Show when=move || picking.get() fallback=|| ()>
                                <h3 class="rh-person-h2">"Your mark"</h3>
                                <p class="rh-settings-note">
                                    "Pick a face and a colour, or keep the one your key draws. \
                                     This is local for now \u{2014} the wire carries no mark, so \
                                     other people still see the one your identity derives."
                                </p>
                                <div class="rh-mark-picker">
                                    {(0..crate::avatar::GLYPH_COUNT).map(|g| {
                                        view! {
                                            <button
                                                class="rh-mark-choice"
                                                class:on=move || {
                                                    app.my_mark.get().map(|m| m.glyph) == Some(g)
                                                }
                                                title=crate::avatar::glyph_name(g)
                                                aria-label=crate::avatar::glyph_name(g)
                                                on:click=move |_| {
                                                    let color = app
                                                        .my_mark
                                                        .get_untracked()
                                                        .map(|m| m.color)
                                                        .unwrap_or(0);
                                                    app.set_my_mark(Some(crate::avatar::ChosenMark {
                                                        glyph: g,
                                                        color,
                                                    }));
                                                }
                                                inner_html=move || {
                                                    let c = app
                                                        .my_mark
                                                        .get()
                                                        .map(|m| m.color)
                                                        .unwrap_or(0);
                                                    crate::avatar::glyph_svg(g, c, 32)
                                                }
                                            ></button>
                                        }
                                    }).collect_view()}
                                </div>
                                <div class="rh-mark-colors">
                                    {(0..crate::avatar::PALETTE.len()).map(|c| {
                                        view! {
                                            <button
                                                class="rh-mark-color"
                                                class:on=move || {
                                                    app.my_mark.get().map(|m| m.color) == Some(c)
                                                }
                                                aria-label=format!("Colour {}", c + 1)
                                                style=format!(
                                                    "background:{}",
                                                    crate::avatar::PALETTE[c],
                                                )
                                                on:click=move |_| {
                                                    let glyph = app
                                                        .my_mark
                                                        .get_untracked()
                                                        .map(|m| m.glyph)
                                                        .unwrap_or(0);
                                                    app.set_my_mark(Some(crate::avatar::ChosenMark {
                                                        glyph,
                                                        color: c,
                                                    }));
                                                }
                                            ></button>
                                        }
                                    }).collect_view()}
                                    <button
                                        class="rh-btn ghost small"
                                        on:click=move |_| app.set_my_mark(None)
                                    >"Use my key's mark"</button>
                                </div>
                            </Show>

                            // What the key is for, in plain sections rather
                            // than one intimidating paragraph.
                            <h3 class="rh-person-h2">"Backup and restore"</h3>
                            <p class="rh-settings-note">
                                "Your key lives only on this machine. Back it up and you can be \
                                 the same person on another device \u{2014} or after a reinstall. \
                                 Lose it with no backup and that identity is gone: nobody can \
                                 reissue it, which is the point of it being yours."
                            </p>
                            <div class="rh-you-backup">
                                <button class="rh-btn" on:click=move |_| backup_identity(app)>
                                    "Back up identity\u{2026}"
                                </button>
                                <button
                                    class="rh-btn ghost"
                                    on:click=move |_| restoring.update(|r| *r = !*r)
                                >
                                    {move || if restoring.get() { "Cancel" } else { "Restore\u{2026}" }}
                                </button>
                            </div>
                            <Show when=move || restoring.get() fallback=|| ()>
                                <div class="rh-restore">
                                    <p class="rh-restore-warn">
                                        "Restoring replaces the identity on this machine. Anything \
                                         signed by the current key \u{2014} your friendships \u{2014} \
                                         stops applying, so back this one up first if you might \
                                         want it back."
                                    </p>
                                    <textarea
                                        class="rh-input rh-restore-text"
                                        placeholder="Paste the contents of your identity backup file\u{2026}"
                                        aria-label="Identity backup contents"
                                        prop:value=restore_text
                                        on:input=move |ev| restore_text.set(event_target_value(&ev))
                                    ></textarea>
                                    <button
                                        class="rh-btn"
                                        prop:disabled=move || restore_text.get().trim().is_empty()
                                        on:click=move |_| {
                                            match app.restore_identity(&restore_text.get()) {
                                                Ok(fp) => {
                                                    restore_text.set(String::new());
                                                    restoring.set(false);
                                                    app.notify(
                                                        crate::toasts::ToastKind::Success,
                                                        format!("Restored identity {fp}."),
                                                    );
                                                }
                                                Err(why) => {
                                                    app.notify(crate::toasts::ToastKind::Warn, why);
                                                }
                                            }
                                        }
                                    >"Restore this identity"</button>
                                </div>
                            </Show>

                            <h3 class="rh-person-h2">"What this key does"</h3>
                            <dl class="rh-you-facts">
                                <div class="rh-you-fact">
                                    <dt>"It travels with you"</dt>
                                    <dd>
                                        "The same key names you on every burrow, so your \
                                         sightings coalesce even when your handle differs from \
                                         place to place."
                                    </dd>
                                </div>
                                <div class="rh-you-fact">
                                    <dt>"It proves possession"</dt>
                                    <dd>
                                        "At each handshake the burrow challenges you and you \
                                         sign a one-time nonce \u{2014} enough to stop someone \
                                         copying your public key out of a roster and wearing it."
                                    </dd>
                                </div>
                                <div class="rh-you-fact">
                                    <dt>"Relay-proof over QUIC only"</dt>
                                    <dd>
                                        "Over QUIC the burrow binds that signature to its own \
                                         TLS certificate, so a proof you give one burrow can't \
                                         be replayed to another. This client speaks WebSocket, \
                                         which has nothing to bind to \u{2014} here it proves \
                                         possession and nothing more."
                                    </dd>
                                </div>
                                <div class="rh-you-fact">
                                    <dt>"It is not a name"</dt>
                                    <dd>
                                        "A mark means \u{201c}this is probably them\u{201d}, not \
                                         a security guarantee. Check fingerprints out of band \
                                         when it matters."
                                    </dd>
                                </div>
                            </dl>
                        }
                    })}
                </Show>
            </section>
        </main>
    }
}

/// Download the identity backup as a file.
///
/// A file, not a clipboard copy: a private key in the clipboard is one paste
/// away from a chat window, and a backup you deliberately save is one you can
/// actually keep.
fn backup_identity(app: AppState) {
    let Some(doc) = app.identity_backup() else {
        app.notify(
            crate::toasts::ToastKind::Warn,
            "No identity to back up yet.".to_string(),
        );
        return;
    };
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let name = app
            .you
            .get_untracked()
            .map(|y| format!("rabbithole-identity-{}.json", y.fingerprint))
            .unwrap_or_else(|| "rabbithole-identity.json".to_string());
        let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(&doc));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/json");
        let saved = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)
            .ok()
            .and_then(|blob| web_sys::Url::create_object_url_with_blob(&blob).ok())
            .and_then(|url| {
                let document = web_sys::window()?.document()?;
                let a = document
                    .create_element("a")
                    .ok()?
                    .dyn_into::<web_sys::HtmlAnchorElement>()
                    .ok()?;
                a.set_href(&url);
                a.set_download(&name);
                a.click();
                let _ = web_sys::Url::revoke_object_url(&url);
                Some(())
            });
        app.notify(
            if saved.is_some() {
                crate::toasts::ToastKind::Success
            } else {
                crate::toasts::ToastKind::Warn
            },
            if saved.is_some() {
                "Saved your identity backup \u{2014} keep it somewhere only you can read."
                    .to_string()
            } else {
                "Couldn't save the backup file here.".to_string()
            },
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = doc;
}

/// Copy without a toast — for buttons that confirm in place (see the About
/// window's copy control). Best-effort: a context with no clipboard simply
/// does nothing, which the caller's own animation already implies.
pub(crate) fn copy_text_quiet(text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use js_sys::{Function, Reflect};
        use wasm_bindgen::{JsCast, JsValue};
        if let Some(w) = web_sys::window() {
            let nav = JsValue::from(w.navigator());
            let _ = Reflect::get(&nav, &"clipboard".into())
                .ok()
                .and_then(|clip| {
                    let f = Reflect::get(&clip, &"writeText".into())
                        .ok()?
                        .dyn_into::<Function>()
                        .ok()?;
                    f.call1(&clip, &JsValue::from_str(text)).ok()
                });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = text;
}

/// Copy text to the clipboard, with a toast either way — a Copy button that
/// silently does nothing is worse than no button.
fn copy_text(text: &str, app: AppState) {
    #[cfg(target_arch = "wasm32")]
    {
        // Reflect rather than web_sys::Clipboard: the crate builds without
        // that feature, and one call doesn't earn enabling it.
        use js_sys::{Function, Reflect};
        use wasm_bindgen::{JsCast, JsValue};
        let text = text.to_string();
        let copied = web_sys::window().and_then(|w| {
            let nav = JsValue::from(w.navigator());
            let clip = Reflect::get(&nav, &"clipboard".into()).ok()?;
            let write = Reflect::get(&clip, &"writeText".into())
                .ok()?
                .dyn_into::<Function>()
                .ok()?;
            write.call1(&clip, &JsValue::from_str(&text)).ok()
        });
        if copied.is_some() {
            app.notify(crate::toasts::ToastKind::Success, "Copied.".to_string());
            return;
        }
        app.notify(
            crate::toasts::ToastKind::Warn,
            "Clipboard unavailable here.".to_string(),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (text, app);
}

/// The user's presence status — a single control that fans the chosen status
/// (Online / Away / Invisible) to **every** connected burrow via
/// [`AppState::set_presence`]. Invisible is Cheshire mode.
///
/// A button and a listbox rather than a `<select>`: an `<option>` cannot carry
/// the status colour, and the native control's label sat visibly low in the
/// header's pill. Click or Return opens it, arrows move, Return or click
/// chooses, Escape or a click anywhere else closes it.
#[component]
fn PresenceControl() -> impl IntoView {
    use rabbithole_proto::presence::PresenceState;
    const CHOICES: [(PresenceState, &str, &str); 3] = [
        (PresenceState::Online, "Online", "rh-pres on"),
        (PresenceState::Away, "Away", "rh-pres away"),
        (PresenceState::Invisible, "Invisible", "rh-pres hidden"),
    ];
    let app = expect_context::<AppState>();
    let open = create_rw_signal(false);
    let current = move || {
        let now = app.presence.get();
        CHOICES
            .iter()
            .copied()
            .find(|(state, _, _)| *state == now)
            .unwrap_or(CHOICES[0])
    };
    // Open onto the current choice, the way a Mac pop-up button does.
    create_effect(move |_| {
        if open.get() {
            crate::a11y::focus_id("rh-presence-current");
        }
    });
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let away = window_event_listener(leptos::ev::mousedown, move |ev| {
            if !open.get_untracked() {
                return;
            }
            let inside = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .and_then(|el| el.closest(".rh-presence-wrap").ok().flatten())
                .is_some();
            if !inside {
                open.set(false);
            }
        });
        on_cleanup(move || away.remove());
    }
    view! {
        <div
            class="rh-presence-wrap"
            on:keydown=move |ev| {
                if ev.key() == "Escape" && open.get_untracked() {
                    ev.prevent_default();
                    open.set(false);
                    crate::a11y::focus_id("rh-presence-button");
                } else if ev.key() == "Tab" {
                    open.set(false);
                }
            }
        >
            <button
                type="button"
                id="rh-presence-button"
                class="rh-presence"
                aria-haspopup="listbox"
                aria-expanded=move || if open.get() { "true" } else { "false" }
                aria-label=move || format!("Your status: {}", current().1)
                on:click=move |_| open.update(|o| *o = !*o)
            >
                <span class=move || current().2 aria-hidden="true"></span>
                <span>{move || current().1}</span>
                <span class="rh-presence-chevron" aria-hidden="true" inner_html=crate::icons::chevron_down_icon()></span>
            </button>
            <Show when=move || open.get() fallback=|| ()>
                <ul
                    class="rh-menu"
                    role="listbox"
                    aria-label="Your status"
                    on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-menu-item")
                >
                    {CHOICES
                        .iter()
                        .copied()
                        .map(|(state, label, dot)| {
                            let chosen = move || app.presence.get() == state;
                            view! {
                                <li role="none">
                                    <button
                                        type="button"
                                        class="rh-menu-item"
                                        role="option"
                                        id=move || chosen().then_some("rh-presence-current")
                                        aria-selected=move || if chosen() { "true" } else { "false" }
                                        on:click=move |_| {
                                            app.set_presence(state);
                                            open.set(false);
                                            crate::a11y::focus_id("rh-presence-button");
                                        }
                                    >
                                        <span class=dot aria-hidden="true"></span>
                                        <span class="rh-menu-label">{label}</span>
                                        <span class="rh-menu-check" aria-hidden="true">
                                            {move || chosen().then(|| view! {
                                                <span inner_html=crate::icons::check_icon()></span>
                                            })}
                                        </span>
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            </Show>
        </div>
    }
}

/// The app's one confirmation dialog, for the things that can't be taken
/// back. `window.confirm()` is not an option: a desktop webview answers it
/// `false` without showing anything, which is how Leave came to do nothing in
/// the app. A modal is right here and nowhere else in this client: the choice
/// needs protected focus. Cancel is the default, as on a Mac; Escape and a
/// click outside cancel too.
#[component]
pub fn ConfirmDialog() -> impl IntoView {
    let app = expect_context::<AppState>();
    let ask = app.confirm;
    create_effect(move |_| {
        if ask.with(Option::is_some) {
            crate::a11y::focus_id("rh-confirm-cancel");
        }
    });
    view! {
        <Show when=move || ask.with(Option::is_some) fallback=|| ()>
            <div class="rh-confirm-backdrop" on:click=move |_| app.answer_confirm(false)>
                <div
                    class="rh-confirm"
                    role="alertdialog"
                    aria-modal="true"
                    aria-labelledby="rh-confirm-title"
                    aria-describedby="rh-confirm-body"
                    on:click=|ev| ev.stop_propagation()
                    on:keydown=move |ev| {
                        if ev.key() == "Escape" {
                            ev.prevent_default();
                            app.answer_confirm(false);
                        }
                    }
                >
                    <h2 id="rh-confirm-title">
                        {move || ask.with(|a| a.as_ref().map(|a| a.title.clone()))}
                    </h2>
                    <p id="rh-confirm-body">
                        {move || ask.with(|a| a.as_ref().map(|a| a.body.clone()))}
                    </p>
                    <div class="rh-confirm-actions">
                        <button
                            type="button"
                            id="rh-confirm-cancel"
                            class="rh-btn ghost"
                            on:click=move |_| app.answer_confirm(false)
                        >
                            "Cancel"
                        </button>
                        <button
                            type="button"
                            class="rh-btn danger"
                            on:click=move |_| app.answer_confirm(true)
                        >
                            {move || ask.with(|a| a.as_ref().map(|a| a.action.clone()))}
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// The header/status bar: server name, connection state, nav links, theme
/// toggle.
#[component]
pub fn StatusBar() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.focused().state;
    let title = move || {
        let name = state.with(|s| s.server_name.clone());
        if name.is_empty() {
            "RabbitHole".to_string()
        } else {
            name
        }
    };
    // The label says something only when there is something to say:
    // "Connecting…", "Reconnecting…", "Offline". Healthy is the dot alone;
    // spelling out "Online" next to a presence menu that also said "Online"
    // was the same word meaning two things in one row.
    let conn_label =
        move || (!state.with(|s| s.conn.is_live())).then(|| state.with(|s| s.conn.label()));
    let location = leptos_router::use_location();
    let in_burrow =
        move || crate::palette::scope_of(&location.pathname.get()) == crate::palette::Scope::Burrow;
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
    // A prominent connection banner when a live session isn't healthy.
    let banner = move || crate::conn::conn_banner(state.with(|s| s.conn), app.focused().live.get());
    // The connection label and status line are polite live regions so
    // transient states ("Connecting…", "Reconnecting…", command errors)
    // announce without stealing focus; the now-playing segment sits in an
    // always-mounted `role="status"` slot (collapsed via CSS when empty) so
    // track changes announce too. The dot is decorative — the label beside
    // it carries the state as text.
    view! {
        // `data-tauri-drag-region` lets the desktop window be dragged by the
        // header's empty surface; the attribute only fires when the header
        // itself is the mousedown target, so every control in it stays
        // clickable. Inert in a browser tab.
        <header class="rh-header" data-tauri-drag-region="true">
            <span class=dot_class aria-hidden="true"></span>
            <span class="rh-conn" role="status">{conn_label}</span>
            <span class="rh-title"><span class="rh-title-text">{title}</span></span>
            <span class="rh-spacer"></span>
            <span class="rh-live-slot" role="status">
                <Show when=move || radio.with(|r| r.on_air().is_some()) fallback=|| ()>
                    <A href="/radio" class="rh-radio-now">{now_playing}</A>
                </Show>
            </span>
            // Leaving is only meaningful for a burrow you actually joined,
            // and only while you're in it: on People or Transfers there is
            // nothing on screen to leave. It asks first.
            <Show when=move || { app.can_leave() && in_burrow() } fallback=|| ()>
                <button
                    class="rh-btn ghost rh-leave"
                    title="Leave this burrow"
                    on:click=move |_| app.ask_leave()
                >
                    "Leave"
                </button>
            </Show>
            <PresenceControl/>
        </header>
        <Show when=move || banner().is_some() fallback=|| ()>
            {move || banner().map(|b| view! {
                <div class=move || format!("rh-banner {}", b.tone) role="status">
                    <span class="rh-banner-text">{b.text}</span>
                    {b.action.map(|label| view! {
                        <button
                            type="button"
                            class="rh-btn ghost"
                            on:click=move |_| app.reconnect()
                        >
                            {label}
                        </button>
                    })}
                </div>
            })}
        </Show>
        <SectionStrip/>
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
    let list_ref = create_node_ref::<leptos::html::Ul>();
    // Keep the selection on screen. Arrowing below the fold moved the
    // highlight somewhere invisible — Spotlight and Raycast never let that
    // happen. Direct scrollTop math instead of scroll_into_view so only the
    // list moves, never the page, and instantly (right under reduced motion).
    #[cfg(target_arch = "wasm32")]
    create_effect(move |_| {
        let i = selected.get();
        let Some(list) = list_ref.get() else { return };
        let items = list.children();
        let Some(item) = items.item(i as u32) else {
            return;
        };
        use wasm_bindgen::JsCast;
        let Ok(item) = item.dyn_into::<web_sys::HtmlElement>() else {
            return;
        };
        let (top, height) = (item.offset_top(), item.offset_height());
        let (view_top, view_h) = (list.scroll_top(), list.client_height());
        if top < view_top {
            list.set_scroll_top(top);
        } else if top + height > view_top + view_h {
            list.set_scroll_top(top + height - view_h);
        }
    });
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

    // Global keys, wasm only: ⌘K / Ctrl-K toggles the palette, and ⌘1…⌘9 jump
    // straight to a section. The digit shortcuts are what a native app has and
    // a web page doesn't; they work from anywhere, including mid-message, since
    // no composer binds a modified digit.
    #[cfg(target_arch = "wasm32")]
    {
        let jump = use_navigate();
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if !(ev.meta_key() || ev.ctrl_key()) {
                return;
            }
            if ev.key().eq_ignore_ascii_case("k") {
                ev.prevent_default();
                open.update(|o| *o = !*o);
                return;
            }
            if let Some(section) = crate::palette::section_for_digit(&ev.key()) {
                ev.prevent_default();
                open.set(false);
                jump(section.route, Default::default());
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
                    <ul class="rh-palette-list" role="listbox" aria-label="Sections" node_ref=list_ref>
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
                            ><span inner_html=crate::icons::close_icon()></span></button>
                        </div>
                    }
                }
            />
        </div>
    }
}

/// What a row of the burrow browser needs from the window it sits in.
#[derive(Clone, Copy)]
struct Browsing {
    /// The selected place. On the connect window this *is* the form's address.
    endpoint: RwSignal<String>,
    /// The connect form's handle; a saved burrow brings its own.
    handle: RwSignal<String>,
    /// The burrows signed into before, so "Forget" can take one off the shelf.
    recent: RwSignal<Vec<crate::recent::RecentBurrow>>,
    /// Connect to the selected burrow now.
    on_open: Callback<()>,
    /// Standing alone (the in-app Looking Glass) there is no form beside the
    /// list, so a selected row carries its own Connect button.
    standalone: bool,
}

/// One line of the burrow browser.
///
/// A Mac list row, not a web card: one click (or arrowing onto it) selects,
/// which puts the burrow's address in the form; a double-click or Return
/// connects. The selected row opens up to show the whole description, the
/// address, and what can be done with it (bookmark, rename, forget), so the
/// list stays dense and nothing is hidden for good.
#[component]
fn GlassRow(row: crate::connect::Row, browsing: Browsing) -> impl IntoView {
    use crate::connect::Shelf;
    let app = expect_context::<AppState>();
    let Browsing {
        endpoint,
        handle,
        recent,
        on_open,
        standalone,
    } = browsing;
    let place = store_value(row.endpoint.clone());
    let name = store_value(row.name.clone());
    let saved_handle = store_value(match &row.shelf {
        Shelf::Yours { handle } => Some(handle.clone()),
        Shelf::Bookmark { handle } => handle.clone(),
        _ => None,
    });
    let selected =
        move || place.with_value(|p| endpoint.with(|e| crate::connect::same_place(e, p)));
    let pick = move || {
        if !selected() {
            endpoint.set(place.get_value());
        }
        if let Some(h) = saved_handle.get_value() {
            if handle.with_untracked(|now| *now != h) {
                handle.set(h);
            }
        }
    };
    // Each burrow gets a face, the way a Hotline tracker row had its icon:
    // the same pixel mark people wear, grown from the burrow's address.
    let mark = crate::avatar::mark_svg(
        &crate::avatar::seed_for(None, &crate::connect::host(&row.endpoint)),
        22,
    );
    let down = row.reachable == Some(false);
    let pip = row
        .reachable
        .map(|up| if up { "rh-dot on" } else { "rh-dot off" });
    let presence = row
        .reachable
        .map(|up| if up { "Online." } else { "Offline." });
    // A burrow nobody can reach has no one online *now*, whatever it last said.
    let users = if down { None } else { row.users };
    let uptime = row.uptime.map(|p| format!("{}%", p.min(100)));
    let as_handle = saved_handle.get_value();
    let description = row.description.clone();
    let address = row.endpoint.clone();
    let listeners = row.listeners.join(" \u{00b7} ");
    let shelf = store_value(row.shelf.clone());
    let renaming = create_rw_signal(false);
    let new_name = create_rw_signal(row.name.clone());
    let bookmark = move |_| {
        if let Err(why) = place.with_value(|p| name.with_value(|n| app.add_bookmark(p, n))) {
            app.notify(crate::toasts::ToastKind::Warn, why.message().to_string());
        }
    };
    let forget = move |_| {
        place.with_value(|p| {
            #[cfg(target_arch = "wasm32")]
            crate::recent::forget(p);
            recent.update(|l| l.retain(|r| !crate::connect::same_place(&r.endpoint, p)));
        });
    };
    view! {
        <li class="rh-glass-item" class:selected=selected>
            <button
                type="button"
                class="rh-glass-row"
                class:off=down
                tabindex="-1"
                aria-pressed=move || if selected() { "true" } else { "false" }
                on:click=move |_| pick()
                on:focus=move |_| pick()
                on:dblclick=move |_| {
                    pick();
                    on_open.call(());
                }
                on:keydown=move |ev| {
                    if ev.key() == "Enter" {
                        ev.prevent_default();
                        pick();
                        on_open.call(());
                    }
                }
            >
                <span class="rh-glass-mark">
                    <span inner_html=mark></span>
                    {pip.map(|class| view! { <span class=class aria-hidden="true"></span> })}
                </span>
                <span class="rh-glass-name">{row.name.clone()}</span>
                <span class="rh-glass-desc">
                    {presence.map(|p| view! { <span class="rh-visually-hidden">{p}" "</span> })}
                    {as_handle.map(|h| view! { <span class="rh-glass-as">"as "{h}</span> })}
                    {description}
                </span>
                <span class="rh-glass-users">
                    {if down {
                        view! { <span class="rh-glass-down">"offline"</span> }.into_view()
                    } else {
                        users
                            .map(|n| view! {
                                {n}<span class="rh-visually-hidden">" online"</span>
                            })
                            .into_view()
                    }}
                </span>
                <span class="rh-glass-uptime">
                    {uptime.map(|u| view! {
                        {u}<span class="rh-visually-hidden">" uptime"</span>
                    })}
                </span>
            </button>
            // The opened row. A sibling of the button, not inside it, so its
            // controls are real controls.
            <Show when=selected fallback=|| ()>
                <div class="rh-glass-detail">
                    <p class="rh-glass-more">
                        <code>{address.clone()}</code>
                        {(!listeners.is_empty()).then(|| view! { <span>{listeners.clone()}</span> })}
                        {(shelf.get_value() == Shelf::Demo).then(|| view! { <span>"seeded demo"</span> })}
                    </p>
                    <Show
                        when=move || renaming.get()
                        fallback=move || view! {
                            <div class="rh-glass-actions">
                                {standalone.then(|| view! {
                                    <button
                                        type="button"
                                        class="rh-btn small"
                                        on:click=move |_| on_open.call(())
                                    >
                                        "Connect\u{2026}"
                                    </button>
                                })}
                                {match shelf.get_value() {
                                    Shelf::Bookmark { .. } => view! {
                                        <button
                                            type="button"
                                            class="rh-btn ghost small"
                                            on:click=move |_| {
                                                new_name.set(name.get_value());
                                                renaming.set(true);
                                            }
                                        >
                                            "Rename"
                                        </button>
                                        <button
                                            type="button"
                                            class="rh-btn ghost small"
                                            on:click=move |_| place.with_value(|p| app.remove_bookmark(p))
                                        >
                                            "Remove bookmark"
                                        </button>
                                    }
                                    .into_view(),
                                    Shelf::Yours { .. } => view! {
                                        <button type="button" class="rh-btn ghost small" on:click=bookmark>
                                            "Bookmark"
                                        </button>
                                        <button
                                            type="button"
                                            class="rh-btn ghost small"
                                            title="Take it off this list and drop its saved session"
                                            on:click=forget
                                        >
                                            "Forget"
                                        </button>
                                    }
                                    .into_view(),
                                    Shelf::Listed => view! {
                                        <button type="button" class="rh-btn ghost small" on:click=bookmark>
                                            "Bookmark"
                                        </button>
                                    }
                                    .into_view(),
                                    Shelf::Demo => ().into_view(),
                                }}
                            </div>
                        }
                    >
                        <form
                            class="rh-glass-rename"
                            on:submit=move |ev: leptos::ev::SubmitEvent| {
                                ev.prevent_default();
                                place.with_value(|p| app.rename_bookmark(p, &new_name.get_untracked()));
                                renaming.set(false);
                            }
                        >
                            <input
                                class="rh-input"
                                aria-label="Bookmark name"
                                prop:value=new_name
                                on:input=move |ev| new_name.set(event_target_value(&ev))
                            />
                            <button type="submit" class="rh-btn small">"Save"</button>
                            <button
                                type="button"
                                class="rh-btn ghost small"
                                on:click=move |_| renaming.set(false)
                            >
                                "Cancel"
                            </button>
                        </form>
                    </Show>
                </div>
            </Show>
        </li>
    }
}

/// One shelf of the browser: a title and its rows, or nothing when it is empty.
#[component]
fn GlassShelf(
    title: &'static str,
    #[prop(into)] rows: Signal<Vec<crate::connect::Row>>,
    browsing: Browsing,
) -> impl IntoView {
    view! {
        <Show when=move || rows.with(|r| !r.is_empty()) fallback=|| ()>
            <h3 class="rh-glass-group">{title}</h3>
            <ul class="rh-glass-list" aria-label=title>
                <For
                    each=move || rows.get()
                    key=|r| r.key()
                    children=move |row| view! { <GlassRow row browsing/> }
                />
            </ul>
        </Show>
    }
}

/// The burrow browser: Hotline's tracker window and its bookmarks, as one
/// list (design spec §3). *Bookmarks* you chose to keep, *Recent* burrows you
/// have signed into, and *Discover*, what a live directory lists. One dense
/// list with a face per burrow, a filter, a way to add a bookmark by address,
/// and a status strip that says where the listing came from.
///
/// It fills in a form rather than owning one: the connect window puts it
/// beside the sign-in form, and the in-app Looking Glass stands it alone.
#[component]
fn BurrowBrowser(
    endpoint: RwSignal<String>,
    handle: RwSignal<String>,
    recent: RwSignal<Vec<crate::recent::RecentBurrow>>,
    #[prop(into)] on_open: Callback<()>,
    #[prop(optional)] standalone: bool,
) -> impl IntoView {
    use crate::servers::DirectorySource;
    let app = expect_context::<AppState>();
    let browsing = Browsing {
        endpoint,
        handle,
        recent,
        on_open,
        standalone,
    };
    let listed = app.servers;
    let source = app.directory_source;
    let loading = app.directory_loading;
    let bookmarks = app.bookmarks;
    let probes = app.probes;
    // Ask the network on arrival: what is in memory may be the built-in
    // sample, which this list never shows as places. A render effect so a
    // route re-entry doesn't fire it against a disposed view.
    create_render_effect(move |seen: Option<bool>| {
        if seen.is_none() {
            app.load_directory();
        }
        true
    });
    // Knock on the places no directory vouches for, once each: when this
    // opens, when a listing arrives, when a bookmark is added. Probes are read
    // untracked so a settling knock doesn't ask for another.
    let knock_unknown = move |again: bool| {
        let wanted = bookmarks.with(|b| {
            recent.with(|r| listed.with(|l| source.with(|s| crate::connect::to_knock(b, r, l, s))))
        });
        let wanted = if again {
            wanted
        } else {
            probes.with_untracked(|known| {
                wanted
                    .into_iter()
                    .filter(|e| !known.contains_key(&crate::connect::probe_key(e)))
                    .collect()
            })
        };
        app.knock_on(wanted);
    };
    create_render_effect(move |_| knock_unknown(false));

    let filter_text = create_rw_signal(String::new());
    let kept = Signal::derive(move || {
        crate::connect::filter(
            bookmarks.with(|b| {
                recent.with(|r| {
                    listed.with(|l| probes.with(|p| crate::connect::bookmarked(b, r, l, p)))
                })
            }),
            &filter_text.get(),
        )
    });
    let yours = Signal::derive(move || {
        crate::connect::filter(
            bookmarks.with(|b| {
                recent.with(|r| listed.with(|l| probes.with(|p| crate::connect::yours(r, l, b, p))))
            }),
            &filter_text.get(),
        )
    });
    let discover = Signal::derive(move || {
        crate::connect::filter(
            bookmarks.with(|b| {
                recent.with(|r| {
                    listed.with(|l| source.with(|s| crate::connect::discover(l, s, r, b)))
                })
            }),
            &filter_text.get(),
        )
    });
    let demos =
        Signal::derive(move || crate::connect::filter(crate::connect::demos(), &filter_text.get()));
    let nothing_shown = move || {
        kept.with(Vec::is_empty)
            && yours.with(Vec::is_empty)
            && discover.with(Vec::is_empty)
            && demos.with(Vec::is_empty)
    };
    let unanswered = move || source.with(|s| *s == DirectorySource::Seeded);
    let filtering = move || !filter_text.with(|q| q.trim().is_empty());

    // Adding a bookmark by address: the burrow that isn't on any list.
    let adding = create_rw_signal(false);
    let add_name = create_rw_signal(String::new());
    let add_address = create_rw_signal(String::new());
    let add_error = create_rw_signal(None::<&'static str>);
    let open_add = move |_| {
        // Whatever address is already in hand is the likely one to keep.
        let current = endpoint.get_untracked();
        let fresh = !crate::connect::is_demo(&current)
            && !bookmarks.with_untracked(|b| crate::bookmarks::is_bookmarked(b, &current));
        add_address.set(if fresh { current } else { String::new() });
        add_name.set(String::new());
        add_error.set(None);
        adding.update(|a| *a = !*a);
        if adding.get_untracked() {
            crate::a11y::focus_id("rh-glass-add-address");
        }
    };
    let submit_add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        match app.add_bookmark(&add_address.get_untracked(), &add_name.get_untracked()) {
            Ok(()) => {
                endpoint.set(add_address.get_untracked().trim().to_string());
                adding.set(false);
            }
            Err(why) => add_error.set(Some(why.message())),
        }
    };

    view! {
        <section class="rh-connect-main" aria-label="Burrows">
            <header class="rh-glass-head" data-tauri-drag-region="true">
                <h2>"Looking Glass"</h2>
                // A <label>, so a click on the lens lands in the field.
                <label class="rh-glass-search">
                    <span inner_html=crate::icons::search_icon()></span>
                    <input
                        class="rh-input"
                        type="search"
                        aria-label="Filter burrows"
                        placeholder="Filter"
                        prop:value=filter_text
                        on:input=move |ev| filter_text.set(event_target_value(&ev))
                    />
                </label>
                <button
                    type="button"
                    class="rh-glass-tool"
                    aria-label="Add a bookmark by address"
                    title="Add a bookmark by address"
                    aria-expanded=move || if adding.get() { "true" } else { "false" }
                    on:click=open_add
                >
                    <span inner_html=crate::icons::plus_icon()></span>
                </button>
                <button
                    type="button"
                    class="rh-glass-tool rh-glass-refresh"
                    class:busy=move || loading.get()
                    aria-label="Refresh the list"
                    title="Refresh the list"
                    prop:disabled=move || loading.get()
                    on:click=move |_| {
                        app.load_directory();
                        knock_unknown(true);
                    }
                >
                    <span inner_html=crate::icons::refresh_icon()></span>
                </button>
            </header>
            <div class="rh-glass-cols" aria-hidden="true">
                <span></span>
                <span>"Burrow"</span>
                <span>"About"</span>
                <span class="num">"Online"</span>
                <span class="num uptime">"Uptime"</span>
            </div>
            <div
                class="rh-glass-scroll"
                tabindex="0"
                role="group"
                aria-label="Burrows to connect to. Arrow keys move, Return connects."
                on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-glass-row")
            >
                <Show when=move || adding.get() fallback=|| ()>
                    <form class="rh-glass-add" on:submit=submit_add>
                        <input
                            class="rh-input"
                            aria-label="Name for the bookmark (optional)"
                            placeholder="Name (optional)"
                            prop:value=add_name
                            on:input=move |ev| add_name.set(event_target_value(&ev))
                        />
                        <input
                            id="rh-glass-add-address"
                            class="rh-input rh-login-address"
                            aria-label="Burrow address"
                            autocapitalize="off"
                            spellcheck="false"
                            placeholder="wss://burrow.example"
                            prop:value=add_address
                            on:input=move |ev| {
                                add_error.set(None);
                                add_address.set(event_target_value(&ev));
                            }
                        />
                        <button type="submit" class="rh-btn small">"Add bookmark"</button>
                        <button
                            type="button"
                            class="rh-btn ghost small"
                            on:click=move |_| adding.set(false)
                        >
                            "Cancel"
                        </button>
                        {move || add_error.get().map(|why| view! {
                            <p class="rh-field-hint error" role="alert">{why}</p>
                        })}
                    </form>
                </Show>
                <GlassShelf title="Bookmarks" rows=kept browsing/>
                <GlassShelf title="Recent" rows=yours browsing/>
                <GlassShelf title="Demo burrows" rows=demos browsing/>
                <GlassShelf title="Discover" rows=discover browsing/>
                // The listing is on its way and there is nothing to show
                // yet: row-shaped placeholders, not an empty pane.
                <Show when=move || loading.get() && unanswered() && !filtering() fallback=|| ()>
                    <h3 class="rh-glass-group">"Discover"</h3>
                    <Skeleton rows=5/>
                </Show>
                // Nobody answered. Say so, and say what still works.
                <Show when=move || !loading.get() && unanswered() && !filtering() fallback=|| ()>
                    <EmptyState
                        icon="/servers"
                        title="No directory answered"
                        sub="The looking glass is how public burrows get found. You can still connect to any burrow by its address, and bookmark it."
                        action=view! {
                            <button
                                type="button"
                                class="rh-btn ghost small"
                                on:click=move |_| app.load_directory()
                            >
                                "Try again"
                            </button>
                        }.into_view()
                    />
                </Show>
                <Show when=move || filtering() && nothing_shown() fallback=|| ()>
                    <p class="rh-empty">"No burrow matches that filter."</p>
                </Show>
                <Show
                    when=move || {
                        !loading.get() && !unanswered() && !filtering() && discover.with(Vec::is_empty)
                    }
                    fallback=|| ()
                >
                    <p class="rh-empty">"No other burrows are listed right now."</p>
                </Show>
            </div>
            <footer class="rh-glass-status" role="status">
                <span>
                    {move || listed.with(|l| source.with(|s| {
                        crate::connect::status_line(loading.get(), s, l)
                    }))}
                </span>
                <Show when=move || !unanswered() fallback=|| ()>
                    <span class="rh-glass-via">
                        "via "{move || source.with(|s| s.label().to_string())}
                    </span>
                </Show>
            </footer>
        </section>
    }
}

/// The connect window.
///
/// Laid out like a Mac app's welcome window rather than a web sign-in card:
/// who this is and the way in on the left, the places you could go on the
/// right. The right side is Hotline's tracker window, reinterpreted (design
/// spec §3): *Your burrows* over *Discover*, one dense list with a face per
/// burrow, a filter, and a status strip that says where the listing came from.
///
/// Still a real `<form>` (Return submits from any field) with `<label for=…>`
/// on every input; the list only ever fills that form in.
#[component]
pub fn Login() -> impl IntoView {
    let app = expect_context::<AppState>();
    let navigate = use_navigate();
    // Reconnect-on-launch: the burrows you've signed into before (endpoint +
    // handle only). The most recent seeds the form; all of them are a shelf.
    #[cfg(target_arch = "wasm32")]
    let recent = crate::recent::load();
    #[cfg(not(target_arch = "wasm32"))]
    let recent: Vec<crate::recent::RecentBurrow> = Vec::new();
    let last = recent.first().cloned();
    let recent = create_rw_signal(recent);
    // Prefill the endpoint: the server browser's pick wins, else the last
    // burrow, else the local default. A burrow's WebSocket listener is 4654
    // by default (README, `ws_addr`). A dev build opens on its first seeded
    // burrow instead, so the demo is one Return away.
    let endpoint = create_rw_signal(
        app.pending_endpoint
            .get_untracked()
            .or_else(|| last.as_ref().map(|b| b.endpoint.clone()))
            .or_else(|| crate::connect::demos().first().map(|d| d.endpoint.clone()))
            .unwrap_or_else(|| "ws://localhost:4654".to_string()),
    );
    // The Looking Glass hands its pick over in the URL (`/?server=…`): a
    // signal set in one component and read by another during the same
    // navigation races the new component's first read.
    let query = leptos_router::use_query_map();
    let handle = create_rw_signal(last.as_ref().map(|b| b.handle.clone()).unwrap_or_default());
    create_render_effect(move |_| {
        if let Some(ep) = query.with(|q| q.get("server").cloned()) {
            if !ep.is_empty() {
                // A burrow you have been to brings the handle you used there.
                if let Some(h) = recent.with_untracked(|r| {
                    r.iter()
                        .find(|b| crate::connect::same_place(&b.endpoint, &ep))
                        .map(|b| b.handle.clone())
                }) {
                    handle.set(h);
                }
                endpoint.set(ep);
            }
        }
    });
    let password = create_rw_signal(String::new());
    let handle_missing = create_rw_signal(false);
    // Whether this is a seeded demo burrow is a fact about the address, so the
    // form asks no "real or demo?" question: a demo row fills in a `demo://`
    // address and everything else is a live connection.
    let is_demo = move || endpoint.with(|e| crate::connect::is_demo(e));

    // A `Callback` that OWNS the navigate function, not a closure over a
    // stored one. Connecting focuses the new burrow, which remounts the routed
    // tree and disposes this component mid-call: anything of ours held in a
    // `StoredValue` is gone by the time the next line runs (it panicked with
    // "could not get stored value" exactly there), while a value the running
    // closure owns outlives the scope that made it.
    let go = Callback::new(move |()| {
        app.pending_notice.set(None);
        app.pending_endpoint.set(None);
        let place = endpoint.get_untracked();
        if place.trim().is_empty() {
            crate::a11y::focus_id(a11y::LOGIN_SERVER_ID);
            return;
        }
        // A handle is what signs you in. Without one the session used to
        // connect and then sit there unauthenticated, and the button simply
        // did nothing: say what is missing, and go to it.
        let who = handle.get_untracked();
        if who.trim().is_empty() {
            handle_missing.set(true);
            crate::a11y::focus_id(a11y::LOGIN_HANDLE_ID);
            return;
        }
        if !crate::connect::is_demo(&place) {
            // Live: open a real socket + authenticate; state fills from
            // transport events (the handshake sets the header to Online, and
            // the lobby fills with live chat once signed in).
            app.connect_live(place, who, password.get_untracked());
            navigate("/lobby", Default::default());
            return;
        }
        // The demo path, dev builds only. Without the `demo` feature there is
        // no seeded burrow to join: a shipped build must never present
        // fabricated data as a place.
        #[cfg(feature = "demo")]
        {
            let demo = crate::client::DEMO_BURROWS
                .iter()
                .find(|d| d.endpoint == place)
                .unwrap_or(&crate::client::DEMO_BURROWS[0]);
            app.join_demo(demo, &who);
            app.notify(
                crate::toasts::ToastKind::Success,
                format!("Signed in as {who}"),
            );
            let waiting = app.focused().state.with(|s| s.dm_threads.len());
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
        }
        #[cfg(not(feature = "demo"))]
        app.notify(
            crate::toasts::ToastKind::Warn,
            "That is a demo address, and this build has no demo burrows.".to_string(),
        );
    });
    let on_open = go;

    // The button names where it is going when the address is a known place.
    let go_label = move || {
        let known = app.bookmarks.with(|b| {
            recent.with(|r| {
                app.servers.with(|l| {
                    app.directory_source.with(|src| {
                        let none = crate::connect::Probes::new();
                        let mut rows = crate::connect::bookmarked(b, r, l, &none);
                        rows.extend(crate::connect::yours(r, l, b, &none));
                        rows.extend(crate::connect::discover(l, src, r, b));
                        rows.extend(crate::connect::demos());
                        rows
                    })
                })
            })
        });
        endpoint.with(|e| crate::connect::connect_label(e, &known))
    };

    view! {
        <main class="rh-connect" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-connect-side" aria-label="Sign in">
                <div class="rh-connect-brand">
                    <img
                        class="rh-connect-logo"
                        src="/logo.png"
                        alt=""
                        width="112"
                        height="112"
                        draggable="false"
                    />
                    <h1 id=a11y::VIEW_TITLE_ID tabindex="-1">"RabbitHole"</h1>
                    <p class="rh-connect-tagline">"Many burrows, one you."</p>
                </div>
                // Why you're here, when you didn't choose to be: an expired
                // session or a refused sign-in names itself above the form
                // instead of vanishing with a five-second toast.
                {move || app.pending_notice.get().map(|n| view! {
                    <p class="rh-login-notice" role="status">{n}</p>
                })}
                <form
                    class="rh-login"
                    on:submit=move |ev: leptos::ev::SubmitEvent| {
                        ev.prevent_default();
                        go.call(());
                    }
                >
                    <label for=a11y::LOGIN_SERVER_ID>"Burrow"</label>
                    <input
                        id=a11y::LOGIN_SERVER_ID
                        class="rh-input rh-login-address"
                        autocomplete="off"
                        autocapitalize="off"
                        spellcheck="false"
                        placeholder="ws://localhost:4654"
                        prop:value=endpoint
                        on:input=move |ev| endpoint.set(event_target_value(&ev))
                    />
                    <p class="rh-field-hint">
                        "ws:// reaches a burrow on this machine; anything further away needs wss://."
                    </p>
                    <label for=a11y::LOGIN_HANDLE_ID>"Handle"</label>
                    <input
                        id=a11y::LOGIN_HANDLE_ID
                        class="rh-input"
                        autocomplete="username"
                        autocapitalize="off"
                        spellcheck="false"
                        placeholder="your handle"
                        aria-invalid=move || {
                            if handle_missing.get() && handle.with(|h| h.trim().is_empty()) {
                                "true"
                            } else {
                                "false"
                            }
                        }
                        prop:value=handle
                        on:input=move |ev| {
                            handle_missing.set(false);
                            handle.set(event_target_value(&ev));
                        }
                    />
                    <Show
                        when=move || handle_missing.get() && handle.with(|h| h.trim().is_empty())
                        fallback=|| ()
                    >
                        <p class="rh-field-hint error" role="alert">
                            "Pick a handle first. It is the name people there will see."
                        </p>
                    </Show>
                    // A seeded demo burrow has no accounts to sign in to.
                    <Show when=move || !is_demo() fallback=|| ()>
                        <label for="rh-login-password">"Password"</label>
                        <input
                            id="rh-login-password"
                            class="rh-input"
                            type="password"
                            autocomplete="current-password"
                            placeholder="password"
                            prop:value=password
                            on:input=move |ev| password.set(event_target_value(&ev))
                        />
                        <p class="rh-field-hint">"Leave it empty to visit as a guest."</p>
                    </Show>
                    <button class="rh-btn rh-connect-go" type="submit">
                        <span>{go_label}</span>
                    </button>
                </form>
                // Here to add a burrow, not because there is nowhere to be:
                // the rail is hidden on this window, so say the way back.
                <div class="rh-connect-foot">
                    <Show when=move || app.has_burrows() fallback=|| ()>
                        <A class="rh-connect-back" href="/lobby">
                            <span inner_html=crate::icons::chevron_left_icon()></span>
                            "Back to "
                            {move || {
                                app.focused_tracked()
                                    .name
                                    .get()
                                    .filter(|n| !n.is_empty())
                                    .unwrap_or_else(|| "your burrow".into())
                            }}
                        </A>
                    </Show>
                    <p class="rh-connect-version">
                        {concat!("Version ", env!("CARGO_PKG_VERSION"))}
                    </p>
                </div>
            </section>
            <BurrowBrowser endpoint handle recent on_open/>
        </main>
    }
}

/// Sidebar listing the handles present in the room.
#[component]
pub fn WhoList() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.focused().state;
    view! {
        // `rh-present` marks the lobby roster specifically: on narrow screens
        // it renders as a horizontal presence strip above the chat, while the
        // DM view's `.rh-who` conversation list keeps the stacked layout.
        <aside class="rh-who rh-present">
            <h2>"Present"</h2>
            <ul>
                <For
                    each=move || state.with(|s| s.who.clone())
                    key=|p| (p.screen_name.clone(), p.state)
                    children=move |p| {
                        use rabbithole_proto::presence::PresenceState;
                        let dot = match p.state {
                            PresenceState::Online => "rh-pres on",
                            PresenceState::Away => "rh-pres away",
                            PresenceState::Idle => "rh-pres idle",
                            _ => "rh-pres off",
                        };
                        // A warren mark: the pixel face that makes a name
                        // recognisable at a glance (Hotline's user icon, derived
                        // from the person's identity rather than picked).
                        let mark = crate::avatar::mark_svg(
                            &crate::avatar::seed_for(p.key.as_deref(), &p.screen_name),
                            22,
                        );
                        let href = format!(
                            "/people/{}",
                            crate::sightings::seed_of(p.key.as_deref(), &p.screen_name),
                        );
                        view! {
                            <li>
                                <A href=href class="rh-who-row">
                                    <span class="rh-mark" inner_html=mark></span>
                                    <span class=dot aria-hidden="true"></span>
                                    <span class="rh-who-name">{p.screen_name}</span>
                                </A>
                            </li>
                        }
                    }
                />
            </ul>
        </aside>
    }
}

/// Placeholder rows shown while a list request is in flight, so a view never
/// claims "nothing here" for data that is merely still on the wire. Shimmering
/// bars of varied width read as "content is coming" without pretending to be
/// content (aria-hidden + a busy status for assistive tech).
#[component]
fn Skeleton(
    /// How many placeholder rows to draw.
    #[prop(default = 3)]
    rows: usize,
) -> impl IntoView {
    // Varied widths so the placeholder reads as text, not a progress bar.
    let widths = ["78%", "56%", "67%", "45%", "72%"];
    view! {
        <div class="rh-skeleton" role="status" aria-label="Loading\u{2026}">
            {(0..rows)
                .map(|i| {
                    let w = widths[i % widths.len()];
                    view! { <div class="rh-skeleton-row" aria-hidden="true" style=format!("width:{w}")></div> }
                })
                .collect_view()}
        </div>
    }
}

/// A friendly centered empty state for a panel with nothing to show yet: a
/// quiet decorative mark, a headline, and a warmer line of guidance. Reuses
/// the lobby's `.rh-chat-empty` styling so every view's "nothing here" moment
/// reads the same.
#[component]
pub(crate) fn EmptyState(
    /// The section this empty state belongs to (a route path): its drawn
    /// icon is the mark. It used to be a dingbat, which rendered as whatever
    /// glyph the platform font had and matched nothing else in the app.
    icon: &'static str,
    /// One-line headline.
    #[prop(into)]
    title: String,
    /// A sentence of guidance under the headline.
    #[prop(into)]
    sub: String,
    /// The one thing to do about it, when there is one (a button).
    #[prop(optional)]
    action: Option<View>,
) -> impl IntoView {
    view! {
        <div class="rh-chat-empty">
            <div class="rh-chat-empty-mark" aria-hidden="true" inner_html=crate::icons::section_icon(icon)></div>
            <p class="rh-chat-empty-title">{title}</p>
            <p class="rh-chat-empty-sub">{sub}</p>
            {action.map(|a| view! { <div class="rh-chat-empty-action">{a}</div> })}
        </div>
    }
}

/// A list that failed to arrive: says so, and offers to ask again. Shown in
/// place of a skeleton that would otherwise shimmer forever, and instead of
/// an empty state that would claim the burrow has nothing.
#[component]
fn LoadFailed(
    /// What failed to load, in the product's words ("the boards").
    what: &'static str,
    /// Re-issue the request.
    #[prop(into)]
    on_retry: Callback<()>,
) -> impl IntoView {
    view! {
        <div class="rh-load-failed" role="alert">
            <span>{format!("Couldn\u{2019}t load {what}.")}</span>
            <button class="rh-btn ghost small" on:click=move |_| on_retry.call(())>
                "Try again"
            </button>
        </div>
    }
}

/// The main view: header, chat scrollback, compose box, and who-list.
#[component]
pub fn Lobby() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.focused().state;
    let draft = create_rw_signal(String::new());
    // Follow the newest line while the reader is at the bottom; offer a
    // "new messages" jump instead of yanking them out of history otherwise.
    let log = crate::scroll::ChatScroll::install(move || state.with(|s| s.messages.len()));
    // The view! macro wants a bare identifier for `node_ref=`.
    let log_node = log.node;

    let send = move || {
        let text = draft.get();
        if text.trim().is_empty() {
            return;
        }
        // Routes over the live socket when connected, else the mock seam.
        app.send_chat(text);
        draft.set(String::new());
        // Your own message always brings you back to the newest line.
        log.jump();
    };

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Lobby"</h1>
            <section class="rh-chat" aria-label="Lobby chat">
                // The burrow's news, where you actually land. It used to live
                // only inside the welcome sheet, so a burrow with no MOTD showed
                // no news at all — and dismissing the sheet lost it for good.
                <FrontPage/>
                // role=log: an implicitly polite live region — new messages
                // are announced without moving focus off the compose box.
                <div
                    class="rh-scroll"
                    role="log"
                    aria-label="Chat messages"
                    node_ref=log_node
                    on:scroll=move |_| log.on_scroll()
                >
                    <Show
                        when=move || state.with(|s| s.messages.is_empty())
                        fallback=|| ()
                    >
                        <EmptyState
                            icon="/lobby"
                            title="Quiet in here"
                            sub="Say hello \u{2014} the lobby's yours to open."
                        />
                    </Show>
                    <ul class="rh-lines">
                        <For
                            // Rows carry a `head` flag: the first line of a
                            // sender's burst shows the name + time, follow-ups
                            // render as bare grouped lines. A row's head-ness
                            // depends only on the (immutable) previous line,
                            // so the index stays a sound key.
                            each=move || {
                                state.with(|s| {
                                    s.messages
                                        .iter()
                                        .enumerate()
                                        .map(|(i, line)| {
                                            let head = i == 0
                                                || !crate::state::continues_group(
                                                    &s.messages[i - 1].from,
                                                    s.messages[i - 1].at_unix_ms,
                                                    &line.from,
                                                    line.at_unix_ms,
                                                );
                                            (i, line.clone(), head)
                                        })
                                        .collect::<Vec<_>>()
                                })
                            }
                            key=|(i, _, _)| *i
                            children=move |(_, line, head)| view! {
                                <li class=if head { "rh-line rh-line-head" } else { "rh-line rh-line-cont" }>
                                    // The speaker's warren mark opens each burst,
                                    // so you know who is talking before you read
                                    // the name — Hotline's user icon, in chat.
                                    {head.then(|| {
                                        let mark = crate::avatar::mark_svg(&line.from, 20);
                                        view! { <span class="rh-mark rh-line-mark" inner_html=mark></span> }
                                    })}
                                    // The name opens the person. Keyed the
                                    // way People keys them: their identity
                                    // when the roster knows it, else the
                                    // handle, so a rename doesn't lose them.
                                    {head.then(|| {
                                        let key = state.with_untracked(|s| {
                                            s.who
                                                .iter()
                                                .find(|p| p.screen_name == line.from)
                                                .and_then(|p| p.key.clone())
                                        });
                                        let href = format!(
                                            "/people/{}",
                                            crate::sightings::seed_of(key.as_deref(), &line.from),
                                        );
                                        view! { <A href=href class="rh-from">{line.from.clone()}</A> }
                                    })}
                                    {(line.at_unix_ms != 0).then(|| view! {
                                        <span class="rh-line-time">
                                            {crate::clock::local_hhmm(line.at_unix_ms)}
                                        </span>
                                    })}
                                    // Rendered, not raw: the wire has always
                                    // carried whatever was typed, and a message
                                    // with `**this**` in it should read as bold
                                    // rather than as punctuation.
                                    <span
                                        class="rh-rich rh-line-text"
                                        inner_html=crate::markdown::inline_to_html(&line.text)
                                    ></span>
                                </li>
                            }
                        />
                    </ul>
                </div>
                <Show when=move || log.unseen.get() fallback=|| ()>
                    <button class="rh-jump-new" on:click=move |_| log.jump()>
                        <span class="rh-btn-icon" inner_html=crate::icons::arrow_down_icon()></span>"New messages"
                    </button>
                </Show>
                <Composer
                    draft=draft
                    label="Message the lobby"
                    placeholder="Message the lobby\u{2026}"
                    on_send=move |_| send()
                    can_send=Signal::derive(move || {
                        app.online() && !draft.get().trim().is_empty()
                    })
                    send_label="Send"
                    chat=true
                />
            </section>
            <WhoList/>
        </main>
    }
}

/// A **rich text composer**: a formatting bar over a growing text area, with an
/// optional live preview.
///
/// What it produces is markdown, always — see [`crate::markdown`] for why that
/// and not HTML. The two modes differ only in what they *show* you:
///
/// * **Rich** shows the formatting bar and, where the caller asks for it, a
///   preview of the rendered result. You never have to know markdown syntax.
/// * **Markdown** hides the preview and sets the text in a monospaced face, for
///   people who'd rather just type it.
///
/// It is deliberately not a `contenteditable` WYSIWYG surface. Those look
/// closer to "rich text" for about a day, and then you spend forever fighting
/// browsers over what a paste, an undo, or a caret at a boundary means — and
/// what you get out at the end still has to be converted back to markdown.
/// Textarea plus preview is what GitHub, Reddit and every forum settled on, and
/// it round-trips exactly.
#[component]
pub fn Composer(
    /// The draft being edited.
    draft: RwSignal<String>,
    /// Accessible name for the text area.
    label: &'static str,
    /// Placeholder text.
    placeholder: &'static str,
    /// Called when the composer asks to send (Enter, or the button).
    #[prop(into)]
    on_send: Callback<()>,
    /// Whether sending is currently possible.
    #[prop(into)]
    can_send: Signal<bool>,
    /// Label for the send button.
    send_label: &'static str,
    /// Show a live preview in rich mode. Right for a post; noise for chat,
    /// where the scrollback *is* the preview.
    #[prop(optional)]
    preview: bool,
    /// Start tall (a post) rather than one line (a chat message).
    #[prop(optional)]
    tall: bool,
    /// A chat line, not a document: one growing row with a send button,
    /// the formatting bar folded behind an "Aa" toggle, no hint line.
    /// A lobby message should feel like typing into a room; the full
    /// post editor above an empty scrollback was the heaviest thing on
    /// the screen.
    #[prop(optional)]
    chat: bool,
) -> impl IntoView {
    use crate::compose::{Format, TOOLBAR};
    let markdown_mode = create_rw_signal(false);
    let bar_open = create_rw_signal(false);
    let area = create_node_ref::<leptos::html::Textarea>();

    // Apply a toolbar format to the current selection, then put the caret back
    // where the pure logic said it belongs — otherwise it jumps to the end and
    // you can't chain two buttons.
    let format = move |fmt: Format| {
        let Some(el) = area.get() else { return };
        #[cfg(target_arch = "wasm32")]
        {
            let text = el.value();
            let (start, end) = (
                el.selection_start().ok().flatten().unwrap_or(0) as usize,
                el.selection_end().ok().flatten().unwrap_or(0) as usize,
            );
            let edit = crate::compose::apply(&text, start, end, fmt);
            draft.set(edit.text.clone());
            el.set_value(&edit.text);
            let _ = el.set_selection_start(Some(edit.start as u32));
            let _ = el.set_selection_end(Some(edit.end as u32));
            let _ = el.focus();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (el, fmt);
        }
    };

    // The formatting bar and the text area are one thing each, used by both
    // layouts, so they are built by closures rather than written twice.
    let bar = move || {
        view! {
            <div class="rh-format-bar" role="toolbar" aria-label="Formatting">
                {TOOLBAR
                    .into_iter()
                    .map(|f| {
                        let (glyph, name, key) = f.button();
                        let title = match key {
                            Some(k) => format!("{name} (\u{2318}{})", k.to_ascii_uppercase()),
                            None => name.to_string(),
                        };
                        view! {
                            <button
                                type="button"
                                class="rh-format-btn"
                                title=title
                                aria-label=name
                                // Keep focus in the text area: a toolbar button
                                // that steals it loses the selection it acts on.
                                on:mousedown=move |ev| ev.prevent_default()
                                on:click=move |_| format(f)
                            >
                                {if matches!(f, Format::Link) {
                                    view! { <span inner_html=crate::icons::link_icon()></span> }.into_view()
                                } else {
                                    glyph.into_view()
                                }}
                            </button>
                        }
                    })
                    .collect_view()}
                <span class="rh-format-spacer"></span>
                <button
                    type="button"
                    class="rh-format-btn rh-format-mode"
                    class:on=move || markdown_mode.get()
                    aria-pressed=move || markdown_mode.get().to_string()
                    title="Type markdown directly"
                    on:click=move |_| markdown_mode.update(|m| *m = !*m)
                >
                    "Markdown"
                </button>
            </div>
        }
    };
    let field = move || {
        view! {
            <textarea
                node_ref=area
                class="rh-input rh-compose-area"
                class:tall=move || { tall }
                aria-label=label
                placeholder=placeholder
                prop:rows=move || if chat { crate::compose::rows_for(&draft.get()) } else { 2 }
                prop:value=draft
                on:input=move |ev| draft.set(event_target_value(&ev))
                on:keydown=move |ev| {
                    // ⌘/Ctrl shortcuts for the bar's own buttons.
                    if ev.meta_key() || ev.ctrl_key() {
                        if let Some(f) = crate::compose::shortcut(&ev.key()) {
                            ev.prevent_default();
                            format(f);
                            return;
                        }
                    }
                    if crate::compose::sends_on_enter(&ev.key(), ev.shift_key())
                        && can_send.get()
                    {
                        ev.prevent_default();
                        on_send.call(());
                    }
                }
            ></textarea>
        }
    };

    view! {
        <div class="rh-composer" class:markdown=move || markdown_mode.get() class:chat=move || { chat }>
            {if chat {
                view! {
                    <Show when=move || bar_open.get() fallback=|| ()>{bar()}</Show>
                    <div class="rh-compose-row">
                        <button
                            type="button"
                            class="rh-compose-iconbtn"
                            class:on=move || bar_open.get()
                            aria-pressed=move || bar_open.get().to_string()
                            title="Formatting"
                            aria-label="Formatting"
                            on:mousedown=move |ev| ev.prevent_default()
                            on:click=move |_| bar_open.update(|o| *o = !*o)
                        >
                            "Aa"
                        </button>
                        {field()}
                        <button
                            type="button"
                            class="rh-compose-iconbtn rh-compose-send"
                            title=send_label
                            aria-label=send_label
                            prop:disabled=move || !can_send.get()
                            on:click=move |_| on_send.call(())
                            inner_html=crate::icons::send_icon()
                        ></button>
                    </div>
                }
                .into_view()
            } else {
                view! {
                    {bar()}
                    {field()}
                    // The preview renders the same markdown the recipient will
                    // see, so "what it looks like" is never a guess.
                    <Show
                        when=move || { preview && !markdown_mode.get() && !draft.get().trim().is_empty() }
                        fallback=|| ()
                    >
                        <div class="rh-preview">
                            <span class="rh-preview-label">"Preview"</span>
                            <div
                                class="rh-rich"
                                inner_html=move || crate::markdown::to_html(&draft.get())
                            ></div>
                        </div>
                    </Show>
                    <div class="rh-compose-actions">
                        <span class="rh-compose-hint">
                            {move || if markdown_mode.get() {
                                "Markdown \u{2014} Enter to send, Shift+Enter for a new line"
                            } else {
                                "Enter to send, Shift+Enter for a new line"
                            }}
                        </span>
                        <button
                            class="rh-btn"
                            type="button"
                            prop:disabled=move || !can_send.get()
                            on:click=move |_| on_send.call(())
                        >
                            {send_label}
                        </button>
                    </div>
                }
                .into_view()
            }}
        </div>
    }
}

/// The board tree: every board links to its `/boards/:slug` reading view.
#[component]
pub fn Boards() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.focused().state;
    app.load_boards();

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <section class="rh-panel">
                <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">"Boards"</h1>
                // Loading vs. genuinely empty: a skeleton while the board list is
                // in flight, the warm empty state only once it has actually arrived.
                <Show when=move || state.with(|s| s.loading.boards) fallback=|| ()>
                    <Skeleton rows=3/>
                </Show>
                <Show when=move || state.with(|s| s.load_failed && s.boards.is_empty()) fallback=|| ()>
                    <LoadFailed what="the boards" on_retry=move |_| app.load_boards()/>
                </Show>
                <Show
                    when=move || state.with(|s| !s.loading.boards && !s.load_failed && s.boards.is_empty())
                    fallback=|| ()
                >
                    <EmptyState
                        icon="/boards"
                        title="No boards yet"
                        sub="This burrow hasn't opened any boards to post on."
                    />
                </Show>
                <ul class="rh-tree" tabindex="0" aria-label="Boards" on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-board-link")>
                    <For
                        each=move || state.with(|s| s.boards.clone())
                        key=|b| b.slug.clone()
                        children=move |b| {
                            let href = format!("/boards/{}", b.slug);
                            view! {
                                <li class="rh-tree-item">
                                    // (Router <A> takes no tabindex in leptos 0.6, so board rows stay
                                    // individual Tab stops — a board list is a handful of rows, not a
                                    // forty-row file table, so the cost is small. Arrows still work.)
                                    <A href=href class="rh-board-link rh-row">
                                        <span class="rh-row-icon" inner_html=crate::icons::section_icon("/boards")></span>
                                        <span class="rh-row-main">
                                            <span class="rh-board-name">{b.name}</span>
                                            <span class="rh-board-desc">{b.description}</span>
                                        </span>
                                        {crate::state::unread_badge(b.unread as usize).map(|n| view! {
                                            <span class="rh-pip" aria-label=format!("{} unread", b.unread)>{n}</span>
                                        })}
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
    let state = app.focused().state;
    let params = use_params_map();

    // Re-select the board whenever the `:slug` route param changes.
    //
    // A *render* effect, for the same reason as the DM view's: `create_effect`
    // queues its first run for after the current tick, and if this view is
    // disposed in that window — the `<Routes>` remount on a burrow-focus
    // change, or a second navigation — the queued run resolves against a
    // disposed owner and panics, which left the thread list showing its
    // loading skeleton forever.
    create_render_effect(move |_| {
        if let Some(slug) = params.with(|p| p.get("slug").cloned()) {
            // Arriving here by link, before the board list has loaded, the
            // title fell back to the slug ("general"). Ask for the list so
            // the board can say its own name.
            if state.with_untracked(|s| s.boards.is_empty()) {
                app.load_boards();
            }
            app.select_board(&slug);
        }
    });

    let new_subject = create_rw_signal(String::new());
    let new_body = create_rw_signal(String::new());
    // The new-thread form opens on request. Always open, it was a subject
    // field plus a tall editor sitting under every thread list whether or
    // not you had anything to say.
    let new_open = create_rw_signal(false);
    let post = move || {
        let slug = state.with(|s| s.selected_board.clone()).unwrap_or_default();
        app.post_thread(&slug, &new_subject.get(), &new_body.get());
        new_subject.set(String::new());
        new_body.set(String::new());
        new_open.set(false);
    };

    let reply_body = create_rw_signal(String::new());
    let reply = move || {
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
                <A href="/boards" class="rh-back"><span class="rh-back-icon" inner_html=crate::icons::chevron_left_icon()></span>"All boards"</A>
                <div class="rh-panel-head">
                    <h1 class="rh-panel-title" id=a11y::VIEW_TITLE_ID tabindex="-1">{board_name}</h1>
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        aria-expanded=move || new_open.get().to_string()
                        prop:disabled=move || !app.online()
                        on:click=move |_| new_open.update(|o| *o = !*o)
                    >
                        {move || if new_open.get() { "Cancel" } else { "New thread" }}
                    </button>
                </div>
                <Show when=move || state.with(|s| s.loading.threads) fallback=|| ()>
                    <Skeleton rows=3/>
                </Show>
                <Show
                    when=move || state.with(|s| !s.loading.threads && s.threads.is_empty())
                    fallback=|| ()
                >
                    <p class="rh-empty">"No threads yet \u{2014} start the first one."</p>
                </Show>
                // The thread list is a navigation index beside the reader, not
                // the main content — so it stays a narrow column with a dense
                // two-line row (subject, then author · replies · activity)
                // rather than a squeezed table.
                <ul class="rh-tree rh-threadtable" tabindex="0" aria-label="Threads" on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-thread-link")>
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
                                        tabindex="-1"
                                        aria-current=move || selected.get().then_some("true")
                                        on:click=move |_| app.open_thread(id.clone())
                                    >
                                        <span class="rh-thread-title">{t.title}</span>
                                        <span class="rh-thread-meta">
                                            {t.author}
                                            <span class="rh-dot-sep" aria-hidden="true">"\u{00b7}"</span>
                                            {format!("{} {}", t.replies, if t.replies == 1 { "reply" } else { "replies" })}
                                            <span class="rh-dot-sep" aria-hidden="true">"\u{00b7}"</span>
                                            {crate::files::relative_day(
                                                t.last_activity_unix_ms / 1000,
                                                crate::clock::now_ms() / 1000,
                                            )}
                                        </span>
                                    </button>
                                </li>
                            }
                        }
                    />
                </ul>
                <Show when=move || new_open.get() fallback=|| ()>
                <div class="rh-newthread">
                    <input
                        class="rh-input"
                        placeholder="New thread subject\u{2026}"
                        aria-label="New thread subject"
                        prop:value=new_subject
                        prop:disabled=move || !app.online()
                        on:input=move |ev| new_subject.set(event_target_value(&ev))
                    />
                    // A post is a document, so it gets the preview: unlike chat,
                    // there's no scrollback about to show you the result.
                    <Composer
                        draft=new_body
                        label="First post body"
                        placeholder="Write the first post\u{2026}"
                        on_send=move |_| post()
                        // The subject is part of what makes this sendable.
                        // `post_thread` refuses a subjectless post, and the
                        // composer clears the draft either way — so without
                        // this you could write a long post, forget the
                        // subject, hit Post, and watch it vanish with no
                        // explanation.
                        can_send=Signal::derive(move || {
                            app.online()
                                && !new_body.get().trim().is_empty()
                                && !new_subject.get().trim().is_empty()
                        })
                        send_label="Post thread"
                        preview=true
                        tall=true
                    />
                </div>
                </Show>
            </section>
            <section class="rh-panel rh-reader" aria-label="Thread posts">
                <Show
                    when=move || state.with(|s| s.selected_thread.is_some())
                    fallback=|| view! {
                        <EmptyState
                            icon="/boards"
                            title="Nothing open"
                            sub="Pick a thread to read it."
                        />
                    }
                >
                    <div class="rh-posts">
                        <For
                            each=move || state.with(|s| s.posts.clone())
                            key=|p| p.id.clone()
                            children=move |p| {
                                let mark = crate::avatar::mark_svg(&p.author, 22);
                                let when = (p.at_unix_ms != 0).then(|| {
                                    let day = crate::files::relative_day(
                                        p.at_unix_ms / 1000,
                                        crate::clock::now_ms() / 1000,
                                    );
                                    format!("{day} \u{00b7} {}", crate::clock::local_hhmm(p.at_unix_ms))
                                });
                                // Its author may take a post down, and so may
                                // anyone who moderates boards. The burrow
                                // decides; this only avoids offering a button
                                // that would be refused.
                                const BOARD_MODERATE: u64 = 1 << 22;
                                let me = app.focused();
                                let author = p.author.clone();
                                let may_remove = {
                                    let author = author.clone();
                                    move || {
                                        !p.removed
                                            && (me.caps.get() & BOARD_MODERATE != 0
                                                // A board names its authors `handle@origin`.
                                                || author.split('@').next()
                                                    == Some(me.handle.get().as_str()))
                                    }
                                };
                                let post_id = p.id.clone();
                                let asked_author = author.clone();
                                view! {
                                    <article class="rh-post" class:removed=p.removed>
                                        <header class="rh-post-head">
                                            <span class="rh-mark" inner_html=mark></span>
                                            <span class="rh-from">{author}</span>
                                            {when.map(|w| view! { <span class="rh-post-when">{w}</span> })}
                                            <Show when=may_remove.clone() fallback=|| ()>
                                                <button
                                                    type="button"
                                                    class="rh-post-remove"
                                                    on:click={
                                                        let (id, who) = (post_id.clone(), asked_author.clone());
                                                        move |_| app.confirm.set(Some(
                                                            crate::app::ConfirmAsk::delete_post(&id, &who),
                                                        ))
                                                    }
                                                >
                                                    "Remove"
                                                </button>
                                            </Show>
                                        </header>
                                        {if p.removed {
                                            view! { <p class="rh-post-gone">"This post was removed."</p> }
                                                .into_view()
                                        } else {
                                            view! {
                                                <div
                                                    class="rh-rich rh-post-body"
                                                    inner_html=crate::markdown::to_html(&p.body)
                                                ></div>
                                            }
                                            .into_view()
                                        }}
                                    </article>
                                }
                            }
                        />
                    </div>
                    <div class="rh-reply">
                        <Composer
                            draft=reply_body
                            label="Reply body"
                            placeholder="Write a reply\u{2026}"
                            on_send=move |_| reply()
                            can_send=Signal::derive(move || {
                                app.online() && !reply_body.get().trim().is_empty()
                            })
                            send_label="Reply"
                            preview=true
                        />
                    </div>
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
    let state = app.focused().state;
    let draft = create_rw_signal(String::new());
    // A guest gets the gate below, not a request the server will refuse.
    let is_guest = app.focused().is_guest;
    if !is_guest.get_untracked() {
        app.load_dms();
    }

    // Follow the newest message unless the reader has scrolled up to history.
    let log = crate::scroll::ChatScroll::install(move || {
        state.with(|s| s.active_dm().map(|t| t.messages.len()).unwrap_or(0))
    });
    // The view! macro wants a bare identifier for `node_ref=`.
    let log_node = log.node;
    // Switching conversations always lands on its newest message. A render
    // effect for the same reason as ChatScroll::install: create_effect's
    // queued first run panics if this view is disposed in the same tick (the
    // <Routes> remount on burrow-focus change).
    create_render_effect(move |_| {
        state.with(|s| s.selected_dm.clone());
        log.jump();
    });

    let send = move || {
        let text = draft.get();
        if text.trim().is_empty() {
            return;
        }
        app.send_dm(&text);
        draft.set(String::new());
        // Your own message always brings you back to the newest line.
        log.jump();
    };

    let new_peer = create_rw_signal(String::new());
    let start = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let peer = new_peer.get();
        if peer.trim().is_empty() {
            return;
        }
        app.select_dm(peer.trim());
        new_peer.set(String::new());
    };

    view! {
        <StatusBar/>
        <main class="rh-body" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">
                "Direct messages"
            </h1>
            // Direct messages travel between accounts, and a guest has none
            // behind the handle. The server refuses the list with Forbidden;
            // this used to surface as "Couldn't load your conversations. Try
            // again", a retry that could never succeed. Now the section says
            // what it needs and offers the way there.
            <Show
                when=move || is_guest.get()
                fallback=move || view! {
            <aside class="rh-who rh-convos">
                <h2>"Conversations"</h2>
                <Show when=move || state.with(|s| s.loading.dms) fallback=|| ()>
                    <Skeleton rows=3/>
                </Show>
                <Show when=move || state.with(|s| s.load_failed && s.dm_threads.is_empty()) fallback=|| ()>
                    <LoadFailed what="your conversations" on_retry=move |_| app.load_dms()/>
                </Show>
                <Show
                    when=move || state.with(|s| !s.loading.dms && !s.load_failed && s.dm_threads.is_empty())
                    fallback=|| ()
                >
                    <p class="rh-empty">"No conversations yet \u{2014} message a handle to start one."</p>
                </Show>
                <form class="rh-dm-start" on:submit=start>
                    <input
                        class="rh-input"
                        placeholder="Message a handle\u{2026}"
                        aria-label="Start a conversation with a handle"
                        prop:value=new_peer
                        on:input=move |ev| new_peer.set(event_target_value(&ev))
                    />
                </form>
                <ul tabindex="0" aria-label="Conversations" on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-dm-peer")>
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
                            // The same warren mark the lobby and People use —
                            // the row used to draw an anonymous gradient blob.
                            let mark = crate::avatar::mark_svg(
                                &crate::avatar::seed_for(None, &t.peer),
                                26,
                            );
                            let (preview, when) = t
                                .preview()
                                .map(|(text, at)| (text.to_string(), at))
                                .unwrap_or_default();
                            let when = (when != 0).then(|| crate::clock::local_hhmm(when));
                            let pip = crate::state::unread_badge(t.unread as usize);
                            let unread = t.unread;
                            view! {
                                <li>
                                    <button
                                        class=class
                                        tabindex="-1"
                                        aria-current=move || current().then_some("true")
                                        on:click=move |_| app.select_dm(&id)
                                    >
                                        <span class="rh-mark rh-dm-mark" inner_html=mark></span>
                                        <span class="rh-dm-main">
                                            <span class="rh-dm-name">{t.peer}</span>
                                            {(!preview.is_empty()).then(|| view! {
                                                <span class="rh-dm-preview">{preview}</span>
                                            })}
                                        </span>
                                        <span class="rh-dm-side">
                                            {when.map(|w| view! { <span class="rh-dm-when">{w}</span> })}
                                            {pip.map(|b| view! {
                                                <span class="rh-pip" aria-label=format!("{unread} unread")>{b}</span>
                                            })}
                                        </span>
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
                        <EmptyState
                            icon="/dms"
                            title="No conversation open"
                            sub="Pick a conversation, or message a handle to start one."
                        />
                    }
                >
                    <div
                        class="rh-scroll"
                        role="log"
                        aria-label="Conversation messages"
                        node_ref=log_node
                        on:scroll=move |_| log.on_scroll()
                    >
                        <ul class="rh-lines">
                            <For
                                // Same sender-burst grouping as the lobby.
                                each=move || {
                                    state.with(|s| {
                                        // Key rows by conversation + index so
                                        // switching conversations re-creates
                                        // rows instead of reusing them.
                                        let convo = s.selected_dm.clone().unwrap_or_default();
                                        let msgs = s
                                            .active_dm()
                                            .map(|t| t.messages.clone())
                                            .unwrap_or_default();
                                        msgs.iter()
                                            .enumerate()
                                            .map(|(i, m)| {
                                                let head = i == 0
                                                    || !crate::state::continues_group(
                                                        &msgs[i - 1].from,
                                                        msgs[i - 1].at_unix_ms,
                                                        &m.from,
                                                        m.at_unix_ms,
                                                    );
                                                (format!("{convo}#{i}"), m.clone(), head)
                                            })
                                            .collect::<Vec<_>>()
                                    })
                                }
                                key=|(k, _, _)| k.clone()
                                children=move |(_, m, head)| view! {
                                    <li class=if head { "rh-line rh-line-head" } else { "rh-line rh-line-cont" }>
                                        {head.then(|| {
                                            let mark = crate::avatar::mark_svg(&m.from, 20);
                                            view! { <span class="rh-mark rh-line-mark" inner_html=mark></span> }
                                        })}
                                        {head.then(|| view! {
                                            <span class="rh-from">{m.from.clone()}</span>
                                        })}
                                        {(m.at_unix_ms != 0).then(|| view! {
                                            <span class="rh-line-time">
                                                {crate::clock::local_hhmm(m.at_unix_ms)}
                                            </span>
                                        })}
                                        <span
                                            class="rh-rich rh-line-text"
                                            inner_html=crate::markdown::inline_to_html(&m.text)
                                        ></span>
                                    </li>
                                }
                            />
                        </ul>
                    </div>
                    <Show when=move || log.unseen.get() fallback=|| ()>
                        <button class="rh-jump-new" on:click=move |_| log.jump()>
                            <span class="rh-btn-icon" inner_html=crate::icons::arrow_down_icon()></span>"New messages"
                        </button>
                    </Show>
                    <Composer
                        draft=draft
                        label="Write a direct message"
                        placeholder="Write a message\u{2026}"
                        on_send=move |_| send()
                        can_send=Signal::derive(move || {
                            app.online() && !draft.get().trim().is_empty()
                        })
                        send_label="Send"
                        chat=true
                    />
                </Show>
            </section>
                }
            >
                <section class="rh-dm-gate" aria-label="Sign in to message">
                    <EmptyState
                        icon="/dms"
                        title="Messages need an account"
                        sub="You\u{2019}re here as a guest. Sign in with an account to send and receive direct messages."
                        action=view! {
                            <button class="rh-btn" on:click=move |_| app.sign_in_as_member()>
                                "Sign in"
                            </button>
                        }.into_view()
                    />
                </section>
            </Show>
        </main>
    }
}

/// Member directory: a searchable list plus a profile card for the selected
/// member.
#[component]
pub fn Directory() -> impl IntoView {
    let app = expect_context::<AppState>();
    let state = app.focused().state;
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
                <Show
                    when=move || state.with(|s| s.matching_members().is_empty())
                    fallback=|| ()
                >
                    // An empty directory and an unmatched search read
                    // differently: the first is about the burrow, the second
                    // about the query.
                    {move || state.with(|s| {
                        if s.loading.members {
                            // Still on the wire — don't claim the burrow is empty.
                            view! { <Skeleton rows=4/> }.into_view()
                        } else if s.load_failed && s.members.is_empty() {
                            view! { <LoadFailed what="the members" on_retry=move |_| app.load_members()/> }.into_view()
                        } else if s.members.is_empty() {
                            view! {
                                <EmptyState
                                    icon="/directory"
                                    title="Nobody here yet"
                                    sub="Members appear here as they join this burrow."
                                />
                            }
                            .into_view()
                        } else {
                            let q = s.directory_query.clone();
                            view! {
                                <p class="rh-empty">
                                    {format!("No one matches \u{201c}{q}\u{201d}.")}
                                </p>
                            }
                            .into_view()
                        }
                    })}
                </Show>
                <ul class="rh-tree" tabindex="0" aria-label="Members" on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-member-link")>
                    <For
                        each=move || state.with(|s| s.matching_members())
                        // Keyed by handle ALONE, with presence read reactively
                        // inside the row. The old key included `online` so a
                        // presence flip would re-render the dot (the security
                        // review's stale-row fix) — but recreating the row
                        // destroys the element, and now that rows hold keyboard
                        // focus (keynav), that ejected the user to <body>
                        // mid-navigation. A reactive read updates the dot in
                        // place: both reviews stay satisfied.
                        key=|m| m.handle.clone()
                        children=move |m| {
                            let handle = m.handle.clone();
                            let presence_of = {
                                let handle = m.handle.clone();
                                // The live roster is the truth. The stored
                                // member row's `online` is whatever the
                                // directory reply said (never updated by
                                // presence deltas), so reading it here left
                                // every dot grey for people who were plainly
                                // in the lobby.
                                move || {
                                    state.with(|s| {
                                        s.who.iter().any(|p| p.screen_name == handle)
                                    })
                                }
                            };
                            let dot = {
                                let p = presence_of.clone();
                                move || if p() { "rh-dot on" } else { "rh-dot off" }
                            };
                            // The dot alone carried presence; keep it
                            // decorative and speak the state as hidden text.
                            let spoken = move || if presence_of() { "Online:" } else { "Offline:" };
                            view! {
                                <li class="rh-tree-item">
                                    <button
                                        class="rh-member-link"
                                        tabindex="-1"
                                        on:click=move |_| app.select_member(&handle)
                                    >
                                        <span class="rh-mark" inner_html=crate::avatar::mark_svg(&crate::avatar::seed_for(None, &m.handle), 22)></span>
                                        <span class=dot aria-hidden="true"></span>
                                        <span class="rh-visually-hidden">{spoken}</span>
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
                    when=move || {
                        state.with(|s| s.active_member().is_some() || s.selected_profile.is_some())
                    }
                    fallback=|| view! {
                        <EmptyState
                            icon="/directory"
                            title="No one selected"
                            sub="Pick a member to see their profile card."
                        />
                    }
                >
                    {move || state.with(|s| {
                        // Prefer the full live profile card; fall back to the
                        // directory-row summary (mock, or before it loads).
                        if let Some(p) = &s.selected_profile {
                            let status = if p.online { "Online" } else { "Offline" };
                            let field = |label: &'static str, v: &Option<String>| {
                                v.clone().filter(|x| !x.is_empty()).map(|val| view! {
                                    <p class="rh-card-field">
                                        <span class="rh-card-label">{label}</span>{val}
                                    </p>
                                })
                            };
                            return view! {
                                <div class="rh-card">
                                    {p.avatar_src.clone().map(|src| view! {
                                        <img class="rh-card-avatar" src=src alt="" />
                                    })}
                                    <h2 class="rh-card-name">{p.screen_name.clone()}</h2>
                                    {p.pronouns.clone().filter(|x| !x.is_empty())
                                        .map(|pr| view! { <p class="rh-card-handle">{pr}</p> })}
                                    <p class="rh-card-status">{status}</p>
                                    {p.quote.clone().filter(|x| !x.is_empty())
                                        .map(|q| view! { <p class="rh-card-bio">{q}</p> })}
                                    {field("Location", &p.location)}
                                    {field("Interests", &p.interests)}
                                    {field("Plan", &p.plan)}
                                </div>
                            }.into_view();
                        }
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
                        }).into_view()
                    })}
                </Show>
            </section>
        </main>
    }
}

/// The Looking Glass **server browser**: search + a ranked list of public
/// servers, each with a Connect action that hands its endpoint to the login
/// screen (which prefills on its next mount). Directory data is the host-tested
/// [`crate::servers`] model, filled from rabbithole.directory (then the
/// standard Looking Glass, then a native status-port INDEX) on arrival.
#[component]
pub fn ServerBrowser() -> impl IntoView {
    let navigate = use_navigate();
    // The same browser the connect window has, standing alone: there is no
    // form beside it, so a selected row carries its own Connect, and opening a
    // burrow hands the pick to the connect window in the URL. (A signal set
    // here and read there races the new component's first read; a query
    // param is also shareable and reload-proof.)
    let endpoint = create_rw_signal(String::new());
    let handle = create_rw_signal(String::new());
    #[cfg(target_arch = "wasm32")]
    let recent = create_rw_signal(crate::recent::load());
    #[cfg(not(target_arch = "wasm32"))]
    let recent = create_rw_signal(Vec::<crate::recent::RecentBurrow>::new());
    let on_open = Callback::new(move |()| {
        let pick = endpoint.get_untracked();
        if !pick.trim().is_empty() {
            navigate(
                &format!("/?server={}", crate::servers::encode_param(&pick)),
                Default::default(),
            );
        }
    });
    view! {
        <StatusBar/>
        <main class="rh-body rh-glass-page" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Looking Glass"</h1>
            <BurrowBrowser endpoint handle recent on_open standalone=true/>
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
    let files = app.focused().files;
    app.load_areas();
    app.load_upload_limits();

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
            <section class="rh-panel rh-file-detail" aria-label="File details and transfers">
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
    let files = app.focused().files;
    view! {
        <h2 class="rh-panel-title">"File areas"</h2>
        <Show when=move || files.with(|f| f.areas.is_empty()) fallback=|| ()>
            <EmptyState
                icon="/files"
                title="No file areas yet"
                sub="This burrow hasn't opened a file library."
            />
        </Show>
        <ul class="rh-tree">
            <For
                each=move || files.with(|f| f.areas.clone())
                key=|a| a.slug.clone()
                children=move |a| {
                    let slug = a.slug.clone();
                    view! {
                        <li class="rh-tree-item">
                            <button
                                class="rh-board-link rh-row"
                                on:click=move |_| app.open_area(&slug)
                            >
                                <span class="rh-row-icon" inner_html=crate::icons::section_icon("/files")></span>
                                <span class="rh-row-main">
                                    <span class="rh-board-name">{a.title}</span>
                                    <span class="rh-board-desc">{a.description}</span>
                                </span>
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
    let files = app.focused().files;
    // Type-to-filter state, and the rows that survive it. Folders always show:
    // filtering shouldn't strand you with no way back down the tree.
    let filter = create_rw_signal(String::new());
    // The hidden file input the Upload button drives.
    let picker = create_node_ref::<leptos::html::Input>();
    // Drag-and-drop: highlight while a drag is over the folder, and take the
    // drop. Guarded so a drag that carries no files does nothing.
    // Drag depth, not a boolean: dragenter/dragleave fire for every child the
    // payload crosses (crumbs, toolbar, rows), so a boolean strobes the
    // highlight as the file moves. Native drop targets highlight steadily.
    let drag_depth = create_rw_signal(0i32);
    let dragging = create_rw_signal(false);
    create_effect(move |_| dragging.set(drag_depth.get() > 0));
    // Only a drag that actually carries files is a drop we can take — dragging
    // selected text across the pane must not light it up.
    #[cfg(target_arch = "wasm32")]
    let has_files = |ev: &leptos::ev::DragEvent| {
        ev.data_transfer()
            .map(|dt| {
                let types = dt.types();
                (0..types.length()).any(|i| types.get(i).as_string().as_deref() == Some("Files"))
            })
            .unwrap_or(false)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let has_files = |_ev: &leptos::ev::DragEvent| false;
    let visible = move || {
        let q = filter.get();
        files.with(|f| {
            f.nodes
                .iter()
                .filter(|n| {
                    n.kind == KIND_FOLDER || crate::files::node_matches(&n.name, &n.uploader, &q)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    };

    let leave = move |_| {
        files.update(|f| {
            f.current_area = None;
            f.path.clear();
            f.nodes.clear();
            f.selected = None;
        });
    };

    view! {
        <div
            class="rh-dropzone"
            class:dragging=move || dragging.get()
            on:dragenter=move |ev| {
                if has_files(&ev) {
                    ev.prevent_default();
                    drag_depth.update(|d| *d += 1);
                }
            }
            on:dragover=move |ev| {
                if has_files(&ev) {
                    ev.prevent_default();
                    // The OS cursor shows the + copy badge instead of the
                    // browser default.
                    #[cfg(target_arch = "wasm32")]
                    if let Some(dt) = ev.data_transfer() {
                        dt.set_drop_effect("copy");
                    }
                }
            }
            on:dragleave=move |_| drag_depth.update(|d| *d = (*d - 1).max(0))
            on:drop=move |ev| {
                ev.prevent_default();
                drag_depth.set(0);
                #[cfg(target_arch = "wasm32")]
                if let Some(list) = ev.data_transfer().and_then(|dt| dt.files()) {
                    crate::upload::upload_file_list(app, list);
                }
            }
        >
        <button class="rh-back" on:click=leave><span class="rh-back-icon" inner_html=crate::icons::chevron_left_icon()></span>"All areas"</button>
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
            <Show when=move || may_manage_files(app) fallback=|| ()>
                <NewFolder/>
            </Show>
            // A real picker (the old button uploaded a hardcoded "note.txt").
            // The input is visually hidden; the button drives it, so the control
            // is a proper button with a label instead of a raw file input.
            <input
                type="file"
                multiple
                class="rh-visually-hidden"
                node_ref=picker
                aria-hidden="true"
                tabindex="-1"
                on:change=move |ev| {
                    #[cfg(target_arch = "wasm32")]
                    {
                        use wasm_bindgen::JsCast;
                        if let Some(input) = ev
                            .target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                        {
                            if let Some(list) = input.files() {
                                crate::upload::upload_file_list(app, list);
                            }
                            // Clear, so picking the same file twice still fires.
                            input.set_value("");
                        }
                    }
                    let _ = &ev;
                }
            />
            <button
                class="rh-btn small"
                on:click=move |_| {
                    if let Some(input) = picker.get() {
                        input.click();
                    }
                }
            >
                <span class="rh-btn-icon" inner_html=crate::icons::upload_icon()></span>"Upload\u{2026}"
            </button>
            <span class="rh-toolbar-hint">"or drop files here"</span>
            {move || files
                .with(|f| f.limits.as_ref().and_then(crate::upload::limits_line))
                .map(|line| view! { <span class="rh-toolbar-limits">{line}</span> })}
        </div>
        <MoveBar/>
        <h2 class="rh-visually-hidden">"Folder contents"</h2>
        // Type-to-filter over the current folder — the fastest way through a
        // busy library, matching on name or uploader.
        <input
            class="rh-input rh-file-filter"
            type="search"
            placeholder="Filter this folder\u{2026}"
            aria-label="Filter files by name or uploader"
            prop:value=filter
            on:input=move |ev| filter.set(event_target_value(&ev))
        />
        <Show when=move || files.with(|f| f.nodes.is_empty()) fallback=|| ()>
            <EmptyState
                icon="/files"
                title="Nothing in here"
                sub="This folder has no files yet."
            />
        </Show>
        // A dense columnar table (name · size · kind · uploader · added), the
        // Hotline file browser reinterpreted — not a card gallery.
        <Show when=move || !visible().is_empty() fallback=|| ()>
            <div class="rh-filetable-head" aria-hidden="true">
                <span class="rh-fcol-name">"Name"</span>
                <span class="rh-fcol-size">"Size"</span>
                <span class="rh-fcol-kind">"Kind"</span>
                <span class="rh-fcol-who">"Uploader"</span>
                <span class="rh-fcol-when">"Added"</span>
            </div>
        </Show>
        <Show
            when=move || files.with(|f| !f.nodes.is_empty()) && visible().is_empty()
            fallback=|| ()
        >
            <p class="rh-empty">"Nothing matches that filter."</p>
        </Show>
        <ul class="rh-tree rh-filetable" tabindex="0" aria-label="Files" on:keydown:undelegated=|ev| crate::keynav::handle(&ev, ".rh-file-link")>
            <For
                each=visible
                // A row shows its name and size: a rename must re-render it.
                key=|n| (n.id, n.name.clone(), n.size)
                children=move |n| {
                    let id = n.id;
                    let is_folder = n.kind == KIND_FOLDER;
                    let name = n.name.clone();
                    // A drawn glyph, not the 📁/📄 emoji: this table is the
                    // pane users compare directly to Finder.
                    let icon = crate::icons::file_icon(is_folder);
                    let size = if is_folder {
                        "\u{2014}".to_string()
                    } else {
                        human_size(n.size)
                    };
                    let kind = node_kind_label(n.kind).to_string();
                    let who = if n.uploader.is_empty() {
                        "\u{2014}".to_string()
                    } else {
                        n.uploader.clone()
                    };
                    let when = crate::files::relative_day(
                        n.created_at_unix,
                        crate::clock::now_ms() / 1000,
                    );
                    let folder_name = store_value(n.name.clone());
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
                                tabindex="-1"
                                aria-current=move || selected().then_some("true")
                                on:click=on_click
                                // Double-click gets the file — Finder's and
                                // Hotline's contract, and this table's whole
                                // premise. Folders already opened on the first
                                // click. The filename stays selectable, so
                                // clear the text selection double-click paints.
                                on:dblclick=move |_| {
                                    #[cfg(target_arch = "wasm32")]
                                    {
                                        // Reflect, not web_sys::Selection: the
                                        // crate builds without the Selection
                                        // feature and one call doesn't earn it.
                                        use js_sys::{Function, Reflect};
                                        use wasm_bindgen::JsCast;
                                        if let Some(w) = web_sys::window() {
                                            let _ = Reflect::get(&w, &"getSelection".into())
                                                .ok()
                                                .and_then(|f| f.dyn_into::<Function>().ok())
                                                .and_then(|f| f.call0(&w).ok())
                                                .and_then(|sel| {
                                                    Reflect::get(&sel, &"removeAllRanges".into())
                                                        .ok()
                                                        .and_then(|f| f.dyn_into::<Function>().ok())
                                                        .map(|f| f.call0(&sel))
                                                });
                                        }
                                    }
                                    if !is_folder {
                                        app.download(id);
                                    }
                                }
                            >
                                <span class="rh-fcol-name">
                                    <span class="rh-file-icon" aria-hidden="true" inner_html=icon></span>
                                    <span class="rh-file-name">{n.name.clone()}</span>
                                </span>
                                <span class="rh-fcol-size">{size}</span>
                                <span class="rh-fcol-kind">{kind}</span>
                                <span class="rh-fcol-who">{who}</span>
                                <span class="rh-fcol-when">{when}</span>
                            </button>
                            // A folder opens on its first click, so it is never
                            // "selected" the way a file is: its Send and Remove
                            // live on the row.
                            <Show when=move || is_folder && app.can_send() fallback=|| ()>
                                <button
                                    type="button"
                                    class="rh-file-row-send"
                                    class:beside=move || may_manage_files(app)
                                    aria-label=format!("Send the folder {} to another burrow", folder_name.get_value())
                                    on:click=move |_| app.ask_send(id, &folder_name.get_value(), true)
                                >
                                    "Send\u{2026}"
                                </button>
                            </Show>
                            <Show when=move || is_folder && may_manage_files(app) fallback=|| ()>
                                <button
                                    type="button"
                                    class="rh-file-row-remove"
                                    aria-label=format!("Remove the folder {}", folder_name.get_value())
                                    on:click=move |_| app.confirm.set(Some(
                                        crate::app::ConfirmAsk::delete_node(id, &folder_name.get_value(), true),
                                    ))
                                >
                                    "Remove"
                                </button>
                            </Show>
                        </li>
                    }
                }
            />
        </ul>
        </div>
    }
}

/// The metadata card and download action for the selected file.
#[component]
fn FileDetail() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.focused().files;
    view! {
        <Show
            when=move || files.with(|f| f.selected_node().is_some())
            fallback=|| view! {
                <EmptyState
                    icon="/files"
                    title="No file selected"
                    sub="Choose a file to see its details."
                />
            }
        >
            {move || {
                files.with(|f| {
                    f.selected_node().map(|n| {
                        let id = n.id;
                        // Owned, so the management card can outlive this borrow.
                        let managed = n.clone();
                        let send_name = store_value(n.name.clone());
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
                                <div class="rh-card-actions">
                                    <button class="rh-btn" on:click=move |_| app.download(id)>
                                        "Download"
                                    </button>
                                    <Show when=move || app.can_send() fallback=|| ()>
                                        <button
                                            type="button"
                                            class="rh-btn ghost"
                                            on:click=move |_| app.ask_send(id, &send_name.get_value(), false)
                                        >
                                            "Send to another burrow\u{2026}"
                                        </button>
                                    </Show>
                                </div>
                                <Show when=move || may_manage_files(app) fallback=|| ()>
                                    <NodeManagement node=managed.clone()/>
                                </Show>
                                // Where it comes from is a choice only the
                                // desktop app has: a browser tab cannot reach
                                // peers, so it has the burrow and nothing to pick.
                                <Show
                                    when=move || {
                                        app.download_prefs.with(Option::is_some)
                                            && app.focused_tracked().live.get()
                                    }
                                    fallback=|| ()
                                >
                                    <div class="rh-download-from">
                                        <span class="rh-download-from-label" id="rh-download-from">
                                            "Get it from"
                                        </span>
                                        <div class="rh-seg" role="radiogroup" aria-labelledby="rh-download-from">
                                            {crate::settings::DownloadFrom::ALL
                                                .into_iter()
                                                .map(|choice| view! {
                                                    <button
                                                        type="button"
                                                        class="rh-seg-btn"
                                                        class:on=move || app.settings.get().download_from == choice
                                                        role="radio"
                                                        aria-checked=move || {
                                                            (app.settings.get().download_from == choice).to_string()
                                                        }
                                                        on:click=move |_| {
                                                            app.settings.update(|s| s.download_from = choice);
                                                            app.save_settings();
                                                        }
                                                    >
                                                        {choice.label()}
                                                    </button>
                                                })
                                                .collect_view()}
                                        </div>
                                        <p class="rh-hint">
                                            {move || app.settings.get().download_from.explains()}
                                        </p>
                                    </div>
                                </Show>
                            </div>
                        }
                    })
                })
            }}
        </Show>
    }
}

/// Whether this session may manage files here: the burrow's `FILE_MANAGE`
/// capability. The burrow decides; this only keeps controls that would be
/// refused off the screen.
fn may_manage_files(app: AppState) -> bool {
    const FILE_MANAGE: u64 = 1 << 31;
    app.focused().caps.get() & FILE_MANAGE != 0
}

/// "New folder" where Files is looking, with the drop-box choice.
#[component]
fn NewFolder() -> impl IntoView {
    let app = expect_context::<AppState>();
    let open = create_rw_signal(false);
    let name = create_rw_signal(String::new());
    let dropbox = create_rw_signal(false);
    let ready = move || {
        let n = name.get();
        !n.trim().is_empty() && !n.contains('/')
    };
    view! {
        <button
            type="button"
            class="rh-btn ghost small"
            aria-expanded=move || open.get().to_string()
            on:click=move |_| open.update(|o| *o = !*o)
        >
            "New folder"
        </button>
        <Show when=move || open.get() fallback=|| ()>
            <form
                class="rh-newfolder"
                on:submit=move |ev| {
                    ev.prevent_default();
                    if ready() {
                        app.create_folder_here(&name.get(), dropbox.get());
                        name.set(String::new());
                        dropbox.set(false);
                        open.set(false);
                    }
                }
            >
                <input
                    class="rh-input"
                    aria-label="Folder name"
                    placeholder="Folder name"
                    maxlength="120"
                    prop:value=move || name.get()
                    on:input=move |ev| name.set(event_target_value(&ev))
                />
                <label class="rh-settings-check">
                    <input
                        type="checkbox"
                        prop:checked=move || dropbox.get()
                        on:change=move |ev| dropbox.set(event_target_checked(&ev))
                    />
                    <span>"Drop box: people can put things in and not see what is there"</span>
                </label>
                <button type="submit" class="rh-btn small" disabled=move || !ready()>
                    "Create"
                </button>
            </form>
        </Show>
    }
}

/// What a file manager can do to the selected file or folder: describe it,
/// or remove it. The burrow refuses anyone else.
#[component]
fn NodeManagement(node: rabbithole_proto::filelib::FileNodeView) -> impl IntoView {
    let app = expect_context::<AppState>();
    let id = node.id;
    let is_folder = node.kind == KIND_FOLDER;
    let name = store_value(node.name.clone());
    let icon = store_value(node.icon.clone());
    let saved = store_value(node.comment.clone());
    let comment = create_rw_signal(node.comment.clone());
    let changed = move || comment.get().trim() != saved.get_value();
    let new_name = create_rw_signal(node.name.clone());
    let name_ready = move || {
        let n = new_name.get();
        let n = n.trim();
        !n.is_empty() && !n.contains('/') && n != name.get_value()
    };
    view! {
        <div class="rh-node-manage">
            <Show when=move || !is_folder fallback=|| ()>
                <label class="rh-adm-field">
                    <span>"Description"</span>
                    <span class="rh-adm-inline">
                        <input
                            class="rh-input"
                            maxlength="500"
                            prop:value=move || comment.get()
                            on:input=move |ev| comment.set(event_target_value(&ev))
                        />
                        <button
                            type="button"
                            class="rh-btn ghost small"
                            disabled=move || !changed()
                            on:click=move |_| app.describe_node(
                                id,
                                &name.get_value(),
                                &icon.get_value(),
                                &comment.get(),
                            )
                        >
                            "Save"
                        </button>
                    </span>
                </label>
            </Show>
            <label class="rh-adm-field">
                <span>"Name"</span>
                <span class="rh-adm-inline">
                    <input
                        class="rh-input"
                        maxlength="128"
                        prop:value=move || new_name.get()
                        on:input=move |ev| new_name.set(event_target_value(&ev))
                    />
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        disabled=move || !name_ready()
                        on:click=move |_| app.rename_node(id, &name.get_value(), &new_name.get())
                    >
                        "Rename"
                    </button>
                </span>
            </label>
            <div class="rh-node-manage-row">
                <button
                    type="button"
                    class="rh-btn ghost small"
                    on:click=move |_| app.pick_up_node(id, &name.get_value(), is_folder)
                >
                    "Move\u{2026}"
                </button>
                <button
                    type="button"
                    class="rh-btn ghost small rh-adm-danger"
                    on:click=move |_| app.confirm.set(Some(crate::app::ConfirmAsk::delete_node(
                    id,
                    &name.get_value(),
                    is_folder,
                )))
            >
                {if is_folder { "Remove folder\u{2026}" } else { "Remove file\u{2026}" }}
                </button>
            </div>
        </div>
    }
}

/// While something is carried: what it is, and where it can be put down.
/// The person walks to the folder with the ordinary listing; this bar
/// follows them there.
#[component]
fn MoveBar() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.focused().files;
    let carried = move || files.with(|f| f.carrying.clone());
    let why_not = move || files.with(|f| f.cannot_put_down_here());
    view! {
        <Show when=move || carried().is_some() fallback=|| ()>
            <div class="rh-move-bar" role="status">
                <span class="rh-move-bar-text">
                    "Moving "
                    <strong>{move || carried().map(|c| c.name).unwrap_or_default()}</strong>
                    ". "
                    <span class="rh-move-bar-why">
                        {move || why_not().unwrap_or("Open the folder it belongs in, then put it down.")}
                    </span>
                </span>
                <span class="rh-move-bar-actions">
                    <button
                        type="button"
                        class="rh-btn small"
                        disabled=move || why_not().is_some()
                        on:click=move |_| app.move_node_here()
                    >
                        "Move here"
                    </button>
                    <button type="button" class="rh-btn ghost small" on:click=move |_| app.put_down_node()>
                        "Cancel"
                    </button>
                </span>
            </div>
        </Show>
    }
}

/// The transfer queue: queued / active / done / failed with progress bars.
#[component]
fn TransferQueue() -> impl IntoView {
    let app = expect_context::<AppState>();
    let files = app.focused().files;
    view! {
        <Show when=move || files.with(|f| !f.transfers.is_empty()) fallback=|| ()>
            <h2 class="rh-panel-title">"Transfers"</h2>
            <ul class="rh-queue">
                <For
                    each=move || files.with(|f| f.transfers.clone())
                    key=|t| format!("{}:{}:{:?}:{:?}", t.id, t.percent(), t.status, t.error)
                    children=move |t| {
                        let pct = t.percent();
                        let (badge, bar) = match t.status {
                            TransferStatus::Queued => ("rh-badge", "rh-bar-fill"),
                            TransferStatus::Active => ("rh-badge active", "rh-bar-fill"),
                            TransferStatus::Done => ("rh-badge done", "rh-bar-fill"),
                            TransferStatus::Failed => ("rh-badge failed", "rh-bar-fill failed"),
                        };
                        let cancelled = t.error.as_deref() == Some(crate::upload::CANCELLED);
                        let status = match t.status {
                            TransferStatus::Queued => "queued",
                            TransferStatus::Active => "active",
                            TransferStatus::Done => "done",
                            TransferStatus::Failed if cancelled => "cancelled",
                            TransferStatus::Failed => "failed",
                        };
                        let width = format!("transform:scaleX({})", f64::from(pct) / 100.0);
                        let bar_label = format!("{} transfer progress", t.name);
                        let cancel = (t.dir == crate::files::TransferDir::Upload
                            && (t.status == TransferStatus::Queued
                                || (t.status == TransferStatus::Active && t.done < t.total)))
                            .then_some(t.id);
                        let why = t
                            .error
                            .clone()
                            .filter(|_| t.status == TransferStatus::Failed && !cancelled);
                        view! {
                            <li class="rh-queue-item">
                                <div class="rh-queue-head">
                                    <span class="rh-queue-name">{t.name.clone()}</span>
                                    <span class=badge>{status}</span>
                                    <span class="rh-queue-pct">{format!("{pct}%")}</span>
                                    {cancel.map(|key| view! {
                                        <button
                                            type="button"
                                            class="rh-btn ghost small"
                                            on:click=move |_| app.cancel_upload(key)
                                        >
                                            "Cancel"
                                        </button>
                                    })}
                                </div>
                                {why.map(|why| view! { <p class="rh-queue-why">{why}</p> })}
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
/// the player. All state and logic live in the host-tested [`crate::radio`]
/// (reducer, prefs, stream-address derivation); the wasm-only `<audio>`
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
                    fallback=|| view! {
                        <EmptyState
                            icon="/radio"
                            title="Off the air"
                            sub="Nobody is broadcasting right now."
                        />
                    }
                >
                    <ul class="rh-tree">
                        <For
                            each=stations
                            key=|s| format!("{}:{}:{}:{}:{}", s.station, s.live, s.listeners, s.title, s.name)
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
                                                <span class="rh-station-name">{s.display_name().to_string()}</span>
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
            <section class="rh-panel rh-player" aria-label="Radio player">
                <RadioPlayerPanel/>
            </section>
        </main>
    }
}

/// The player: what is on now (with its cover), the controls, and what played
/// before. Where the audio comes from is the burrow's to say
/// ([`crate::radio::RadioState::stream_url`]); this pane never asks the person
/// for an address. When there is nothing to tune in to, it says which kind of
/// nothing, because "Listen" greyed out with no reason is a dead end.
#[component]
fn RadioPlayerPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let prefs = app.radio_prefs;
    let radio = app.radio;

    // The station on show: the one picked, else whatever is on the air, so
    // the pane is never blank while something is playing.
    let shown = move || {
        radio.with(|r| {
            prefs
                .with(|p| p.station.clone())
                .and_then(|slug| r.get(&slug).cloned())
                .or_else(|| r.on_air().cloned())
        })
    };
    let url = move || shown().and_then(|s| radio.with(|r| r.stream_url(&s.station)));
    let ready = move || url().is_some();
    let enabled = move || prefs.with(|p| p.enabled);
    let muted = move || prefs.with(|p| p.muted);
    let volume_pct = move || (prefs.with(|p| p.volume) * 100.0).round() as i32;
    let cover = move || {
        shown()
            .and_then(|s| s.cover)
            .map(|id| crate::wire::id_to_hex(&id))
            .and_then(|hex| app.radio_covers.with(|m| m.get(&hex).cloned()))
    };
    // Why there is nothing to listen to, when there isn't.
    let reason = move || {
        if ready() {
            return None;
        }
        let station = shown()?;
        Some(if !app.focused_tracked().live.get() {
            "A seeded demo burrow has no audio to stream.".to_string()
        } else if radio.with(|r| r.tuning().is_none()) {
            "Asking the burrow where to tune in\u{2026}".to_string()
        } else if radio.with(|r| r.radio_is_off()) {
            "This burrow isn\u{2019}t streaming audio: its radio listener is off.".to_string()
        } else {
            format!(
                "{} shows what it would play, but nothing is feeding it audio right now.",
                station.display_name()
            )
        })
    };

    view! {
        <h2 class="rh-panel-title">"Now playing"</h2>
        <Show
            when=move || shown().is_some()
            fallback=|| view! {
                <EmptyState
                    icon="/radio"
                    title="Nothing on"
                    sub="When a station goes on the air, it plays here."
                />
            }
        >
            <div class="rh-player-now">
                {move || match cover() {
                    Some(src) => view! {
                        <img class="rh-player-cover" src=src alt="" width="176" height="176"/>
                    }
                    .into_view(),
                    None => {
                        let style = shown()
                            .map(|s| crate::radio::sleeve_style(&s.title, &s.artist))
                            .unwrap_or_default();
                        view! { <div class="rh-player-cover" style=style aria-hidden="true"></div> }
                            .into_view()
                    }
                }}
                <div class="rh-player-track" role="status">
                    <p class="rh-player-title">{move || shown().map(|s| s.title)}</p>
                    <p class="rh-player-artist">{move || shown().map(|s| s.artist)}</p>
                    <p class="rh-player-station">
                        {move || shown().map(|s| {
                            let who = if s.live { format!("DJ {}", s.dj) } else { s.dj.clone() };
                            format!(
                                "{} \u{b7} {} \u{b7} {} listening",
                                s.display_name(),
                                who,
                                s.listeners
                            )
                        })}
                    </p>
                </div>
            </div>
            <fieldset class="rh-fieldset rh-toolbar rh-player-controls">
                <legend class="rh-visually-hidden">"Playback controls"</legend>
                <button
                    class="rh-btn"
                    disabled=move || !ready()
                    on:click=move |_| {
                        // Nothing picked yet: Listen tunes in to what is on show.
                        if prefs.with_untracked(|p| p.station.is_none()) {
                            if let Some(s) = shown() {
                                app.select_station(&s.station);
                            }
                        }
                        app.set_radio_enabled(!prefs.get_untracked().enabled)
                    }
                >
                    {move || if enabled() && ready() { "Stop" } else { "Listen" }}
                </button>
                <button
                    class="rh-btn ghost"
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
            {move || reason().map(|why| view! { <p class="rh-hint">{why}</p> })}
            <h3 class="rh-player-heading">"Recently played"</h3>
            <Show
                when=move || shown().is_some_and(|s| !s.recent.is_empty())
                fallback=|| view! { <p class="rh-hint">"Nothing yet. This is the first track since it came on."</p> }
            >
                <ol class="rh-player-recent">
                    {move || shown().map(|s| {
                        s.recent
                            .into_iter()
                            .map(|p| {
                                let when = (p.started_unix_ms != 0)
                                    .then(|| crate::clock::local_hhmm(p.started_unix_ms as i64));
                                view! {
                                    <li>
                                        <span class="rh-player-recent-title">{p.title}</span>
                                        <span class="rh-player-recent-artist">{p.artist}</span>
                                        {when.map(|w| view! { <span class="rh-player-recent-when">{w}</span> })}
                                    </li>
                                }
                            })
                            .collect_view()
                    })}
                </ol>
            </Show>
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

/// The feeds half of Federation & feeds: the read-only feed table and the
/// live feed and gateway counters (ADMIN 45/46). The gateway switches and the
/// poll interval used to live here as a second, separate editor with its own
/// Save; they are ordinary settings now ([`crate::admin_view`]). State and
/// logic stay in the host-tested [`crate::syndication_admin`].
#[component]
pub(crate) fn SyndicationPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let syn = app.syndication;

    let feeds_unavailable = move || syn.with(|s| s.feeds == FeedsStatus::Unavailable);
    let feeds_loaded = move || syn.with(|s| matches!(s.feeds, FeedsStatus::Listed(_)));
    let feed_rows = move || syn.with(|s| s.feed_rows());
    let has_stats = move || syn.with(|s| s.stats.is_some());
    let gateway_stats = move || syn.with(|s| s.gateway_stats().to_vec());

    view! {
        <h3 class="rh-adm-group-h">"Mapped feeds"</h3>
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
                            <th scope="col">"Last poll"</th>
                            <th scope="col">"State"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For
                            each=feed_rows
                            key=|f| f.url.clone()
                            children=move |f| {
                                let url = f.url.clone();
                                let last = {
                                    let url = url.clone();
                                    move || syn.with(|s| match s.feed_stat(&url) {
                                        Some(stat) => last_poll_label(
                                            stat.last_poll_ms,
                                            crate::clock::now_ms(),
                                        ),
                                        None => "\u{2014}".to_string(),
                                    })
                                };
                                let state = {
                                    let url = url.clone();
                                    move || syn.with(|s| match s.feed_stat(&url) {
                                        Some(stat) => feed_stat_line(stat),
                                        None => s.feed_state_line(),
                                    })
                                };
                                view! {
                                    <tr>
                                        <td class="rh-member-name">{f.url}</td>
                                        <td class="rh-member-handle">{f.board}</td>
                                        <td class="rh-file-meta">{last}</td>
                                        <td class="rh-file-meta">{state}</td>
                                    </tr>
                                }
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

        <h3 class="rh-adm-group-h">"Activity"</h3>
        <Show
            when=has_stats
            fallback=|| view! {
                <p class="rh-hint">
                    "Waiting for a live snapshot of poll outcomes and \
                     gateway counters."
                </p>
            }
        >
            <Show
                when=move || !gateway_stats().is_empty()
                fallback=|| view! {
                    <p class="rh-hint">"No gateway activity recorded this run."</p>
                }
            >
                <table class="rh-table">
                    <thead>
                        <tr>
                            <th scope="col">"Gateway"</th>
                            <th scope="col">"State"</th>
                            <th scope="col">"Counters"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For
                            each=gateway_stats
                            key=|g| g.name.clone()
                            children=move |g| {
                                let (dot, state_text) = if g.enabled {
                                    ("rh-dot on", "enabled")
                                } else {
                                    ("rh-dot off", "disabled")
                                };
                                let counters = if g.counters.is_empty() {
                                    "\u{2014}".to_string()
                                } else {
                                    g.counters
                                        .iter()
                                        .map(|(n, v)| format!("{n} {v}"))
                                        .collect::<Vec<_>>()
                                        .join(" \u{00b7} ")
                                };
                                view! {
                                    <tr>
                                        <td class="rh-member-name">{g.name}</td>
                                        <td>
                                            <span class=dot aria-hidden="true"></span>
                                            <span class="rh-visually-hidden">{state_text}</span>
                                        </td>
                                        <td class="rh-file-meta">{counters}</td>
                                    </tr>
                                }
                            }
                        />
                    </tbody>
                </table>
            </Show>
            <p class="rh-hint">
                "In-memory this run: last poll, conditional-GET 304s, and \
                 dedupe hits reset when the burrow restarts."
            </p>
        </Show>
    }
}

/// Theme editor: edit a pack's tokens with live scoped preview, WCAG
/// contrast warnings, import/export as shareable JSON token files, and an
/// apply-to-my-session action through the custom-pack override slot. All
/// state and validation live in the host-tested [`crate::theme_editor`]; this
/// component only folds [`EditorAction`]s into an `RwSignal<EditorState>`.
/// Admin-gated by virtue of rendering inside [`Admin`]'s capability guard.
#[component]
pub(crate) fn ThemeEditorPanel() -> impl IntoView {
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
                    title="Apply this pack as the burrow theme (CONFIG_ADMIN). Scanlines, fonts, and shadows stay local — the server only accepts hex colours and CSS lengths."
                    on:click=move |_| {
                        let cmd = editor.try_update(|e| e.publish_command()).flatten();
                        if let Some(AdminCommand::SetThemeBundle { bundle }) = cmd {
                            app.publish_theme(bundle);
                        }
                    }
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
