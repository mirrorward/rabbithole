//! The burrow's settings as the admin console holds them: what the burrow
//! described, what the operator has changed and not yet saved, and what came
//! of the last save. DOM-free and host-tested, like [`crate::admin`].
//!
//! Three rules shape it.
//!
//! - **Edits are staged.** Nothing reaches the burrow until Save, so there is
//!   one Save for the console, not one per row, and a half-typed address is
//!   never applied. A draft that is typed back to the saved value stops being
//!   a draft.
//! - **Reset is not Undo.** Reset stages the burrow's *default*; Discard drops
//!   the drafts and returns to what is saved. A row offers Reset whenever what
//!   it shows differs from the default, saved or not.
//! - **Each row answers for itself.** A save is one `ConfigSet` per changed
//!   key, and the burrow may take some and refuse others. The row that was
//!   refused keeps its draft and says why.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rabbithole_proto::admin::{
    config_flag, config_kind, surface_state, ConfigKeyInfo, SurfaceInfo,
};

/// The pending-request marker for [`crate::wire::AdminCommand::DescribeConfig`]:
/// the transport pairs every admin reply with the key it was about, and this
/// stands in for "the description". Not a legal key (`*` is in none).
pub const DESCRIBE: &str = "*describe";

/// The same, for [`crate::wire::AdminCommand::GetSurfaceStatus`].
pub const SURFACES: &str = "*surfaces";

/// The shape of a setting, which decides its control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A text field.
    Text,
    /// A switch.
    Bool,
    /// A number field.
    Number,
    /// A dropdown of [`Setting::choices`].
    Choice,
}

/// One setting, as the burrow described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// The config key.
    pub key: String,
    /// The saved value (always empty for a secret).
    pub value: String,
    /// The burrow's default. `None` when an older burrow could not say.
    pub default: Option<String>,
    /// The control it gets.
    pub kind: Kind,
    /// What a [`Kind::Choice`] may be.
    pub choices: Vec<String>,
    /// A change applies at once. False: saved now, applied at the next restart.
    pub live: bool,
    /// A credential: never read back.
    pub secret: bool,
    /// For a secret: one is stored.
    pub is_set: bool,
    /// Shown for reference only.
    pub read_only: bool,
}

impl Setting {
    fn from_wire(info: &ConfigKeyInfo) -> Self {
        let kind = match info.kind {
            config_kind::BOOL => Kind::Bool,
            config_kind::NUMBER => Kind::Number,
            config_kind::CHOICE if !info.choices.is_empty() => Kind::Choice,
            _ => Kind::Text,
        };
        Self {
            key: info.key.clone(),
            value: info.value.clone(),
            default: Some(info.default.clone()),
            kind,
            choices: info.choices.clone(),
            live: info.has(config_flag::LIVE),
            secret: info.has(config_flag::SECRET),
            is_set: info.has(config_flag::SET),
            read_only: info.has(config_flag::READ_ONLY),
        }
    }

    /// A setting an older burrow could only hand over as a bare value: the
    /// shape is guessed from the value, and there is no default to reset to.
    fn from_bare_value(key: &str, value: &str) -> Self {
        Self {
            key: key.to_string(),
            value: value.to_string(),
            default: None,
            kind: if matches!(value, "true" | "false") {
                Kind::Bool
            } else {
                Kind::Text
            },
            choices: Vec::new(),
            // Unknown. Claim nothing in advance: the reply to a save says
            // whether it took effect, and the row reports that.
            live: true,
            secret: key.ends_with("_password"),
            is_set: false,
            read_only: false,
        }
    }
}

/// What came of saving one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Saved. `live`: it applies now; otherwise at the next restart.
    Saved {
        /// Whether the change is already in effect.
        live: bool,
    },
    /// The burrow refused it, and nothing changed. The sentence says why.
    Refused(String),
}

/// How a surface's report should read beside its switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// It is doing what was asked.
    Good,
    /// It is not, and the line says why.
    Bad,
    /// On, with nothing to do.
    Waiting,
}

