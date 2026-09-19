//! The demo burrow's settings, shaped exactly like a real burrow's.
//!
//! The demo console should show what a live one shows: every section, every
//! switch, the same defaults. So this is a snapshot of what a real burrow says
//! about itself (`ServerConfig::describe` on the defaults), and a host test
//! fails, printing the fresh table, the moment the server's list moves on.

use rabbithole_proto::admin::{config_kind, ConfigKeyInfo};

/// `(key, default, kind, flags)` for every setting a burrow describes.
#[rustfmt::skip]
pub const DEMO_SCHEMA: &[(&str, &str, u8, u8)] = &[
    ("name", "An Unnamed Burrow", 0, 1),
    ("motd", "", 0, 1),
    ("agreement", "", 0, 1),
    ("guest_enabled", "true", 1, 1),
    ("quic_addr", "0.0.0.0:4653", 0, 0),
    ("ws_addr", "127.0.0.1:4654", 0, 0),
    ("ws_allow_insecure_remote", "false", 1, 0),
    ("ws_allowed_origins", "", 0, 8),
    ("ws_public_url", "", 0, 1),
    ("advertise_host", "", 0, 1),
    ("announce_enabled", "true", 1, 1),
    ("announce_trackers", "tracker.rabbit.direct", 0, 8),
    ("announce_ttl_secs", "120", 2, 1),
    ("announce_slug", "", 0, 1),
    ("announce_sysop", "", 0, 1),
    ("announce_description", "", 0, 1),
    ("data_dir", "./burrow-data", 0, 8),
    ("session_ttl_secs", "2592000", 2, 1),
    ("chat_max_len", "4096", 2, 1),
    ("registration_mode", "open", 3, 1),
    ("persona_max", "5", 2, 1),
    ("avatar_max_bytes", "262144", 2, 1),
    ("banner_max_bytes", "1048576", 2, 1),
    ("upload_max_file_bytes", "52428800", 2, 1),
    ("upload_quota_bytes", "0", 2, 1),
    ("max_concurrent_transfers", "0", 2, 1),
    ("transfer_rate_bytes_per_sec", "0", 2, 1),
    ("swarm_advert_ttl_secs", "3600", 2, 1),
    ("swarm_adverts_max", "4096", 2, 1),
    ("swarm_cache_max_bytes", "0", 2, 1),
    ("telnet_enabled", "false", 1, 1),
    ("telnet_addr", "0.0.0.0:2323", 0, 1),
    ("telnet_min_role", "guest", 3, 1),
    ("finger_enabled", "false", 1, 1),
    ("finger_addr", "0.0.0.0:7979", 0, 1),
    ("finger_min_role", "guest", 3, 1),
    ("files_http_base", "", 0, 1),
    ("http_enabled", "false", 1, 1),
    ("http_addr", "0.0.0.0:8080", 0, 1),
    ("http_web_root", "", 0, 1),
    ("nntp_enabled", "false", 1, 1),
    ("nntp_addr", "0.0.0.0:1119", 0, 1),
    ("nntp_min_role", "guest", 3, 1),
    ("nntp_tls_enabled", "false", 1, 1),
    ("nntp_tls_addr", "0.0.0.0:563", 0, 1),
    ("nntp_auth_require_tls", "true", 1, 1),
    ("nntp_feed_enabled", "false", 1, 1),
    ("nntp_feed_addr", "0.0.0.0:1120", 0, 1),
    ("nntp_feed_tls_enabled", "false", 1, 1),
    ("nntp_feed_tls_addr", "0.0.0.0:1563", 0, 1),
    ("radio_enabled", "false", 1, 1),
    ("radio_addr", "0.0.0.0:8000", 0, 1),
    ("radio_public_base", "", 0, 1),
    ("radio_source_enabled", "false", 1, 1),
    ("radio_source_addr", "0.0.0.0:8001", 0, 1),
    ("radio_source_user", "source", 0, 1),
    ("radio_source_password", "", 0, 3),
    ("doors_enabled", "false", 1, 0),
    ("doors_dir", "doors", 0, 0),
    ("doors_max_nodes", "4", 2, 0),
    ("doors_session_max_secs", "3600", 2, 0),
    ("hotline_enabled", "false", 1, 1),
    ("hotline_addr", "0.0.0.0:5500", 0, 1),
    ("hotline_min_role", "guest", 3, 1),
    ("ftn_enabled", "false", 1, 1),
    ("ftn_addr", "0.0.0.0:24554", 0, 1),
    ("ftn_node", "", 0, 1),
    ("ftn_uplink", "", 0, 1),
    ("ftn_uplink_host", "", 0, 1),
    ("ftn_password", "", 0, 3),
    ("ftn_inbound_dir", "ftn/inbound", 0, 1),
    ("ftn_outbound_dir", "ftn/outbound", 0, 1),
    ("qwk_enabled", "false", 1, 1),
    ("qwk_spool_dir", "qwk", 0, 1),
    ("backup_dir", "backups", 0, 1),
    ("syndication_enabled", "false", 1, 1),
    ("syndication_poll_secs", "1800", 2, 1),
    ("federation_enabled", "false", 1, 0),
    ("federation_origin", "", 0, 8),
    ("federation_addr", "0.0.0.0:4655", 0, 0),
    ("s2s_grants_enabled", "false", 1, 1),
    ("s2s_pull_enabled", "false", 1, 1),
    ("s2s_max_concurrent", "2", 2, 1),
    ("s2s_max_bytes", "0", 2, 1),
    ("s2s_grants_to_any", "false", 1, 1),
    ("s2s_pull_from_any", "false", 1, 1),
    ("s2s_private_addresses", "false", 1, 1),
    ("s2s_swarm", "false", 1, 1),
    ("s2s_swarm_sources", "false", 1, 1),
    ("portmap_enabled", "false", 1, 0),
    ("portmap_gateway", "", 0, 0),
    ("portmap_lifetime_secs", "7200", 2, 0),
    ("ratelimit_enabled", "true", 1, 1),
    ("ratelimit_conn_per_min", "30", 2, 1),
    ("ratelimit_conn_burst", "10", 2, 1),
    ("ratelimit_auth_per_min", "5", 2, 1),
    ("ratelimit_auth_burst", "5", 2, 1),
    ("ratelimit_msg_per_sec", "10", 2, 1),
    ("ratelimit_msg_burst", "20", 2, 1),
    ("ratelimit_post_per_min", "6", 2, 1),
    ("ratelimit_post_burst", "6", 2, 1),
    ("ratelimit_transfer_per_min", "10", 2, 1),
    ("ratelimit_transfer_burst", "10", 2, 1),
    ("ratelimit_legacy_per_sec", "20", 2, 1),
    ("ratelimit_legacy_burst", "60", 2, 1),
    ("welcome_featured", "", 0, 1),
    ("welcome_ticker", "", 0, 1),
    ("theme_accent", "", 0, 1),
    ("theme_logo_ansi", "", 0, 1),
    ("theme_name", "", 0, 1),
    ("theme_banner", "", 0, 1),
    ("theme_applied_at_unix", "0", 2, 8),
    ("theme_applied_by", "", 0, 8),
];

