//! The Boards pane of the admin console: the board tree, a form for a new
//! board or category, and each node's own controls.
//!
//! A board's address (its slug) is its identity: its route, its newsgroup
//! name, its place in the gateway maps. So it is chosen once, suggested from
//! the title, and never edited afterwards. Only an empty board can be removed;
//! the burrow refuses the rest, and says so.

use leptos::*;

use crate::admin_people::{slug_is_acceptable, slugify};
use crate::app::{AppState, ConfirmAsk};
use crate::state::BoardNode;

const KIND_CATEGORY: u8 = 0;
const KIND_BOARD: u8 = 2;

fn kind_name(kind: u8) -> &'static str {
    match kind {
        KIND_CATEGORY => "Category",
        1 => "Bundle",
        _ => "Board",
    }
}

/// The tree in the order it reads: each top-level node, then what hangs from
/// it, with a depth to indent by. A node whose parent is gone is shown at the
/// top rather than lost.
fn in_reading_order(tree: &[BoardNode]) -> Vec<(usize, BoardNode)> {
    fn walk(
        tree: &[BoardNode],
        parent: Option<&str>,
        depth: usize,
        out: &mut Vec<(usize, BoardNode)>,
    ) {
        for node in tree.iter().filter(|n| n.parent.as_deref() == parent) {
            out.push((depth, node.clone()));
            if depth < 8 {
                walk(tree, Some(&node.slug), depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, None, 0, &mut out);
    for node in tree {
        let orphan = node
            .parent
            .as_deref()
            .is_some_and(|p| tree.iter().all(|n| n.slug != p));
        if orphan {
            out.push((0, node.clone()));
        }
    }
    out
}

/// The pane.
#[component]
pub fn BoardsPane() -> impl IntoView {
    let app = expect_context::<AppState>();
    app.each_sign_in(move || {
        app.load_boards();
        app.load_board_keeping();
    });
    let adding = create_rw_signal(false);
    let open = create_rw_signal(None::<String>);
    let tree = move || {
        app.focused()
            .state
            .with(|s| in_reading_order(&s.board_tree))
    };
    view! {
        <section class="rh-adm-group" aria-labelledby="rh-adm-boards-h">
            <div class="rh-adm-group-head">
                <h3 class="rh-adm-group-h" id="rh-adm-boards-h">"Board tree"</h3>
                <div class="rh-adm-group-tools">
                    <button
                        type="button"
                        class="rh-btn small"
                        aria-expanded=move || adding.get().to_string()
                        on:click=move |_| adding.update(|a| *a = !*a)
                    >
                        "New board"
                    </button>
                </div>
            </div>
            <Show when=move || adding.get() fallback=|| ()>
                <NewBoard on_done=Callback::new(move |_| adding.set(false))/>
            </Show>
            <div class="rh-adm-rows">
                <For
                    each=tree
                    key=|(depth, n)| (*depth, n.slug.clone(), n.title.clone(), n.description.clone())
                    children=move |(depth, node)| view! { <BoardRow node=node depth=depth open=open/> }
                />
                <Show when=move || tree().is_empty() fallback=|| ()>
                    <p class="rh-adm-empty">
                        "No boards yet. Make one, and people have somewhere to post."
                    </p>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn NewBoard(on_done: Callback<()>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let title = create_rw_signal(String::new());
    let slug = create_rw_signal(String::new());
    // Until the operator types an address of their own, it follows the title.
    let slug_touched = create_rw_signal(false);
    let description = create_rw_signal(String::new());
    let kind = create_rw_signal(KIND_BOARD);
    let parent = create_rw_signal(String::new());
    let categories = move || {
        app.focused().state.with(|s| {
            s.board_tree
                .iter()
                .filter(|n| n.kind != KIND_BOARD)
                .map(|n| (n.slug.clone(), n.title.clone()))
                .collect::<Vec<_>>()
        })
    };
    let taken = move || {
        let wanted = slug.get();
        app.focused()
            .state
            .with(|s| s.board_tree.iter().any(|n| n.slug == wanted))
    };
    let ready =
        move || !title.get().trim().is_empty() && slug_is_acceptable(&slug.get()) && !taken();
    let slug_problem = move || {
        let s = slug.get();
        if s.is_empty() {
            None
        } else if taken() {
            Some("There is already a board at this address.")
        } else if !slug_is_acceptable(&s) {
            Some("Lowercase letters, digits, dots, dashes and underscores, starting with a letter or digit.")
        } else {
            None
        }
    };
    // Close once the burrow has it: the new node is in the tree.
    let sent = create_rw_signal(None::<String>);
    create_effect(move |_| {
        let Some(wanted) = sent.get() else { return };
        if app
            .focused()
            .state
            .with(|s| s.board_tree.iter().any(|n| n.slug == wanted))
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
                    let p = parent.get();
                    app.create_board(
                        &slug.get(),
                        &title.get(),
                        &description.get(),
                        kind.get(),
                        (!p.is_empty()).then_some(p),
                    );
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
                        "Where it lives: in links, and as its newsgroup name. It cannot be changed later.",
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
            <label class="rh-adm-field">
                <span>"Kind"</span>
                <select
                    class="rh-select"
                    on:change=move |ev| kind.set(event_target_value(&ev).parse().unwrap_or(KIND_BOARD))
                >
                    <option value="2" selected=true>"Board: holds posts"</option>
                    <option value="0">"Category: holds boards"</option>
                </select>
            </label>
            <label class="rh-adm-field">
                <span>"Inside"</span>
                <select class="rh-select" on:change=move |ev| parent.set(event_target_value(&ev))>
                    <option value="" selected=true>"Nothing: top level"</option>
                    {move || categories()
                        .into_iter()
                        .map(|(slug, title)| view! { <option value=slug>{title}</option> })
                        .collect_view()}
                </select>
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
fn BoardRow(node: BoardNode, depth: usize, open: RwSignal<Option<String>>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let slug = store_value(node.slug.clone());
    let saved_title = node.title.clone();
    let saved_description = node.description.clone();
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
    // What this board keeps, as the burrow last said: a listing carries
    // neither the limit nor how many threads there are.
    let kept = move || app.admin.with(|a| a.board_keeping.clone());
    let saved_keep = move || kept().board(&slug.get_value()).map(|(max, _)| max);
    let keep = create_rw_signal(String::new());
    // Filled in from the burrow until the operator types: a listing that
    // lands while they are typing must not take back what they wrote.
    let keep_touched = create_rw_signal(false);
    // What was asked for and not yet confirmed. A refused save must leave
    // the number the operator typed where they typed it, not quietly put
    // the old one back.
    let keep_sent = create_rw_signal(None::<u32>);
    create_effect(move |_| {
        let said = saved_keep();
        let sent = keep_sent.get_untracked();
        if sent.is_some() && sent == said {
            // The burrow has what was asked for, so the field can follow
            // the burrow again — unless something else has been typed
            // since, which is still theirs to save.
            keep_sent.set(None);
            if keep.get_untracked().trim().parse::<u32>().ok() == sent {
                keep_touched.set(false);
            }
        }
        if !keep_touched.get_untracked() {
            keep.set(said.map(|max| max.to_string()).unwrap_or_default());
        }
    });
    let typed = move || keep.get().trim().to_string();
    let typed_keep = move || typed().parse::<u32>().ok();
    // A number, or nothing at all. Anything else is not quietly read as
    // "leave it as it is".
    let keep_problem = move || {
        (!typed().is_empty() && typed_keep().is_none())
            .then_some("A whole number, or 0 to keep every thread.")
    };
    // Only what the operator moved is sent: saving a title carries None,
    // which leaves retention exactly as the burrow has it.
    let keep_to_save = move || match typed_keep() {
        Some(n) if Some(n) != saved_keep() => Some(n),
        _ => None,
    };
    let keep_said = move || crate::admin::keeping_line(&kept(), &slug.get_value());
    // A board is its own row, so the field's ids carry its slug.
    let keep_id = store_value(format!("rh-adm-keep-{}", node.slug));
    let keep_help_id = store_value(format!("rh-adm-keep-help-{}", node.slug));
    let keep_help = move || match keep_problem() {
        Some(say) => say.to_string(),
        None if typed().is_empty() && saved_keep().is_some() => {
            format!("{} Left empty, it stays as it is.", keep_said())
        }
        None => keep_said(),
    };
    let changed = {
        let (t, d) = (saved_title.clone(), saved_description.clone());
        move || title.get().trim() != t || description.get().trim() != d || keep_to_save().is_some()
    };
    let can_save = {
        let changed = changed.clone();
        move || changed() && !title.get().trim().is_empty() && keep_problem().is_none()
    };
    let shown_title = store_value(node.title.clone());
    // Where this board sits among its own, and what moving it means: up is
    // after whatever the one above it follows (or first), down is after the
    // one below it.
    let parent = store_value(node.parent.clone());
    let siblings = move || {
        app.focused().state.with(|s| {
            s.board_tree
                .iter()
                .filter(|n| n.parent == parent.get_value())
                .map(|n| n.slug.clone())
                .collect::<Vec<_>>()
        })
    };
    let place = move || {
        let mine = slug.get_value();
        let order = siblings();
        order.iter().position(|s| *s == mine).map(|at| (at, order))
    };
    let can_up = move || place().is_some_and(|(at, _)| at > 0);
    let can_down = move || place().is_some_and(|(at, order)| at + 1 < order.len());
    view! {
        <div class="rh-adm-acct" class:open=is_open>
            <button
                type="button"
                class="rh-adm-acct-line rh-adm-board-line"
                style=format!("padding-left:calc(var(--rh-space-4) + {}rem)", depth as f32 * 1.25)
                aria-expanded=move || is_open().to_string()
                on:click=toggle
            >
                <span class="rh-adm-acct-name">{node.title.clone()}</span>
                <span class="rh-adm-acct-role rh-adm-mono">{node.slug.clone()}</span>
                <span class="rh-adm-acct-class">{kind_name(node.kind)}</span>
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
                        <Show when=move || node.kind == KIND_BOARD fallback=|| ()>
                            <div class="rh-adm-field">
                                // The help line describes the field; it is
                                // not part of its name. Inside the label it
                                // would be read out as one, every time.
                                <label for=keep_id.get_value()>"Threads to keep"</label>
                                <input
                                    class="rh-input"
                                    type="number"
                                    min="0"
                                    max="100000"
                                    id=keep_id.get_value()
                                    aria-describedby=keep_help_id.get_value()
                                    prop:value=move || keep.get()
                                    on:input=move |ev| {
                                        keep_touched.set(true);
                                        keep.set(event_target_value(&ev))
                                    }
                                />
                                <span class="rh-adm-help" id=keep_help_id.get_value()>
                                    {move || keep_help()}
                                </span>
                            </div>
                        </Show>
                    </div>
                    <div class="rh-adm-acct-actions">
                        <button
                            type="button"
                            class="rh-btn ghost small"
                            title="Move up"
                            disabled=move || !can_up()
                            on:click=move |_| {
                                if let Some((at, order)) = place() {
                                    // After whatever the one above follows.
                                    let after = (at >= 2).then(|| order[at - 2].clone());
                                    app.move_board(&slug.get_value(), after);
                                }
                            }
                        >
                            "Move up"
                        </button>
                        <button
                            type="button"
                            class="rh-btn ghost small"
                            title="Move down"
                            disabled=move || !can_down()
                            on:click=move |_| {
                                if let Some((at, order)) = place() {
                                    app.move_board(
                                        &slug.get_value(),
                                        Some(order[at + 1].clone()),
                                    );
                                }
                            }
                        >
                            "Move down"
                        </button>
                        <button
                            type="button"
                            class="rh-btn ghost small rh-adm-danger"
                            on:click=move |_| app.confirm.set(Some(ConfirmAsk::delete_board(
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
                            on:click=move |_| {
                                let keep_arg = keep_to_save();
                                app.update_board(
                                    &slug.get_value(),
                                    &title.get(),
                                    &description.get(),
                                    keep_arg,
                                );
                                match keep_arg {
                                    Some(n) => keep_sent.set(Some(n)),
                                    None => keep_touched.set(false),
                                }
                            }
                        >
                            "Save"
                        </button>
                    </div>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(slug: &str, kind: u8, parent: Option<&str>) -> BoardNode {
        BoardNode {
            slug: slug.into(),
            title: slug.to_uppercase(),
            description: String::new(),
            kind,
            parent: parent.map(str::to_string),
        }
    }

    #[test]
    fn the_tree_reads_top_down_and_loses_nobody() {
        let tree = vec![
            node("zebra", 2, None),
            node("commons", 0, None),
            node("general", 2, Some("commons")),
            node("deep", 0, Some("commons")),
            node("deeper", 2, Some("deep")),
            node("lost", 2, Some("gone")),
        ];
        let order: Vec<(usize, String)> = in_reading_order(&tree)
            .into_iter()
            .map(|(d, n)| (d, n.slug))
            .collect();
        assert_eq!(
            order,
            [
                (0, "zebra".to_string()),
                (0, "commons".to_string()),
                (1, "general".to_string()),
                (1, "deep".to_string()),
                (2, "deeper".to_string()),
                (0, "lost".to_string()),
            ]
        );
        // A cycle cannot hang the console.
        let knot = vec![node("a", 0, Some("b")), node("b", 0, Some("a"))];
        assert!(in_reading_order(&knot).is_empty());
        assert_eq!(kind_name(0), "Category");
        assert_eq!(kind_name(2), "Board");
    }
}
