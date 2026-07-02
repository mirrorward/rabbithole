//! Directory of stations: create/remove/list, enable toggle, listener counts.
//!
//! The [`StationRegistry`] is the station *directory* — the thing a UI lists,
//! an admin toggles on and off, and a mount lookup resolves against. It is
//! deliberately decoupled from audio: an entry is configuration plus presence
//! (who's tuned in), not a broadcast hub. Wiring an entry to an audio
//! [`Station`](rabbithole_audio::Station) via a
//! [`StationController`](crate::StationController) is a separate concern, which
//! keeps this type pure, cheap, and easy to reason about under a lock.
//!
//! # Slugs and mounts
//!
//! Each station has a `slug` (its key, e.g. `"wrbt"`). Its `mount` is the slug
//! with a leading slash (`"/wrbt"`), matching the ICY/HTTP convention later
//! transports will expose. [`StationRegistry::get_by_mount`] accepts either
//! form.
//!
//! # Thread safety
//!
//! The whole table lives behind a single [`parking_lot::Mutex`], so the
//! registry is `Send + Sync` and can be shared across connection handlers.

use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;

use crate::error::RadioError;

/// One station's directory entry (config + live presence).
#[derive(Debug, Default)]
struct StationEntry {
    display_name: String,
    description: String,
    enabled: bool,
    /// Distinct listener ids currently tuned in (set => idempotent join/leave).
    listeners: HashSet<String>,
}

/// A public, point-in-time snapshot of a station's directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StationInfo {
    /// Directory key, e.g. `"wrbt"`.
    pub slug: String,
    /// Mount path, e.g. `"/wrbt"`.
    pub mount: String,
    /// Human-readable station name.
    pub display_name: String,
    /// Longer description for listings.
    pub description: String,
    /// Whether the station is currently broadcasting/available.
    pub enabled: bool,
    /// Number of distinct listeners currently tuned in.
    pub listener_count: usize,
}

/// Configuration for creating a station.
#[derive(Clone, Debug, Default)]
pub struct StationConfig {
    /// Directory key / mount slug.
    pub slug: String,
    /// Human-readable station name.
    pub display_name: String,
    /// Longer description for listings.
    pub description: String,
    /// Whether the station starts enabled.
    pub enabled: bool,
}

/// Thread-safe directory of named stations.
#[derive(Debug, Default)]
pub struct StationRegistry {
    stations: Mutex<HashMap<String, StationEntry>>,
}

/// Normalizes a slug-or-mount into a bare slug (strips a single leading `/`).
fn slug_of(key: &str) -> &str {
    key.strip_prefix('/').unwrap_or(key)
}

/// Builds the mount path for a slug.
fn mount_of(slug: &str) -> String {
    format!("/{slug}")
}

impl StationRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new station.
    ///
    /// Returns [`RadioError::StationExists`] if the slug is already taken.
    pub fn create(&self, config: StationConfig) -> Result<(), RadioError> {
        let mut stations = self.stations.lock();
        if stations.contains_key(&config.slug) {
            return Err(RadioError::StationExists(config.slug));
        }
        stations.insert(
            config.slug,
            StationEntry {
                display_name: config.display_name,
                description: config.description,
                enabled: config.enabled,
                listeners: HashSet::new(),
            },
        );
        Ok(())
    }

    /// Removes a station by slug (or mount). Returns whether one was removed.
    pub fn remove(&self, slug: &str) -> bool {
        self.stations.lock().remove(slug_of(slug)).is_some()
    }

    /// Whether a station with this slug (or mount) is registered.
    pub fn contains(&self, slug: &str) -> bool {
        self.stations.lock().contains_key(slug_of(slug))
    }

    /// Snapshot of one station by slug (or mount).
    pub fn get(&self, slug: &str) -> Option<StationInfo> {
        let stations = self.stations.lock();
        let slug = slug_of(slug);
        stations.get(slug).map(|e| snapshot(slug, e))
    }

    /// Snapshot of one station by mount path (`"/wrbt"`) or bare slug.
    pub fn get_by_mount(&self, mount: &str) -> Option<StationInfo> {
        self.get(mount)
    }

    /// Snapshots of all stations, sorted by slug for deterministic ordering.
    pub fn list(&self) -> Vec<StationInfo> {
        let stations = self.stations.lock();
        let mut out: Vec<StationInfo> =
            stations.iter().map(|(slug, e)| snapshot(slug, e)).collect();
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        out
    }

    /// Enables or disables a station. Returns [`RadioError::StationNotFound`]
    /// if it is not registered.
    pub fn set_enabled(&self, slug: &str, enabled: bool) -> Result<(), RadioError> {
        let mut stations = self.stations.lock();
        let entry = stations
            .get_mut(slug_of(slug))
            .ok_or_else(|| RadioError::StationNotFound(slug.to_string()))?;
        entry.enabled = enabled;
        Ok(())
    }

    /// Whether a station is enabled (`None` if it is not registered).
    pub fn is_enabled(&self, slug: &str) -> Option<bool> {
        self.stations.lock().get(slug_of(slug)).map(|e| e.enabled)
    }

    /// Records a listener joining a station; returns the new listener count.
    ///
    /// Idempotent: the same listener id joining twice counts once. Returns
    /// [`RadioError::StationNotFound`] if the station is not registered.
    pub fn join(&self, slug: &str, listener: impl Into<String>) -> Result<usize, RadioError> {
        let mut stations = self.stations.lock();
        let entry = stations
            .get_mut(slug_of(slug))
            .ok_or_else(|| RadioError::StationNotFound(slug.to_string()))?;
        entry.listeners.insert(listener.into());
        Ok(entry.listeners.len())
    }

    /// Records a listener leaving a station; returns the new listener count.
    ///
    /// Idempotent: leaving when not present is a no-op. Returns
    /// [`RadioError::StationNotFound`] if the station is not registered.
    pub fn leave(&self, slug: &str, listener: &str) -> Result<usize, RadioError> {
        let mut stations = self.stations.lock();
        let entry = stations
            .get_mut(slug_of(slug))
            .ok_or_else(|| RadioError::StationNotFound(slug.to_string()))?;
        entry.listeners.remove(listener);
        Ok(entry.listeners.len())
    }

    /// Current listener count for a station (`None` if not registered).
    pub fn listener_count(&self, slug: &str) -> Option<usize> {
        self.stations
            .lock()
            .get(slug_of(slug))
            .map(|e| e.listeners.len())
    }
}

