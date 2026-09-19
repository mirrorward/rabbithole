//! The Backups pane of the admin console: snapshots of the whole burrow,
//! made, checked and removed from where the operator sits. Restoring stays
//! at the machine with the burrow stopped, and the pane says how.

use leptos::*;
use rabbithole_proto::admin::BackupEntry;

use crate::admin_federation::{backup_line, check_line, stamp_label};
use crate::app::{AppState, ConfirmAsk};

/// The pane.
#[component]
pub fn BackupsPane() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.load_backups();
    let backups = move || app.federation.with(|f| f.backups.clone());
    let dir = move || app.federation.with(|f| f.backups_dir.clone());
    let making = move || app.people.with(|p| p.is_busy("*backup-make"));
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-backups-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-backups-h">
                    "Snapshots"
                    <span class="rh-adm-count">{move || backups().len()}</span>
                </h3>
                <div class="rh-adm-group-tools">
                    <button type="button" class="rh-btn ghost small" on:click=move |_| app.load_backups()>
                        "Refresh"
                    </button>
                    <button
                        type="button"
                        class="rh-btn small"
                        disabled=making
                        on:click=move |_| app.make_backup()
                    >
                        {move || if making() { "Making\u{2026}" } else { "Make a snapshot" }}
                    </button>
                </div>
            </div>
            <p class="rh-adm-group-blurb">
                "A snapshot is the whole burrow at one moment: the database, its identity keys, \
                 the federation approvals and every file, each listed with its hash. Making one \
                 interrupts nobody."
            </p>
            <Show when=move || !dir().is_empty() fallback=|| ()>
                <p class="rh-adm-group-blurb">
                    "Written to " <code class="rh-adm-mono">{dir}</code>
                    ". The folder is a setting under Burrow."
                </p>
            </Show>
            <div class="rh-adm-rows">
                <For
                    each=backups
                    key=|b| b.name.clone()
                    children=move |b| view! { <BackupRow entry=b/> }
                />
                <Show when=move || backups().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">
                        "No snapshots yet. Make one before anything you might want to undo."
                    </p>
                </Show>
            </div>
        </section>
        <section class="rh-adm-group" aria-labelledby="rh-adm-restore-h">
            <h3 class="rh-adm-group-h" id="rh-adm-restore-h">"Restoring"</h3>
            <p class="rh-adm-group-blurb">
                "A running burrow cannot swap its own database out, so restoring is done at the \
                 machine with the burrow stopped. The snapshot is checked first, and the current \
                 data folder is moved aside, never deleted."
            </p>
            <pre class="rh-adm-code">"burrow restore <snapshot folder> --data-dir <data folder>"</pre>
        </section>
    }
}

#[component]
fn BackupRow(entry: BackupEntry) -> impl IntoView {
    let app = expect_context::<AppState>();
    let name = store_value(entry.name.clone());
    let check = move || {
        app.federation
            .with(|f| f.check_of(&name.get_value()).cloned())
    };
    let check_tag = store_value(format!("*backup-verify:{}", entry.name));
    let checking = move || app.people.with(|p| p.is_busy(&check_tag.get_value()));
    let failed = move || check().is_some_and(|c| !c.ok);
    let meta = format!(
        "{} \u{00b7} {}",
        stamp_label(&entry.created_at),
        backup_line(&entry)
    );
    view! {
        <div class="rh-adm-backup">
            <div class="rh-adm-backup-who">
                <span class="rh-adm-backup-name rh-adm-mono">{entry.name.clone()}</span>
                <span class="rh-adm-backup-meta">{meta}</span>
                <Show when=move || check().is_some() fallback=|| ()>
                    <span class="rh-adm-backup-check" class:rh-adm-bad=failed role="status">
                        {move || check().map(|c| check_line(&c)).unwrap_or_default()}
                    </span>
                </Show>
            </div>
            <span class="rh-adm-backup-actions">
                <button
                    type="button"
                    class="rh-btn ghost small"
                    disabled=checking
                    on:click=move |_| app.verify_backup(&name.get_value())
                >
                    {move || if checking() { "Checking\u{2026}" } else { "Check" }}
                </button>
                <button
                    type="button"
                    class="rh-btn ghost small rh-adm-danger"
                    on:click=move |_| app.confirm.set(Some(ConfirmAsk::delete_backup(&name.get_value())))
                >
                    "Remove\u{2026}"
                </button>
            </span>
        </div>
    }
}
