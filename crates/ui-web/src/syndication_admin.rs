//! Pure, DOM-free state for the feeds half of the admin console's
//! **Federation & feeds** section.
//!
//! Like [`crate::admin`] and [`crate::theme_editor`], this module holds no
//! Leptos or `web_sys` types: typed feed snapshots and their display state
//! are unit-tested on the host with `cargo test`.
//!
//! It used to be a second settings editor as well: a gateway matrix with its
//! own toggles, a poll-interval box with its own Save, and a table of which
//! keys "need a restart" that mirrored the server by hand. All of that is the
//! settings model's now ([`crate::admin_settings`]), where the burrow itself
//! says what is live, and what is left here only reads.
//!
//! Configured mappings and their exact-key-joined poll results ride ADMIN65/66.
//! URLs are display-only and redacted server-side; opaque row IDs keep feeds
//! with identical safe URLs distinct. Mutations remain TOML-only plus restart.
//! Older servers get an explicit unavailable state, never speculative parsing
//! of a raw configuration map. Gateway activity retains ADMIN45/46.

use rabbithole_proto::admin::{
    FeedMapping, FeedPollStats, FeedStat, GatewayStat, GatewayStatsReply,
};

use crate::admin::ConfigEntry;
use crate::wire::{AdminCommand, AdminEvent};

/// Config key: master switch for the feed-poll task.
pub const KEY_ENABLED: &str = "syndication_enabled";
/// Config key: base seconds between feed polls, used at the next reschedule.
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

/// Ordinary poller settings read alongside the dedicated mapping snapshot.
pub const LOAD_KEYS: &[&str] = &[KEY_ENABLED, KEY_POLL_SECS];

/// A legacy table-parser result. Live monitoring uses typed FeedMapping rows
/// and does not request or display raw TOML values.
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
    /// The server does not support the dedicated read API.
    Unavailable,
    /// A typed, server-joined snapshot arrived (possibly empty).
    Listed(Vec<FeedMapping>),
    /// This account was refused, distinct from an older server.
    Forbidden,
    /// A failed or timed-out read; no stale rows remain.
    Failed,
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
        cmds.push(AdminCommand::GetFeedMappings);
        cmds
    }

    /// Fold ordinary poller config reads; raw mapping values are ignored.
    /// The legacy KEY_FEEDS failure is retained for older mock callers.
    pub fn apply_get_reply(&mut self, key: &str, events: &[AdminEvent]) {
        for event in events {
            match event {
                AdminEvent::ConfigLoaded { key: k, value } => {
                    if k != KEY_FEEDS {
                        self.upsert_config(k, value);
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

    /// Fold only the reply to the dedicated mapping request. Refusals clear
    /// rows instead of retaining a previous account's configuration.
    pub fn apply_mappings_reply(&mut self, events: &[AdminEvent]) {
        for event in events {
            match event {
                AdminEvent::FeedMappingsLoaded(reply) => {
                    self.feeds = FeedsStatus::Listed(reply.feeds.clone())
                }
                AdminEvent::Failed(detail) => {
                    self.feeds = if detail.contains("Unsupported") {
                        FeedsStatus::Unavailable
                    } else if detail.contains("Forbidden") {
                        FeedsStatus::Forbidden
                    } else {
                        FeedsStatus::Failed
                    }
                }
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
    pub fn feed_rows(&self) -> Vec<FeedMapping> {
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

    /// Legacy snapshot lookup; the configured-feed table never joins on
    /// these display URLs, which can collide after redaction.
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

/// Format server-joined counters in the configured-feed table.
pub fn mapped_stat_line(stat: &FeedPollStats) -> String {
    feed_stat_line(&FeedStat {
        url: String::new(),
        last_poll_ms: stat.last_poll_ms,
        last_status: stat.last_status.clone(),
        items_seen: stat.items_seen,
        items_posted: stat.items_posted,
        dupes_dropped: stat.dupes_dropped,
    })
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

    #[test]
    fn load_commands_read_typed_mappings_not_raw_config() {
        let commands = SynAdminState::load_commands();
        assert_eq!(commands.len(), LOAD_KEYS.len() + 2);
        assert!(commands.contains(&AdminCommand::GetFeedMappings));
        assert!(!LOAD_KEYS.contains(&KEY_FEEDS));
    }

    #[test]
    fn mappings_keep_duplicate_display_urls_and_their_own_counters() {
        use rabbithole_proto::admin::FeedMappingsReply;
        let mut state = SynAdminState::default();
        let mut a = FeedMapping {
            id: [1; 32],
            url: "https://example.test/feed".into(),
            board: "a".into(),
            ..Default::default()
        };
        a.stats.items_posted = 3;
        let mut b = a.clone();
        b.id = [2; 32];
        b.board = "b".into();
        b.stats.items_posted = 7;
        state.apply_mappings_reply(&[AdminEvent::FeedMappingsLoaded(FeedMappingsReply {
            generated_at_ms: 1,
            feeds: vec![a, b],
        })]);
        let rows = state.feed_rows();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].id, rows[1].id);
        assert_eq!(
            (rows[0].stats.items_posted, rows[1].stats.items_posted),
            (3, 7)
        );
        for (reason, expected) in [
            ("Unsupported", FeedsStatus::Unavailable),
            ("Forbidden", FeedsStatus::Forbidden),
            ("Connection lost", FeedsStatus::Failed),
        ] {
            state.apply_mappings_reply(&[AdminEvent::Failed(reason.into())]);
            assert_eq!(state.feeds, expected);
            assert!(state.feed_rows().is_empty());
        }
    }

    #[test]
    fn gateway_stats_cannot_replace_joined_mapping_results() {
        let mut s = SynAdminState::default();
        s.apply_live(
            None,
            &[AdminEvent::GatewayStatsLoaded(GatewayStatsReply {
                feeds: vec![FeedStat {
                    url: "https://same.test/feed".into(),
                    items_posted: 100,
                    ..Default::default()
                }],
                gateways: vec![GatewayStat {
                    name: "nntp".into(),
                    ..Default::default()
                }],
                ..Default::default()
            })],
        );
        assert_eq!(s.feeds, FeedsStatus::NotLoaded);
        assert_eq!(s.gateway_stats().len(), 1);
        assert_eq!(mapped_stat_line(&FeedPollStats::default()), "never polled");
        assert_eq!(last_poll_label(0, 1000), "never");
        assert_eq!(last_poll_label(1000, 61000), "1m ago");
    }

    #[test]
    fn config_reads_only_describe_the_poller() {
        let mut s = SynAdminState::default();
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "false"));
        assert_eq!(s.feed_state_line(), "poller disabled");
        s.apply_get_reply(KEY_ENABLED, &loaded(KEY_ENABLED, "true"));
        s.apply_get_reply(KEY_POLL_SECS, &loaded(KEY_POLL_SECS, "1800"));
        assert_eq!(s.feed_state_line(), "polling every 1800 s");
        s.apply_get_reply(
            KEY_FEEDS,
            &loaded(KEY_FEEDS, "https://user:secret@example.test/feed"),
        );
        assert!(s.value(KEY_FEEDS).is_none());
        assert!(s.feed_rows().is_empty());
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
