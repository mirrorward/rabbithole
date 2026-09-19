//! The admin console: a sections navbar beside one pane, and one Save for
//! everything staged in it.
//!
//! The words come from [`crate::admin_catalog`], the facts (value, default,
//! shape, choices, live or restart) from the burrow by way of
//! [`crate::admin_settings`]. This file only lays them out:
//!
//! - a true/false setting is a switch, a fixed list is a dropdown, a number is
//!   a number field that says what the number means (`1 GiB`, `2 hours`);
//! - a row that differs from the burrow's default offers Reset;
//! - nothing has its own Save. Edits are staged, and a bar at the foot of the
//!   pane says how many there are and saves them together.

use leptos::*;
use leptos_router::{use_navigate, use_params_map, A};

use crate::a11y;
use crate::admin_catalog::{self as catalog, Area, Group, Item, Pane, Section, Unit, SECTIONS};
use crate::admin_settings::{humanize_bytes, humanize_secs, Kind, Load, Outcome};
use crate::app::AppState;
use crate::components::{
    AdminAccountsPanel, AdminClassesPanel, AdminModerationPanel, EmptyState, StatusBar,
    SyndicationPanel, ThemeEditorPanel,
};

/// The console. Gated behind the session's admin capability; the nav entry
/// that reaches it is gated the same way.
#[component]
pub fn Admin() -> impl IntoView {
    let app = expect_context::<AppState>();
    let is_admin = app.focused().is_admin;
    let params = use_params_map();
    let section = create_memo(move |_| {
        params.with(|p| catalog::section(p.get("section").map(String::as_str)).id)
    });

    create_effect(move |_| {
        if is_admin.get() {
            app.load_config();
            app.load_classes();
            app.load_accounts();
            app.load_syndication();
        }
    });

    // `/admin` on its own is the first section, under its own address, so the
    // navbar always has a current entry and Back behaves.
    let navigate = use_navigate();
    create_effect(move |_| {
        if params.with(|p| p.get("section").is_none()) {
            navigate(
                &format!("/admin/{}", SECTIONS[0].id),
                leptos_router::NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
        }
    });

    // Command-S saves, as it does everywhere else on a desktop.
    let on_key = window_event_listener(ev::keydown, move |ev| {
        if (ev.meta_key() || ev.ctrl_key()) && ev.key().eq_ignore_ascii_case("s") {
            ev.prevent_default();
            app.save_admin_settings();
        }
    });
    on_cleanup(move || on_key.remove());

    view! {
        <StatusBar/>
        <main class="rh-admin-main" id=a11y::MAIN_ID tabindex="-1">
            <h1 class="rh-visually-hidden" id=a11y::VIEW_TITLE_ID tabindex="-1">"Admin"</h1>
            <Show
                when=move || is_admin.get()
                fallback=|| view! {
                    <div class="rh-body">
                        <section class="rh-panel">
                            <EmptyState
                                icon="/admin"
                                title="Operators only"
                                sub="This console needs an admin account on this burrow."
                            />
                        </section>
                    </div>
                }
            >
                <div class="rh-adm">
                    <SectionNav current=section/>
                    <div class="rh-adm-pane">
                        {move || {
                            let s = catalog::section(Some(section.get()));
                            view! { <SectionPane section=s/> }
                        }}
                        <SaveBar/>
                    </div>
                </div>
            </Show>
        </main>
    }
}

/// Whether any of a section's settings has an unsaved change.
fn section_is_dirty(app: AppState, section: &'static Section) -> bool {
    app.admin_settings.with(|s| match section.pane {
        Pane::Advanced => s
            .settings
            .iter()
            .any(|x| s.is_dirty(&x.key) && !catalog::is_described(&x.key)),
        _ => section
            .groups
            .iter()
            .flat_map(|g| g.items.iter())
            .any(|i| s.is_dirty(i.key)),
    })
}

/// The sections navbar: a list beside the pane, and a dropdown where there is
/// no room for a list.
#[component]
fn SectionNav(current: Memo<&'static str>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let navigate = use_navigate();
    let links = |area: Area| {
        SECTIONS
            .iter()
            .filter(move |s| s.area == area)
            .map(move |s| {
                let dirty = create_memo(move |_| section_is_dirty(app, s));
                view! {
                    <li>
                        <A href=format!("/admin/{}", s.id) class="rh-adm-link">
                            <span>{s.title}</span>
                            <Show when=move || dirty.get() fallback=|| ()>
                                <span class="rh-adm-link-dot" title="Unsaved changes">
                                    <span class="rh-visually-hidden">"unsaved changes"</span>
                                </span>
                            </Show>
                        </A>
                    </li>
                }
            })
            .collect_view()
    };
    view! {
        <nav class="rh-adm-nav" aria-label="Admin sections">
            <p class="rh-adm-nav-h" id="rh-adm-nav-settings">{Area::Settings.title()}</p>
            <ul aria-labelledby="rh-adm-nav-settings">{links(Area::Settings)}</ul>
            <p class="rh-adm-nav-h" id="rh-adm-nav-manage">{Area::Manage.title()}</p>
            <ul aria-labelledby="rh-adm-nav-manage">{links(Area::Manage)}</ul>
        </nav>
        <label class="rh-adm-jump">
            <span class="rh-visually-hidden">"Admin section"</span>
            <select
                class="rh-select"
                on:change=move |ev| navigate(
                    &format!("/admin/{}", event_target_value(&ev)),
                    Default::default(),
                )
            >
                {[Area::Settings, Area::Manage]
                    .into_iter()
                    .map(|area| view! {
                        <optgroup label=area.title()>
                            {SECTIONS
                                .iter()
                                .filter(|s| s.area == area)
                                .map(|s| view! {
                                    <option value=s.id selected=move || current.get() == s.id>
                                        {s.title}
                                    </option>
                                })
                                .collect_view()}
                        </optgroup>
                    })
                    .collect_view()}
            </select>
        </label>
    }
}

/// One section's pane: its heading, what it is for, and what it holds.
#[component]
fn SectionPane(section: &'static Section) -> impl IntoView {
    let app = expect_context::<AppState>();
    let status = move || app.admin.with(|a| a.status.clone());
    let body = match section.pane {
        Pane::Settings => view! { <SettingsGroups section=section/> }.into_view(),
        Pane::Feeds => view! {
            <SettingsGroups section=section/>
            <section class="rh-adm-group">
                <SyndicationPanel/>
            </section>
        }
        .into_view(),
        Pane::Advanced => view! { <AdvancedGroup/> }.into_view(),
        Pane::People => view! {
            <p class="rh-adm-status" role="status">{status}</p>
            <section class="rh-adm-group"><AdminAccountsPanel/></section>
            <section class="rh-adm-group"><AdminClassesPanel/></section>
        }
        .into_view(),
        Pane::Moderation => view! {
            <p class="rh-adm-status" role="status">{status}</p>
            <section class="rh-adm-group"><AdminModerationPanel/></section>
        }
        .into_view(),
        Pane::Appearance => view! {
            <p class="rh-adm-status" role="status">{status}</p>
            <section class="rh-adm-group"><ThemeEditorPanel/></section>
        }
        .into_view(),
    };
    view! {
        <header class="rh-adm-head">
            <h2 class="rh-adm-title">{section.title}</h2>
            <p class="rh-adm-blurb">{section.blurb}</p>
        </header>
        {body}
    }
}

/// A settings section's groups, or what stands in for them while the burrow
/// has not answered (or cannot).
#[component]
fn SettingsGroups(section: &'static Section) -> impl IntoView {
    let app = expect_context::<AppState>();
    let settings = app.admin_settings;
    let waiting = move || settings.with(|s| s.settings.is_empty() && s.load != Load::Legacy);
    let legacy = move || settings.with(|s| s.load == Load::Legacy);
    // Which of this section's settings the burrow actually has. A memo, so
    // typing in a row does not rebuild the rows.
    let present = create_memo(move |_| {
        settings.with(|s| {
            section
                .groups
                .iter()
                .flat_map(|g| g.items.iter())
                .filter(|i| s.get(i.key).is_some())
                .map(|i| i.key)
                .collect::<Vec<_>>()
        })
    });
    view! {
        <Show when=legacy fallback=|| ()>
            <p class="rh-adm-note">
                "This burrow runs an older version that cannot describe its settings. Only a \
                 few are shown, and none can be reset to a default."
            </p>
        </Show>
        <Show
            when=move || !waiting()
            fallback=|| view! {
                <div class="rh-adm-rows rh-adm-skeleton" aria-hidden="true">
                    <div></div><div></div><div></div>
                </div>
                <p class="rh-visually-hidden" role="status">"Asking the burrow for its settings."</p>
            }
        >
            {move || {
                let have = present.get();
                if have.is_empty() {
                    return view! {
                        <p class="rh-adm-note">"This burrow does not report these settings."</p>
                    }
                    .into_view();
                }
                section
                    .groups
                    .iter()
                    .filter(|g| g.items.iter().any(|i| have.contains(&i.key)))
                    .map(|g| {
                        let items: Vec<Item> = g
                            .items
                            .iter()
                            .filter(|i| have.contains(&i.key))
                            .copied()
                            .collect();
                        view! { <GroupView group=g items=items/> }
                    })
                    .collect_view()
            }}
        </Show>
    }
}

/// A key the burrow named, as the `&'static str` the catalog's rows are made
/// of. Each distinct key is kept once for the life of the page; a burrow has a
/// small, fixed set of them.
fn intern(key: String) -> &'static str {
    thread_local! {
        static KEYS: std::cell::RefCell<std::collections::HashSet<&'static str>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    KEYS.with(|keys| {
        let mut keys = keys.borrow_mut();
        match keys.get(key.as_str()) {
            Some(held) => held,
            None => {
                let held: &'static str = Box::leak(key.into_boxed_str());
                keys.insert(held);
                held
            }
        }
    })
}