/// What a surface is actually doing, in a sentence. `None` when it is simply
/// off: a switch that is off needs no second opinion.
///
/// The setting says what was asked for; this says what happened. They differ
/// exactly when it matters (the port was taken), which is why the console
/// shows the fact and not an echo of the switch.
pub fn surface_line(info: &SurfaceInfo) -> Option<(Tone, String)> {
    match info.state {
        surface_state::LISTENING => Some((Tone::Good, format!("Listening on {}.", info.addr))),
        surface_state::RUNNING => Some((Tone::Good, "Running.".to_string())),
        surface_state::FAILED => {
            let why = if info.detail.to_ascii_lowercase().contains("in use") {
                "something else is already using that address".to_string()
            } else if info
                .detail
                .to_ascii_lowercase()
                .contains("permission denied")
            {
                "the burrow is not allowed to use that port (ports under 1024 need privileges)"
                    .to_string()
            } else {
                info.detail.trim_end_matches('.').to_string()
            };
            Some((Tone::Bad, format!("Not running: {why}.")))
        }
        surface_state::IDLE => Some((
            Tone::Waiting,
            format!(
                "On, with nothing to do: {}.",
                info.detail.trim_end_matches('.')
            ),
        )),
        _ => None,
    }
}

/// How the console came by its settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Load {
    /// Not asked yet.
    #[default]
    Idle,
    /// Asked; no answer yet.
    Loading,
    /// The burrow described itself.
    Described,
    /// An older burrow that cannot describe itself: the well-known keys were
    /// read one at a time. No defaults, so no Reset.
    Legacy,
}

/// The settings model. `Default` is "nothing loaded".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsState {
    /// How the settings were loaded.
    pub load: Load,
    /// Every setting, in the burrow's order.
    pub settings: Vec<Setting>,
    drafts: BTreeMap<String, String>,
    outcomes: HashMap<String, Outcome>,
    in_flight: BTreeSet<String>,
    /// What each surface is doing, by the key of its switch.
    surfaces: HashMap<String, SurfaceInfo>,
}

impl SettingsState {
    /// The console asked the burrow to describe itself.
    pub fn loading(&mut self) {
        if self.load == Load::Idle {
            self.load = Load::Loading;
        }
    }

    /// The burrow's description arrived. Drafts survive a re-description
    /// (it also follows every save), except where the saved value caught up.
    pub fn described(&mut self, entries: &[ConfigKeyInfo]) {
        self.settings = entries.iter().map(Setting::from_wire).collect();
        self.load = Load::Described;
        self.drop_settled_drafts();
    }

    /// The burrow reported what its surfaces are doing.
    pub fn surfaces_reported(&mut self, surfaces: &[SurfaceInfo]) {
        self.surfaces = surfaces
            .iter()
            .map(|s| (s.key.clone(), s.clone()))
            .collect();
    }

    /// What the surface behind the switch `key` is doing, if it is a surface
    /// and has something to say.
    pub fn surface(&self, key: &str) -> Option<(Tone, String)> {
        self.surfaces.get(key).and_then(surface_line)
    }

    /// An older burrow refused to describe itself.
    pub fn describe_refused(&mut self) {
        self.load = Load::Legacy;
    }

    /// One key read the old way (`ConfigGet`), for a [`Load::Legacy`] burrow.
    pub fn bare_value(&mut self, key: &str, value: &str) {
        if self.load == Load::Described {
            return;
        }
        match self.settings.iter_mut().find(|s| s.key == key) {
            Some(s) => s.value = value.to_string(),
            None => self.settings.push(Setting::from_bare_value(key, value)),
        }
        self.drop_settled_drafts();
    }

    fn drop_settled_drafts(&mut self) {
        let settings = &self.settings;
        self.drafts.retain(|key, draft| {
            settings
                .iter()
                .find(|s| &s.key == key)
                .is_some_and(|s| !s.read_only && (s.secret || s.value != *draft))
        });
    }

