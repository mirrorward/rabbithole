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
use serde::{Deserialize, Serialize};

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
