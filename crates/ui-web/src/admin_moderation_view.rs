//! The Moderation pane of the admin console: the report queue, who is
//! connected (with a Disconnect that names them, instead of a box for a
//! session number), a notice to everyone, the hash-deny list, and the audit
//! log.

use leptos::*;
use rabbithole_proto::admin::{report_state, DenyHashEntry, ReportEntry};

use crate::admin_moderation::{
    self as moderation, actions_for, audit_line, deny_line, parse_hash, report_line, short_hash,
    STATES,
};
use crate::admin_people::role_name;
use crate::admin_settings::humanize_secs;
use crate::app::{AppState, ConfirmAsk};
use crate::state::SessionRow;

/// The pane.
#[component]
pub fn ModerationPane() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.load_reports(Some(report_state::OPEN));
    app.load_held();
    app.load_deny_hashes();
    app.load_audit();
    app.refresh_who();
    view! {
        <Reports/>
        <HeldBack/>
        <Sessions/>
        <Broadcast/>
        <DenyList/>
        <AuditLog/>
    }
}

fn when(unix: i64) -> String {
    let now = crate::clock::now_ms() / 1000;
    let ago = (now - unix).max(0) as u64;
    if ago < 60 {
        "just now".to_string()
    } else {
        format!("{} ago", humanize_secs(ago))
    }
}

