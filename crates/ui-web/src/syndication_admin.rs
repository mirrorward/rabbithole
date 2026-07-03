//! Pure, DOM-free state for the admin **Syndication & Gateways** panel.
//!
//! Like [`crate::admin`] and [`crate::theme_editor`], this module holds no
//! Leptos or `web_sys` types: the whole panel model — key loading, the poll
//! interval editor with validation, the gateway matrix derivation, and the
//! defensive `syndication_feeds` parser — is unit-tested on the host with
//! `cargo test`. The panel component in [`crate::components`] owns a reactive
//! `RwSignal<SynAdminState>` and folds replies into it.
//!
//! ## What travels over the wire (and what honestly cannot)
//!
//! Everything here rides the **existing** ADMIN-family config vocabulary
//! ([`AdminCommand::GetConfig`] / [`AdminCommand::SetConfig`] and their
//! [`AdminEvent`] replies) — no new wire messages are invented. The server's
//! `ctl config` surface exposes scalar keys only:
//!
//! - `syndication_enabled`, `syndication_poll_secs`, the `nntp_*`, `ftn_*`
//!   and `qwk_*` knobs: gettable and settable.
//! - `syndication_feeds` (the feed URL → board-slug map) is **TOML-only** —
//!   the server's config get/set has no arm for it, so a `ConfigGet` answers
//!   `NotFound`. The panel still *asks* (a future server slice may expose a
//!   read-only serialization) and folds the outcome totally: a value parses
//!   into read-only [`FeedRow`]s via [`parse_feeds_value`]; a failure lands as
//!   [`FeedsStatus::Unavailable`] and the UI shows the honest
//!   "edit `burrow.toml` + restart" hint. In both cases feeds are never
//!   editable from this panel.
//!
//! ## Live vs. restart-required
//!
//! The wire only reveals whether a key applied live *after* a set (the
//! [`AdminEvent::ConfigApplied`] reply's `applied_live` flag — exactly how
//! [`crate::admin`] surfaces it today). For badges shown *before* any set,
//! [`expected_applies_live`] mirrors the documented server semantics
//! (listener toggles/addresses bind at startup; QWK re-reads config per
//! command). The authoritative per-key answer from a real `ConfigApplied`
//! reply is recorded in [`SynAdminState::learned_live`] and always wins over
//! the expectation.
//!
//! ## Feed monitor
//!
//! No live feed-stats wire message exists yet, so the monitor renders
//! per-feed **configured** state only (mapping, the global enabled flag and
//! poll interval). Live stats (last poll, conditional-GET 304s, dedupe hits)
//! are a clearly-labeled seam for a future server slice.

use std::collections::BTreeMap;

use crate::admin::ConfigEntry;
use crate::wire::{AdminCommand, AdminEvent};

/// Config key: master switch for the feed-poll task (restart-required — the
/// poll task starts at boot).
pub const KEY_ENABLED: &str = "syndication_enabled";
/// Config key: base seconds between feed polls (restart-required).
pub const KEY_POLL_SECS: &str = "syndication_poll_secs";
/// Config key: the feed URL → board-slug map. TOML-only on the server (no
/// `ctl config` arm); see the module docs.
pub const KEY_FEEDS: &str = "syndication_feeds";

/// Smallest accepted poll interval, in seconds (the editor's validity bound;
/// the server additionally enforces a politeness floor at runtime).
pub const POLL_MIN_SECS: i64 = 1;
/// Largest accepted poll interval, in seconds (one week).
pub const POLL_MAX_SECS: i64 = 604_800;
/// The server's runtime politeness floor: polls are never scheduled sooner
/// than this, whatever the configured base (mirrors the syndication service's
/// `PollConfig::default`).
pub const POLL_FLOOR_SECS: i64 = 300;
/// The server's runtime backoff ceiling: polls are never scheduled further
/// out than this (mirrors `PollConfig::default`).
pub const POLL_CEILING_SECS: i64 = 86_400;