/// Every setting the burrow reports that this client has no words for.
#[component]
fn AdvancedGroup() -> impl IntoView {
    let app = expect_context::<AppState>();
    let settings = app.admin_settings;
    let keys = create_memo(move |_| {
        settings.with(|s| {
            s.settings
                .iter()
                .map(|x| x.key.clone())
                .filter(|k| !catalog::is_described(k) && !catalog::owned_elsewhere(k))
                .collect::<Vec<_>>()
        })
    });
    view! {
        {move || {
            let keys = keys.get();
            if keys.is_empty() {
                return view! {
                    <p class="rh-adm-note">
                        "Nothing here. Every setting this burrow has is described in the \
                         sections above."
                    </p>
                }
                .into_view();
            }
            let items: Vec<Item> = keys
                .into_iter()
                .map(|k| catalog::undescribed(intern(k)))
                .collect();
            view! { <div class="rh-adm-rows">{items.into_iter().map(|i| view! { <SettingRow item=i/> }).collect_view()}</div> }
                .into_view()
        }}
    }
}

#[component]
fn GroupView(group: &'static Group, items: Vec<Item>) -> impl IntoView {
    let id = format!("rh-adm-g-{}", group.title.to_lowercase().replace(' ', "-"));
    view! {
        <section class="rh-adm-group" aria-labelledby=id.clone()>
            <h3 class="rh-adm-group-h" id=id>{group.title}</h3>
            {(!group.blurb.is_empty()).then(|| view! {
                <p class="rh-adm-group-blurb">{group.blurb}</p>
            })}
            <div class="rh-adm-rows">
                {items.into_iter().map(|i| view! { <SettingRow item=i/> }).collect_view()}
            </div>
        </section>
    }
}

