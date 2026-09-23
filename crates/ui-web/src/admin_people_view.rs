//! The People pane of the admin console: accounts, the classes that decide
//! what they may do, and invitations.
//!
//! The rules (who may change whom, which roles and capabilities an operator
//! may hand out) are the burrow's, mirrored in [`crate::admin_people`] so this
//! pane never offers what would be refused. An account an operator cannot
//! change says why instead of showing controls that would fail.

use leptos::*;
use rabbithole_proto::admin::{AccountEntry, ClassEntry, InviteEntry};

use crate::admin_people::{self as people, InviteStatus, CAPS};
use crate::admin_settings::humanize_secs;
use crate::app::{AppState, ConfirmAsk};
use crate::components::copy_text_quiet;

/// The pane.
#[component]
pub fn PeoplePane() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.load_invites();
    view! {
        <Accounts/>
        <Classes/>
        <Invitations/>
    }
}

/// The accounts, with a filter, a form for a new one, and each row's controls.
#[component]
fn Accounts() -> impl IntoView {
    let app = expect_context::<AppState>();
    let filter = create_rw_signal(String::new());
    let adding = create_rw_signal(false);
    let open = create_rw_signal(None::<String>);
    let total = move || app.admin.with(|a| a.account_total);
    let shown = move || {
        let needle = filter.get().trim().to_lowercase();
        app.admin.with(|a| {
            a.accounts
                .iter()
                .filter(|acct| needle.is_empty() || acct.login.to_lowercase().contains(&needle))
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-accounts-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-accounts-h">
                    "Accounts"
                    <span class="rh-adm-count">{total}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <input
                        class="rh-input rh-adm-filter"
                        type="search"
                        aria-label="Find an account by login"
                        placeholder="Find"
                        prop:value=move || filter.get()
                        on:input=move |ev| {
                            let typed = event_target_value(&ev);
                            filter.set(typed.clone());
                            // What is loaded is narrowed as they type; a
                            // burrow with more accounts than that is asked,
                            // since the one they want may not be on this page.
                            app.find_accounts(&typed);
                        }
                    />
                    <button
                        type="button"
                        class="rh-btn small"
                        aria-expanded=move || adding.get().to_string()
                        on:click=move |_| adding.update(|a| *a = !*a)
                    >
                        "New account"
                    </button>
                </div>
            </div>
            <Show when=move || adding.get() fallback=|| ()>
                <NewAccount on_done=Callback::new(move |_| adding.set(false))/>
            </Show>
            <div class="rh-adm-rows">
                <For
                    each=shown
                    key=|a| (a.id, a.role, a.class.clone(), a.disabled)
                    children=move |a| view! { <AccountRow account=a open=open/> }
                />
                <Show when=move || shown().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">
                        {move || if filter.get().trim().is_empty() {
                            "No accounts yet."
                        } else {
                            "No account matches that."
                        }}
                    </p>
                </Show>
            </div>
        </section>
    }
}

/// The form for a new account. The role list stops at the operator's own.
#[component]
fn NewAccount(on_done: Callback<()>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let login = create_rw_signal(String::new());
    let password = create_rw_signal(String::new());
    let reveal = create_rw_signal(false);
    let role = create_rw_signal(1u8);
    let my_role = app.focused().role;
    let ready = move || !login.get().trim().is_empty() && password.get().chars().count() >= 8;
    // A success clears the form and closes it; a refusal leaves both as typed.
    create_effect(move |_| {
        let made = app.people.with(|p| p.created.clone());
        if made.is_some_and(|l| l == login.get_untracked().trim()) {
            app.people.update(|p| p.created = None);
            login.set(String::new());
            password.set(String::new());
            on_done.call(());
        }
    });
    view! {
        <form
            class="rh-adm-form"
            on:submit=move |ev| {
                ev.prevent_default();
                if ready() {
                    app.create_account(&login.get(), &password.get(), role.get());
                }
            }
        >
            <label class="rh-adm-field">
                <span>"Login"</span>
                <input
                    class="rh-input"
                    autocomplete="off"
                    spellcheck="false"
                    maxlength="32"
                    prop:value=move || login.get()
                    on:input=move |ev| login.set(event_target_value(&ev))
                />
            </label>
            <label class="rh-adm-field">
                <span>"Password"</span>
                <span class="rh-adm-inline">
                    <input
                        class="rh-input"
                        type=move || if reveal.get() { "text" } else { "password" }
                        autocomplete="new-password"
                        prop:value=move || password.get()
                        on:input=move |ev| password.set(event_target_value(&ev))
                    />
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        on:click=move |_| {
                            password.set(app.generate_password());
                            reveal.set(true);
                        }
                    >
                        "Generate"
                    </button>
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        on:click=move |_| reveal.update(|r| *r = !*r)
                    >
                        {move || if reveal.get() { "Hide" } else { "Show" }}
                    </button>
                </span>
                <small>"At least 8 characters. Give it to them some way other than this burrow."</small>
            </label>
            <label class="rh-adm-field">
                <span>"Role"</span>
                <select
                    class="rh-select"
                    on:change=move |ev| role.set(event_target_value(&ev).parse().unwrap_or(1))
                >
                    {move || people::assignable_roles(my_role.get())
                        .into_iter()
                        .map(|(n, name)| view! {
                            <option value=n.to_string() selected=move || role.get() == n>{name}</option>
                        })
                        .collect_view()}
                </select>
            </label>
            <div class="rh-adm-form-actions">
                <button type="button" class="rh-btn ghost small" on:click=move |_| on_done.call(())>
                    "Cancel"
                </button>
                <button type="submit" class="rh-btn small" disabled=move || !ready()>
                    "Create account"
                </button>
            </div>
        </form>
    }
}

/// One account: a summary line that opens its controls.
#[component]
fn AccountRow(account: AccountEntry, open: RwSignal<Option<String>>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let me = app.focused();
    let login = store_value(account.login.clone());
    let is_open = move || open.get().as_deref() == Some(login.get_value().as_str());
    let toggle = move |_| {
        let l = login.get_value();
        open.update(|o| {
            *o = if o.as_deref() == Some(l.as_str()) {
                None
            } else {
                Some(l)
            }
        });
    };
    let acct = store_value(account.clone());
    let blocked = move || people::why_not(me.role.get(), &me.handle.get(), &acct.get_value());
    let class = account.class.clone().unwrap_or_default();
    let disabled = account.disabled;
    let role = account.role;
    let is_me = move || me.handle.get() == login.get_value();

    let resetting = create_rw_signal(false);
    let new_password = create_rw_signal(String::new());

    view! {
        <div class="rh-adm-acct" class:open=is_open class:off=disabled>
            <button
                type="button"
                class="rh-adm-acct-line"
                aria-expanded=move || is_open().to_string()
                on:click=toggle
            >
                <span class="rh-adm-acct-name">
                    {account.login.clone()}
                    <Show when=is_me fallback=|| ()>
                        <span class="rh-badge">"You"</span>
                    </Show>
                </span>
                <span class="rh-adm-acct-role">{people::role_name(role)}</span>
                <span class="rh-adm-acct-class">
                    {if class.is_empty() { "No class".to_string() } else { class.clone() }}
                </span>
                <span class="rh-adm-acct-state">
                    {if disabled { "Disabled" } else { "Active" }}
                </span>
                <span class="rh-adm-chevron" aria-hidden="true" inner_html=crate::icons::chevron_down_icon()></span>
            </button>
            <Show when=is_open fallback=|| ()>
                <div class="rh-adm-acct-detail">
                    {move || match blocked() {
                        Some(why) => view! { <p class="rh-adm-help">{why}</p> }.into_view(),
                        None => {
                            let class_now = acct.get_value().class.unwrap_or_default();
                            view! {
                                <div class="rh-adm-acct-fields">
                                    <label class="rh-adm-field">
                                        <span>"Role"</span>
                                        <select
                                            class="rh-select"
                                            on:change=move |ev| {
                                                if let Ok(r) = event_target_value(&ev).parse::<u8>() {
                                                    app.set_account_role(&login.get_value(), r);
                                                }
                                            }
                                        >
                                            {people::assignable_roles(me.role.get())
                                                .into_iter()
                                                .map(|(n, name)| view! {
                                                    <option value=n.to_string() selected=n == role>{name}</option>
                                                })
                                                .collect_view()}
                                        </select>
                                    </label>
                                    <label class="rh-adm-field">
                                        <span>"Class"</span>
                                        <select
                                            class="rh-select"
                                            on:change=move |ev| app.set_account_class(
                                                &login.get_value(),
                                                &event_target_value(&ev),
                                            )
                                        >
                                            <option value="" selected=class_now.is_empty()>"No class"</option>
                                            {app.admin.with(|a| a.classes.clone())
                                                .into_iter()
                                                .map(|c| {
                                                    let on = c.name == class_now;
                                                    view! {
                                                        <option value=c.name.clone() selected=on>{c.name.clone()}</option>
                                                    }
                                                })
                                                .collect_view()}
                                        </select>
                                        <small>"A class decides what the account may do. The role decides who may manage it."</small>
                                    </label>
                                </div>
                                <div class="rh-adm-acct-actions">
                                    <button
                                        type="button"
                                        class="rh-btn ghost small"
                                        aria-expanded=move || resetting.get().to_string()
                                        on:click=move |_| resetting.update(|r| *r = !*r)
                                    >
                                        "Reset password\u{2026}"
                                    </button>
                                    <button
                                        type="button"
                                        class="rh-btn ghost small"
                                        on:click=move |_| app.confirm.set(
                                            Some(ConfirmAsk::reset_totp(&login.get_value())),
                                        )
                                    >
                                        "Remove two-factor\u{2026}"
                                    </button>
                                    {if disabled {
                                        view! {
                                            <button
                                                type="button"
                                                class="rh-btn small"
                                                on:click=move |_| app.set_account_disabled(&login.get_value(), false)
                                            >
                                                "Enable"
                                            </button>
                                        }
                                        .into_view()
                                    } else {
                                        view! {
                                            <button
                                                type="button"
                                                class="rh-btn ghost small rh-adm-danger"
                                                on:click=move |_| app.confirm.set(
                                                    Some(ConfirmAsk::disable_account(&login.get_value())),
                                                )
                                            >
                                                "Disable\u{2026}"
                                            </button>
                                        }
                                        .into_view()
                                    }}
                                    <button
                                        type="button"
                                        class="rh-btn ghost small rh-adm-danger"
                                        on:click=move |_| app.ask_remove_account(&login.get_value())
                                    >
                                        "Remove\u{2026}"
                                    </button>
                                </div>
                                <Show when=move || resetting.get() fallback=|| ()>
                                    <form
                                        class="rh-adm-reset-form"
                                        on:submit=move |ev| {
                                            ev.prevent_default();
                                            let pw = new_password.get();
                                            if pw.chars().count() >= 8 {
                                                app.set_account_password(&login.get_value(), &pw);
                                                new_password.set(String::new());
                                                resetting.set(false);
                                            }
                                        }
                                    >
                                        <label class="rh-adm-field">
                                            <span>"New password"</span>
                                            <span class="rh-adm-inline">
                                                <input
                                                    class="rh-input"
                                                    type="text"
                                                    autocomplete="off"
                                                    spellcheck="false"
                                                    prop:value=move || new_password.get()
                                                    on:input=move |ev| new_password.set(event_target_value(&ev))
                                                />
                                                <button
                                                    type="button"
                                                    class="rh-btn ghost small"
                                                    on:click=move |_| new_password.set(app.generate_password())
                                                >
                                                    "Generate"
                                                </button>
                                                <button
                                                    type="submit"
                                                    class="rh-btn small"
                                                    disabled=move || new_password.get().chars().count() < 8
                                                >
                                                    "Set password"
                                                </button>
                                            </span>
                                            <small>
                                                "They are signed out everywhere, and the old password stops working."
                                            </small>
                                        </label>
                                    </form>
                                </Show>
                            }
                            .into_view()
                        }
                    }}
                </div>
            </Show>
        </div>
    }
}

/// The classes. Opening one shows what it allows, as named capabilities.
#[component]
fn Classes() -> impl IntoView {
    let app = expect_context::<AppState>();
    let open = create_rw_signal(None::<String>);
    let new_name = create_rw_signal(String::new());
    let classes = move || app.admin.with(|a| a.classes.clone());
    // A new class starts as a copy of what a member may do: an empty class is
    // an account that cannot see the burrow it signed in to.
    let starting_mask = move || {
        app.admin.with(|a| {
            a.classes
                .iter()
                .find(|c| c.name == "member")
                .map(|c| c.base_mask)
                .unwrap_or(0)
        })
    };
    let name_free = move || {
        let name = new_name.get().trim().to_string();
        !name.is_empty() && app.admin.with(|a| a.classes.iter().all(|c| c.name != name))
    };
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-classes-h">
            <h3 class="rh-adm-group-h" id="rh-adm-classes-h">"Classes"</h3>
            <p class="rh-adm-group-blurb">
                "A class is a set of capabilities. Change one and everyone in it changes with it, \
                 at once."
            </p>
            <div class="rh-adm-rows">
                <For
                    each=classes
                    key=|c| (c.name.clone(), c.base_mask, c.members)
                    children=move |c| view! { <ClassRow class=c open=open/> }
                />
            </div>
            <form
                class="rh-adm-add"
                on:submit=move |ev| {
                    ev.prevent_default();
                    if name_free() {
                        let name = new_name.get().trim().to_string();
                        app.save_class(&name, starting_mask() & app.focused().caps.get());
                        open.set(Some(name));
                        new_name.set(String::new());
                    }
                }
            >
                <input
                    class="rh-input"
                    aria-label="Name for a new class"
                    placeholder="New class name"
                    maxlength="32"
                    prop:value=move || new_name.get()
                    on:input=move |ev| new_name.set(event_target_value(&ev))
                />
                <button type="submit" class="rh-btn ghost small" disabled=move || !name_free()>
                    "Add class"
                </button>
            </form>
        </section>
    }
}

#[component]
fn ClassRow(class: ClassEntry, open: RwSignal<Option<String>>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let me = app.focused();
    let name = store_value(class.name.clone());
    let saved = class.base_mask;
    let draft = create_rw_signal(saved);
    let is_open = move || open.get().as_deref() == Some(name.get_value().as_str());
    let toggle = move |_| {
        let n = name.get_value();
        open.update(|o| {
            *o = if o.as_deref() == Some(n.as_str()) {
                None
            } else {
                Some(n)
            }
        });
    };
    let count = CAPS
        .iter()
        .flat_map(|g| g.caps.iter())
        .filter(|c| people::has(saved, c))
        .count();
    let members = match class.members {
        1 => "1 account".to_string(),
        n => format!("{n} accounts"),
    };
    view! {
        <div class="rh-adm-acct" class:open=is_open>
            <button
                type="button"
                class="rh-adm-acct-line rh-adm-class-line"
                aria-expanded=move || is_open().to_string()
                on:click=toggle
            >
                <span class="rh-adm-acct-name">{class.name.clone()}</span>
                <span class="rh-adm-acct-role">{members}</span>
                <span class="rh-adm-acct-class">{format!("{count} capabilities")}</span>
                <span class="rh-adm-chevron" aria-hidden="true" inner_html=crate::icons::chevron_down_icon()></span>
            </button>
            <Show when=is_open fallback=|| ()>
                <div class="rh-adm-acct-detail">
                    {CAPS
                        .iter()
                        .map(|group| view! {
                            <fieldset class="rh-adm-caps">
                                <legend>{group.title}</legend>
                                {group
                                    .caps
                                    .iter()
                                    .map(|cap| {
                                        let cap = *cap;
                                        // Nobody grants what they do not hold.
                                        let mine = move || people::has(me.caps.get(), &cap);
                                        view! {
                                            <label class="rh-adm-cap" class:locked=move || !mine()>
                                                <input
                                                    type="checkbox"
                                                    prop:checked=move || people::has(draft.get(), &cap)
                                                    disabled=move || !mine()
                                                    on:change=move |ev| draft.update(|m| {
                                                        *m = people::with(*m, &cap, event_target_checked(&ev))
                                                    })
                                                />
                                                <span>
                                                    <strong>{cap.name}</strong>
                                                    <small>{cap.help}</small>
                                                </span>
                                            </label>
                                        }
                                    })
                                    .collect_view()}
                            </fieldset>
                        })
                        .collect_view()}
                    <div class="rh-adm-acct-actions">
                        <button
                            type="button"
                            class="rh-btn ghost small"
                            disabled=move || draft.get() == saved
                            on:click=move |_| draft.set(saved)
                        >
                            "Discard"
                        </button>
                        <button
                            type="button"
                            class="rh-btn small"
                            disabled=move || draft.get() == saved
                            on:click=move |_| app.save_class(&name.get_value(), draft.get())
                        >
                            "Save class"
                        </button>
                    </div>
                </div>
            </Show>
        </div>
    }
}

fn invite_line(invite: &InviteEntry) -> (String, bool) {
    match people::invite_status(invite, crate::clock::now_ms() / 1000) {
        InviteStatus::Open(left) => (
            format!("Open for {}", humanize_secs(left.max(1) as u64)),
            true,
        ),
        InviteStatus::UsedBy(who) => (format!("Used by {who}"), false),
        InviteStatus::Expired => ("Expired".to_string(), false),
    }
}

/// Invitations: make one, copy it, withdraw one nobody has used.
#[component]
fn Invitations() -> impl IntoView {
    let app = expect_context::<AppState>();
    let ttl = create_rw_signal(7 * 86_400i64);
    let invites = move || app.people.with(|p| p.invites.clone());
    let needed = move || {
        app.admin_settings
            .with(|s| s.shown("registration_mode") == "invite")
    };
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-invites-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-invites-h">"Invitations"</h3>
                <div class="rh-adm-group-tools">
                    <label class="rh-visually-hidden" for="rh-adm-invite-ttl">"Good for"</label>
                    <select
                        id="rh-adm-invite-ttl"
                        class="rh-select"
                        on:change=move |ev| ttl.set(event_target_value(&ev).parse().unwrap_or(604_800))
                    >
                        <option value="86400">"Good for a day"</option>
                        <option value="604800" selected=true>"Good for a week"</option>
                        <option value="2592000">"Good for 30 days"</option>
                    </select>
                    <button type="button" class="rh-btn small" on:click=move |_| app.create_invite(ttl.get())>
                        "New invitation"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                {move || if needed() {
                    "New accounts need one of these codes. Each works once."
                } else {
                    "Nobody needs a code right now: New accounts, under Access, is not set to require one."
                }}
            </p>
            <div class="rh-adm-rows">
                <For
                    each=invites
                    key=|i| (i.code.clone(), i.used_by.clone(), i.expires_at)
                    children=move |invite| {
                        let (line, open) = invite_line(&invite);
                        let code = store_value(invite.code.clone());
                        let copied = create_rw_signal(false);
                        view! {
                            <div class="rh-adm-invite" class:spent=!open>
                                <code class="rh-adm-invite-code">{invite.code.clone()}</code>
                                <span class="rh-adm-invite-by">"from " {invite.created_by.clone()}</span>
                                <span class="rh-adm-invite-state">{line}</span>
                                <span class="rh-adm-invite-actions">
                                    {open.then(|| view! {
                                        <button
                                            type="button"
                                            class="rh-btn ghost small"
                                            on:click=move |_| {
                                                copy_text_quiet(&code.get_value());
                                                copied.set(true);
                                                set_timeout(
                                                    move || copied.set(false),
                                                    std::time::Duration::from_millis(1400),
                                                );
                                            }
                                        >
                                            {move || if copied.get() { "Copied" } else { "Copy" }}
                                        </button>
                                        <button
                                            type="button"
                                            class="rh-btn ghost small rh-adm-danger"
                                            on:click=move |_| app.confirm.set(
                                                Some(ConfirmAsk::revoke_invite(&code.get_value())),
                                            )
                                        >
                                            "Withdraw\u{2026}"
                                        </button>
                                    })}
                                </span>
                            </div>
                        }
                    }
                />
                <Show when=move || invites().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">"No invitations yet."</p>
                </Show>
            </div>
        </section>
    }
}
