//! The "Send to another burrow" dialog: pick one of the other burrows the
//! person is on, walk to a folder there, and send. The burrow the files are on
//! signs a permission naming the other; the other fetches them itself, and
//! its queue row and a toast say how it went.
//!
//! The destination's folders are listed by awaited requests on its own
//! socket, so its Files view is left exactly where the person had it.
//! Browser-only: it speaks to live burrows.

use leptos::*;
use rabbithole_proto::filelib::{
    AreaList, AreaListRequest, FileAreaView, FolderListRequest, NodeList, PullGrantAsk,
    PullGrantIssued, PullGrantRequest, RemotePull, RemotePullAccepted,
};
use rabbithole_proto::ErrorCode;
use wasm_bindgen_futures::spawn_local;

use crate::app::{AppState, SendAsk, ServerId};
use crate::files::KIND_FOLDER;
use crate::send::{self, Place};
use crate::toasts::ToastKind;

/// A folder on the destination, as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sub {
    name: String,
    is_dropbox: bool,
}

/// The dialog. Mounted once; shows while [`AppState::sending`] holds a file.
#[component]
pub fn SendDialog() -> impl IntoView {
    let app = expect_context::<AppState>();
    let ask = app.sending;
    let dest = create_rw_signal(None::<ServerId>);
    let areas = create_rw_signal(Vec::<FileAreaView>::new());
    let subs = create_rw_signal(Vec::<Sub>::new());
    let place = create_rw_signal(Place::default());
    let loading = create_rw_signal(false);
    let busy = create_rw_signal(false);
    let problem = create_rw_signal(None::<String>);

    // A fresh start each time it opens.
    create_effect(move |_| {
        if ask.with(Option::is_some) {
            dest.set(None);
            areas.set(Vec::new());
            subs.set(Vec::new());
            place.set(Place::default());
            problem.set(None);
            busy.set(false);
            crate::a11y::focus_id("rh-send-cancel");
        }
    });

    let close = move || {
        if !busy.get_untracked() {
            ask.set(None);
        }
    };
    let targets = move || app.send_targets();
    let what = move || ask.with(|a| a.as_ref().map(|a| a.name.clone()).unwrap_or_default());
    let area_title = move || {
        let slug = place.with(|p| p.area.clone());
        areas.with(|list| {
            list.iter()
                .find(|a| Some(&a.slug) == slug.as_ref())
                .map(|a| a.title.clone())
                .unwrap_or_else(|| slug.unwrap_or_default())
        })
    };

    let choose_burrow = move |id: ServerId| {
        dest.set(Some(id.clone()));
        place.set(Place::default());
        subs.set(Vec::new());
        problem.set(None);
        list_areas(app, id, areas, loading, problem);
    };
    let enter = move |area: Option<String>, path: Vec<String>| {
        let Some(id) = dest.get_untracked() else {
            return;
        };
        let Some(slug) = area.clone() else {
            place.set(Place::default());
            subs.set(Vec::new());
            return;
        };
        place.set(Place {
            area,
            path: path.clone(),
        });
        list_folders(app, id, slug, path, subs, loading, problem);
    };
    let send_here = move |_| {
        let (Some(ask_now), Some(to)) = (ask.get_untracked(), dest.get_untracked()) else {
            return;
        };
        if place.with_untracked(|p| p.area.is_none()) {
            return;
        }
        busy.set(true);
        problem.set(None);
        start(app, ask_now, to, place.get_untracked(), busy, problem);
    };

    view! {
        <Show when=move || ask.with(Option::is_some) fallback=|| ()>
            <div class="rh-confirm-backdrop" on:click=move |_| close()>
                <div
                    class="rh-confirm rh-send"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby="rh-send-title"
                    on:click=|ev| ev.stop_propagation()
                    on:keydown=move |ev| {
                        if ev.key() == "Escape" {
                            ev.prevent_default();
                            close();
                        }
                    }
                >
                    <h2 id="rh-send-title">
                        {move || format!("Send \u{201c}{}\u{201d} to another burrow", what())}
                    </h2>
                    <p class="rh-send-lead">
                        "The other burrow fetches it from this one itself, and files it under \
                         your name there."
                    </p>

                    <p class="rh-send-step">"To"</p>
                    <Show
                        when=move || !targets().is_empty()
                        fallback=|| view! {
                            <p class="rh-send-empty">
                                "You are not on another burrow right now. Join one from the rail, \
                                 then come back."
                            </p>
                        }
                    >
                        <div class="rh-send-burrows" role="radiogroup" aria-label="Burrow to send to">
                            <For
                                each=targets
                                key=|(id, name)| (id.0.clone(), name.clone())
                                children=move |(id, name)| {
                                    let mine = store_value(id.clone());
                                    let chosen = move || dest.with(|d| d.as_ref() == Some(&mine.get_value()));
                                    let pick = id.clone();
                                    view! {
                                        <button
                                            type="button"
                                            role="radio"
                                            class="rh-send-burrow"
                                            class:active=chosen
                                            aria-checked=move || chosen().to_string()
                                            on:click=move |_| choose_burrow(pick.clone())
                                        >
                                            {name}
                                        </button>
                                    }
                                }
                            />
                        </div>
                    </Show>

                    <Show when=move || dest.with(Option::is_some) fallback=|| ()>
                        <p class="rh-send-step">"Into"</p>
                        <div class="rh-send-place">
                            <Show
                                when=move || place.with(|p| p.area.is_some())
                                fallback=move || view! {
                                    <ul class="rh-send-list" aria-label="File areas">
                                        <For
                                            each=move || areas.get()
                                            key=|a| a.slug.clone()
                                            children=move |a| {
                                                let slug = a.slug.clone();
                                                view! {
                                                    <li>
                                                        <button
                                                            type="button"
                                                            class="rh-send-row"
                                                            on:click=move |_| enter(Some(slug.clone()), Vec::new())
                                                        >
                                                            <span class="rh-file-icon" aria-hidden="true" inner_html=crate::icons::file_icon(true)></span>
                                                            <span>{a.title.clone()}</span>
                                                        </button>
                                                    </li>
                                                }
                                            }
                                        />
                                        <Show when=move || !loading.get() && areas.with(Vec::is_empty) fallback=|| ()>
                                            <li class="rh-send-empty">"That burrow has no file areas."</li>
                                        </Show>
                                    </ul>
                                }
                            >
                                <nav class="rh-send-crumbs" aria-label="Folder">
                                    <button type="button" class="rh-crumb" on:click=move |_| enter(None, Vec::new())>
                                        "All areas"
                                    </button>
                                    <span class="rh-crumb sep" aria-hidden="true">"/"</span>
                                    <button
                                        type="button"
                                        class="rh-crumb"
                                        on:click=move |_| enter(place.with_untracked(|p| p.area.clone()), Vec::new())
                                    >
                                        {area_title}
                                    </button>
                                    {move || {
                                        let path = place.with(|p| p.path.clone());
                                        path.iter()
                                            .enumerate()
                                            .map(|(i, seg)| {
                                                let upto: Vec<String> = path[..=i].to_vec();
                                                view! {
                                                    <span class="rh-crumb sep" aria-hidden="true">"/"</span>
                                                    <button
                                                        type="button"
                                                        class="rh-crumb"
                                                        on:click=move |_| enter(
                                                            place.with_untracked(|p| p.area.clone()),
                                                            upto.clone(),
                                                        )
                                                    >
                                                        {seg.clone()}
                                                    </button>
                                                }
                                            })
                                            .collect_view()
                                    }}
                                </nav>
                                <ul class="rh-send-list" aria-label="Folders">
                                    <For
                                        each=move || subs.get()
                                        key=|s| s.name.clone()
                                        children=move |s| {
                                            let name = s.name.clone();
                                            view! {
                                                <li>
                                                    <button
                                                        type="button"
                                                        class="rh-send-row"
                                                        on:click=move |_| {
                                                            let (area, mut path) = place.with_untracked(|p| (p.area.clone(), p.path.clone()));
                                                            path.push(name.clone());
                                                            enter(area, path);
                                                        }
                                                    >
                                                        <span class="rh-file-icon" aria-hidden="true" inner_html=crate::icons::file_icon(true)></span>
                                                        <span>{s.name.clone()}</span>
                                                        {s.is_dropbox.then(|| view! { <span class="rh-send-note">"drop box"</span> })}
                                                    </button>
                                                </li>
                                            }
                                        }
                                    />
                                    <Show when=move || !loading.get() && subs.with(Vec::is_empty) fallback=|| ()>
                                        <li class="rh-send-empty">"No folders here. It can go right here."</li>
                                    </Show>
                                </ul>
                            </Show>
                        </div>
                    </Show>

                    <Show when=move || problem.with(Option::is_some) fallback=|| ()>
                        <p class="rh-send-problem" role="alert">{move || problem.get()}</p>
                    </Show>

                    <div class="rh-confirm-actions">
                        <button
                            type="button"
                            id="rh-send-cancel"
                            class="rh-btn ghost"
                            disabled=move || busy.get()
                            on:click=move |_| close()
                        >
                            "Cancel"
                        </button>
                        <button
                            type="button"
                            class="rh-btn"
                            disabled=move || busy.get() || place.with(|p| p.area.is_none())
                            on:click=send_here
                        >
                            {move || {
                                if busy.get() {
                                    "Sending\u{2026}".to_string()
                                } else if place.with(|p| p.area.is_some()) {
                                    format!("Send to {}", place.with(|p| p.label(&area_title())))
                                } else {
                                    "Send".to_string()
                                }
                            }}
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// List the destination's file areas.
fn list_areas(
    app: AppState,
    id: ServerId,
    areas: RwSignal<Vec<FileAreaView>>,
    loading: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
) {
    let Some(session) = app.session_of(&id) else {
        return;
    };
    let ws = session.ws.get_value();
    let name = app.burrow_name(&id);
    loading.set(true);
    spawn_local(async move {
        let reply = ws.call(&AreaListRequest).await;
        loading.set(false);
        match reply
            .and_then(|f| f.decode::<AreaList>())
            .and_then(Result::ok)
        {
            Some(list) => areas.set(list.areas),
            None => problem.set(Some(format!("Could not list the file areas on {name}."))),
        }
    });
}

/// List the folders in one folder of the destination.
fn list_folders(
    app: AppState,
    id: ServerId,
    area: String,
    path: Vec<String>,
    subs: RwSignal<Vec<Sub>>,
    loading: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
) {
    let Some(session) = app.session_of(&id) else {
        return;
    };
    let ws = session.ws.get_value();
    let name = app.burrow_name(&id);
    let folder = (!path.is_empty()).then(|| path.join("/"));
    subs.set(Vec::new());
    loading.set(true);
    spawn_local(async move {
        let reply = ws.call(&FolderListRequest::new(area, folder)).await;
        loading.set(false);
        match reply
            .and_then(|f| f.decode::<NodeList>())
            .and_then(Result::ok)
        {
            Some(list) => subs.set(
                list.nodes
                    .into_iter()
                    .filter(|n| n.kind == KIND_FOLDER)
                    .map(|n| Sub {
                        name: n.name,
                        is_dropbox: n.is_dropbox,
                    })
                    .collect(),
            ),
            None => problem.set(Some(format!("Could not list that folder on {name}."))),
        }
    });
}

/// Ask the source for a grant naming the destination, hand it to the
/// destination, and put the pull on the destination's queue.
fn start(
    app: AppState,
    ask: SendAsk,
    to: ServerId,
    place: Place,
    busy: RwSignal<bool>,
    problem: RwSignal<Option<String>>,
) {
    let (Some(source), Some(dest)) = (app.session_of(&ask.source), app.session_of(&to)) else {
        busy.set(false);
        problem.set(Some(
            "One of the burrows is gone. Close this and try again.".into(),
        ));
        return;
    };
    let from_name = app.burrow_name(&ask.source);
    let to_name = app.burrow_name(&to);
    let Some(dest_key) = dest.ws.get_value().server_key() else {
        busy.set(false);
        problem.set(Some(format!(
            "Could not tell which burrow {to_name} is. Reconnect to it and try again."
        )));
        return;
    };
    let from_ws = source.ws.get_value();
    let to_ws = dest.ws.get_value();
    let Some(area) = place.area.clone() else {
        return;
    };
    let folder = place.folder();
    spawn_local(async move {
        let fail = |why: String| {
            busy.set(false);
            problem.set(Some(why));
        };
        // Ask with the host this app reaches the source at: a destination
        // that is not the source's peer connects there. A source that
        // predates the question is asked the older way.
        let host = send::reach_host(&ask.source.0);
        let mut reply = from_ws
            .call(&PullGrantAsk::new(dest_key, vec![ask.node_id], host))
            .await;
        if reply.as_ref().and_then(|f| f.error) == Some(ErrorCode::Unsupported) {
            reply = from_ws
                .call(&PullGrantRequest::new(dest_key, vec![ask.node_id]))
                .await;
        }
        let issued = match reply {
            Some(f) if f.error.is_none() => f.decode::<PullGrantIssued>().and_then(Result::ok),
            Some(f) => {
                return fail(send::grant_refusal(
                    f.error, &ask.name, &from_name, &to_name,
                ));
            }
            None => return fail(send::grant_refusal(None, &ask.name, &from_name, &to_name)),
        };
        let Some(issued) = issued else {
            return fail(send::grant_refusal(None, &ask.name, &from_name, &to_name));
        };
        let reply = to_ws
            .call(&RemotePull::new(issued.grant.clone(), area, folder))
            .await;
        let accepted = match reply {
            Some(f) if f.error.is_none() => f.decode::<RemotePullAccepted>().and_then(Result::ok),
            Some(f) => {
                return fail(send::pull_refusal(
                    f.error,
                    &ask.name,
                    ask.is_folder,
                    issued.bytes,
                    &from_name,
                    &to_name,
                ));
            }
            None => {
                return fail(send::pull_refusal(
                    None,
                    &ask.name,
                    ask.is_folder,
                    issued.bytes,
                    &from_name,
                    &to_name,
                ))
            }
        };
        let Some(accepted) = accepted else {
            return fail(send::pull_refusal(
                None,
                &ask.name,
                ask.is_folder,
                issued.bytes,
                &from_name,
                &to_name,
            ));
        };
        dest.files.update(|f| {
            f.pull_started(accepted.pull_id, &ask.name, accepted.bytes);
        });
        busy.set(false);
        app.sending.set(None);
        app.notify(
            ToastKind::Info,
            format!(
                "Sending \u{201c}{}\u{201d} to {to_name}: {}.",
                ask.name,
                send::amount(accepted.files, accepted.bytes)
            ),
        );
        if issued.skipped > 0 {
            app.notify(
                ToastKind::Warn,
                format!(
                    "{} file{} in \u{201c}{}\u{201d} stay behind: you may not send them, or their names do not travel.",
                    issued.skipped,
                    if issued.skipped == 1 { "" } else { "s" },
                    ask.name
                ),
            );
        }
    });
}
