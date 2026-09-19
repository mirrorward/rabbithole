//! The File areas pane of the admin console: the libraries, a form for a new
//! one, and each area's own controls. What is *in* an area (folders, files,
//! drop boxes) is managed from the Files view, where the operator can see it.
//!
//! An area's address (its slug) is in every path and every download link, so
//! it is chosen once and never edited. Only an empty area can be removed; the
//! burrow refuses the rest and says so.

use leptos::*;
use rabbithole_proto::filelib::FileAreaView;

use crate::admin_people::{slug_is_acceptable, slugify};
use crate::app::{AppState, ConfirmAsk};

/// The pane.
#[component]
pub fn AreasPane() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.load_areas();
    let adding = create_rw_signal(false);
    let open = create_rw_signal(None::<String>);
    let areas = move || app.focused().files.with(|f| f.areas.clone());
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-areas-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-areas-h">"Areas"</h3>
                <div class="rh-adm-group-tools">
                    <button
                        type="button"
                        class="rh-btn small"
                        aria-expanded=move || adding.get().to_string()
                        on:click=move |_| adding.update(|a| *a = !*a)
                    >
                        "New area"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "A library people browse, upload to and download from. Folders, drop boxes and \
                 the files themselves are managed in Files, where you can see them."
            </p>
            <p class="rh-adm-group-blurb">
                "How big one file may be and how much each person may keep are under "
                <A href="/admin/files">"Files & transfers"</A>
                "."
            </p>
            <Show when=move || adding.get() fallback=|| ()>
                <NewArea on_done=Callback::new(move |_| adding.set(false))/>
            </Show>
            <div class="rh-adm-rows">
                <For
                    each=areas
                    key=|a| (a.slug.clone(), a.title.clone(), a.description.clone())
                    children=move |area| view! { <AreaRow area=area open=open/> }
                />
                <Show when=move || areas().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">
                        "No areas yet. Make one, and people have somewhere to put files."
                    </p>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn NewArea(on_done: Callback<()>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let title = create_rw_signal(String::new());
    let slug = create_rw_signal(String::new());
    let slug_touched = create_rw_signal(false);
    let description = create_rw_signal(String::new());
    let taken = move || {
        let wanted = slug.get();
        app.focused()
            .files
            .with(|f| f.areas.iter().any(|a| a.slug == wanted))
    };
    let ready =
        move || !title.get().trim().is_empty() && slug_is_acceptable(&slug.get()) && !taken();
    let slug_problem = move || {
        let s = slug.get();
        if s.is_empty() {
            None
        } else if taken() {
            Some("There is already an area at this address.")
        } else if !slug_is_acceptable(&s) {
            Some("Lowercase letters, digits, dots, dashes and underscores, starting with a letter or digit.")
        } else {
            None
        }
    };
    let sent = create_rw_signal(None::<String>);
    create_effect(move |_| {
        let Some(wanted) = sent.get() else { return };
        if app
            .focused()
            .files
            .with(|f| f.areas.iter().any(|a| a.slug == wanted))
        {
            on_done.call(());
        }
    });
    view! {
        <form
            class="rh-adm-form"
            on:submit=move |ev| {
                ev.prevent_default();
                if ready() {
                    app.create_area(&slug.get(), &title.get(), &description.get());
                    sent.set(Some(slug.get()));
                }
            }
        >
            <label class="rh-adm-field">
                <span>"Title"</span>
                <input
                    class="rh-input"
                    maxlength="80"
                    prop:value=move || title.get()
                    on:input=move |ev| {
                        let t = event_target_value(&ev);
                        if !slug_touched.get_untracked() {
                            slug.set(slugify(&t));
                        }
                        title.set(t);
                    }
                />
            </label>
            <label class="rh-adm-field">
                <span>"Address"</span>
                <input
                    class="rh-input rh-adm-mono"
                    autocomplete="off"
                    spellcheck="false"
                    maxlength="64"
                    aria-invalid=move || slug_problem().is_some().to_string()
                    prop:value=move || slug.get()
                    on:input=move |ev| {
                        slug_touched.set(true);
                        slug.set(event_target_value(&ev));
                    }
                />
                <small class:rh-adm-bad=move || slug_problem().is_some()>
                    {move || slug_problem().unwrap_or(
                        "In every path and download link. It cannot be changed later.",
                    )}
                </small>
            </label>
            <label class="rh-adm-field rh-adm-wide">
                <span>"Description"</span>
                <input
                    class="rh-input"
                    maxlength="200"
                    prop:value=move || description.get()
                    on:input=move |ev| description.set(event_target_value(&ev))
                />
            </label>
            <div class="rh-adm-form-actions">
                <button type="button" class="rh-btn ghost small" on:click=move |_| on_done.call(())>
                    "Cancel"
                </button>
                <button type="submit" class="rh-btn small" disabled=move || !ready()>
                    "Create"
                </button>
            </div>
        </form>
    }
}