/// What a number means, said beside its field.
fn echo(item: &Item, shown: &str) -> String {
    let Ok(n) = shown.trim().parse::<u64>() else {
        return String::new();
    };
    if n == 0 && !item.zero.is_empty() {
        return format!("0 means {}", item.zero);
    }
    match item.unit {
        Unit::Bytes if n >= 1024 => humanize_bytes(n),
        Unit::BytesPerSec if n >= 1024 => format!("{} per second", humanize_bytes(n)),
        Unit::Seconds if n >= 60 => humanize_secs(n),
        _ => String::new(),
    }
}

/// One setting: its name and what it does on the left, its control on the
/// right, and under the name whatever is worth saying about its state.
#[component]
fn SettingRow(item: Item) -> impl IntoView {
    let app = expect_context::<AppState>();
    let settings = app.admin_settings;
    let key = item.key;
    let Some(meta) = settings.with_untracked(|s| s.get(key).cloned()) else {
        return ().into_view();
    };
    let input_id = a11y::config_input_id(key);
    let help_id = format!("{input_id}-help");

    let shown = create_memo(move |_| settings.with(|s| s.shown(key)));
    let dirty = create_memo(move |_| settings.with(|s| s.is_dirty(key)));
    let can_reset = create_memo(move |_| settings.with(|s| s.can_reset(key)));
    let invalid = create_memo(move |_| settings.with(|s| s.invalid(key)));
    let outcome = create_memo(move |_| settings.with(|s| s.outcome(key).cloned()));
    let is_set = create_memo(move |_| settings.with(|s| s.get(key).is_some_and(|x| x.is_set)));
    let stage = move |value: String| settings.update(|s| s.stage(key, &value));

    let default_said = match (&meta.default, meta.secret, meta.kind) {
        (_, true, _) => "none".to_string(),
        (Some(d), _, Kind::Bool) => if d == "true" { "on" } else { "off" }.to_string(),
        (Some(d), _, Kind::Choice) => item.choice_label(d),
        (Some(d), ..) if d.is_empty() => "empty".to_string(),
        (Some(d), ..) => d.clone(),
        (None, ..) => String::new(),
    };
    let reset_label = format!("Reset {} to its default: {}", item.label, default_said);

    let control = if meta.read_only {
        let text = if meta.value.is_empty() {
            "Not set".to_string()
        } else {
            meta.value.clone()
        };
        view! { <span class="rh-adm-fixed" id=input_id.clone()>{text}</span> }.into_view()
    } else if meta.secret {
        view! {
            <input
                id=input_id.clone()
                class="rh-input rh-adm-text"
                type="password"
                autocomplete="new-password"
                aria-describedby=help_id.clone()
                placeholder=move || if is_set.get() { "Set. Type to replace it" } else { "Not set" }
                prop:value=move || shown.get()
                on:input=move |ev| stage(event_target_value(&ev))
            />
        }
        .into_view()
    } else {
        match meta.kind {
            Kind::Bool => view! {
                <input
                    id=input_id.clone()
                    class="rh-switch"
                    type="checkbox"
                    role="switch"
                    aria-describedby=help_id.clone()
                    prop:checked=move || shown.get() == "true"
                    on:change=move |ev| stage(event_target_checked(&ev).to_string())
                />
            }
            .into_view(),
            Kind::Choice => {
                let choices = meta.choices.clone();
                view! {
                    <select
                        id=input_id.clone()
                        class="rh-select rh-adm-choice"
                        aria-describedby=help_id.clone()
                        on:change=move |ev| stage(event_target_value(&ev))
                    >
                        {choices
                            .into_iter()
                            .map(|value| {
                                let label = item.choice_label(&value);
                                let this = value.clone();
                                view! {
                                    <option value=value selected=move || shown.get() == this>
                                        {label}
                                    </option>
                                }
                            })
                            .collect_view()}
                    </select>
                }
                .into_view()
            }
            Kind::Number => view! {
                <span class="rh-adm-number">
                    <input
                        id=input_id.clone()
                        class="rh-input rh-adm-num"
                        inputmode="numeric"
                        autocomplete="off"
                        aria-describedby=help_id.clone()
                        aria-invalid=move || invalid.get().is_some().to_string()
                        prop:value=move || shown.get()
                        on:input=move |ev| stage(event_target_value(&ev))
                    />
                    <span class="rh-adm-echo">{move || echo(&item, &shown.get())}</span>
                </span>
            }
            .into_view(),
            Kind::Text if item.long => view! {
                <textarea
                    id=input_id.clone()
                    class="rh-input rh-adm-long"
                    rows="4"
                    aria-describedby=help_id.clone()
                    prop:value=move || shown.get()
                    on:input=move |ev| stage(event_target_value(&ev))
                ></textarea>
            }
            .into_view(),
            Kind::Text => view! {
                <input
                    id=input_id.clone()
                    class="rh-input rh-adm-text"
                    autocomplete="off"
                    spellcheck="false"
                    aria-describedby=help_id.clone()
                    prop:value=move || shown.get()
                    on:input=move |ev| stage(event_target_value(&ev))
                />
            }
            .into_view(),
        }
    };

    let restart_note =
        (!meta.live && !meta.read_only).then_some("Applies when the burrow restarts.");
    let state_line = move || match (invalid.get(), outcome.get()) {
        (Some(why), _) => Some(("bad", why.to_string())),
        (None, Some(Outcome::Refused(why))) => Some(("bad", why)),
        (None, Some(Outcome::Saved { live: true })) => Some(("good", "Saved.".to_string())),
        (None, Some(Outcome::Saved { live: false })) => {
            Some(("good", "Saved. Restart the burrow to apply it.".to_string()))
        }
        (None, None) => None,
    };

    view! {
        <div
            class="rh-adm-row"
            class:inline=meta.kind == Kind::Bool && !meta.secret && !meta.read_only
            class:long=item.long
            class:edited=move || dirty.get()
        >
            <div class="rh-adm-row-text">
                <label class="rh-adm-label" for=input_id>{item.label}</label>
                {(!item.help.is_empty()).then(|| view! {
                    <p class="rh-adm-help" id=help_id.clone()>{item.help}</p>
                })}
                <p class="rh-adm-meta">
                    {restart_note.map(|n| view! { <span class="rh-adm-restart">{n}</span> })}
                    <Show when=move || dirty.get() fallback=|| ()>
                        <span class="rh-adm-edited">"Not saved yet"</span>
                    </Show>
                    <Show when=move || can_reset.get() fallback=|| ()>
                        <button
                            type="button"
                            class="rh-adm-reset"
                            title=reset_label.clone()
                            aria-label=reset_label.clone()
                            on:click=move |_| settings.update(|s| s.reset(key))
                        >
                            "Reset"
                        </button>
                    </Show>
                </p>
                {move || state_line().map(|(tone, text)| view! {
                    <p class="rh-adm-state" class:bad=tone == "bad" role="status">{text}</p>
                })}
            </div>
            <div class="rh-adm-control">{control}</div>
        </div>
    }
    .into_view()
}