/// The report queue, one state at a time.
#[component]
fn Reports() -> impl IntoView {
    let app = expect_context::<AppState>();
    let filter = create_rw_signal(report_state::OPEN);
    let reports = move || app.moderation.with(|m| m.reports.clone());
    let total = move || app.moderation.with(|m| m.total);
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-reports-h">
            <h3 class="rh-adm-group-h" id="rh-adm-reports-h">"Reports"</h3>
            <p class="rh-adm-group-blurb">
                "What people flagged. Look into one to claim it, then resolve or dismiss it \
                 with a note that stays on the record."
            </p>
            <div class="rh-adm-tabs" role="tablist" aria-label="Report state">
                {STATES
                    .iter()
                    .map(|(state, label)| {
                        let state = *state;
                        view! {
                            <button
                                type="button"
                                class="rh-adm-tab"
                                role="tab"
                                aria-selected=move || (filter.get() == state).to_string()
                                on:click=move |_| {
                                    filter.set(state);
                                    app.load_reports(Some(state));
                                }
                            >
                                {*label}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            <div class="rh-adm-rows">
                <For
                    each=reports
                    key=|r| (r.id, r.state, r.resolution.clone())
                    children=move |r| view! { <ReportRow report=r/> }
                />
                <Show when=move || reports().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">
                        {move || match filter.get() {
                            report_state::OPEN => "Nothing waiting. Quiet is good.",
                            _ => "Nothing here.",
                        }}
                    </p>
                </Show>
            </div>
            <Show when=move || { total() > reports().len() as u64 } fallback=|| ()>
                <p class="rh-adm-help">
                    {move || format!("Showing the newest {} of {}.", reports().len(), total())}
                </p>
            </Show>
        </section>
    }
}

/// What a moderator is holding back: content that reads as absent
/// everywhere until it is let through. The burrow has honoured this on
/// every surface since 0.260.0; this is where it is done and undone.
#[component]
fn HeldBack() -> impl IntoView {
    let app = expect_context::<AppState>();
    let held = move || app.moderation.with(|m| m.held.clone());
    let unknown = move || app.moderation.with(|m| m.held_unknown);
    // A page at a time: how many are not on it.
    let more = move || {
        app.moderation
            .with(|m| m.held_total.saturating_sub(m.held.len() as u64))
    };
    let some_more = move || more() != 0;
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-held-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-held-h">
                    "Held back"
                    <span class="rh-adm-count">{move || held().len()}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        on:click=move |_| app.load_held()
                    >
                        "Refresh"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "Held content is not there as far as anybody else is concerned: not in a \
                 listing, not over Hotline or news, not to another burrow. Let it through \
                 when it turns out to be fine."
            </p>
            <div class="rh-adm-rows">
                <For
                    each=held
                    key=|h| (h.subject_kind, h.subject_ref.clone())
                    children=move |h| {
                        let (kind, subject) = (h.subject_kind, h.subject_ref.clone());
                        let line = moderation::held_line(&h);
                        view! {
                            <div class="rh-adm-row">
                                <span class="rh-adm-row-line">{line}</span>
                                <button
                                    type="button"
                                    class="rh-btn ghost small"
                                    on:click=move |_| app.let_through(kind, subject.clone())
                                >
                                    "Let it through"
                                </button>
                            </div>
                        }
                    }
                />
                <Show when=move || held().is_empty() && !unknown() fallback=|| ()>
                    <p class="rh-adm-empty">"Nothing is held back."</p>
                </Show>
                <Show when=unknown fallback=|| ()>
                    <p class="rh-adm-empty">
                        "This burrow is too old to say what it is holding back. It still holds \
                         what it was told to; it cannot list it here."
                    </p>
                </Show>
                <Show when=some_more fallback=|| ()>
                    <p class="rh-adm-empty">
                        {move || format!("And {} more, not shown.", more())}
                    </p>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn ReportRow(report: ReportEntry) -> impl IntoView {
    let app = expect_context::<AppState>();
    let id = report.id;
    let note = create_rw_signal(String::new());
    let line = report_line(&report);
    // Holding back what a report is about, from the report itself: the
    // reason on the hold is the note, so the record says why.
    let (kind, subject) = (report.subject_kind, report.subject_ref.clone());
    let holdable = moderation::can_hold(kind);
    let held = {
        let subject = store_value(subject.clone());
        move || {
            subject.with_value(|s| {
                app.moderation
                    .with(|m| moderation::is_held(&m.held, kind, s))
            })
        }
    };
    let hold_subject = subject.clone();
    let free_subject = subject;
    let why = report.reason.clone();
    let mut meta = vec![
        format!("reported {}", when(report.created_at_unix)),
        format!("by account #{}", report.reporter_account),
    ];
    if !report.resolver.is_empty() {
        meta.push(format!(
            "{} by {}",
            moderation::state_name(report.state).to_lowercase(),
            report.resolver
        ));
    }
    if !report.resolution.trim().is_empty() {
        meta.push(format!("\u{201c}{}\u{201d}", report.resolution.trim()));
    }
    let actions = actions_for(report.state);
    view! {
        <div class="rh-adm-report">
            <div>
                <p class="rh-adm-report-line">{line}</p>
                <p class="rh-adm-report-meta">{meta.join(" \u{00b7} ")}</p>
            </div>
            <div class="rh-adm-report-actions">
                <Show when=move || holdable && !held() fallback=|| ()>
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        on:click={
                            let subject = hold_subject.clone();
                            let why = why.clone();
                            move |_| {
                                let note = note.get();
                                let reason = if note.trim().is_empty() { why.clone() } else { note };
                                app.hold_back(kind, subject.clone(), &reason)
                            }
                        }
                    >
                        "Hold it back"
                    </button>
                </Show>
                <Show when=move || holdable && held() fallback=|| ()>
                    <button
                        type="button"
                        class="rh-btn ghost small"
                        on:click={
                            let subject = free_subject.clone();
                            move |_| app.let_through(kind, subject.clone())
                        }
                    >
                        "Let it through"
                    </button>
                </Show>
                {actions
                    .iter()
                    .map(|(action, label)| {
                        let action = *action;
                        let primary = *label == "Resolve";
                        view! {
                            <button
                                type="button"
                                class=if primary { "rh-btn small" } else { "rh-btn ghost small" }
                                on:click=move |_| app.resolve_report(id, action, &note.get())
                            >
                                {*label}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            {(!actions.is_empty()).then(|| view! {
                <div class="rh-adm-report-note">
                    <input
                        class="rh-input"
                        aria-label="Note for the record"
                        placeholder="A note for the record (optional)"
                        maxlength="500"
                        prop:value=move || note.get()
                        on:input=move |ev| note.set(event_target_value(&ev))
                    />
                </div>
            })}
        </div>
    }
}

/// Every session on the burrow, with a Disconnect that names them.
#[component]
fn Sessions() -> impl IntoView {
    let app = expect_context::<AppState>();
    let sessions = move || app.focused().state.with(|s| s.sessions.clone());
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-sessions-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-sessions-h">
                    "Connected now"
                    <span class="rh-adm-count">{move || sessions().len()}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <button type="button" class="rh-btn ghost small" on:click=move |_| app.refresh_who()>
                        "Refresh"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "One row per connection: a person on two devices is here twice. Disconnecting \
                 ends that one session; to keep someone out, disable the account under People."
            </p>
            <div class="rh-adm-rows">
                <For
                    each=sessions
                    key=|s| s.session_id
                    children=move |s| view! { <SessionLine session=s/> }
                />
                <Show when=move || sessions().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">"Nobody is connected."</p>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn SessionLine(session: SessionRow) -> impl IntoView {
    let app = expect_context::<AppState>();
    let me = app.focused();
    let id = session.session_id;
    let who = store_value(session.screen_name.clone());
    let transport = store_value(session.transport.clone());
    // Below yourself, and never yourself: the burrow refuses the rest.
    let may_kick = move || {
        me.role.get() == 4 || (session.role < me.role.get() && who.get_value() != me.handle.get())
    };
    let is_me = move || who.get_value() == me.handle.get();
    view! {
        <div class="rh-adm-session">
            <span class="rh-adm-session-name">
                {session.screen_name.clone()}
                <Show when=is_me fallback=|| ()>
                    " " <span class="rh-badge">"You"</span>
                </Show>
            </span>
            <span class="rh-adm-session-meta">{role_name(session.role)}</span>
            <span class="rh-adm-session-meta">
                {format!("{} \u{00b7} {}", session.transport, humanize_secs(session.connected_secs.max(1)))}
            </span>
            <span>
                <Show when=may_kick fallback=|| ()>
                    <button
                        type="button"
                        class="rh-btn ghost small rh-adm-danger"
                        on:click=move |_| app.confirm.set(Some(ConfirmAsk::kick(
                            id,
                            &who.get_value(),
                            &transport.get_value(),
                        )))
                    >
                        "Disconnect\u{2026}"
                    </button>
                </Show>
            </span>
        </div>
    }
}

/// A notice to everyone connected.
#[component]
fn Broadcast() -> impl IntoView {
    let app = expect_context::<AppState>();
    let text = create_rw_signal(String::new());
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-broadcast-h">
            <h3 class="rh-adm-group-h" id="rh-adm-broadcast-h">"Say something to everyone"</h3>
            <p class="rh-adm-group-blurb">
                "A notice every connected person sees at once, wherever they are in the burrow."
            </p>
            <form
                class="rh-adm-broadcast"
                on:submit=move |ev| {
                    ev.prevent_default();
                    if !text.get().trim().is_empty() {
                        app.broadcast_notice(&text.get());
                        text.set(String::new());
                    }
                }
            >
                <textarea
                    class="rh-input"
                    aria-label="Notice"
                    placeholder="The burrow restarts in ten minutes\u{2026}"
                    rows="2"
                    maxlength="500"
                    prop:value=move || text.get()
                    on:input=move |ev| text.set(event_target_value(&ev))
                ></textarea>
                <button type="submit" class="rh-btn small" disabled=move || text.get().trim().is_empty()>
                    "Send"
                </button>
            </form>
        </section>
    }
}

/// Content refused by its hash, wherever it is uploaded.
#[component]
fn DenyList() -> impl IntoView {
    let app = expect_context::<AppState>();
    let hash = create_rw_signal(String::new());
    let reason = create_rw_signal(String::new());
    let deny = move || app.moderation.with(|m| m.deny.clone());
    let parsed = move || parse_hash(&hash.get());
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-deny-h">
            <h3 class="rh-adm-group-h" id="rh-adm-deny-h">"Refused content"</h3>
            <p class="rh-adm-group-blurb">
                "A file is refused by what it is, not what it is called: its blake3 id, the one \
                 shown in a file\u{2019}s details. An upload of the same bytes is turned away \
                 wherever it comes in."
            </p>
            <div class="rh-adm-rows">
                <For
                    each=deny
                    key=|d| d.hash
                    children=move |d| view! { <DenyLine entry=d/> }
                />
                <Show when=move || deny().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">"Nothing is refused."</p>
                </Show>
            </div>
            <form
                class="rh-adm-add"
                on:submit=move |ev| {
                    ev.prevent_default();
                    if let Some(h) = parsed() {
                        app.add_deny_hash(h, &reason.get());
                        hash.set(String::new());
                        reason.set(String::new());
                    }
                }
            >
                <input
                    class="rh-input rh-adm-mono"
                    aria-label="blake3 id to refuse"
                    placeholder="blake3 id, 64 hex characters"
                    autocomplete="off"
                    spellcheck="false"
                    prop:value=move || hash.get()
                    on:input=move |ev| hash.set(event_target_value(&ev))
                />
                <input
                    class="rh-input"
                    aria-label="Why"
                    placeholder="Why"
                    maxlength="200"
                    prop:value=move || reason.get()
                    on:input=move |ev| reason.set(event_target_value(&ev))
                />
                <button type="submit" class="rh-btn ghost small" disabled=move || parsed().is_none()>
                    "Refuse"
                </button>
            </form>
        </section>
    }
}

#[component]
fn DenyLine(entry: DenyHashEntry) -> impl IntoView {
    let app = expect_context::<AppState>();
    let hash = entry.hash;
    view! {
        <div class="rh-adm-deny">
            <code class="rh-adm-mono" title=hex::encode(hash)>{short_hash(&hash)}</code>
            <span class="rh-adm-deny-why">{deny_line(&entry)}</span>
            <button
                type="button"
                class="rh-btn ghost small"
                on:click=move |_| app.remove_deny_hash(hash)
            >
                "Allow again"
            </button>
        </div>
    }
}

/// What operators and moderators did here.
#[component]
fn AuditLog() -> impl IntoView {
    let app = expect_context::<AppState>();
    let audit = move || {
        let mut lines = app.moderation.with(|m| m.audit.clone());
        lines.reverse();
        lines
    };
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-audit-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-audit-h">"Audit log"</h3>
                <div class="rh-adm-group-tools">
                    <button type="button" class="rh-btn ghost small" on:click=move |_| app.load_audit()>
                        "Refresh"
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "Every change an operator or moderator made, newest first. A password is never \
                 in it."
            </p>
            <div class="rh-adm-rows rh-adm-audit">
                <For
                    each=audit
                    key=|e| (e.at, e.actor.clone(), e.action.clone(), e.detail.clone())
                    children=move |e| view! {
                        <div class="rh-adm-audit-line">
                            <span class="rh-adm-audit-when">{when(e.at)}</span>
                            <span>{audit_line(&e)}</span>
                        </div>
                    }
                />
                <Show when=move || audit().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">"Nothing on record yet."</p>
                </Show>
            </div>
        </section>
    }
}