    /// The setting for `key`, if the burrow has one.
    pub fn get(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// What the row shows: the draft if there is one, else the saved value.
    pub fn shown(&self, key: &str) -> String {
        self.drafts
            .get(key)
            .cloned()
            .or_else(|| self.get(key).map(|s| s.value.clone()))
            .unwrap_or_default()
    }

    /// Stage `value` for `key`. Typing the saved value back is not a change.
    pub fn stage(&mut self, key: &str, value: &str) {
        let Some(setting) = self.get(key) else {
            return;
        };
        if setting.read_only {
            return;
        }
        // A secret's saved value is never known, so "" cannot mean "unchanged"
        // once one is stored: there it means "clear it".
        let unchanged = if setting.secret {
            value.is_empty() && !setting.is_set
        } else {
            setting.value == value
        };
        self.outcomes.remove(key);
        if unchanged {
            self.drafts.remove(key);
        } else {
            self.drafts.insert(key.to_string(), value.to_string());
        }
    }

    /// Flip a switch.
    pub fn toggle(&mut self, key: &str) {
        let next = if self.shown(key) == "true" {
            "false"
        } else {
            "true"
        };
        self.stage(key, next);
    }

    /// Whether the row offers Reset: it shows something other than the
    /// burrow's default, and the default is known.
    pub fn can_reset(&self, key: &str) -> bool {
        let Some(setting) = self.get(key) else {
            return false;
        };
        let Some(default) = &setting.default else {
            return false;
        };
        if setting.read_only {
            return false;
        }
        if setting.secret {
            // The default of every credential is "none".
            return match self.drafts.get(key) {
                Some(draft) => !draft.is_empty(),
                None => setting.is_set,
            };
        }
        self.shown(key) != *default
    }

    /// Stage the burrow's default for `key`.
    pub fn reset(&mut self, key: &str) {
        if let Some(default) = self.get(key).and_then(|s| s.default.clone()) {
            self.stage(key, &default);
        }
    }

    /// Whether `key` has an unsaved change.
    pub fn is_dirty(&self, key: &str) -> bool {
        self.drafts.contains_key(key)
    }

    /// Why the draft for `key` cannot be saved as typed, if it cannot. Checked
    /// here so Save is never a round trip to be told a number needs digits.
    pub fn invalid(&self, key: &str) -> Option<&'static str> {
        let draft = self.drafts.get(key)?;
        let setting = self.get(key)?;
        match setting.kind {
            Kind::Number if draft.trim().parse::<i128>().is_err() => Some("Enter a whole number."),
            Kind::Choice if !setting.choices.iter().any(|c| c == draft) => {
                Some("Choose one of the listed values.")
            }
            _ => None,
        }
    }

    /// How many settings have unsaved changes.
    pub fn dirty_count(&self) -> usize {
        self.drafts.len()
    }

    /// How many of the unsaved changes only take effect after a restart.
    pub fn restart_count(&self) -> usize {
        self.drafts
            .keys()
            .filter(|k| self.get(k).is_some_and(|s| !s.live))
            .count()
    }

    /// Whether every draft could be saved as typed.
    pub fn can_save(&self) -> bool {
        !self.drafts.is_empty()
            && !self.saving()
            && self.drafts.keys().all(|k| self.invalid(k).is_none())
    }

    /// Drop every unsaved change.
    pub fn discard(&mut self) {
        self.drafts.clear();
        self.outcomes.clear();
    }

    /// Start a save: the `(key, value)` pairs to send, in a stable order.
    /// Empty when there is nothing to save or a draft is invalid.
    pub fn begin_save(&mut self) -> Vec<(String, String)> {
        if !self.can_save() {
            return Vec::new();
        }
        self.outcomes.clear();
        let batch: Vec<(String, String)> = self
            .drafts
            .iter()
            .map(|(k, v)| (k.clone(), v.trim_end_matches(['\r', '\n']).to_string()))
            .collect();
        self.in_flight = batch.iter().map(|(k, _)| k.clone()).collect();
        batch
    }