#[component]
fn AreaRow(area: FileAreaView, open: RwSignal<Option<String>>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let slug = store_value(area.slug.clone());
    let saved_title = area.title.clone();
    let saved_description = area.description.clone();
    let title = create_rw_signal(saved_title.clone());
    let description = create_rw_signal(saved_description.clone());
    let is_open = move || open.get().as_deref() == Some(slug.get_value().as_str());
    let toggle = move |_| {
        let s = slug.get_value();
        open.update(|o| {
            *o = if o.as_deref() == Some(s.as_str()) {
                None
            } else {
                Some(s)
            }
        });
    };
    let can_save = {
        let (t, d) = (saved_title.clone(), saved_description.clone());
        move || {
            !title.get().trim().is_empty()
                && (title.get().trim() != t || description.get().trim() != d)
        }
    };
    let shown_title = store_value(area.title.clone());
    view! {
        <div class="rh-adm-acct" class:open=is_open>
            <button
                type="button"
                class="rh-adm-acct-line rh-adm-board-line"
                aria-expanded=move || is_open().to_string()
                on:click=toggle
            >
                <span class="rh-adm-acct-name">{area.title.clone()}</span>
                <span class="rh-adm-acct-role rh-adm-mono">{area.slug.clone()}</span>
                <span class="rh-adm-acct-class">{area.description.clone()}</span>
                <span class="rh-adm-chevron" aria-hidden="true" inner_html=crate::icons::chevron_down_icon()></span>
            </button>
            <Show when=is_open fallback=|| ()>
                <div class="rh-adm-acct-detail">
                    <div class="rh-adm-acct-fields">
                        <label class="rh-adm-field">
                            <span>"Title"</span>
                            <input
                                class="rh-input"
                                maxlength="80"
                                prop:value=move || title.get()
                                on:input=move |ev| title.set(event_target_value(&ev))
                            />
                        </label>
                        <label class="rh-adm-field">
                            <span>"Description"</span>
                            <input
                                class="rh-input"
                                maxlength="200"
                                prop:value=move || description.get()
                                on:input=move |ev| description.set(event_target_value(&ev))
                            />
                        </label>
                    </div>
                    <div class="rh-adm-acct-actions">
                        <A href="/files" class="rh-btn ghost small">"Open in Files"</A>
                        <button
                            type="button"
                            class="rh-btn ghost small rh-adm-danger"
                            on:click=move |_| app.confirm.set(Some(ConfirmAsk::delete_area(
                                &slug.get_value(),
                                &shown_title.get_value(),
                            )))
                        >
                            "Remove\u{2026}"
                        </button>
                        <button
                            type="button"
                            class="rh-btn small"
                            disabled={
                                let can_save = can_save.clone();
                                move || !can_save()
                            }
                            on:click=move |_| app.update_area(
                                &slug.get_value(),
                                &title.get(),
                                &description.get(),
                            )
                        >
                            "Save"
                        </button>
                    </div>
                </div>
            </Show>
        </div>
    }
}

use leptos_router::A;