/// Every config key the panel loads on entry, `syndication_feeds` included
/// (see the module docs for why asking is still the right move).
pub const LOAD_KEYS: &[&str] = &[
    KEY_ENABLED,
    KEY_POLL_SECS,
    KEY_FEEDS,
    "nntp_enabled",
    "nntp_addr",
    "nntp_tls_enabled",
    "nntp_tls_addr",
    "nntp_feed_enabled",
    "nntp_feed_addr",
    "nntp_feed_tls_enabled",
    "nntp_feed_tls_addr",
    "ftn_enabled",
    "ftn_addr",
    "qwk_enabled",
];

/// The gateway families the matrix summarises: display label, the boolean
/// enabled key, and the listener address key (`None` for non-listener
/// surfaces like QWK and the syndication poller).
const FAMILIES: &[(&str, &str, Option<&str>)] = &[
    ("NNTP reader", "nntp_enabled", Some("nntp_addr")),
    ("NNTP reader TLS", "nntp_tls_enabled", Some("nntp_tls_addr")),
    (
        "NNTP peer feed",
        "nntp_feed_enabled",
        Some("nntp_feed_addr"),
    ),
    (
        "NNTP peer feed TLS",
        "nntp_feed_tls_enabled",
        Some("nntp_feed_tls_addr"),
    ),
    ("FTN binkp", "ftn_enabled", Some("ftn_addr")),
    ("QWK offline mail", "qwk_enabled", None),
    ("Syndication", KEY_ENABLED, None),
];

/// One configured feed: URL → destination board slug. Read-only in the panel
/// (the map itself is TOML-only server-side).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FeedRow {
    /// The feed URL (the map key in `burrow.toml`).
    pub url: String,
    /// The board slug fresh items are posted to.
    pub board: String,
}

/// What the panel knows about `syndication_feeds`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FeedsStatus {
    /// No reply folded yet.
    #[default]
    NotLoaded,
    /// The server refused the key — the real server today: feeds are
    /// TOML-only, edited in `burrow.toml` and applied by restart.
    Unavailable,
    /// A value arrived and parsed (possibly to zero rows). Still read-only.
    Listed(Vec<FeedRow>),
}

/// One row of the gateway matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRow {
    /// Display label for the network family.
    pub family: &'static str,
    /// The boolean config key the toggle drives.
    pub toggle_key: &'static str,
    /// Loaded enabled state (`None` until the key loads / if unparsable).
    pub enabled: Option<bool>,
    /// Listener port parsed from the family's `*_addr` value, if any.
    pub port: Option<u16>,
    /// Whether a set of `toggle_key` applies live (`false` = restart
    /// required). Learned from `ConfigApplied` replies when available, else
    /// the documented expectation.
    pub applies_live: bool,
}

/// The Syndication & Gateways panel model. `Default` is the empty, unloaded
/// state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SynAdminState {
    /// Resolved config key/value pairs, upserted from `ConfigLoaded` replies.
    pub config: Vec<ConfigEntry>,
    /// What we know about the feed map.
    pub feeds: FeedsStatus,
    /// The poll-interval editor's draft text.
    pub poll_draft: String,
    /// Inline validation (or server rejection) error for the poll editor.
    pub poll_error: Option<String>,
    /// Authoritative live-vs-restart answers learned from `ConfigApplied`
    /// replies, keyed by config key. Wins over [`expected_applies_live`].
    pub learned_live: BTreeMap<String, bool>,
    /// One-line status for the panel.
    pub status: String,
}

impl SynAdminState {
    /// The `GetConfig` commands that load the panel, one per key in
    /// [`LOAD_KEYS`]. The caller pairs each command's replies back through
    /// [`apply_get_reply`](Self::apply_get_reply) with the same key.
    pub fn load_commands() -> Vec<AdminCommand> {
        LOAD_KEYS
            .iter()
            .map(|key| AdminCommand::GetConfig {
                key: (*key).to_string(),
            })
            .collect()
    }