/// The one Save. It appears when something is staged, says how much, and goes
/// away when there is nothing left to say.
#[component]
fn SaveBar() -> impl IntoView {
    let app = expect_context::<AppState>();
    let settings = app.admin_settings;
    let count = create_memo(move |_| settings.with(|s| s.dirty_count()));
    let restart = create_memo(move |_| settings.with(|s| s.restart_count()));
    let saving = create_memo(move |_| settings.with(|s| s.saving()));
    let can_save = create_memo(move |_| settings.with(|s| s.can_save()));
    let blocked = create_memo(move |_| {
        settings.with(|s| s.dirty_count() > 0 && !s.saving() && !s.can_save())
    });
    let headline = move || match count.get() {
        1 => "1 unsaved change".to_string(),
        n => format!("{n} unsaved changes"),
    };
    let detail = move || {
        if blocked.get() {
            "Fix the setting marked in red to save.".to_string()
        } else {
            match (restart.get(), count.get()) {
                (0, _) => String::new(),
                (1, 1) => "It applies when the burrow restarts.".to_string(),
                (r, c) if r == c => "They apply when the burrow restarts.".to_string(),
                (1, _) => "1 applies when the burrow restarts.".to_string(),
                (r, _) => format!("{r} apply when the burrow restarts."),
            }
        }
    };
    view! {
        <div
            class="rh-adm-savebar"
            class:show=move || { count.get() > 0 || saving.get() }
            role="region"
            aria-label="Unsaved changes"
        >
            <p class="rh-adm-savebar-text" role="status">
                <strong>{headline}</strong>
                <span>{detail}</span>
            </p>
            <button
                type="button"
                class="rh-btn ghost small"
                disabled=move || saving.get()
                on:click=move |_| settings.update(|s| s.discard())
            >
                "Discard"
            </button>
            <button
                type="button"
                class="rh-btn small"
                disabled=move || !can_save.get()
                on:click=move |_| app.save_admin_settings()
            >
                {move || if saving.get() { "Saving\u{2026}" } else { "Save changes" }}
            </button>
        </div>
    }
}
