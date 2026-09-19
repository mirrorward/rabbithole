//! Pure, DOM-free state for the feeds half of the admin console's
//! **Federation & feeds** section.
//!
//! Like [`crate::admin`] and [`crate::theme_editor`], this module holds no
//! Leptos or `web_sys` types: the feed table's model and the defensive
//! `syndication_feeds` parser are unit-tested on the host with `cargo test`.
//!
//! It used to be a second settings editor as well: a gateway matrix with its
//! own toggles, a poll-interval box with its own Save, and a table of which
//! keys "need a restart" that mirrored the server by hand. All of that is the
//! settings model's now ([`crate::admin_settings`]), where the burrow itself
//! says what is live, and what is left here only reads.
//!
//! ## What the wire cannot say
//!
//! `syndication_feeds` (the feed URL → board-slug map) is **TOML-only**: the
//! server's config get/set has no arm for it, so a `ConfigGet` answers
//! `NotFound`. The pane still *asks* (a future server slice may expose a
//! read-only serialization) and folds the outcome totally: a value parses
//! into read-only [`FeedRow`]s via [`parse_feeds_value`]; a failure lands as
//! [`FeedsStatus::Unavailable`] and the UI shows the honest "edit
//! `burrow.toml`" hint. Either way feeds are never editable here.
//!
//! ## Feed monitor
//!
//! Live counters ride [`AdminCommand::GetGatewayStats`] →
//! [`AdminEvent::GatewayStatsLoaded`] (ADMIN 45/46). The panel asks on load
//! and folds last-poll / status / seen / posted / dupes per feed plus the
//! per-gateway activity rows. Configured state (enabled + poll interval)
//! remains the fallback when a snapshot has not arrived.

use rabbithole_proto::admin::{FeedStat, GatewayStat, GatewayStatsReply};

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
pub const LOAD_KEYS: &[&str] = &[KEY_ENABLED, KEY_POLL_SECS, KEY_FEEDS];

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

/// The Syndication & Gateways panel model. `Default` is the empty, unloaded
/// state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SynAdminState {
    /// Resolved config key/value pairs, upserted from `ConfigLoaded` replies.
    pub config: Vec<ConfigEntry>,
    /// What we know about the feed map.
    pub feeds: FeedsStatus,
    /// One-line status for the panel.
    pub status: String,
    /// Latest live gateway/feed snapshot, if one has arrived.
    pub stats: Option<GatewayStatsReply>,
}

impl SynAdminState {
    /// The `GetConfig` commands that load the panel, one per key in
    /// [`LOAD_KEYS`]. The caller pairs each command's replies back through
    /// [`apply_get_reply`](Self::apply_get_reply) with the same key.
    pub fn load_commands() -> Vec<AdminCommand> {
        let mut cmds: Vec<AdminCommand> = LOAD_KEYS
            .iter()
            .map(|key| AdminCommand::GetConfig {
                key: (*key).to_string(),
            })
            .collect();
        cmds.push(AdminCommand::GetGatewayStats);
        cmds
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
                }
                AdminEvent::Failed(detail) => {
                    if key == KEY_FEEDS {
                        self.feeds = FeedsStatus::Unavailable;
                    } else {
                        self.status = format!("Error loading {key}: {detail}");
                    }
                }
                AdminEvent::GatewayStatsLoaded(reply) => {
                    self.stats = Some(reply.clone());
                }
                // Acks and other admin replies carry nothing for a get.
                _ => {}
            }
        }
    }

    /// Fold a live (or mock) admin reply that may be a config get, a config
    /// set, or the gateway-stats snapshot. `key` is the in-flight config key
    /// when the transport paired the request; `None` for unpaired snapshots.
    pub fn apply_live(&mut self, key: Option<&str>, events: &[AdminEvent]) {
        if events
            .iter()
            .any(|e| matches!(e, AdminEvent::GatewayStatsLoaded(_)))
        {
            self.apply_get_reply("", events);
            return;
        }
        // A `ConfigApplied` is the settings model's business
        // ([`crate::admin_settings`]); this pane only reads.
        if events
            .iter()
            .any(|e| matches!(e, AdminEvent::ConfigApplied { .. }))
        {
            return;
        }
        self.apply_get_reply(key.unwrap_or(""), events);
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

    /// The feed rows for the monitor, when listed.
    pub fn feed_rows(&self) -> Vec<FeedRow> {
        match &self.feeds {
            FeedsStatus::Listed(rows) => rows.clone(),
            _ => Vec::new(),
        }
    }

    /// One-line configured state shared by every feed row: whether the
    /// poller is on and how often it fires. Used when no live snapshot
    /// has arrived yet.
    pub fn feed_state_line(&self) -> String {
        match (self.enabled(), self.poll_secs()) {
            (Some(true), Some(secs)) => format!("polling every {secs} s"),
            (Some(true), None) => "polling (interval unknown)".to_string(),
            (Some(false), _) => "poller disabled".to_string(),
            (None, _) => "poller state unknown".to_string(),
        }
    }

    /// Live stats for `url`, if the latest snapshot mentioned it.
    pub fn feed_stat(&self, url: &str) -> Option<&FeedStat> {
        self.stats.as_ref()?.feeds.iter().find(|f| f.url == url)
    }

    /// Per-gateway activity rows from the latest snapshot.
    pub fn gateway_stats(&self) -> &[GatewayStat] {
        self.stats
            .as_ref()
            .map(|s| s.gateways.as_slice())
            .unwrap_or(&[])
    }
}

