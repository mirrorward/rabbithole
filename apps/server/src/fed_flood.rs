//! Trusted origin-key state for federation board-event flood-fill.
//!
//! A federated board event is signed by its author and by its origin server.
//! Signature verification therefore needs an authenticated `origin -> key`
//! binding. A relaying peer is never allowed to create that binding: an origin
//! is trusted only after a direct, handshake-authenticated peer session or an
//! explicit local operator pin. Relays may carry events only for origins that
//! are already trusted here.
//!
//! Trusted pins persist in a versioned file at
//! `<data_dir>/federation/origin_keys.json`. The former unversioned
//! first-seen map is deliberately not imported because its entries may have
//! been established by an untrusted relay before this provenance gate existed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use parking_lot::{Mutex, RwLock};
use rabbithole_federation::is_valid_server_name;
use serde::{Deserialize, Serialize};

const MAX_ORIGINS: usize = 4096;
const PIN_FILE_VERSION: u32 = 2;

/// How an origin-key binding became trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginTrust {
    /// The key was proven on a direct, approved S2S session whose announced
    /// server name normalizes to this origin.
    DirectPeer,
    /// A local operator explicitly pinned the origin and key.
    Operator,
}

impl OriginTrust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectPeer => "direct_peer",
            Self::Operator => "operator",
        }
    }
}

