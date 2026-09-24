//! The Peers pane of the admin console: the burrows this one has met over
//! federation, each with Approve or Revoke, and the origins whose signing
//! keys are believed for relayed posts, with a way to pin one by hand.
//!
//! Approving binds a key to an origin for good, so the row asks for an
//! origin only when the peer did not announce one. A peer listed in
//! `burrow.toml` is approved by that listing and cannot be revoked here.

use leptos::*;
use rabbithole_proto::admin::{peer_state, OriginEntry, PeerEntry};

use crate::admin_federation::{
    origin_is_acceptable, parse_key, peer_state_name, peer_title, short_key, trust_name,
};
use crate::app::{AppState, ConfirmAsk};

/// The pane.
#[component]
pub fn PeersPane() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.each_sign_in(move || {
        app.load_peers();
        app.load_origins();
    });
    view! {
        <Peers/>
        <Origins/>
    }
}

#[component]
fn Peers() -> impl IntoView {
    let app = expect_context::<AppState>();
    let peers = move || app.federation.with(|f| f.peers.clone());
    let waiting = move || {
        peers()
            .iter()
            .filter(|p| p.state == peer_state::PENDING)
            .count()
    };
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-peers-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-peers-h">
                    "Peers"
                    <span class="rh-adm-count">{move || peers().len()}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <button type="button" class="rh-btn ghost small" on:click=move |_| app.load_peers()>
                        "Refresh"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "Burrows that have introduced themselves to this one. Approving one binds its key \
                 to its name for good; posts, catalogs and search then flow both ways from its \
                 next connection. Revoking closes the session."
            </p>
            <Show when={move || waiting() > 0} fallback=|| ()>
                <p class="rh-adm-group-blurb rh-adm-peer-waiting" role="status">
                    {move || match waiting() {
                        1 => "One burrow is waiting for your approval.".to_string(),
                        n => format!("{n} burrows are waiting for your approval."),
                    }}
                </p>
            </Show>
            <div class="rh-adm-rows">
                <For
                    each=peers
                    key=|p| (p.key, p.state, p.approved, p.origin.clone(), p.name.clone())
                    children=move |p| view! { <PeerRow peer=p/> }
                />
                <Show when=move || peers().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">
                        "No burrow has knocked yet. With Federation turned on under Settings, a \
                         burrow that dials this one appears here, waiting."
                    </p>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn PeerRow(peer: PeerEntry) -> impl IntoView {
    let app = expect_context::<AppState>();
    let key = peer.key;
    let title = store_value(peer_title(&peer));
    let origin_known = peer.origin.as_deref().is_some_and(|o| !o.trim().is_empty());
    let pending = peer.state == peer_state::PENDING;
    let approved = peer.approved;
    let configured = peer.configured;
    let origin = create_rw_signal(String::new());
    let may_approve = move || origin_known || origin_is_acceptable(origin.get().trim());
    let approve_tag = store_value(format!("*peer-approve:{}", hex::encode(key)));
    let busy = move || app.people.with(|p| p.is_busy(&approve_tag.get_value()));
    let meta = {
        let mut parts = vec![peer
            .origin
            .clone()
            .filter(|o| !o.trim().is_empty())
            .unwrap_or_else(|| "no origin announced".to_string())];
        parts.push(short_key(&peer.key));
        if let Some(addr) = peer.addr.as_deref().filter(|a| !a.is_empty()) {
            parts.push(format!("from {addr}"));
        }
        parts.join(" \u{00b7} ")
    };
    let state_text = peer_state_name(peer.state);
    view! {
        <div class="rh-adm-peer" class:pending=pending>
            <div class="rh-adm-peer-who">
                <span class="rh-adm-peer-name">{title.get_value()}</span>
                <span class="rh-adm-peer-meta">{meta}</span>
            </div>
            <span class="rh-adm-peer-state">
                {state_text}
                <Show when=move || configured fallback=|| ()>
                    " " <span class="rh-badge">"In burrow.toml"</span>
                </Show>
            </span>
            <span class="rh-adm-peer-actions">
                <Show when=move || !approved fallback=|| ()>
                    <Show when=move || !origin_known fallback=|| ()>
                        <input
                            class="rh-input rh-adm-mono"
                            aria-label="Origin to bind this peer to"
                            placeholder="origin (grove.example)"
                            autocomplete="off"
                            spellcheck="false"
                            maxlength="253"
                            prop:value=move || origin.get()
                            on:input=move |ev| origin.set(event_target_value(&ev))
                        />
                    </Show>
                    <button
                        type="button"
                        class="rh-btn small"
                        disabled=move || !may_approve() || busy()
                        on:click=move |_| app.approve_peer(
                            key,
                            (!origin_known).then(|| origin.get().trim().to_string()),
                        )
                    >
                        {move || if busy() { "Approving\u{2026}" } else { "Approve" }}
                    </button>
                </Show>
                <Show when=move || approved && !configured fallback=|| ()>
                    <button
                        type="button"
                        class="rh-btn ghost small rh-adm-danger"
                        on:click=move |_| app.confirm.set(Some(ConfirmAsk::revoke_peer(
                            key,
                            &title.get_value(),
                        )))
                    >
                        "Revoke\u{2026}"
                    </button>
                </Show>
            </span>
        </div>
    }
}

#[component]
fn Origins() -> impl IntoView {
    let app = expect_context::<AppState>();
    let origins = move || app.federation.with(|f| f.origins.clone());
    let origin = create_rw_signal(String::new());
    let key_text = create_rw_signal(String::new());
    let parsed = move || parse_key(&key_text.get());
    let ready = move || origin_is_acceptable(origin.get().trim()) && parsed().is_some();
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-origins-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-origins-h">
                    "Trusted origins"
                    <span class="rh-adm-count">{move || origins().len()}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <button type="button" class="rh-btn ghost small" on:click=move |_| app.load_origins()>
                        "Refresh"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "Whose posts are believed when they arrive by way of another burrow: an origin, \
                 and the key it signs with. A direct peer is trusted once it is approved and \
                 connects. Pin one by hand when you have its key from elsewhere. Once bound, an \
                 origin keeps its key."
            </p>
            <div class="rh-adm-rows">
                <For
                    each=origins
                    key=|o| (o.origin.clone(), o.key)
                    children=move |o| view! { <OriginLine entry=o/> }
                />
                <Show when=move || origins().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">"No origin is trusted yet."</p>
                </Show>
            </div>
            <form
                class="rh-adm-add"
                on:submit=move |ev| {
                    ev.prevent_default();
                    if let Some(k) = parsed().filter(|_| ready()) {
                        app.pin_origin(origin.get().trim(), k);
                        origin.set(String::new());
                        key_text.set(String::new());
                    }
                }
            >
                <input
                    class="rh-input rh-adm-mono"
                    aria-label="Origin to pin"
                    placeholder="origin (grove.example)"
                    autocomplete="off"
                    spellcheck="false"
                    maxlength="253"
                    prop:value=move || origin.get()
                    on:input=move |ev| origin.set(event_target_value(&ev))
                />
                <input
                    class="rh-input rh-adm-mono"
                    aria-label="Its signing key"
                    placeholder="signing key, 64 hex"
                    autocomplete="off"
                    spellcheck="false"
                    prop:value=move || key_text.get()
                    on:input=move |ev| key_text.set(event_target_value(&ev))
                />
                <button type="submit" class="rh-btn ghost small" disabled=move || !ready()>
                    "Pin"
                </button>
            </form>
        </section>
    }
}

#[component]
fn OriginLine(entry: OriginEntry) -> impl IntoView {
    view! {
        <div class="rh-adm-origin">
            <span class="rh-adm-origin-name">{entry.origin.clone()}</span>
            <span class="rh-adm-mono" title=hex::encode(entry.key)>{short_key(&entry.key)}</span>
            <span class="rh-adm-origin-trust">{trust_name(entry.trust)}</span>
        </div>
    }
}