/// Human last-poll label. `now_ms <= 0` (host tests / unknown clock) falls
/// back to a UTC time-of-day so the string stays deterministic.
pub fn last_poll_label(last_poll_ms: i64, now_ms: i64) -> String {
    if last_poll_ms <= 0 {
        return "never".to_string();
    }
    if now_ms <= 0 {
        return crate::clock::utc_hhmm(last_poll_ms);
    }
    let ago_s = now_ms.saturating_sub(last_poll_ms) / 1000;
    if ago_s < 60 {
        format!("{ago_s}s ago")
    } else if ago_s < 3600 {
        format!("{}m ago", ago_s / 60)
    } else if ago_s < 86_400 {
        format!("{}h ago", ago_s / 3600)
    } else {
        format!("{}d ago", ago_s / 86_400)
    }
}

/// One-line live outcome for a feed row.
pub fn feed_stat_line(stat: &FeedStat) -> String {
    let status = match stat.last_status.as_str() {
        "" if stat.last_poll_ms == 0 => return "never polled".to_string(),
        "ok" => "ok",
        "not_modified" => "not modified",
        "error" => "error",
        other => other,
    };
    format!(
        "{status} · {} seen · {} posted · {} dupes",
        stat.items_seen, stat.items_posted, stat.dupes_dropped
    )
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
        assert_eq!(cmds.len(), LOAD_KEYS.len() + 1);
        for (cmd, key) in cmds.iter().zip(LOAD_KEYS) {
            assert_eq!(
                cmd,
                &AdminCommand::GetConfig {
                    key: (*key).to_string()
                }
            );
        }
        assert_eq!(cmds.last(), Some(&AdminCommand::GetGatewayStats));
        // The feeds key is asked for even though today's server refuses it.
        assert!(LOAD_KEYS.contains(&KEY_FEEDS));
    }

    #[test]
    fn get_replies_upsert_by_key() {
        let mut s = SynAdminState::default();
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "false"));
        assert_eq!(s.enabled(), Some(false));
        s.apply_get_reply(KEY_POLL_SECS, &loaded(KEY_POLL_SECS, "1800"));
        assert_eq!(s.poll_secs(), Some(1800));
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
    fn live_stats_fold_onto_the_matching_feed() {
        let mut s = loaded_state();
        let reply = GatewayStatsReply {
            generated_at_ms: 1_700_000_000_000,
            feeds: vec![FeedStat {
                url: "https://a.example/feed.xml".into(),
                last_poll_ms: 1_700_000_000_000,
                last_status: "ok".into(),
                items_seen: 12,
                items_posted: 9,
                dupes_dropped: 3,
            }],
            gateways: vec![GatewayStat {
                name: "nntp".into(),
                enabled: true,
                counters: vec![("posts".into(), 4), ("sessions".into(), 7)],
            }],
        };
        s.apply_live(None, &[AdminEvent::GatewayStatsLoaded(reply)]);
        let stat = s.feed_stat("https://a.example/feed.xml").expect("row");
        assert_eq!(stat.items_posted, 9);
        assert_eq!(feed_stat_line(stat), "ok · 12 seen · 9 posted · 3 dupes");
        assert_eq!(s.gateway_stats().len(), 1);
        assert_eq!(s.gateway_stats()[0].name, "nntp");
        assert_eq!(last_poll_label(0, 1_700_000_000_000), "never");
        assert_eq!(last_poll_label(1_783_780_507_000, 0), "14:35");
        assert_eq!(last_poll_label(1_000, 61_000), "1m ago");
        assert_eq!(feed_stat_line(&FeedStat::default()), "never polled");
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