    /// Fold the reply events of a `GetConfig` for `key`. Total: unknown or
    /// out-of-family events are ignored; a failure for [`KEY_FEEDS`] is the
    /// *expected* real-server outcome and marks the map
    /// [`FeedsStatus::Unavailable`] rather than raising an error.
    pub fn apply_get_reply(&mut self, key: &str, events: &[AdminEvent]) {
        for event in events {
            match event {
                AdminEvent::ConfigLoaded { key: k, value } => {
                    self.upsert_config(k, value);
                    if k == KEY_FEEDS {
                        self.feeds = FeedsStatus::Listed(parse_feeds_value(value));
                    }
                    if k == KEY_POLL_SECS {
                        // Sync the editor to the authoritative value.
                        self.poll_draft = value.clone();
                        self.poll_error = None;
                    }
                }
                AdminEvent::Failed(detail) => {
                    if key == KEY_FEEDS {
                        self.feeds = FeedsStatus::Unavailable;
                    } else {
                        self.status = format!("Error loading {key}: {detail}");
                    }
                }
                // Acks and other admin replies carry nothing for a get.
                _ => {}
            }
        }
    }

    /// Fold the reply events of a `SetConfig` for `key`: record the
    /// authoritative live-vs-restart answer and surface a status line. A
    /// failure on [`KEY_POLL_SECS`] also lands inline on the poll editor.
    pub fn apply_set_reply(&mut self, key: &str, events: &[AdminEvent]) {
        for event in events {
            match event {
                AdminEvent::ConfigApplied { applied_live } => {
                    self.learned_live.insert(key.to_string(), *applied_live);
                    self.status = if *applied_live {
                        format!("Saved {key}; applied live.")
                    } else {
                        format!("Saved {key}; a restart is required to apply it.")
                    };
                }
                AdminEvent::Failed(detail) => {
                    self.status = format!("Error saving {key}: {detail}");
                    if key == KEY_POLL_SECS {
                        self.poll_error = Some(detail.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// Insert or replace a config pair keyed by `key`.
    fn upsert_config(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.config.iter_mut().find(|c| c.key == key) {
            slot.value = value.to_string();
        } else {
            self.config.push(ConfigEntry {
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    }

    /// The value currently held for `key`, if it has been read.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.config
            .iter()
            .find(|c| c.key == key)
            .map(|c| c.value.as_str())
    }

    /// The loaded `syndication_enabled` flag, if readable.
    pub fn enabled(&self) -> Option<bool> {
        self.value(KEY_ENABLED).and_then(parse_bool_value)
    }

    /// The loaded `syndication_poll_secs`, if readable.
    pub fn poll_secs(&self) -> Option<i64> {
        self.value(KEY_POLL_SECS).and_then(|v| v.parse().ok())
    }

    /// Replace the poll editor draft and re-validate it inline.
    pub fn set_poll_draft(&mut self, draft: &str) {
        self.poll_draft = draft.to_string();
        self.poll_error = validate_poll_secs(draft).err();
    }

    /// Whether the poll draft differs from the loaded value.
    pub fn poll_dirty(&self) -> bool {
        match self.value(KEY_POLL_SECS) {
            Some(loaded) => loaded != self.poll_draft.trim(),
            None => !self.poll_draft.trim().is_empty(),
        }
    }

    /// The `SetConfig` that saves the poll draft — `Some` only when the draft
    /// is valid *and* differs from the loaded value.
    pub fn poll_save_command(&self) -> Option<AdminCommand> {
        let secs = validate_poll_secs(&self.poll_draft).ok()?;
        if !self.poll_dirty() {
            return None;
        }
        Some(AdminCommand::SetConfig {
            key: KEY_POLL_SECS.to_string(),
            value: secs.to_string(),
        })
    }

    /// The `SetConfig` that flips a boolean key — `Some` only when the key
    /// has loaded and parses as a bool (no blind toggles).
    pub fn toggle_command(&self, key: &str) -> Option<AdminCommand> {
        let current = self.value(key).and_then(parse_bool_value)?;
        Some(AdminCommand::SetConfig {
            key: key.to_string(),
            value: (!current).to_string(),
        })
    }

    /// Whether a set of `key` applies live: the answer learned from a real
    /// `ConfigApplied` reply when one exists, else the documented expectation
    /// ([`expected_applies_live`]), else `false` (assume restart — the honest
    /// default for an unknown key).
    pub fn applies_live(&self, key: &str) -> bool {
        self.learned_live
            .get(key)
            .copied()
            .or_else(|| expected_applies_live(key))
            .unwrap_or(false)
    }

    /// Derive the gateway matrix from the loaded config pairs: one row per
    /// family in [`FAMILIES`], populated with whatever has loaded so far.
    pub fn gateway_matrix(&self) -> Vec<GatewayRow> {
        FAMILIES
            .iter()
            .map(|(family, toggle_key, addr_key)| GatewayRow {
                family,
                toggle_key,
                enabled: self.value(toggle_key).and_then(parse_bool_value),
                port: addr_key
                    .and_then(|k| self.value(k))
                    .and_then(parse_addr_port),
                applies_live: self.applies_live(toggle_key),
            })
            .collect()
    }

    /// The feed rows for the monitor, when listed.
    pub fn feed_rows(&self) -> Vec<FeedRow> {
        match &self.feeds {
            FeedsStatus::Listed(rows) => rows.clone(),
            _ => Vec::new(),
        }
    }

    /// One-line configured state shared by every feed row: whether the
    /// poller is on and how often it fires. Purely config-derived — live
    /// per-feed stats have no wire message yet.
    pub fn feed_state_line(&self) -> String {
        match (self.enabled(), self.poll_secs()) {
            (Some(true), Some(secs)) => format!("polling every {secs} s"),
            (Some(true), None) => "polling (interval unknown)".to_string(),
            (Some(false), _) => "poller disabled".to_string(),
            (None, _) => "poller state unknown".to_string(),
        }
    }
}

/// Validate a poll-interval draft: a positive integer number of seconds
/// within `POLL_MIN_SECS..=POLL_MAX_SECS`. Returns the parsed value or a
/// human-readable error.
pub fn validate_poll_secs(draft: &str) -> Result<i64, String> {
    let t = draft.trim();
    if t.is_empty() {
        return Err("Enter a poll interval in seconds.".to_string());
    }
    let secs: i64 = t
        .parse()
        .map_err(|_| format!("{t:?} is not a whole number of seconds."))?;
    if secs < POLL_MIN_SECS {
        return Err(format!("Interval must be at least {POLL_MIN_SECS} s."));
    }
    if secs > POLL_MAX_SECS {
        return Err(format!(
            "Interval must be at most {POLL_MAX_SECS} s (one week)."
        ));
    }
    Ok(secs)
}

/// The documented live-vs-restart expectation for the keys this panel
/// touches, mirroring the server's config semantics: listener toggles and
/// addresses bind at startup (restart); QWK re-reads config per command
/// (live); the syndication poll task starts at boot (restart). `None` for
/// keys outside the panel's vocabulary.
pub fn expected_applies_live(key: &str) -> Option<bool> {
    match key {
        "qwk_enabled" | "qwk_spool_dir" | "nntp_min_role" | "nntp_auth_require_tls" => Some(true),
        "nntp_enabled"
        | "nntp_addr"
        | "nntp_tls_enabled"
        | "nntp_tls_addr"
        | "nntp_feed_enabled"
        | "nntp_feed_addr"
        | "nntp_feed_tls_enabled"
        | "nntp_feed_tls_addr"
        | "ftn_enabled"
        | "ftn_addr"
        | KEY_ENABLED
        | KEY_POLL_SECS => Some(false),
        _ => None,
    }
}

/// Parse a server bool serialization, accepting the same spellings the
/// server's own parser does. `None` for anything else.
pub fn parse_bool_value(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parse the port out of a `SocketAddr` display string (`0.0.0.0:1119`,
/// `[::]:563`). `None` when it doesn't look like one.
pub fn parse_addr_port(addr: &str) -> Option<u16> {
    let (_, port) = addr.trim().rsplit_once(':')?;
    port.parse().ok()
}

/// Parse a `syndication_feeds` serialization into feed rows — **total** and
/// defensive, never panicking, always returning (possibly empty).
///
/// The on-disk shape is a TOML table (what `burrow.toml` holds under
/// `[syndication_feeds]`); a config-get exposure would most plausibly carry
/// either the table body or an inline table. Accepted forms:
///
/// - table body lines: `"https://…" = "board"` (keys/values quoted or bare),
///   with `[syndication_feeds]`-style header lines, comments and blanks
///   skipped;
/// - an inline table: `{ "https://…" = "board", … }`.
///
/// Malformed lines/pairs are skipped, quoted keys may contain `=` (URLs with
/// query strings), and rows come back sorted by URL with duplicates removed.
pub fn parse_feeds_value(value: &str) -> Vec<FeedRow> {
    let text = value.trim();
    let mut rows: Vec<FeedRow> = Vec::new();
    let body = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .map(str::trim);
    match body {
        // Inline table: split on commas. (A comma inside a quoted URL would
        // split wrongly; the halves then fail pair-parsing and are skipped —
        // degraded, never wrong or panicking.)
        Some(inner) => rows.extend(inner.split(',').filter_map(parse_feed_pair)),
        None => rows.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
                .filter_map(parse_feed_pair),
        ),
    }
    rows.sort();
    rows.dedup_by(|a, b| a.url == b.url);
    rows
}

/// Parse one `key = value` pair into a [`FeedRow`]. `None` for anything
/// malformed or empty.
fn parse_feed_pair(s: &str) -> Option<FeedRow> {
    let s = s.trim().trim_end_matches(',').trim();
    // A quoted key may contain '=' (query-string URLs), so find its closing
    // quote before looking for the separator.
    let (url, rest) = match s.strip_prefix('"') {
        Some(stripped) => {
            let end = stripped.find('"')?;
            (stripped[..end].to_string(), &stripped[end + 1..])
        }
        None => {
            let eq = s.find('=')?;
            (s[..eq].trim().to_string(), &s[eq..])
        }
    };
    let board = unquote(rest.trim_start().strip_prefix('=')?.trim());
    if url.is_empty() || board.is_empty() {
        return None;
    }
    Some(FeedRow { url, board })
}

/// Strip one layer of matching single or double quotes, if present.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let stripped = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')));
    stripped.unwrap_or(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand: a `ConfigLoaded` reply.
    fn loaded(key: &str, value: &str) -> Vec<AdminEvent> {
        vec![AdminEvent::ConfigLoaded {
            key: key.into(),
            value: value.into(),
        }]
    }

    /// A state with the full gateway/syndication key set loaded, mirroring
    /// server defaults except where noted.
    fn loaded_state() -> SynAdminState {
        let mut s = SynAdminState::default();
        for (key, value) in [
            (KEY_ENABLED, "true"),
            (KEY_POLL_SECS, "1800"),
            ("nntp_enabled", "true"),
            ("nntp_addr", "0.0.0.0:1119"),
            ("nntp_tls_enabled", "false"),
            ("nntp_tls_addr", "0.0.0.0:563"),
            ("nntp_feed_enabled", "false"),
            ("nntp_feed_addr", "0.0.0.0:1120"),
            ("nntp_feed_tls_enabled", "false"),
            ("nntp_feed_tls_addr", "0.0.0.0:1563"),
            ("ftn_enabled", "true"),
            ("ftn_addr", "0.0.0.0:24554"),
            ("qwk_enabled", "true"),
        ] {
            s.apply_get_reply(key, &loaded(key, value));
        }
        s.apply_get_reply(
            KEY_FEEDS,
            &loaded(KEY_FEEDS, "\"https://a.example/feed.xml\" = \"general\"\n"),
        );
        s
    }

    #[test]
    fn load_commands_cover_every_panel_key() {
        let cmds = SynAdminState::load_commands();
        assert_eq!(cmds.len(), LOAD_KEYS.len());
        for (cmd, key) in cmds.iter().zip(LOAD_KEYS) {
            assert_eq!(
                cmd,
                &AdminCommand::GetConfig {
                    key: (*key).to_string()
                }
            );
        }
        // The feeds key is asked for even though today's server refuses it.
        assert!(LOAD_KEYS.contains(&KEY_FEEDS));
    }

    #[test]
    fn get_replies_upsert_and_sync_the_poll_draft() {
        let mut s = SynAdminState::default();
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "false"));
        assert_eq!(s.enabled(), Some(false));
        s.apply_get_reply(KEY_POLL_SECS, &loaded(KEY_POLL_SECS, "1800"));
        assert_eq!(s.poll_secs(), Some(1800));
        assert_eq!(s.poll_draft, "1800");
        assert!(s.poll_error.is_none());
        // A re-read updates in place (no duplicate entries).
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "true"));
        assert_eq!(s.enabled(), Some(true));
        assert_eq!(s.config.len(), 2);
    }

    #[test]
    fn feeds_value_lists_rows_and_failure_marks_toml_only() {
        let mut s = SynAdminState::default();
        assert_eq!(s.feeds, FeedsStatus::NotLoaded);
        // The real server today: ConfigGet(syndication_feeds) → NotFound.
        s.apply_get_reply(
            KEY_FEEDS,
            &[AdminEvent::Failed("server error: NotFound".into())],
        );
        assert_eq!(s.feeds, FeedsStatus::Unavailable);
        // Expected — no scary status line for the honest TOML-only outcome.
        assert!(s.status.is_empty());
        // A value (the mock, or a future read-only exposure) parses to rows.
        s.apply_get_reply(
            KEY_FEEDS,
            &loaded(
                KEY_FEEDS,
                "\"https://b.example/rss\" = \"tech\"\n\"https://a.example/atom\" = \"general\"\n",
            ),
        );
        assert_eq!(
            s.feed_rows(),
            vec![
                FeedRow {
                    url: "https://a.example/atom".into(),
                    board: "general".into()
                },
                FeedRow {
                    url: "https://b.example/rss".into(),
                    board: "tech".into()
                },
            ]
        );
    }

    #[test]
    fn get_failure_on_ordinary_keys_surfaces_on_status() {
        let mut s = SynAdminState::default();
        s.apply_get_reply("nntp_enabled", &[AdminEvent::Failed("Forbidden".into())]);
        assert!(s.status.contains("nntp_enabled"));
        assert!(s.status.contains("Forbidden"));
    }

    #[test]
    fn poll_draft_validation_vectors() {
        assert_eq!(validate_poll_secs("1800"), Ok(1800));
        assert_eq!(validate_poll_secs(" 900 "), Ok(900));
        assert_eq!(validate_poll_secs("1"), Ok(POLL_MIN_SECS));
        assert_eq!(validate_poll_secs("604800"), Ok(POLL_MAX_SECS));
        assert!(validate_poll_secs("0").is_err());
        assert!(validate_poll_secs("-5").is_err());
        assert!(validate_poll_secs("604801").is_err());
        assert!(validate_poll_secs("abc").is_err());
        assert!(validate_poll_secs("18.5").is_err());
        assert!(validate_poll_secs("").is_err());
    }

    #[test]
    fn poll_editor_flow_load_edit_validate_save() {
        let mut s = SynAdminState::default();
        s.apply_get_reply(KEY_POLL_SECS, &loaded(KEY_POLL_SECS, "1800"));
        // Unedited: nothing to save.
        assert!(!s.poll_dirty());
        assert_eq!(s.poll_save_command(), None);
        // An invalid edit parks an inline error and never yields a command.
        s.set_poll_draft("0");
        assert!(s.poll_error.is_some());
        assert_eq!(s.poll_save_command(), None);
        // A valid edit clears it and yields the SetConfig.
        s.set_poll_draft("900");
        assert!(s.poll_error.is_none());
        assert_eq!(
            s.poll_save_command(),
            Some(AdminCommand::SetConfig {
                key: KEY_POLL_SECS.into(),
                value: "900".into(),
            })
        );
        // The set reply is honest about the restart, and a follow-up re-read
        // (the app reloads the key after a save) re-syncs the draft.
        s.apply_set_reply(
            KEY_POLL_SECS,
            &[AdminEvent::ConfigApplied {
                applied_live: false,
            }],
        );
        assert!(s.status.contains("restart"));
        s.apply_get_reply(KEY_POLL_SECS, &loaded(KEY_POLL_SECS, "900"));
        assert!(!s.poll_dirty());
    }

    #[test]
    fn set_failure_on_poll_key_lands_inline() {
        let mut s = SynAdminState::default();
        s.apply_set_reply(
            KEY_POLL_SECS,
            &[AdminEvent::Failed("server error: BadValue".into())],
        );
        assert!(s.poll_error.as_deref().unwrap_or("").contains("BadValue"));
        assert!(s.status.contains(KEY_POLL_SECS));
    }

    #[test]
    fn toggle_command_flips_only_loaded_parsable_bools() {
        let mut s = SynAdminState::default();
        // Not loaded yet: no blind toggles.
        assert_eq!(s.toggle_command(KEY_ENABLED), None);
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "false"));
        assert_eq!(
            s.toggle_command(KEY_ENABLED),
            Some(AdminCommand::SetConfig {
                key: KEY_ENABLED.into(),
                value: "true".into(),
            })
        );
        // An unparsable value refuses to toggle.
        s.apply_get_reply("ftn_enabled", &loaded("ftn_enabled", "maybe"));
        assert_eq!(s.toggle_command("ftn_enabled"), None);
    }

    #[test]
    fn learned_applied_live_wins_over_expectation() {
        let mut s = SynAdminState::default();
        // Expectation: QWK applies live, syndication needs a restart.
        assert!(s.applies_live("qwk_enabled"));
        assert!(!s.applies_live(KEY_ENABLED));
        // Unknown keys default to the honest "assume restart".
        assert!(!s.applies_live("mystery_key"));
        // A real ConfigApplied reply overrides the expectation.
        s.apply_set_reply(
            "qwk_enabled",
            &[AdminEvent::ConfigApplied {
                applied_live: false,
            }],
        );
        assert!(!s.applies_live("qwk_enabled"));
        assert!(s.status.contains("restart"));
        s.apply_set_reply(
            KEY_ENABLED,
            &[AdminEvent::ConfigApplied { applied_live: true }],
        );
        assert!(s.applies_live(KEY_ENABLED));
        assert!(s.status.contains("applied live"));
    }

    #[test]
    fn expected_applies_live_mirrors_server_semantics() {
        assert_eq!(expected_applies_live("qwk_enabled"), Some(true));
        assert_eq!(expected_applies_live("nntp_auth_require_tls"), Some(true));
        assert_eq!(expected_applies_live("nntp_enabled"), Some(false));
        assert_eq!(expected_applies_live("nntp_tls_enabled"), Some(false));
        assert_eq!(expected_applies_live("ftn_enabled"), Some(false));
        assert_eq!(expected_applies_live(KEY_ENABLED), Some(false));
        assert_eq!(expected_applies_live(KEY_POLL_SECS), Some(false));
        assert_eq!(expected_applies_live("server.name"), None);
    }

    #[test]
    fn gateway_matrix_derives_from_loaded_pairs() {
        let s = loaded_state();
        let matrix = s.gateway_matrix();
        assert_eq!(matrix.len(), 7);
        let row = |family: &str| {
            matrix
                .iter()
                .find(|r| r.family == family)
                .unwrap_or_else(|| panic!("no {family} row"))
        };
        let reader = row("NNTP reader");
        assert_eq!(reader.enabled, Some(true));
        assert_eq!(reader.port, Some(1119));
        assert!(!reader.applies_live, "listener toggles need a restart");
        assert_eq!(row("NNTP reader TLS").port, Some(563));
        assert_eq!(row("NNTP peer feed").enabled, Some(false));
        assert_eq!(row("NNTP peer feed TLS").port, Some(1563));
        assert_eq!(row("FTN binkp").port, Some(24554));
        let qwk = row("QWK offline mail");
        assert_eq!(qwk.enabled, Some(true));
        assert_eq!(qwk.port, None, "QWK is not a listener");
        assert!(qwk.applies_live);
        let syn = row("Syndication");
        assert_eq!(syn.enabled, Some(true));
        assert_eq!(syn.port, None);
        assert!(!syn.applies_live);
    }

    #[test]
    fn gateway_matrix_before_any_load_is_all_unknowns() {
        let matrix = SynAdminState::default().gateway_matrix();
        assert_eq!(matrix.len(), 7);
        assert!(matrix.iter().all(|r| r.enabled.is_none()));
        assert!(matrix.iter().all(|r| r.port.is_none()));
    }

    #[test]
    fn feed_state_line_reads_from_config_only() {
        let mut s = SynAdminState::default();
        assert_eq!(s.feed_state_line(), "poller state unknown");
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "false"));
        assert_eq!(s.feed_state_line(), "poller disabled");
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "true"));
        assert_eq!(s.feed_state_line(), "polling (interval unknown)");
        s.apply_get_reply(KEY_POLL_SECS, &loaded(KEY_POLL_SECS, "1800"));
        assert_eq!(s.feed_state_line(), "polling every 1800 s");
    }

    #[test]
    fn bool_values_parse_like_the_server() {
        for v in ["true", "TRUE", "1", "yes", "on", " On "] {
            assert_eq!(parse_bool_value(v), Some(true), "{v:?}");
        }
        for v in ["false", "0", "no", "off", "OFF"] {
            assert_eq!(parse_bool_value(v), Some(false), "{v:?}");
        }
        for v in ["", "maybe", "2"] {
            assert_eq!(parse_bool_value(v), None, "{v:?}");
        }
    }

    #[test]
    fn addr_ports_parse_from_socketaddr_displays() {
        assert_eq!(parse_addr_port("0.0.0.0:1119"), Some(1119));
        assert_eq!(parse_addr_port("[::]:563"), Some(563));
        assert_eq!(parse_addr_port("127.0.0.1:0"), Some(0));
        assert_eq!(parse_addr_port("not-an-addr"), None);
        assert_eq!(parse_addr_port("host:99999"), None);
        assert_eq!(parse_addr_port(""), None);
    }

    #[test]
    fn feeds_parse_toml_table_body() {
        let rows = parse_feeds_value(
            "\"https://blog.example.org/feed.xml\" = \"general\"\n\
             \"https://warren.example/atom.xml\" = \"tech\"\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].url, "https://blog.example.org/feed.xml");
        assert_eq!(rows[0].board, "general");
        assert_eq!(rows[1].board, "tech");
    }

    #[test]
    fn feeds_parse_skips_headers_comments_and_garbage() {
        let rows = parse_feeds_value(
            "[syndication_feeds]\n\
             # the news\n\
             \"https://a.example/rss\" = \"general\"\n\
             \n\
             this line is nonsense\n\
             \"\" = \"empty-url-skipped\"\n\
             \"https://b.example/rss\" = \"\"\n",
        );
        assert_eq!(
            rows,
            vec![FeedRow {
                url: "https://a.example/rss".into(),
                board: "general".into()
            }]
        );
    }

    #[test]
    fn feeds_parse_inline_table_and_bare_forms() {
        let rows =
            parse_feeds_value("{ \"https://a.example/rss\" = \"general\", bare-key = 'tech' }");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].url, "bare-key");
        assert_eq!(rows[0].board, "tech");
        assert_eq!(rows[1].url, "https://a.example/rss");
        assert_eq!(rows[1].board, "general");
    }

    #[test]
    fn feeds_parse_quoted_url_may_contain_equals() {
        let rows = parse_feeds_value("\"https://a.example/feed?format=rss&x=1\" = \"general\"");
        assert_eq!(
            rows,
            vec![FeedRow {
                url: "https://a.example/feed?format=rss&x=1".into(),
                board: "general".into()
            }]
        );
    }

    #[test]
    fn feeds_parse_is_total_on_junk_and_dedupes() {
        assert!(parse_feeds_value("").is_empty());
        assert!(parse_feeds_value("{}").is_empty());
        assert!(parse_feeds_value("{ , , }").is_empty());
        assert!(parse_feeds_value("= = =\n\"\" = \"\"").is_empty());
        // Duplicate URLs collapse to the first (sorted) row.
        let rows = parse_feeds_value("\"u\" = \"a\"\n\"u\" = \"b\"\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url, "u");
    }
}