    /// Whether a save is waiting on the burrow.
    pub fn saving(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// The burrow took the change to `key`.
    pub fn saved(&mut self, key: &str, live: bool) {
        if !self.in_flight.remove(key) {
            return;
        }
        if let Some(draft) = self.drafts.remove(key) {
            if let Some(s) = self.settings.iter_mut().find(|s| s.key == key) {
                if s.secret {
                    s.is_set = !draft.is_empty();
                } else {
                    s.value = draft;
                }
            }
        }
        self.outcomes
            .insert(key.to_string(), Outcome::Saved { live });
    }

    /// The burrow refused the change to `key`. The draft stays, so the
    /// operator can fix it instead of retyping it.
    pub fn refused(&mut self, key: &str, detail: &str) {
        if !self.in_flight.remove(key) {
            return;
        }
        self.outcomes
            .insert(key.to_string(), Outcome::Refused(why_refused(detail)));
    }

    /// What came of the last save of `key`.
    pub fn outcome(&self, key: &str) -> Option<&Outcome> {
        self.outcomes.get(key)
    }

    /// One sentence about the save that just finished, for a toast. `None`
    /// while replies are still due or when nothing was saved.
    pub fn save_summary(&self) -> Option<(bool, String)> {
        if self.saving() || self.outcomes.is_empty() {
            return None;
        }
        let saved = self
            .outcomes
            .values()
            .filter(|o| matches!(o, Outcome::Saved { .. }))
            .count();
        let restart = self
            .outcomes
            .values()
            .filter(|o| matches!(o, Outcome::Saved { live: false }))
            .count();
        let refused = self.outcomes.len() - saved;
        let mut line = match saved {
            0 => String::new(),
            1 => "Saved 1 setting.".to_string(),
            n => format!("Saved {n} settings."),
        };
        if restart > 0 {
            line.push_str(match restart {
                1 if saved == 1 => " It takes effect when the burrow restarts.",
                1 => " 1 takes effect when the burrow restarts.",
                _ => " Some take effect when the burrow restarts.",
            });
        }
        if refused > 0 {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&match refused {
                1 => "1 setting was refused; it says why.".to_string(),
                n => format!("{n} settings were refused; each says why."),
            });
        }
        Some((refused == 0, line))
    }
}

/// A refusal, in the operator's words. `detail` is what the wire layer made of
/// the error frame (`server error: BadRequest`).
fn why_refused(detail: &str) -> String {
    if detail.contains("BadRequest") {
        "The burrow refused that value.".to_string()
    } else if detail.contains("Forbidden") {
        "You are not allowed to change settings on this burrow.".to_string()
    } else if detail.contains("Internal") {
        "The burrow could not write its config file, so nothing changed.".to_string()
    } else if detail.contains("NotFound") {
        "This burrow has no such setting.".to_string()
    } else {
        format!("The burrow did not take it: {detail}")
    }
}

