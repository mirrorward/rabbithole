//! Server configuration: TOML file + environment overrides, with a shared
//! live handle for the fields that hot-reload safely.
//!
//! Precedence: defaults < TOML file < `RABBITHOLE_*` environment variables
//! < runtime `ctl config set` edits. Listener addresses require a restart;
//! identity/text fields (name, MOTD, agreement, guest policy) apply live.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use rabbithole_legacy_doors::DoorDef;
use serde::{Deserialize, Serialize};

/// A configured server-to-server federation dial target (Wave 9). Entries are
/// implicitly admin-approved on this side (we chose to dial them); the peer
/// still approves *us* before a session is established.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FederationPeer {
    /// Human-readable label for logs/status.
    pub name: String,
    /// `host:port` to dial (the peer's `federation_addr`).
    pub addr: String,
    /// TLS SNI / certificate name to expect (default "localhost" for
    /// self-signed burrows).
    pub server_name: String,
    /// The peer's expected Ed25519 server key, hex-encoded (32 bytes). Empty
    /// = accept whatever the peer presents (still fingerprint-pinned).
    pub key: String,
    /// The peer's pinned TLS certificate blake3 fingerprint, hex-encoded.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Display name of this burrow.
    pub name: String,
    /// Message of the day, shown in the welcome push.
    pub motd: String,
    /// Agreement text users must accept before participating
    /// (empty = no agreement gate).
    pub agreement: String,
    /// Whether guests may sign in.
    pub guest_enabled: bool,
    /// QUIC listener (primary transport).
    pub quic_addr: SocketAddr,
    /// WebSocket listener (fallback transport).
    pub ws_addr: SocketAddr,
    /// Where the database, blobs, keys, and ctl socket live.
    pub data_dir: PathBuf,
    /// Session token lifetime in seconds.
    pub session_ttl_secs: i64,
    /// Maximum chat line length in bytes.
    pub chat_max_len: usize,
    /// Registration policy: "open", "invite", or "closed".
    pub registration_mode: String,
    /// Maximum personas per account.
    pub persona_max: u32,
    /// Size caps for profile art blobs, in bytes.
    pub avatar_max_bytes: usize,
    pub banner_max_bytes: usize,
    /// Per-account file-library upload quota in bytes (0 = unlimited).
    pub upload_quota_bytes: u64,
    /// Max simultaneous in-flight transfers per account (0 = unlimited).
    pub max_concurrent_transfers: u32,
    /// Per-transfer download bandwidth cap in bytes/sec (0 = unlimited).
    pub transfer_rate_bytes_per_sec: u64,
    /// Max TTL granted to a swarm advertisement, in seconds.
    pub swarm_advert_ttl_secs: u32,
    /// Max live swarm advertisements per account (0 = unlimited).
    pub swarm_adverts_max: u32,
    /// Cap on disk used by unreferenced (cache/swarm) blobs, in bytes; the
    /// maintenance sweep evicts oldest-first over this. 0 = unlimited
    /// ("mirror": keep everything the server has ever held).
    pub swarm_cache_max_bytes: u64,
    /// Serve the legacy telnet BBS surface on `telnet_addr`.
    pub telnet_enabled: bool,
    /// Telnet listener address (default 0.0.0.0:2323 — 23 needs privilege).
    pub telnet_addr: SocketAddr,
    /// Serve the finger surface (RFC 1288) on `finger_addr`.
    pub finger_enabled: bool,
    /// Finger listener address (default 0.0.0.0:7979 — 79 needs privilege).
    pub finger_addr: SocketAddr,
    /// Serve the legacy NNTP reader/poster surface (RFC 3977) on `nntp_addr`.
    pub nntp_enabled: bool,
    /// NNTP listener address (default 0.0.0.0:1119 — 119 needs privilege).
    pub nntp_addr: SocketAddr,
    /// Serve the Icecast-compatible radio delivery surface on `radio_addr`.
    pub radio_enabled: bool,
    /// Radio listener address (default 0.0.0.0:8000 — the Icecast convention).
    pub radio_addr: SocketAddr,
    /// Accept inbound DJ source connections (SOURCE/PUT) on `radio_source_addr`.
    /// This is the *ingest* surface (a DJ pushing a live stream), distinct from
    /// the `radio_addr` *delivery* surface (players pulling). Off by default.
    pub radio_source_enabled: bool,
    /// DJ source ingest listener address (default 0.0.0.0:8001 — beside the
    /// Icecast delivery port 8000).
    pub radio_source_addr: SocketAddr,
    /// Username a DJ source must present (HTTP Basic). Default "source", the
    /// Icecast convention.
    pub radio_source_user: String,
    /// Password a DJ source must present (HTTP Basic). Empty refuses every
    /// source connection (fail safe: no blank-password broadcasting, and
    /// guests are always refused).
    pub radio_source_password: String,
    /// Library playlist sources: station mount slug -> file-area slug. Each
    /// entry pulls that area's audio files into the station's rotation via the
    /// playlist engine. Serialized as a TOML table and edited on disk (like
    /// `ftn_areas`), not via `ctl config set`.
    pub radio_library_areas: std::collections::HashMap<String, String>,
    /// Host classic door games on the telnet BBS surface. Off by default;
    /// requires `telnet_enabled` and at least one `[[doors]]` entry to do
    /// anything.
    pub doors_enabled: bool,
    /// Working root for door sessions: per-node drop directories
    /// (`node1/`, `node2/`, …) are created under it and hold the drop files.
    /// Relative paths resolve under `data_dir`.
    pub doors_dir: PathBuf,
    /// How many door nodes (simultaneous door sessions) the shared pool
    /// offers. `0` refuses every door launch.
    pub doors_max_nodes: u16,
    /// Wall-clock cap on one door session, in seconds (`0` = unlimited).
    /// A door's own `daily_limit_mins` lowers it further; whichever budget
    /// is smaller wins.
    pub doors_session_max_secs: u64,
    /// Installed door games, a TOML `[[doors]]` array of tables (see
    /// `rabbithole-legacy-doors` for the field reference). Edited on disk
    /// (like `ftn_areas`), not via `ctl config set`.
    pub doors: Vec<DoorDef>,
    /// Serve the Hotline-compatible surface on `hotline_addr`.
    pub hotline_enabled: bool,
    /// Hotline listener address (default 0.0.0.0:5500 — the classic Hotline port).
    pub hotline_addr: SocketAddr,
    /// Serve the FidoNet (FTN) binkp mailer gateway on `ftn_addr`.
    pub ftn_enabled: bool,
    /// binkp listener address (default 0.0.0.0:24554 — the IANA binkp port).
    pub ftn_addr: SocketAddr,
    /// This system's FTN node address (e.g. "2:280/464"). Empty disables
    /// tossing/scanning even when the listener is up.
    pub ftn_node: String,
    /// Uplink/boss FTN node address for outbound mail (e.g. "2:280/1").
    pub ftn_uplink: String,
    /// Uplink binkp host:port to dial for outbound polls
    /// (e.g. "hub.example.org:24554").
    pub ftn_uplink_host: String,
    /// binkp session password shared with the uplink ("" or "-" = unsecured).
    pub ftn_password: String,
    /// Inbound spool directory for received PKT/bundle files. Relative paths
    /// resolve under `data_dir`.
    pub ftn_inbound_dir: PathBuf,
    /// Outbound Binkley-Style Outbound (BSO) directory for staged PKT files.
    /// Relative paths resolve under `data_dir`.
    pub ftn_outbound_dir: PathBuf,
    /// Echomail AREA tag → board slug map, driving the echomail↔board gateway
    /// in both directions.
    pub ftn_areas: std::collections::HashMap<String, String>,
    /// Poll the configured `syndication_feeds` and post fresh items to their
    /// mapped boards (Wave 10). Off by default.
    pub syndication_enabled: bool,
    /// Feed URL → board slug map, driving the RSS/Atom → board ingest.
    /// Serialized as a TOML table and edited on disk (like `ftn_areas`), not
    /// via `ctl config set`.
    pub syndication_feeds: std::collections::HashMap<String, String>,
    /// Base interval between feed polls, in seconds. Per-feed error backoff
    /// and feed-declared TTLs stretch it; a politeness floor caps how low it
    /// can effectively go.
    pub syndication_poll_secs: i64,
    /// Serve the server-to-server (S2S) federation peering surface on
    /// `federation_addr` and dial `federation_peers` (Wave 9). Off by default.
    pub federation_enabled: bool,
    /// Federation S2S listener address (default 0.0.0.0:4655 — alongside the
    /// QUIC 4653 / WebSocket 4654 client transports).
    pub federation_addr: SocketAddr,
    /// Configured peer dial targets. Serialized as an array of tables in TOML;
    /// edited on disk (not via `ctl config set`), like `ftn_areas`.
    pub federation_peers: Vec<FederationPeer>,
    /// Welcome-screen featured block (title on first line, body after).
    pub welcome_featured: String,
    /// Welcome-screen one-line ticker.
    pub welcome_ticker: String,
    /// Theme accent color as hex "RRGGBB" (empty = none).
    pub theme_accent: String,
    /// Theme ANSI logo art (also the future telnet banner).
    pub theme_logo_ansi: String,
    /// Keyword teleport map: word → "room:<name>" | "user:<name>" | "url:<…>".
    pub keywords: std::collections::HashMap<String, String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "An Unnamed Burrow".into(),
            motd: String::new(),
            agreement: String::new(),
            guest_enabled: true,
            quic_addr: "0.0.0.0:4653".parse().expect("valid"),
            ws_addr: "0.0.0.0:4654".parse().expect("valid"),
            data_dir: PathBuf::from("./burrow-data"),
            session_ttl_secs: 60 * 60 * 24 * 30, // 30 days
            chat_max_len: 4096,
            registration_mode: "open".into(),
            persona_max: 5,
            avatar_max_bytes: 256 * 1024,
            banner_max_bytes: 1024 * 1024,
            upload_quota_bytes: 0,
            max_concurrent_transfers: 0,
            transfer_rate_bytes_per_sec: 0,
            swarm_advert_ttl_secs: 3600,
            swarm_adverts_max: 4096,
            swarm_cache_max_bytes: 0,
            telnet_enabled: false,
            telnet_addr: "0.0.0.0:2323".parse().expect("valid"),
            finger_enabled: false,
            finger_addr: "0.0.0.0:7979".parse().expect("valid"),
            nntp_enabled: false,
            nntp_addr: "0.0.0.0:1119".parse().expect("valid"),
            radio_enabled: false,
            radio_addr: "0.0.0.0:8000".parse().expect("valid"),
            radio_source_enabled: false,
            radio_source_addr: "0.0.0.0:8001".parse().expect("valid"),
            radio_source_user: "source".into(),
            radio_source_password: String::new(),
            radio_library_areas: std::collections::HashMap::new(),
            doors_enabled: false,
            doors_dir: PathBuf::from("doors"),
            doors_max_nodes: 4,
            doors_session_max_secs: 3600,
            doors: Vec::new(),
            hotline_enabled: false,
            hotline_addr: "0.0.0.0:5500".parse().expect("valid"),
            ftn_enabled: false,
            ftn_addr: "0.0.0.0:24554".parse().expect("valid"),
            ftn_node: String::new(),
            ftn_uplink: String::new(),
            ftn_uplink_host: String::new(),
            ftn_password: String::new(),
            ftn_inbound_dir: PathBuf::from("ftn/inbound"),
            ftn_outbound_dir: PathBuf::from("ftn/outbound"),
            ftn_areas: std::collections::HashMap::new(),
            syndication_enabled: false,
            syndication_feeds: std::collections::HashMap::new(),
            syndication_poll_secs: 1800,
            federation_enabled: false,
            federation_addr: "0.0.0.0:4655".parse().expect("valid"),
            federation_peers: Vec::new(),
            welcome_featured: String::new(),
            welcome_ticker: String::new(),
            theme_accent: String::new(),
            theme_logo_ansi: String::new(),
            keywords: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("bad value for {key}: {detail}")]
    BadValue { key: String, detail: String },
    #[error("unknown config key: {0}")]
    UnknownKey(String),
}