/// The values a dropdown offers. (A snapshot row has no room for a list, and
/// there are only two lists.)
pub fn demo_choices(key: &str) -> &'static [&'static str] {
    if key == "registration_mode" {
        &["open", "invite", "closed"]
    } else if key.ends_with("_min_role") {
        &["guest", "user", "moderator", "admin"]
    } else {
        &[]
    }
}

/// The demo burrow describing itself: the snapshot, with `held` laid over the
/// defaults.
pub fn describe(held: &[(String, String)]) -> Vec<ConfigKeyInfo> {
    DEMO_SCHEMA
        .iter()
        .map(|&(key, default, kind, flags)| {
            let value = held
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or(default);
            let mut info = ConfigKeyInfo::new(key, value, default)
                .kind(kind)
                .flags(flags);
            if kind == config_kind::CHOICE {
                info = info.choices(demo_choices(key).iter().copied());
            }
            info
        })
        .collect()
}

/// What the demo burrow's surfaces are "doing": each one that is switched on
/// is listening on its configured address, as a healthy burrow's would be.
pub fn surfaces(held: &[(String, String)]) -> Vec<rabbithole_proto::admin::SurfaceInfo> {
    use rabbithole_proto::admin::{surface_state, SurfaceInfo};
    let value = |key: &str| -> String {
        held.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .or_else(|| {
                DEMO_SCHEMA
                    .iter()
                    .find(|(k, ..)| *k == key)
                    .map(|(_, d, ..)| (*d).to_string())
            })
            .unwrap_or_default()
    };
    DEMO_SCHEMA
        .iter()
        .map(|(key, ..)| *key)
        .filter(|key| key.ends_with("_enabled"))
        .filter_map(|key| {
            let stem = key.trim_end_matches("_enabled");
            let addr = value(&format!("{stem}_addr"));
            let on = value(key) == "true";
            match (stem, addr.is_empty()) {
                ("syndication", _) => Some(if on {
                    SurfaceInfo::new(key, surface_state::RUNNING)
                } else {
                    SurfaceInfo::new(key, surface_state::OFF)
                }),
                // Only surfaces with an address of their own are supervised.
                (_, true) | ("federation", _) => None,
                _ if on => Some(SurfaceInfo::new(key, surface_state::LISTENING).addr(addr)),
                _ => Some(SurfaceInfo::new(key, surface_state::OFF)),
            }
        })
        .collect()
}