/// A byte count the way a person says it (`1.5 GiB`), for the echo beside a
/// size field. Binary units, because that is what the burrow counts in.
pub fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return if bytes == 1 {
            "1 byte".to_string()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A span of seconds the way a person says it (`2 hours`, `1 day 6 hours`).
pub fn humanize_secs(secs: u64) -> String {
    const STEPS: [(u64, &str); 4] = [
        (86_400, "day"),
        (3_600, "hour"),
        (60, "minute"),
        (1, "second"),
    ];
    if secs == 0 {
        return "0 seconds".to_string();
    }
    let mut parts = Vec::new();
    let mut rest = secs;
    for (size, name) in STEPS {
        let n = rest / size;
        rest %= size;
        if n > 0 {
            parts.push(format!("{n} {name}{}", if n == 1 { "" } else { "s" }));
        }
        if parts.len() == 2 {
            break;
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burrow() -> SettingsState {
        let mut s = SettingsState::default();
        s.loading();
        s.described(&[
            ConfigKeyInfo::new("name", "Kevin\u{2019}s Burrow", "An Unnamed Burrow")
                .flags(config_flag::LIVE),
            ConfigKeyInfo::new("guest_enabled", "true", "true")
                .kind(config_kind::BOOL)
                .flags(config_flag::LIVE),
            ConfigKeyInfo::new("nntp_enabled", "false", "false").kind(config_kind::BOOL),
            ConfigKeyInfo::new("chat_max_len", "4096", "4096")
                .kind(config_kind::NUMBER)
                .flags(config_flag::LIVE),
            ConfigKeyInfo::new("registration_mode", "open", "open")
                .kind(config_kind::CHOICE)
                .flags(config_flag::LIVE)
                .choices(["open", "invite", "closed"]),
            ConfigKeyInfo::new("radio_source_password", "", "")
                .flags(config_flag::SECRET | config_flag::SET),
            ConfigKeyInfo::new("data_dir", "./burrow-data", "./data").flags(config_flag::READ_ONLY),
        ]);
        s
    }

    #[test]
    fn a_description_decides_each_control() {
        let s = burrow();
        assert_eq!(s.load, Load::Described);
        assert_eq!(s.get("guest_enabled").unwrap().kind, Kind::Bool);
        assert_eq!(s.get("chat_max_len").unwrap().kind, Kind::Number);
        assert_eq!(s.get("registration_mode").unwrap().kind, Kind::Choice);
        assert_eq!(s.get("name").unwrap().kind, Kind::Text);
        assert!(s.get("name").unwrap().live);
        assert!(!s.get("nntp_enabled").unwrap().live);
        assert!(s.get("data_dir").unwrap().read_only);
        // A choice with nothing to choose from is just text.
        let mut odd = SettingsState::default();
        odd.described(&[ConfigKeyInfo::new("x", "a", "a").kind(config_kind::CHOICE)]);
        assert_eq!(odd.get("x").unwrap().kind, Kind::Text);
    }

    #[test]
    fn edits_are_staged_and_typing_the_saved_value_back_is_not_a_change() {
        let mut s = burrow();
        assert_eq!(s.dirty_count(), 0);
        s.stage("chat_max_len", "2048");
        s.toggle("nntp_enabled");
        assert_eq!(s.shown("chat_max_len"), "2048");
        assert_eq!(s.shown("nntp_enabled"), "true");
        assert_eq!(
            s.get("chat_max_len").unwrap().value,
            "4096",
            "not saved yet"
        );
        assert_eq!(s.dirty_count(), 2);
        assert_eq!(s.restart_count(), 1, "nntp_enabled needs a restart");

        s.stage("chat_max_len", "4096");
        s.toggle("nntp_enabled");
        assert_eq!(s.dirty_count(), 0);
        // A setting shown for reference cannot be staged at all.
        s.stage("data_dir", "/tmp/elsewhere");
        assert!(!s.is_dirty("data_dir"));
    }

    #[test]
    fn reset_offers_the_default_and_discard_returns_to_what_is_saved() {
        let mut s = burrow();
        assert!(s.can_reset("name"), "saved, and not the default");
        assert!(!s.can_reset("chat_max_len"), "already the default");
        assert!(!s.can_reset("data_dir"));

        s.reset("name");
        assert_eq!(s.shown("name"), "An Unnamed Burrow");
        assert!(s.is_dirty("name"));
        assert!(!s.can_reset("name"), "it now shows the default");

        s.discard();
        assert_eq!(s.shown("name"), "Kevin\u{2019}s Burrow");
        assert!(s.can_reset("name"));

        // A draft away from the default offers Reset before it is ever saved.
        s.stage("chat_max_len", "100");
        assert!(s.can_reset("chat_max_len"));
    }

    #[test]
    fn a_draft_that_cannot_be_saved_blocks_the_save_and_says_why() {
        let mut s = burrow();
        s.stage("chat_max_len", "lots");
        assert_eq!(s.invalid("chat_max_len"), Some("Enter a whole number."));
        assert!(!s.can_save());
        assert!(s.begin_save().is_empty());
        s.stage("chat_max_len", "8192");
        assert!(s.invalid("chat_max_len").is_none());
        s.stage("registration_mode", "whenever");
        assert!(s.invalid("registration_mode").is_some());
        s.stage("registration_mode", "invite");
        assert!(s.can_save());
    }

    #[test]
    fn a_save_is_answered_row_by_row() {
        let mut s = burrow();
        s.stage("name", "Wonderland");
        s.stage("nntp_enabled", "true");
        s.stage("registration_mode", "invite");
        let batch = s.begin_save();
        assert_eq!(batch.len(), 3);
        assert!(s.saving());
        assert!(!s.can_save(), "no second save while one is out");
        assert!(s.save_summary().is_none());

        s.saved("name", true);
        s.saved("nntp_enabled", false);
        s.refused("registration_mode", "server error: BadRequest");
        assert!(!s.saving());

        assert_eq!(s.get("name").unwrap().value, "Wonderland");
        assert_eq!(
            s.outcome("nntp_enabled"),
            Some(&Outcome::Saved { live: false })
        );
        assert_eq!(
            s.outcome("registration_mode"),
            Some(&Outcome::Refused("The burrow refused that value.".into()))
        );
        // The refused row keeps what was typed.
        assert_eq!(s.shown("registration_mode"), "invite");
        assert_eq!(s.dirty_count(), 1);

        let (clean, line) = s.save_summary().unwrap();
        assert!(!clean);
        assert_eq!(
            line,
            "Saved 2 settings. 1 takes effect when the burrow restarts. \
             1 setting was refused; it says why."
        );
        // A reply nobody is waiting for changes nothing.
        s.saved("motd", true);
        assert!(s.outcome("motd").is_none());
    }

    #[test]
    fn the_summary_reads_right_for_one_setting() {
        let mut s = burrow();
        s.stage("nntp_enabled", "true");
        s.begin_save();
        s.saved("nntp_enabled", false);
        assert_eq!(
            s.save_summary().unwrap(),
            (
                true,
                "Saved 1 setting. It takes effect when the burrow restarts.".to_string()
            )
        );
        s.stage("name", "X");
        s.begin_save();
        s.saved("name", true);
        assert_eq!(s.save_summary().unwrap().1, "Saved 1 setting.");
    }

    #[test]
    fn a_credential_is_typed_never_shown_and_cleared_by_reset() {
        let mut s = burrow();
        assert_eq!(s.shown("radio_source_password"), "");
        assert!(s.can_reset("radio_source_password"), "one is stored");

        s.stage("radio_source_password", "hunter2");
        assert!(s.is_dirty("radio_source_password"));
        s.stage("radio_source_password", "");
        assert!(
            s.is_dirty("radio_source_password"),
            "empty means clear it, because one is stored"
        );
        s.begin_save();
        s.saved("radio_source_password", false);
        let pw = s.get("radio_source_password").unwrap();
        assert!(!pw.is_set);
        assert!(pw.value.is_empty());
        assert!(!s.can_reset("radio_source_password"));
        // With none stored, an empty field is not a change.
        s.stage("radio_source_password", "");
        assert!(!s.is_dirty("radio_source_password"));
    }

    #[test]
    fn a_new_description_keeps_drafts_that_still_differ() {
        let mut s = burrow();
        s.stage("name", "Wonderland");
        s.stage("chat_max_len", "2048");
        // Someone else saved chat_max_len = 2048 meanwhile.
        let mut entries: Vec<ConfigKeyInfo> = vec![
            ConfigKeyInfo::new("name", "Kevin\u{2019}s Burrow", "An Unnamed Burrow"),
            ConfigKeyInfo::new("chat_max_len", "2048", "4096").kind(config_kind::NUMBER),
        ];
        s.described(&entries);
        assert!(s.is_dirty("name"));
        assert!(!s.is_dirty("chat_max_len"), "the saved value caught up");
        // A key the burrow no longer has takes its draft with it.
        entries.remove(0);
        s.described(&entries);
        assert_eq!(s.dirty_count(), 0);
    }

    #[test]
    fn an_older_burrow_is_read_key_by_key_without_reset() {
        let mut s = SettingsState::default();
        s.loading();
        s.describe_refused();
        s.bare_value("guest_enabled", "true");
        s.bare_value("name", "Old Warren");
        s.bare_value("name", "Older Warren");
        assert_eq!(s.load, Load::Legacy);
        assert_eq!(s.settings.len(), 2);
        assert_eq!(s.get("guest_enabled").unwrap().kind, Kind::Bool);
        assert_eq!(s.shown("name"), "Older Warren");
        s.stage("name", "Something Else");
        assert!(!s.can_reset("name"), "no default is known");
        assert_eq!(
            s.restart_count(),
            0,
            "liveness unknown, so no restart is claimed"
        );
        // Once described, bare reads are ignored.
        let mut d = burrow();
        d.bare_value("name", "stale");
        assert_eq!(d.shown("name"), "Kevin\u{2019}s Burrow");
    }

    #[test]
    fn a_switch_is_told_what_actually_happened() {
        let mut s = burrow();
        assert!(s.surface("nntp_enabled").is_none(), "nothing reported yet");
        s.surfaces_reported(&[
            SurfaceInfo::new("nntp_enabled", surface_state::LISTENING).addr("0.0.0.0:1119"),
            SurfaceInfo::new("telnet_enabled", surface_state::FAILED)
                .detail("Address already in use (os error 48)"),
            SurfaceInfo::new("finger_enabled", surface_state::FAILED)
                .detail("Permission denied (os error 13)"),
            SurfaceInfo::new("hotline_enabled", surface_state::FAILED).detail("no route."),
            SurfaceInfo::new("syndication_enabled", surface_state::IDLE)
                .detail("no feeds are mapped in burrow.toml"),
            SurfaceInfo::new("ftn_enabled", surface_state::OFF),
        ]);
        assert_eq!(
            s.surface("nntp_enabled"),
            Some((Tone::Good, "Listening on 0.0.0.0:1119.".into()))
        );
        assert_eq!(
            s.surface("telnet_enabled"),
            Some((
                Tone::Bad,
                "Not running: something else is already using that address.".into()
            ))
        );
        assert!(s
            .surface("finger_enabled")
            .unwrap()
            .1
            .contains("under 1024"));
        assert_eq!(
            s.surface("hotline_enabled").unwrap().1,
            "Not running: no route."
        );
        assert_eq!(s.surface("syndication_enabled").unwrap().0, Tone::Waiting);
        assert!(
            s.surface("ftn_enabled").is_none(),
            "off needs no second opinion"
        );
        assert!(s.surface("guest_enabled").is_none(), "not a surface");
    }

    #[test]
    fn sizes_and_spans_read_the_way_people_say_them() {
        assert_eq!(humanize_bytes(0), "0 bytes");
        assert_eq!(humanize_bytes(1), "1 byte");
        assert_eq!(humanize_bytes(1023), "1023 bytes");
        assert_eq!(humanize_bytes(1024), "1 KiB");
        assert_eq!(humanize_bytes(1_572_864), "1.5 MiB");
        assert_eq!(humanize_bytes(1_073_741_824), "1 GiB");
        assert_eq!(humanize_bytes(u64::MAX), "16777216 TiB");

        assert_eq!(humanize_secs(0), "0 seconds");
        assert_eq!(humanize_secs(1), "1 second");
        assert_eq!(humanize_secs(90), "1 minute 30 seconds");
        assert_eq!(humanize_secs(7_200), "2 hours");
        assert_eq!(humanize_secs(108_000), "1 day 6 hours");
        assert_eq!(humanize_secs(2_592_000), "30 days");
    }
}
