//! Your profile on the focused burrow. Drafts stay put until the server
//! acknowledges a save; profile text is never written to another session.

use leptos::*;
use rabbithole_proto::persona::{PersonaInfo, PersonaUpdate, Profile};

use crate::app::{AppState, ServerId};
use crate::avatar::ChosenMark;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IconChoice {
    #[default]
    Keep,
    Remove,
    Mark(ChosenMark),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields {
    pub pronouns: String,
    pub location: String,
    pub interests: String,
    pub quote: String,
    pub plan: String,
    pub directory_visible: bool,
}

impl Fields {
    fn from_persona(persona: &PersonaInfo) -> Self {
        let p = &persona.profile;
        Self {
            pronouns: p.pronouns.clone().unwrap_or_default(),
            location: p.location.clone().unwrap_or_default(),
            interests: p.interests.clone().unwrap_or_default(),
            quote: p.quote.clone().unwrap_or_default(),
            plan: p.plan.clone().unwrap_or_default(),
            directory_visible: persona.directory_visible,
        }
    }

    /// Restore only edited fields, preserving fresh server values elsewhere.
    #[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
    fn restore_changes(&mut self, baseline: &Self, edited: &Self) {
        for (current, before, after) in [
            (&mut self.pronouns, &baseline.pronouns, &edited.pronouns),
            (&mut self.location, &baseline.location, &edited.location),
            (&mut self.interests, &baseline.interests, &edited.interests),
            (&mut self.quote, &baseline.quote, &edited.quote),
            (&mut self.plan, &baseline.plan, &edited.plan),
        ] {
            if before != after {
                *current = after.clone();
            }
        }
        if baseline.directory_visible != edited.directory_visible {
            self.directory_visible = edited.directory_visible;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DraftKey {
    burrow: ServerId,
    persona: i64,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
struct CachedDraft {
    screen_name: String,
    baseline: Fields,
    fields: Fields,
    icon: IconChoice,
    revision: u64,
}

/// No storage serialization: unpublished text never leaves session memory.
/// Revisions keep an old save acknowledgement from deleting newer edits.
#[derive(Debug, Default)]
pub struct DraftCache {
    entries: std::collections::HashMap<DraftKey, CachedDraft>,
    next_revision: u64,
}

// Cached reads are triggered by wasm network replies, or model tests.
#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
impl DraftCache {
    fn remember(&mut self, draft: &ProfileDraft) {
        let (Some(key), Some(persona)) = (&draft.key, &draft.persona) else {
            return;
        };
        if !draft.dirty() {
            self.entries.remove(key);
            return;
        }
        self.next_revision += 1;
        self.entries.insert(
            key.clone(),
            CachedDraft {
                screen_name: persona.screen_name.clone(),
                baseline: Fields::from_persona(persona),
                fields: draft.fields.clone(),
                icon: draft.icon,
                revision: self.next_revision,
            },
        );
    }

    fn restore(&mut self, burrow: ServerId, draft: &mut ProfileDraft) {
        let Some(persona) = &draft.persona else {
            return;
        };
        let key = DraftKey {
            burrow,
            persona: persona.id,
        };
        if let Some(cached) = self.entries.get(&key) {
            // Persona IDs are server-owned; additionally reject changed
            // handles so a replaced account cannot inherit an old draft.
            if cached.screen_name == persona.screen_name {
                draft
                    .fields
                    .restore_changes(&cached.baseline, &cached.fields);
                draft.icon = cached.icon;
            }
        }
        draft.key = Some(key.clone());
        if !draft.dirty() {
            self.entries.remove(&key);
        }
    }

    fn revision(&self, key: &DraftKey) -> Option<u64> {
        self.entries.get(key).map(|entry| entry.revision)
    }

    fn acknowledge(&mut self, key: &DraftKey, revision: Option<u64>) {
        if revision.is_some() && self.revision(key) == revision {
            self.entries.remove(key);
        }
    }

    fn discard(&mut self, key: &DraftKey) {
        self.entries.remove(key);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileDraft {
    key: Option<DraftKey>,
    pub persona: Option<PersonaInfo>,
    pub fields: Fields,
    pub loading: bool,
    pub saving: bool,
    pub error: Option<String>,
    pub saved: bool,
    pub icon: IconChoice,
    pub avatar_src: Option<String>,
}

/// Snapshot edits synchronously; route disposal cannot race a queued effect.
fn edit(app: AppState, draft: RwSignal<ProfileDraft>, update: impl FnOnce(&mut ProfileDraft)) {
    draft.update(update);
    draft.with_untracked(|draft| {
        app.profile_drafts
            .update_value(|cache| cache.remember(draft))
    });
}

fn revert(app: AppState, draft: RwSignal<ProfileDraft>) {
    if let Some(key) = draft.with_untracked(|d| d.key.clone()) {
        app.profile_drafts.update_value(|cache| cache.discard(&key));
    }
    draft.update(ProfileDraft::revert);
}

impl ProfileDraft {
    pub fn dirty(&self) -> bool {
        self.persona.as_ref().is_some_and(|p| {
            self.fields != Fields::from_persona(p) || self.icon != IconChoice::Keep
        })
    }

    pub fn accept(&mut self, persona: PersonaInfo, saved: bool) {
        self.fields = Fields::from_persona(&persona);
        self.persona = Some(persona);
        self.loading = false;
        self.saving = false;
        self.error = None;
        self.saved = saved;
        self.icon = IconChoice::Keep;
    }

    pub fn revert(&mut self) {
        if let Some(persona) = &self.persona {
            self.fields = Fields::from_persona(persona);
        }
        self.error = None;
        self.saved = false;
        self.icon = IconChoice::Keep;
    }

    pub fn fail(&mut self, why: String) {
        self.loading = false;
        self.saving = false;
        self.error = Some(why);
        self.saved = false;
    }

    pub fn update(&self) -> Option<PersonaUpdate> {
        let mut update = PersonaUpdate::default();
        update.id = self.persona.as_ref()?.id;
        // None means "leave unchanged" on the server. Empty fields must be
        // Some("") to actually clear text somebody previously published.
        update.profile = Some(Profile::new(
            Some(self.fields.location.clone()),
            Some(self.fields.interests.clone()),
            Some(self.fields.quote.clone()),
            Some(self.fields.plan.clone()),
            Some(self.fields.pronouns.clone()),
        ));
        update.directory_visible = Some(self.fields.directory_visible);
        if self.icon == IconChoice::Remove {
            update.avatar = Some(None);
        }
        Some(update)
    }
}

/// The inline form in You. The active persona comes from the burrow, not
/// from a handle lookup that could choose another persona on the account.
#[component]
pub fn ProfileEditor() -> impl IntoView {
    let app = expect_context::<AppState>();
    let draft = create_rw_signal(ProfileDraft::default());
    let generation = create_rw_signal(0_u64);
    let scope = store_value(None::<(RwSignal<crate::state::UiState>, String)>);
    let connected = move || app.focused_tracked().state.with(|s| s.conn.is_live());
    let eligible = move || {
        let session = app.focused_tracked();
        app.has_burrows() && session.live.get() && !session.is_guest.get()
    };

    // Track focus and sign-ins. A reconnect refreshes clean forms; a dirty
    // draft is kept so a dropped connection cannot erase somebody's work.
    create_effect(move |_| {
        let session = app.focused_tracked();
        let _ = session.ready.get();
        let account = session.handle.get();
        let may_edit = eligible();
        untrack(move || {
            let owner = (session.state, account);
            let changed = scope.get_value().as_ref() != Some(&owner);
            scope.set_value(Some(owner));
            if changed || !may_edit {
                generation.update(|g| *g += 1);
                draft.set(ProfileDraft::default());
            }
            if may_edit && !draft.with_untracked(ProfileDraft::dirty) {
                crate::app::defer(move || load(app, draft, generation));
            }
        });
    });

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if eligible() && connected() && draft.with_untracked(|d| d.dirty() && !d.saving) {
            save(app, draft, generation);
        }
    };

    view! {
        <style>{include_str!("profile_edit.css")}</style>
        <section class="rh-profile-editor" aria-labelledby="rh-my-profile-title">
            <h2 class="rh-person-h2" id="rh-my-profile-title">"Your burrow profile"</h2>
            <Show
                when=eligible
                fallback=move || view! {
                    <p class="rh-settings-note">
                        {move || if !app.has_burrows() {
                            "Connect to a burrow and sign in with an account to make a profile."
                        } else if !app.focused_tracked().live.get() {
                            "Profiles are saved on a real burrow. Connect to one to make yours."
                        } else {
                            "Sign in with an account to make a profile. Guest names have no saved profile."
                        }}
                    </p>
                }
            >
                <p class="rh-settings-note">
                    {move || format!("How people know you on {}. These details are saved only on this burrow.", app.focused_burrow_label())}
                </p>
                <Show when=move || draft.with(|d| d.loading) fallback=|| ()>
                    <p class="rh-settings-note" role="status">"Loading your profile…"</p>
                </Show>
                <Show when=move || draft.with(|d| d.persona.is_some()) fallback=|| ()>
                    <form on:submit=submit>
                        <p class="rh-profile-edit-name">
                            {move || draft.with(|d| d.persona.as_ref().map(|p| format!("@{}", p.screen_name)).unwrap_or_default())}
                        </p>
                        <fieldset class="rh-profile-edit-fields" disabled=move || draft.with(|d| d.saving || d.loading)>
                            <ProfileIcon draft/>
                            <div class="rh-profile-edit-grid">
                                <ProfileField draft field="pronouns" label="Pronouns"/>
                                <ProfileField draft field="location" label="Location"/>
                                <ProfileField draft field="interests" label="Interests"/>
                                <ProfileField draft field="quote" label="A few words about you"/>
                            </div>
                            <label class="rh-profile-edit-field" for="rh-profile-plan">
                                <span>"Your .plan"</span>
                                <textarea
                                    class="rh-textarea"
                                    id="rh-profile-plan"
                                    rows="4"
                                    prop:value=move || draft.with(|d| d.fields.plan.clone())
                                    on:input=move |ev| edit(app, draft, |d| {
                                        d.fields.plan = event_target_value(&ev);
                                        d.saved = false;
                                    })
                                ></textarea>
                            </label>
                            <p class="rh-field-hint">"What you're working on, looking for, or thinking about."</p>
                            <label class="rh-settings-check">
                                <input
                                    type="checkbox"
                                    prop:checked=move || draft.with(|d| d.fields.directory_visible)
                                    on:change=move |ev| edit(app, draft, |d| {
                                        d.fields.directory_visible = event_target_checked(&ev);
                                        d.saved = false;
                                    })
                                />
                                <span>"Show my profile in this burrow's member directory"</span>
                            </label>
                        </fieldset>
                        <div class="rh-profile-edit-actions">
                            <button
                                type="submit"
                                class="rh-btn small"
                                prop:disabled=move || !connected() || draft.with(|d| !d.dirty() || d.saving || d.loading)
                            >
                                {move || if draft.with(|d| d.saving) { "Saving…" } else { "Save profile" }}
                            </button>
                            <button
                                type="button"
                                class="rh-btn ghost small"
                                prop:disabled=move || draft.with(|d| !d.dirty() || d.saving)
                                on:click=move |_| revert(app, draft)
                            >"Revert changes"</button>
                            <span class="rh-field-hint" role="status">
                                {move || if draft.with(|d| d.saved) {
                                    "Profile saved."
                                } else if !connected() {
                                    "Reconnect to save. Your edits are kept here."
                                } else if draft.with(ProfileDraft::dirty) {
                                    "Unsaved changes · kept until you close the app"
                                } else { "" }}
                            </span>
                        </div>
                    </form>
                </Show>
                <Show when=move || draft.with(|d| d.error.is_some()) fallback=|| ()>
                    <p class="rh-field-hint error" role="alert">{move || draft.with(|d| d.error.clone())}</p>
                    <Show when=move || draft.with(|d| d.persona.is_none()) fallback=|| ()>
                        <button
                            type="button"
                            class="rh-btn ghost small"
                            prop:disabled=move || !connected() || draft.with(|d| d.loading)
                            on:click=move |_| load(app, draft, generation)
                        >"Try again"</button>
                    </Show>
                </Show>
            </Show>
        </section>
    }
}

#[component]
fn ProfileIcon(draft: RwSignal<ProfileDraft>) -> impl IntoView {
    let app = expect_context::<AppState>();
    let choosing = create_rw_signal(false);
    let local_mark = move || {
        app.my_mark.get().unwrap_or_else(|| {
            let mark =
                crate::avatar::mark(&app.you.get().map(|you| you.public_hex).unwrap_or_default());
            ChosenMark {
                glyph: mark.glyph,
                color: mark.color,
            }
        })
    };
    let selection = move || {
        draft.with(|d| match d.icon {
            IconChoice::Mark(mark) => mark,
            _ => local_mark(),
        })
    };
    let choose = move |mark| {
        edit(app, draft, |d| {
            d.icon = IconChoice::Mark(mark);
            d.saved = false;
        })
    };
    view! {
        <div class="rh-profile-icon">
            <div class="rh-profile-icon-line">
                {move || {
                    let src = draft.with(|d| if d.icon == IconChoice::Keep { d.avatar_src.clone() } else { None });
                    if let Some(src) = src {
                        view! { <img class="rh-profile-icon-preview" src=src alt="Your published profile icon"/> }.into_view()
                    } else {
                        let mark = draft.with(|d| match d.icon {
                            IconChoice::Mark(mark) => mark,
                            _ => {
                                let mark = crate::avatar::mark(&app.you.get().map(|you| you.public_hex).unwrap_or_default());
                                ChosenMark { glyph: mark.glyph, color: mark.color }
                            }
                        });
                        view! { <span class="rh-mark rh-profile-icon-preview" aria-label="Profile icon preview" inner_html=crate::avatar::glyph_svg(mark.glyph, mark.color, 64)></span> }.into_view()
                    }
                }}
                <div>
                    <span class="rh-profile-icon-label">"Profile icon"</span>
                    <p class="rh-field-hint">"Choose a face for this burrow. Save profile to share it."</p>
                    <div class="rh-profile-edit-actions">
                        <button type="button" class="rh-btn ghost small" aria-expanded=move || choosing.get().to_string() on:click=move |_| choosing.update(|on| *on = !*on)>
                            {move || if choosing.get() { "Done choosing" } else { "Choose icon" }}
                        </button>
                        <button type="button" class="rh-btn ghost small" on:click=move |_| choose(local_mark())>"Use my local mark"</button>
                        <Show when=move || draft.with(|d| d.persona.as_ref().is_some_and(|p| p.avatar.is_some()) || matches!(d.icon, IconChoice::Mark(_))) fallback=|| ()>
                            <button type="button" class="rh-btn ghost small" on:click=move |_| edit(app, draft, |d| {
                                d.icon = if d.persona.as_ref().is_some_and(|p| p.avatar.is_some()) { IconChoice::Remove } else { IconChoice::Keep };
                                d.saved = false;
                            })>"Remove icon"</button>
                        </Show>
                    </div>
                </div>
            </div>
            <Show when=move || choosing.get() fallback=|| ()>
                <div class="rh-mark-picker" role="group" aria-label="Profile icon">
                    {(0..crate::avatar::GLYPH_COUNT).map(|glyph| view! {
                        <button
                            type="button"
                            class="rh-mark-choice"
                            class:on=move || draft.with(|d| matches!(d.icon, IconChoice::Mark(mark) if mark.glyph == glyph))
                            aria-label=crate::avatar::glyph_name(glyph)
                            aria-pressed=move || draft.with(|d| matches!(d.icon, IconChoice::Mark(mark) if mark.glyph == glyph)).to_string()
                            on:click=move |_| choose(ChosenMark { glyph, color: selection().color })
                            inner_html=move || crate::avatar::glyph_svg(glyph, selection().color, 32)
                        ></button>
                    }).collect_view()}
                </div>
                <div class="rh-mark-colors" role="group" aria-label="Profile icon colour">
                    {(0..crate::avatar::PALETTE.len()).map(|color| view! {
                        <button
                            type="button"
                            class="rh-mark-color"
                            class:on=move || selection().color == color
                            aria-label=format!("Profile colour {}", color + 1)
                            aria-pressed=move || (selection().color == color).to_string()
                            style=format!("background:{}", crate::avatar::PALETTE[color])
                            on:click=move |_| choose(ChosenMark { glyph: selection().glyph, color })
                        ></button>
                    }).collect_view()}
                </div>
            </Show>
        </div>
    }
}

#[component]
fn ProfileField(
    draft: RwSignal<ProfileDraft>,
    field: &'static str,
    label: &'static str,
) -> impl IntoView {
    let app = expect_context::<AppState>();
    let id = format!("rh-profile-{field}");
    view! {
        <label class="rh-profile-edit-field" for=id.clone()>
            <span>{label}</span>
            <input
                class="rh-input"
                id=id
                prop:value=move || draft.with(|d| match field {
                    "pronouns" => d.fields.pronouns.clone(),
                    "location" => d.fields.location.clone(),
                    "interests" => d.fields.interests.clone(),
                    _ => d.fields.quote.clone(),
                })
                on:input=move |ev| edit(app, draft, |d| {
                    let value = event_target_value(&ev);
                    match field {
                        "pronouns" => d.fields.pronouns = value,
                        "location" => d.fields.location = value,
                        "interests" => d.fields.interests = value,
                        _ => d.fields.quote = value,
                    }
                    d.saved = false;
                })
            />
        </label>
    }
}

#[cfg(target_arch = "wasm32")]
fn load(app: AppState, draft: RwSignal<ProfileDraft>, generation: RwSignal<u64>) {
    if draft
        .try_get_untracked()
        .is_none_or(|d| d.dirty() || d.saving)
    {
        return;
    }
    let session = app.focused();
    let burrow = ServerId(untrack(|| app.focused_endpoint()));
    if !session.live.get_untracked() || session.is_guest.get_untracked() {
        return;
    }
    let ws = session.ws.get_value();
    generation.update(|g| *g += 1);
    let ticket = generation.get_untracked();
    draft.update(|d| {
        d.loading = true;
        d.error = None;
    });
    wasm_bindgen_futures::spawn_local(async move {
        let reply = ws
            .call(&rabbithole_proto::persona::PersonaListRequest)
            .await;
        let result = crate::wire::own_persona_reply(reply.as_ref());
        let avatar_src = if let Ok(persona) = &result {
            if let Some(id) = persona.avatar {
                ws.call(&rabbithole_proto::blob::BlobGet::new(id))
                    .await
                    .as_ref()
                    .and_then(crate::wire::frame_to_blob)
                    .filter(|bytes| *blake3::hash(bytes).as_bytes() == id)
                    .map(|bytes| crate::wire::blob_to_data_url(&bytes))
            } else {
                None
            }
        } else {
            None
        };
        if generation.try_get_untracked() != Some(ticket) || app.focused().state != session.state {
            return;
        }
        let _ = draft.try_update(|d| match result {
            Ok(persona) => {
                d.avatar_src = avatar_src;
                d.accept(persona, false);
                app.profile_drafts
                    .update_value(|cache| cache.restore(burrow, d));
            }
            Err(why) => d.fail(why),
        });
    });
}

#[cfg(target_arch = "wasm32")]
fn save(app: AppState, draft: RwSignal<ProfileDraft>, generation: RwSignal<u64>) {
    let Some(mut update) = draft.with_untracked(ProfileDraft::update) else {
        return;
    };
    let icon = draft.with_untracked(|d| d.icon);
    let session = app.focused();
    let Some(key) = draft.with_untracked(|d| d.key.clone()) else {
        return;
    };
    if key.burrow.0 != untrack(|| app.focused_endpoint())
        || draft.with_untracked(|d| {
            d.persona.as_ref().map(|p| p.screen_name.as_str())
                != Some(session.handle.get_untracked().as_str())
        })
    {
        return;
    }
    let revision = app.profile_drafts.with_value(|cache| cache.revision(&key));
    let signed_in = session.ready.get_untracked();
    let ws = session.ws.get_value();
    generation.update(|g| *g += 1);
    let ticket = generation.get_untracked();
    draft.update(|d| {
        d.saving = true;
        d.error = None;
        d.saved = false;
    });
    wasm_bindgen_futures::spawn_local(async move {
        let mut icon_src = None;
        let result = async {
            if let IconChoice::Mark(mark) = icon {
                let bytes = crate::avatar::glyph_png(mark.glyph, mark.color).map_err(|_| {
                    "Your icon could not be prepared. Try another icon.".to_string()
                })?;
                let request = rabbithole_proto::blob::BlobPut::new(
                    rabbithole_proto::blob::BlobPurpose::Avatar,
                    bytes.clone(),
                );
                let reply = ws.call(&request).await;
                update.avatar = Some(Some(crate::wire::avatar_blob_reply(
                    reply.as_ref(),
                    &bytes,
                )?));
                icon_src = Some(crate::wire::blob_to_data_url(&bytes));
            }
            // An upload may outlive a reconnect/account switch. Never send
            // the subsequent profile update through a different sign-in.
            if session.ready.try_get_untracked() != Some(signed_in) {
                return Err("Your sign-in changed before the profile was saved. Your edits are kept; try again.".to_string());
            }
            if app.profile_drafts.try_with_value(|cache| cache.revision(&key)) != Some(revision) {
                return Err("A newer draft replaced this save. Your latest edits are kept.".to_string());
            }
            let reply = ws.call(&update).await;
            crate::wire::updated_persona_reply(reply.as_ref(), update.id)
        }
        .await;
        if result.is_ok() {
            // The request still belongs to its original burrow after this
            // editor unmounts. Only its submitted revision may be removed.
            let _ = app
                .profile_drafts
                .try_update_value(|cache| cache.acknowledge(&key, revision));
        }
        if generation.try_get_untracked() != Some(ticket) || app.focused().state != session.state {
            return;
        }
        let _ = draft.try_update(|d| match result {
            Ok(persona) => {
                if icon != IconChoice::Keep {
                    d.avatar_src = icon_src;
                }
                d.accept(persona, true);
            }
            Err(why) => d.fail(why),
        });
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn load(_: AppState, _: RwSignal<ProfileDraft>, _: RwSignal<u64>) {}
#[cfg(not(target_arch = "wasm32"))]
fn save(_: AppState, _: RwSignal<ProfileDraft>, _: RwSignal<u64>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(cache: &mut DraftCache, endpoint: &str, persona: PersonaInfo) -> ProfileDraft {
        let mut draft = ProfileDraft::default();
        draft.accept(persona, false);
        cache.restore(ServerId(endpoint.into()), &mut draft);
        draft
    }

    #[test]
    fn route_return_restores_edits_and_icon_without_overwriting_fresh_server_fields() {
        let mut cache = DraftCache::default();
        let mut persona = PersonaInfo::new(7, "alice");
        persona.profile.quote = Some("Original".into());
        persona.profile.location = Some("Seattle".into());
        let mut first = loaded(&mut cache, "wss://one", persona.clone());
        first.fields.quote = "Unpublished thought".into();
        first.icon = IconChoice::Mark(ChosenMark { glyph: 3, color: 2 });
        first.saving = true;
        first.error = Some("Old connection".into());
        cache.remember(&first);
        drop(first);

        persona.profile.location = Some("Portland".into());
        let restored = loaded(&mut cache, "wss://one", persona);
        assert_eq!(restored.fields.quote, "Unpublished thought");
        assert_eq!(restored.fields.location, "Portland");
        assert_eq!(
            restored.icon,
            IconChoice::Mark(ChosenMark { glyph: 3, color: 2 })
        );
        assert!(restored.dirty());
        assert!(!restored.saving);
        assert!(!restored.loading);
        assert!(restored.error.is_none());
    }

    #[test]
    fn cached_edits_never_cross_burrows_personas_or_replaced_accounts() {
        let mut cache = DraftCache::default();
        let persona = PersonaInfo::new(7, "alice");
        let mut first = loaded(&mut cache, "wss://one", persona.clone());
        first.fields.plan = "Private unfinished draft".into();
        cache.remember(&first);
        assert!(!loaded(&mut cache, "wss://two", persona).dirty());
        assert!(!loaded(&mut cache, "wss://one", PersonaInfo::new(8, "alice")).dirty());
        assert!(!loaded(&mut cache, "wss://one", PersonaInfo::new(7, "bob")).dirty());
    }

    #[test]
    fn acknowledgements_after_unmount_clear_only_the_submitted_revision() {
        let mut cache = DraftCache::default();
        let persona = PersonaInfo::new(7, "alice");
        let mut first = loaded(&mut cache, "wss://one", persona.clone());
        first.fields.plan = "First edit".into();
        cache.remember(&first);
        let key = first.key.clone().unwrap();
        let submitted = cache.revision(&key);
        drop(first);

        let mut reopened = loaded(&mut cache, "wss://one", persona.clone());
        reopened.fields.plan = "Newer edit".into();
        cache.remember(&reopened);
        cache.acknowledge(&key, submitted);
        assert_eq!(
            loaded(&mut cache, "wss://one", persona.clone()).fields.plan,
            "Newer edit"
        );

        let latest = cache.revision(&key);
        cache.acknowledge(&key, latest);
        assert!(!loaded(&mut cache, "wss://one", persona).dirty());
    }

    #[test]
    fn explicit_revert_removes_the_staged_draft_before_the_next_mount() {
        let mut cache = DraftCache::default();
        let persona = PersonaInfo::new(7, "alice");
        let mut first = loaded(&mut cache, "wss://one", persona.clone());
        first.fields.quote = "Discard this".into();
        first.icon = IconChoice::Remove;
        cache.remember(&first);
        cache.discard(first.key.as_ref().unwrap());
        first.revert();
        assert!(!first.dirty());
        assert!(!loaded(&mut cache, "wss://one", persona).dirty());
    }

    #[test]
    fn icon_removal_is_explicit_and_revert_keeps_the_published_avatar() {
        let mut persona = PersonaInfo::new(7, "alice");
        persona.avatar = Some([1; 32]);
        let mut draft = ProfileDraft::default();
        draft.accept(persona, false);
        draft.icon = IconChoice::Remove;
        assert!(draft.dirty());
        assert_eq!(draft.update().unwrap().avatar, Some(None));
        draft.revert();
        assert!(!draft.dirty());
        assert_eq!(draft.update().unwrap().avatar, None);
        assert_eq!(draft.persona.unwrap().avatar, Some([1; 32]));
    }

    #[test]
    fn clearing_text_is_explicit_and_leaves_images_untouched() {
        let mut persona = PersonaInfo::new(7, "alice");
        persona.profile.quote = Some("Old quote".into());
        persona.avatar = Some([1; 32]);
        let mut draft = ProfileDraft::default();
        draft.accept(persona, false);
        draft.fields.quote.clear();
        draft.fields.directory_visible = false;
        assert!(draft.dirty());
        let update = draft.update().unwrap();
        assert_eq!(update.id, 7);
        assert_eq!(update.profile.unwrap().quote.as_deref(), Some(""));
        assert_eq!(update.directory_visible, Some(false));
        assert_eq!(update.avatar, None);
        assert_eq!(update.banner, None);
    }

    #[test]
    fn refusal_keeps_draft_and_revert_restores_the_last_confirmed_profile() {
        let mut persona = PersonaInfo::new(7, "alice");
        persona.profile.location = Some("Seattle".into());
        let mut draft = ProfileDraft::default();
        draft.accept(persona, false);
        draft.fields.location = "Portland".into();
        draft.saving = true;
        draft.fail("Disconnected".into());
        assert_eq!(draft.fields.location, "Portland");
        assert!(draft.dirty());
        assert!(!draft.saving);
        assert!(!draft.saved);
        draft.revert();
        assert_eq!(draft.fields.location, "Seattle");
        assert!(!draft.dirty());
        assert!(draft.error.is_none());
    }

    #[test]
    fn successful_save_takes_the_servers_acknowledged_values() {
        let mut draft = ProfileDraft::default();
        assert!(draft.update().is_none());
        let mut persona = PersonaInfo::new(7, "alice");
        persona.profile.plan = Some("A new project".into());
        draft.accept(persona, true);
        assert!(draft.saved);
        assert!(!draft.dirty());
        assert_eq!(draft.fields.plan, "A new project");
    }
}