/// Whether a change to `key` applies without a restart, as the snapshot has it.
pub fn applies_live(key: &str) -> Option<bool> {
    DEMO_SCHEMA
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|(_, _, _, flags)| flags & rabbithole_proto::admin::config_flag::LIVE != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::admin::config_flag;
    use rabbithole_server_core::config::KeyKind;

    /// What a real burrow says, as rows of this file's table.
    fn fresh() -> Vec<(String, String, u8, u8)> {
        rabbithole_server_core::ServerConfig::default()
            .describe()
            .into_iter()
            .map(|i| {
                let kind = match i.kind {
                    KeyKind::Text => config_kind::TEXT,
                    KeyKind::Bool => config_kind::BOOL,
                    KeyKind::Number => config_kind::NUMBER,
                    KeyKind::Choice => config_kind::CHOICE,
                };
                let mut flags = 0;
                if i.applies_live {
                    flags |= config_flag::LIVE;
                }
                if i.secret {
                    flags |= config_flag::SECRET;
                }
                if i.read_only {
                    flags |= config_flag::READ_ONLY;
                }
                (i.key.to_string(), i.default, kind, flags)
            })
            .collect()
    }

    #[test]
    fn the_snapshot_is_what_a_real_burrow_says() {
        let fresh = fresh();
        let held: Vec<(String, String, u8, u8)> = DEMO_SCHEMA
            .iter()
            .map(|&(k, d, kind, f)| (k.to_string(), d.to_string(), kind, f))
            .collect();
        if fresh != held {
            let table: String = fresh
                .iter()
                .map(|(k, d, kind, f)| format!("    ({k:?}, {d:?}, {kind}, {f}),\n"))
                .collect();
            panic!("DEMO_SCHEMA is stale. Replace its rows with:\n{table}");
        }
        for i in rabbithole_server_core::ServerConfig::default().describe() {
            assert_eq!(demo_choices(i.key), i.choices, "{}", i.key);
        }
    }

    #[test]
    fn held_values_lie_over_the_defaults() {
        let held = vec![("name".to_string(), "Rabbit Lobby".to_string())];
        let described = describe(&held);
        let name = described.iter().find(|e| e.key == "name").unwrap();
        assert_eq!(name.value, "Rabbit Lobby");
        assert_eq!(name.default, "An Unnamed Burrow");
        let mode = described
            .iter()
            .find(|e| e.key == "registration_mode")
            .unwrap();
        assert_eq!(mode.choices, ["open", "invite", "closed"]);
        assert_eq!(applies_live("name"), Some(true));
        assert_eq!(applies_live("quic_addr"), Some(false));
        assert_eq!(applies_live("nonsense"), None);
    }
}