/// Builds a public snapshot from an internal entry.
fn snapshot(slug: &str, entry: &StationEntry) -> StationInfo {
    StationInfo {
        slug: slug.to_string(),
        mount: mount_of(slug),
        display_name: entry.display_name.clone(),
        description: entry.description.clone(),
        enabled: entry.enabled,
        listener_count: entry.listeners.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(slug: &str) -> StationConfig {
        StationConfig {
            slug: slug.to_string(),
            display_name: format!("{slug} FM"),
            description: "test station".to_string(),
            enabled: true,
        }
    }

    #[test]
    fn create_list_and_remove() {
        let reg = StationRegistry::new();
        reg.create(config("wrbt")).unwrap();
        reg.create(config("kbun")).unwrap();

        let list = reg.list();
        assert_eq!(list.len(), 2);
        // Sorted by slug.
        assert_eq!(list[0].slug, "kbun");
        assert_eq!(list[1].slug, "wrbt");
        assert_eq!(list[1].mount, "/wrbt");

        assert!(reg.remove("wrbt"));
        assert!(!reg.remove("wrbt"));
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn duplicate_create_errors() {
        let reg = StationRegistry::new();
        reg.create(config("wrbt")).unwrap();
        assert_eq!(
            reg.create(config("wrbt")).unwrap_err(),
            RadioError::StationExists("wrbt".to_string())
        );
    }

    #[test]
    fn lookup_by_slug_or_mount() {
        let reg = StationRegistry::new();
        reg.create(config("wrbt")).unwrap();
        assert!(reg.get("wrbt").is_some());
        assert!(reg.get_by_mount("/wrbt").is_some());
        assert!(reg.get("/wrbt").is_some());
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn enable_toggle() {
        let reg = StationRegistry::new();
        reg.create(config("wrbt")).unwrap();
        assert_eq!(reg.is_enabled("wrbt"), Some(true));
        reg.set_enabled("wrbt", false).unwrap();
        assert_eq!(reg.is_enabled("wrbt"), Some(false));
        assert_eq!(
            reg.set_enabled("nope", true).unwrap_err(),
            RadioError::StationNotFound("nope".to_string())
        );
    }

    #[test]
    fn listener_accounting_is_idempotent() {
        let reg = StationRegistry::new();
        reg.create(config("wrbt")).unwrap();
        assert_eq!(reg.join("wrbt", "alice").unwrap(), 1);
        assert_eq!(reg.join("wrbt", "alice").unwrap(), 1); // idempotent
        assert_eq!(reg.join("wrbt", "bob").unwrap(), 2);
        assert_eq!(reg.listener_count("wrbt"), Some(2));
        assert_eq!(reg.leave("wrbt", "alice").unwrap(), 1);
        assert_eq!(reg.leave("wrbt", "ghost").unwrap(), 1); // idempotent
        assert_eq!(reg.listener_count("wrbt"), Some(1));
    }

    #[test]
    fn listener_ops_on_missing_station_error() {
        let reg = StationRegistry::new();
        assert!(reg.join("nope", "alice").is_err());
        assert!(reg.leave("nope", "alice").is_err());
        assert_eq!(reg.listener_count("nope"), None);
    }
}