/// One trusted origin binding exposed to status/tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPin {
    pub origin: String,
    pub key: [u8; 32],
    pub trust: OriginTrust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    Inserted,
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustedKey {
    key: [u8; 32],
    trust: OriginTrust,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinFile {
    version: u32,
    origins: BTreeMap<String, PersistedPin>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPin {
    key: String,
    trust: OriginTrust,
}

/// The bounded trusted origin-key registry.
pub struct FloodState {
    origins: RwLock<HashMap<String, TrustedKey>>,
    path: Option<PathBuf>,
    /// Serializes check/insert/persist so concurrent first pins cannot race or
    /// snapshot stale state. Readers never take this lock.
    persist_lock: Mutex<()>,
}

impl Default for FloodState {
    fn default() -> Self {
        Self {
            origins: RwLock::new(HashMap::new()),
            path: None,
            persist_lock: Mutex::new(()),
        }
    }
}

impl FloodState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load only versioned, provenance-bearing pins. Legacy first-seen files
    /// are ignored fail-closed and replaced only after a trusted pin is added.
    pub fn load(data_dir: &Path) -> Self {
        let path = pins_path(data_dir);
        Self {
            origins: RwLock::new(read_pins(&path)),
            path: Some(path),
            persist_lock: Mutex::new(()),
        }
    }

    pub fn resolve(&self, origin: &str) -> Option<[u8; 32]> {
        self.origins.read().get(origin).map(|pin| pin.key)
    }

    pub fn pins(&self) -> Vec<OriginPin> {
        let mut pins: Vec<_> = self
            .origins
            .read()
            .iter()
            .map(|(origin, pin)| OriginPin {
                origin: origin.clone(),
                key: pin.key,
                trust: pin.trust,
            })
            .collect();
        pins.sort_by(|a, b| a.origin.cmp(&b.origin));
        pins
    }

    pub fn trust_direct(&self, origin: &str, key: [u8; 32]) -> Result<PinOutcome> {
        self.trust(origin, key, OriginTrust::DirectPeer)
    }

    pub fn trust_operator(&self, origin: &str, key: [u8; 32]) -> Result<PinOutcome> {
        self.trust(origin, key, OriginTrust::Operator)
    }

    /// Establish a trusted binding. Conflicting origin keys and one key
    /// claiming multiple origins both fail closed; rotation/aliases require a
    /// future explicit continuity protocol rather than silent replacement.
    fn trust(&self, origin: &str, key: [u8; 32], trust: OriginTrust) -> Result<PinOutcome> {
        if !is_valid_server_name(origin) {
            bail!("origin must be a valid lowercase federation server name");
        }
        let _persist = self.persist_lock.lock();
        let mut next = self.origins.read().clone();
        if let Some(existing) = next.get(origin) {
            if existing.key == key {
                return Ok(PinOutcome::Existing);
            }
            bail!("origin {origin} is already pinned to a different key");
        }
        if let Some((claimed, _)) = next.iter().find(|(_, pin)| pin.key == key) {
            bail!("server key is already pinned to origin {claimed}");
        }
        if next.len() >= MAX_ORIGINS {
            bail!("trusted origin-key registry is full");
        }
        next.insert(origin.to_string(), TrustedKey { key, trust });

        if let Some(path) = &self.path {
            persist_pins(path, &next)
                .with_context(|| format!("persist trusted federation origin {}", path.display()))?;
        }
        // The durable snapshot is authoritative. Publish it to readers only
        // after the atomic rename succeeds.
        *self.origins.write() = next;
        Ok(PinOutcome::Inserted)
    }

    pub fn len(&self) -> usize {
        self.origins.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn pins_path(data_dir: &Path) -> PathBuf {
    data_dir.join("federation").join("origin_keys.json")
}

fn read_pins(path: &Path) -> HashMap<String, TrustedKey> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<PinFile>(&bytes) else {
        tracing::warn!(
            path = %path.display(),
            "federation: ignoring legacy or unreadable origin-key file; re-pin indirect origins explicitly"
        );
        return HashMap::new();
    };
    if file.version != PIN_FILE_VERSION {
        tracing::warn!(
            path = %path.display(),
            version = file.version,
            "federation: ignoring unsupported origin-key file version"
        );
        return HashMap::new();
    }
    if file.origins.len() > MAX_ORIGINS {
        tracing::warn!(
            path = %path.display(),
            "federation: ignoring over-cap origin-key file"
        );
        return HashMap::new();
    }

    let mut loaded = HashMap::new();
    for (origin, pin) in file.origins {
        let Some(key) = hex::decode(&pin.key)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        else {
            tracing::warn!(path = %path.display(), "federation: ignoring invalid origin-key file");
            return HashMap::new();
        };
        if !is_valid_server_name(&origin)
            || loaded
                .values()
                .any(|existing: &TrustedKey| existing.key == key)
        {
            tracing::warn!(path = %path.display(), "federation: ignoring conflicting origin-key file");
            return HashMap::new();
        }
        loaded.insert(
            origin,
            TrustedKey {
                key,
                trust: pin.trust,
            },
        );
    }
    loaded
}

fn persist_pins(path: &Path, map: &HashMap<String, TrustedKey>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let origins = map
        .iter()
        .map(|(origin, pin)| {
            (
                origin.clone(),
                PersistedPin {
                    key: hex::encode(pin.key),
                    trust: pin.trust,
                },
            )
        })
        .collect();
    let bytes = serde_json::to_vec_pretty(&PinFile {
        version: PIN_FILE_VERSION,
        origins,
    })?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_pins_are_conflict_and_alias_safe() {
        let state = FloodState::new();
        assert_eq!(
            state.trust_direct("warren-c", [1u8; 32]).unwrap(),
            PinOutcome::Inserted
        );
        assert_eq!(
            state.trust_direct("warren-c", [1u8; 32]).unwrap(),
            PinOutcome::Existing
        );
        assert!(state.trust_operator("warren-c", [2u8; 32]).is_err());
        assert!(state.trust_operator("victim", [1u8; 32]).is_err());
        assert_eq!(state.resolve("warren-c"), Some([1u8; 32]));
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn pins_persist_with_provenance_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        {
            let state = FloodState::load(dir.path());
            state.trust_direct("warren-b", [7u8; 32]).unwrap();
            state.trust_operator("warren-c", [9u8; 32]).unwrap();
        }
        let state = FloodState::load(dir.path());
        assert_eq!(state.resolve("warren-b"), Some([7u8; 32]));
        assert_eq!(state.resolve("warren-c"), Some([9u8; 32]));
        assert_eq!(state.pins()[0].trust, OriginTrust::DirectPeer);
        assert_eq!(state.pins()[1].trust, OriginTrust::Operator);
    }

    #[test]
    fn legacy_first_seen_file_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = pins_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"victim":"0101010101010101010101010101010101010101010101010101010101010101"}"#,
        )
        .unwrap();
        let state = FloodState::load(dir.path());
        assert!(state.is_empty(), "legacy relay-learned pins fail closed");
        state.trust_operator("victim", [2u8; 32]).unwrap();
        assert_eq!(
            FloodState::load(dir.path()).resolve("victim"),
            Some([2u8; 32])
        );
    }

    #[test]
    fn one_corrupt_v2_entry_rejects_the_entire_trust_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = pins_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"version":2,"origins":{"good":{"key":"0101010101010101010101010101010101010101010101010101010101010101","trust":"operator"},"bad":{"key":"not-hex","trust":"operator"}}}"#,
        )
        .unwrap();
        assert!(
            FloodState::load(dir.path()).is_empty(),
            "partially corrupt trust state must never be partially promoted"
        );
    }

    #[test]
    fn invalid_and_excess_origins_are_rejected() {
        let state = FloodState::new();
        assert!(state.trust_operator("Victim Example", [1u8; 32]).is_err());
        for i in 0..MAX_ORIGINS {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&(i as u64).to_le_bytes());
            state.trust_operator(&format!("origin-{i}"), key).unwrap();
        }
        assert!(state.trust_operator("one-too-many", [0xff; 32]).is_err());
        assert_eq!(state.len(), MAX_ORIGINS);
    }

    #[test]
    fn persistence_failure_rolls_back_visibility() {
        let dir = tempfile::tempdir().unwrap();
        let path = pins_path(dir.path());
        // Make the final file path a directory so the atomic rename fails on
        // every platform without relying on process privilege semantics.
        std::fs::create_dir_all(&path).unwrap();
        let state = FloodState {
            origins: RwLock::new(HashMap::new()),
            path: Some(path),
            persist_lock: Mutex::new(()),
        };
        assert!(state.trust_operator("warren-c", [3u8; 32]).is_err());
        assert!(
            state.resolve("warren-c").is_none(),
            "a pin is never visible after durable installation fails"
        );
    }

    #[test]
    fn concurrent_conflicting_installs_authorize_exactly_one_key() {
        let state = std::sync::Arc::new(FloodState::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut joins = Vec::new();
        for key in [[4u8; 32], [5u8; 32]] {
            let state = state.clone();
            let barrier = barrier.clone();
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                state.trust_operator("warren-c", key)
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
        let pinned = state.resolve("warren-c");
        assert!(pinned == Some([4u8; 32]) || pinned == Some([5u8; 32]));
    }
}
