//! The stations half of the Radio pane: what each station is doing, and
//! what its rotation could not play.
//!
//! A silent station used to look exactly like a working one. The burrow
//! knows the difference and now says so ([`crate::radio_admin`] turns it
//! into sentences); this shows it where the operator already is.

use leptos::*;

use crate::app::AppState;
use crate::radio_admin::{area_line, left_out_line, listeners_line, station_line};

/// The panel, under the Radio section's settings.
#[component]
pub fn StationsPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.load_stations();
    let stations = move || app.federation.with(|f| f.stations.clone());
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-stations-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-stations-h">
                    "Stations"
                    <span class="rh-adm-count">{move || stations().len()}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        on:click=move |_| app.load_stations()
                    >
                        "Refresh"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "A station plays a file area. It sends one kind of sound, so a track of \
                 another kind is passed over rather than cutting the stream off for \
                 whoever is listening."
            </p>
            <Show
                when=move || !stations().is_empty()
                fallback=|| view! {
                    <p class="rh-adm-empty">
                        "No stations. Map a file area to one with "
                        <code>"radio_library_areas"</code>
                        " to put a library on the air."
                    </p>
                }
            >
                <ul class="rh-adm-list">
                    {move || stations().into_iter().map(|s| {
                        let left = left_out_line(&s.left_out);
                        let tracks = s.left_out.clone();
                        view! {
                            <li class="rh-adm-row">
                                <div class="rh-adm-row-main">
                                    <span class="rh-adm-row-title">{s.name.clone()}</span>
                                    <span class="rh-adm-row-sub">{station_line(&s)}</span>
                                    <span class="rh-adm-row-sub">{area_line(&s.area)}</span>
                                </div>
                                <div class="rh-adm-row-side">
                                    <span class="rh-adm-pill">{listeners_line(s.listeners)}</span>
                                    {s.live.then(|| view! {
                                        <span class="rh-adm-pill warn">"DJ live"</span>
                                    })}
                                </div>
                                {left.map(|line| view! {
                                    <details class="rh-adm-detail">
                                        <summary>{line}</summary>
                                        <ul class="rh-adm-sublist">
                                            {tracks.into_iter().map(|t| view! {
                                                <li>
                                                    <span class="rh-adm-row-title">{t.title}</span>
                                                    <span class="rh-adm-row-sub">{t.reason}</span>
                                                </li>
                                            }).collect_view()}
                                        </ul>
                                    </details>
                                })}
                            </li>
                        }
                    }).collect_view()}
                </ul>
            </Show>
        </section>
    }
}