impl ServerConfig {
    /// Load from a TOML file (missing file = defaults), then apply
    /// `RABBITHOLE_*` environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut cfg = match path {
            Some(p) if p.exists() => toml::from_str(&std::fs::read_to_string(p)?)?,
            _ => ServerConfig::default(),
        };
        cfg.apply_env(|k| std::env::var(k).ok())?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Apply environment overrides through an injectable getter (testable).
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        if let Some(v) = get("RABBITHOLE_NAME") {
            self.name = v;
        }
        if let Some(v) = get("RABBITHOLE_MOTD") {
            self.motd = v;
        }
        if let Some(v) = get("RABBITHOLE_AGREEMENT") {
            self.agreement = v;
        }
        if let Some(v) = get("RABBITHOLE_GUEST_ENABLED") {
            self.guest_enabled = parse_bool("RABBITHOLE_GUEST_ENABLED", &v)?;
        }
        if let Some(v) = get("RABBITHOLE_QUIC_ADDR") {
            self.quic_addr = parse_addr("RABBITHOLE_QUIC_ADDR", &v)?;
        }
        if let Some(v) = get("RABBITHOLE_WS_ADDR") {
            self.ws_addr = parse_addr("RABBITHOLE_WS_ADDR", &v)?;
        }
        if let Some(v) = get("RABBITHOLE_DATA_DIR") {
            self.data_dir = PathBuf::from(v);
        }
        Ok(())
    }

    /// Runtime get by dotted key (for `ctl config get`).
    pub fn get_key(&self, key: &str) -> Result<String, ConfigError> {
        Ok(match key {
            "name" => self.name.clone(),
            "motd" => self.motd.clone(),
            "agreement" => self.agreement.clone(),
            "guest_enabled" => self.guest_enabled.to_string(),
            "quic_addr" => self.quic_addr.to_string(),
            "ws_addr" => self.ws_addr.to_string(),
            "data_dir" => self.data_dir.display().to_string(),
            "session_ttl_secs" => self.session_ttl_secs.to_string(),
            "chat_max_len" => self.chat_max_len.to_string(),
            "registration_mode" => self.registration_mode.clone(),
            "persona_max" => self.persona_max.to_string(),
            "avatar_max_bytes" => self.avatar_max_bytes.to_string(),
            "banner_max_bytes" => self.banner_max_bytes.to_string(),
            "upload_quota_bytes" => self.upload_quota_bytes.to_string(),
            "max_concurrent_transfers" => self.max_concurrent_transfers.to_string(),
            "transfer_rate_bytes_per_sec" => self.transfer_rate_bytes_per_sec.to_string(),
            "swarm_advert_ttl_secs" => self.swarm_advert_ttl_secs.to_string(),
            "swarm_adverts_max" => self.swarm_adverts_max.to_string(),
            "swarm_cache_max_bytes" => self.swarm_cache_max_bytes.to_string(),
            "telnet_enabled" => self.telnet_enabled.to_string(),
            "telnet_addr" => self.telnet_addr.to_string(),
            "finger_enabled" => self.finger_enabled.to_string(),
            "finger_addr" => self.finger_addr.to_string(),
            "nntp_enabled" => self.nntp_enabled.to_string(),
            "nntp_addr" => self.nntp_addr.to_string(),
            "radio_enabled" => self.radio_enabled.to_string(),
            "radio_addr" => self.radio_addr.to_string(),
            "radio_source_enabled" => self.radio_source_enabled.to_string(),
            "radio_source_addr" => self.radio_source_addr.to_string(),
            "radio_source_user" => self.radio_source_user.clone(),
            "radio_source_password" => self.radio_source_password.clone(),
            "doors_enabled" => self.doors_enabled.to_string(),
            "doors_dir" => self.doors_dir.display().to_string(),
            "doors_max_nodes" => self.doors_max_nodes.to_string(),
            "doors_session_max_secs" => self.doors_session_max_secs.to_string(),
            "hotline_enabled" => self.hotline_enabled.to_string(),
            "hotline_addr" => self.hotline_addr.to_string(),
            "ftn_enabled" => self.ftn_enabled.to_string(),
            "ftn_addr" => self.ftn_addr.to_string(),
            "ftn_node" => self.ftn_node.clone(),
            "ftn_uplink" => self.ftn_uplink.clone(),
            "ftn_uplink_host" => self.ftn_uplink_host.clone(),
            "ftn_password" => self.ftn_password.clone(),
            "ftn_inbound_dir" => self.ftn_inbound_dir.display().to_string(),
            "ftn_outbound_dir" => self.ftn_outbound_dir.display().to_string(),
            "syndication_enabled" => self.syndication_enabled.to_string(),
            "syndication_poll_secs" => self.syndication_poll_secs.to_string(),
            "federation_enabled" => self.federation_enabled.to_string(),
            "federation_addr" => self.federation_addr.to_string(),
            "welcome_featured" => self.welcome_featured.clone(),
            "welcome_ticker" => self.welcome_ticker.clone(),
            "theme_accent" => self.theme_accent.clone(),
            "theme_logo_ansi" => self.theme_logo_ansi.clone(),
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        })
    }

    /// Runtime set by key. Returns whether the change applies live
    /// (`true`) or needs a restart (`false`).
    pub fn set_key(&mut self, key: &str, value: &str) -> Result<bool, ConfigError> {
        match key {
            "name" => {
                self.name = value.to_string();
                Ok(true)
            }
            "motd" => {
                self.motd = value.to_string();
                Ok(true)
            }
            "agreement" => {
                self.agreement = value.to_string();
                Ok(true)
            }
            "guest_enabled" => {
                self.guest_enabled = parse_bool(key, value)?;
                Ok(true)
            }
            "session_ttl_secs" => {
                self.session_ttl_secs = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "chat_max_len" => {
                self.chat_max_len = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "welcome_featured" => {
                self.welcome_featured = value.to_string();
                Ok(true)
            }
            "welcome_ticker" => {
                self.welcome_ticker = value.to_string();
                Ok(true)
            }
            "theme_accent" => {
                let v = value.trim_start_matches('#');
                if !v.is_empty() && (v.len() != 6 || hex::decode(v).is_err()) {
                    return Err(ConfigError::BadValue {
                        key: key.into(),
                        detail: value.into(),
                    });
                }
                self.theme_accent = v.to_string();
                Ok(true)
            }
            "theme_logo_ansi" => {
                self.theme_logo_ansi = value.to_string();
                Ok(true)
            }
            "registration_mode" => {
                if !["open", "invite", "closed"].contains(&value) {
                    return Err(ConfigError::BadValue {
                        key: key.into(),
                        detail: value.into(),
                    });
                }
                self.registration_mode = value.to_string();
                Ok(true)
            }
            "persona_max" => {
                self.persona_max = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "avatar_max_bytes" => {
                self.avatar_max_bytes = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "banner_max_bytes" => {
                self.banner_max_bytes = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "upload_quota_bytes" => {
                self.upload_quota_bytes = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "max_concurrent_transfers" => {
                self.max_concurrent_transfers =
                    value.parse().map_err(|_| ConfigError::BadValue {
                        key: key.into(),
                        detail: value.into(),
                    })?;
                Ok(true)
            }
            "transfer_rate_bytes_per_sec" => {
                self.transfer_rate_bytes_per_sec =
                    value.parse().map_err(|_| ConfigError::BadValue {
                        key: key.into(),
                        detail: value.into(),
                    })?;
                Ok(true)
            }
            "swarm_advert_ttl_secs" => {
                self.swarm_advert_ttl_secs = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "swarm_adverts_max" => {
                self.swarm_adverts_max = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "swarm_cache_max_bytes" => {
                self.swarm_cache_max_bytes = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(true)
            }
            "telnet_enabled" => {
                self.telnet_enabled = parse_bool(key, value)?;
                Ok(false) // listener binds at startup
            }
            "telnet_addr" => {
                self.telnet_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "finger_enabled" => {
                self.finger_enabled = parse_bool(key, value)?;
                Ok(false)
            }
            "finger_addr" => {
                self.finger_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "nntp_enabled" => {
                self.nntp_enabled = parse_bool(key, value)?;
                Ok(false)
            }
            "nntp_addr" => {
                self.nntp_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "radio_enabled" => {
                self.radio_enabled = parse_bool(key, value)?;
                Ok(false)
            }
            "radio_addr" => {
                self.radio_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "radio_source_enabled" => {
                self.radio_source_enabled = parse_bool(key, value)?;
                Ok(false)
            }
            "radio_source_addr" => {
                self.radio_source_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "radio_source_user" => {
                self.radio_source_user = value.to_string();
                Ok(false)
            }
            "radio_source_password" => {
                self.radio_source_password = value.to_string();
                Ok(false)
            }
            "doors_enabled" => {
                self.doors_enabled = parse_bool(key, value)?;
                Ok(false) // the door host is assembled at startup
            }
            "doors_dir" => {
                self.doors_dir = PathBuf::from(value);
                Ok(false)
            }
            "doors_max_nodes" => {
                self.doors_max_nodes = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(false)
            }
            "doors_session_max_secs" => {
                self.doors_session_max_secs = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(false)
            }
            "hotline_enabled" => {
                self.hotline_enabled = parse_bool(key, value)?;
                Ok(false)
            }
            "hotline_addr" => {
                self.hotline_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "ftn_enabled" => {
                self.ftn_enabled = parse_bool(key, value)?;
                Ok(false)
            }
            "ftn_addr" => {
                self.ftn_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "ftn_node" => {
                self.ftn_node = value.to_string();
                Ok(false)
            }
            "ftn_uplink" => {
                self.ftn_uplink = value.to_string();
                Ok(false)
            }
            "ftn_uplink_host" => {
                self.ftn_uplink_host = value.to_string();
                Ok(false)
            }
            "ftn_password" => {
                self.ftn_password = value.to_string();
                Ok(false)
            }
            "ftn_inbound_dir" => {
                self.ftn_inbound_dir = PathBuf::from(value);
                Ok(false)
            }
            "ftn_outbound_dir" => {
                self.ftn_outbound_dir = PathBuf::from(value);
                Ok(false)
            }
            "syndication_enabled" => {
                self.syndication_enabled = parse_bool(key, value)?;
                Ok(false) // the poll task starts at boot
            }
            "syndication_poll_secs" => {
                self.syndication_poll_secs = value.parse().map_err(|_| ConfigError::BadValue {
                    key: key.into(),
                    detail: value.into(),
                })?;
                Ok(false)
            }
            "federation_enabled" => {
                self.federation_enabled = parse_bool(key, value)?;
                Ok(false) // listener binds at startup
            }
            "federation_addr" => {
                self.federation_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "quic_addr" => {
                self.quic_addr = parse_addr(key, value)?;
                Ok(false) // restart required
            }
            "ws_addr" => {
                self.ws_addr = parse_addr(key, value)?;
                Ok(false)
            }
            "data_dir" => {
                self.data_dir = PathBuf::from(value);
                Ok(false)
            }
            other => Err(ConfigError::UnknownKey(other.to_string())),
        }
    }
}

fn parse_bool(key: &str, v: &str) -> Result<bool, ConfigError> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::BadValue {
            key: key.into(),
            detail: v.into(),
        }),
    }
}

fn parse_addr(key: &str, v: &str) -> Result<SocketAddr, ConfigError> {
    v.parse().map_err(|_| ConfigError::BadValue {
        key: key.into(),
        detail: v.into(),
    })
}

/// Shared, live-mutable configuration handle.
#[derive(Clone)]
pub struct LiveConfig(Arc<RwLock<ServerConfig>>);

impl LiveConfig {
    pub fn new(cfg: ServerConfig) -> Self {
        Self(Arc::new(RwLock::new(cfg)))
    }

    pub fn read(&self) -> ServerConfig {
        self.0.read().clone()
    }

    pub fn get_key(&self, key: &str) -> Result<String, ConfigError> {
        self.0.read().get_key(key)
    }

    /// Set a key; returns whether it applied live.
    pub fn set_key(&self, key: &str, value: &str) -> Result<bool, ConfigError> {
        self.0.write().set_key(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_and_validation() {
        let mut cfg = ServerConfig::default();
        cfg.apply_env(|k| match k {
            "RABBITHOLE_NAME" => Some("Wonderland".into()),
            "RABBITHOLE_GUEST_ENABLED" => Some("off".into()),
            "RABBITHOLE_QUIC_ADDR" => Some("127.0.0.1:9999".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.name, "Wonderland");
        assert!(!cfg.guest_enabled);
        assert_eq!(cfg.quic_addr.port(), 9999);

        let bad = cfg.apply_env(|k| (k == "RABBITHOLE_GUEST_ENABLED").then(|| "maybe".into()));
        assert!(matches!(bad, Err(ConfigError::BadValue { .. })));
    }

    #[test]
    fn file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow.toml");
        let cfg = ServerConfig {
            name: "The Warren".into(),
            ..ServerConfig::default()
        };
        cfg.save(&path).unwrap();

        // No env in this test.
        let loaded = {
            let mut c: ServerConfig =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            c.apply_env(|_| None).unwrap();
            c
        };
        assert_eq!(loaded.name, "The Warren");
    }

    #[test]
    fn set_key_reports_liveness() {
        let live = LiveConfig::new(ServerConfig::default());
        assert!(live.set_key("motd", "hi").unwrap());
        assert!(!live.set_key("quic_addr", "0.0.0.0:1").unwrap());
        assert_eq!(live.get_key("motd").unwrap(), "hi");
        assert!(matches!(
            live.set_key("nope", "x"),
            Err(ConfigError::UnknownKey(_))
        ));
    }
}
