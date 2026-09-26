//! Device defaults and account-scoped choices for signed burrow themes.
//! The current protocol syncs Off versus enabled; Minimal remains a local cap.

use crate::server_theme::ServerOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Full,
    Minimal,
    Off,
}
impl ThemeMode {
    pub const ALL: [Self; 3] = [Self::Full, Self::Minimal, Self::Off];
    pub fn key(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Minimal => "minimal",
            Self::Off => "off",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Minimal => "Minimal",
            Self::Off => "Off",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == value)
    }
    pub fn overlay(self, server: &ServerOverlay) -> Option<ServerOverlay> {
        match self {
            Self::Off => None,
            Self::Full => Some(server.clone()),
            Self::Minimal => Some(ServerOverlay {
                shared: Default::default(),
                ..server.clone()
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    endpoint: String,
    login: String,
}
impl Owner {
    /// Only a known original login can own a persistent choice. A display name
    /// is not an account identifier: two accounts can use the same persona.
    pub fn new(endpoint: &str, login: &str) -> Option<Self> {
        let login = login.trim().to_ascii_lowercase();
        if login.is_empty() {
            return None;
        }
        Some(Self {
            endpoint: crate::bookmarks::credential_endpoint(endpoint)?,
            login,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Override {
    owner: Owner,
    mode: ThemeMode,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemePreferences {
    pub default: ThemeMode,
    overrides: Vec<Override>,
}
impl ThemePreferences {
    pub fn load(raw: Option<&str>, legacy_disabled: bool) -> Self {
        raw.and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_else(|| Self {
                default: if legacy_disabled {
                    ThemeMode::Off
                } else {
                    ThemeMode::Full
                },
                ..Self::default()
            })
    }
    pub fn choice(&self, owner: &Owner) -> Option<ThemeMode> {
        self.overrides
            .iter()
            .find(|entry| &entry.owner == owner)
            .map(|entry| entry.mode)
    }
    pub fn set(&mut self, owner: Owner, mode: Option<ThemeMode>) {
        self.overrides.retain(|entry| entry.owner != owner);
        if let Some(mode) = mode {
            self.overrides.push(Override { owner, mode });
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyncState {
    #[default]
    Local,
    Checking,
    Loading,
    Saved,
    Unavailable,
}
impl SyncState {
    pub fn message(self) -> &'static str {
        match self {
        Self::Local => "Your choice applies on this device.",
        Self::Checking => "Checking your account preference…",
        Self::Loading => "Saving your account preference…",
        Self::Saved => "On or off is saved to your account. Minimal is a choice on this device.",
        Self::Unavailable => "The burrow could not save this preference. Your choice still applies on this device; changing it retries.",
    }
    }
}

#[cfg(target_arch = "wasm32")]
pub mod storage {
    use super::*;
    pub const KEY: &str = "rh.theme.preferences.v1";
    pub fn load() -> ThemePreferences {
        let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
        let raw = storage
            .as_ref()
            .and_then(|s| s.get_item(KEY).ok().flatten());
        let legacy = crate::server_theme::storage::load_disabled()
            || !crate::settings::storage::load().appearance.use_burrow_theme;
        ThemePreferences::load(raw.as_deref(), legacy)
    }
    pub fn save(prefs: &ThemePreferences) -> bool {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|storage| {
                serde_json::to_string(prefs)
                    .ok()
                    .map(|raw| storage.set_item(KEY, &raw).is_ok())
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_to_original_login_and_canonical_endpoint() {
        let alice = Owner::new("ws://LOCALHOST:4654/", "Alice").unwrap();
        let bob = Owner::new("ws://localhost:4654", "bob").unwrap();
        let elsewhere = Owner::new("ws://localhost:4655", "alice").unwrap();
        let mut prefs = ThemePreferences::default();
        prefs.set(alice.clone(), Some(ThemeMode::Minimal));
        assert_eq!(
            prefs.choice(&Owner::new("ws://localhost:4654", "ALICE").unwrap()),
            Some(ThemeMode::Minimal)
        );
        assert_eq!(prefs.choice(&bob), None);
        assert_eq!(prefs.choice(&elsewhere), None);
        prefs.set(alice.clone(), None);
        assert_eq!(prefs.choice(&alice), None);
        assert_eq!(Owner::new("ws://localhost:4654", ""), None);
        // SQLite accounts use NOCASE: ASCII folds, Unicode does not.
        assert_ne!(
            Owner::new("ws://localhost:4654", "K"),
            Owner::new("ws://localhost:4654", "K")
        );
        assert_ne!(
            Owner::new("ws://localhost:4654", "Ä"),
            Owner::new("ws://localhost:4654", "ä")
        );
    }
    #[test]
    fn legacy_opt_out_is_preserved_until_new_preferences_exist() {
        assert_eq!(ThemePreferences::load(None, true).default, ThemeMode::Off);
        assert_eq!(
            ThemePreferences::load(Some("garbage"), true).default,
            ThemeMode::Off
        );
        let mut prefs = ThemePreferences {
            default: ThemeMode::Minimal,
            ..Default::default()
        };
        let owner = Owner::new("ws://localhost:4654", "alice").unwrap();
        prefs.set(owner.clone(), Some(ThemeMode::Off));
        let loaded = ThemePreferences::load(Some(&serde_json::to_string(&prefs).unwrap()), true);
        assert_eq!(loaded, prefs);
        assert_eq!(loaded.choice(&owner), Some(ThemeMode::Off));
    }
    #[test]
    fn minimal_accepts_colors_but_keeps_all_local_metrics() {
        let overlay = ServerOverlay {
            name: "Test".into(),
            light: [("--rh-accent".into(), "#a34700".into())].into(),
            shared: [("--rh-radius".into(), "0".into())].into(),
            ..Default::default()
        };
        let minimal = ThemeMode::Minimal.overlay(&overlay).unwrap();
        assert_eq!(minimal.light, overlay.light);
        assert!(minimal.shared.is_empty());
        assert!(ThemeMode::Off.overlay(&overlay).is_none());
        assert_eq!(ThemeMode::Full.overlay(&overlay), Some(overlay));
    }
}
