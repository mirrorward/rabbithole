//! Bounded, versioned saved listings. Signatures remain untrusted bytes until
//! each caller invokes `DirectoryServer::verification` with its current clock.
use crate::DirectoryServer;
use serde::{Deserialize, Serialize};

pub const MAX_CACHE_BYTES: usize = 512 * 1024;
pub const MAX_CACHE_ROWS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedListing {
    version: u8,
    source_key: String,
    pub source_label: String,
    pub fetched_at_ms: i64,
    pub servers: Vec<DirectoryServer>,
    /// Independent high-water statements survive visibly invalid refreshes.
    #[serde(default)]
    history: Vec<DirectoryServer>,
}

impl CachedListing {
    pub fn source_key(&self) -> &str {
        &self.source_key
    }
}

fn bounded(row: &DirectoryServer) -> bool {
    row.name.len() <= 1024
        && row.endpoint.len() <= 2048
        && row.description.len() <= 4096
        && row.listeners.len() <= 32
        && row.listeners.iter().all(|s| s.len() <= 64)
        && row.proof.as_ref().is_none_or(|p| p.within_limits())
}

/// Snapshot up to 128 rows under an exact, caller-controlled source key.
/// Oversized individual rows are omitted; the tail is trimmed to the byte cap.
pub fn encode(source_key: &str, fetched_at_ms: i64, rows: &[DirectoryServer]) -> Option<String> {
    encode_with_label(source_key, source_key, fetched_at_ms, rows)
}

pub fn encode_with_label(
    source_key: &str,
    source_label: &str,
    fetched_at_ms: i64,
    rows: &[DirectoryServer],
) -> Option<String> {
    update(
        source_key,
        source_label,
        fetched_at_ms,
        &mut rows.to_vec(),
        None,
    )
}

/// Refresh visible rows and independently retain the newest authenticated
/// statements. Bad or missing new proof stays visible without erasing history.
pub fn update(
    source_key: &str,
    source_label: &str,
    fetched_at_ms: i64,
    rows: &mut [DirectoryServer],
    previous: Option<&CachedListing>,
) -> Option<String> {
    if source_key.is_empty()
        || source_key.len() > 256
        || source_label.len() > 256
        || fetched_at_ms < 0
    {
        return None;
    }
    let previous = previous.filter(|saved| saved.source_key == source_key);
    let mut history = Vec::new();
    // Also seed from older schema-v1 snapshots that predate history.
    if let Some(saved) = previous {
        for row in saved.history.iter().chain(&saved.servers) {
            remember_proof(&mut history, row, fetched_at_ms);
        }
    }
    retain_newer_at(&history, rows, fetched_at_ms);
    for row in rows.iter() {
        remember_proof(&mut history, row, fetched_at_ms);
    }
    let mut saved = CachedListing {
        version: 1,
        source_key: source_key.into(),
        source_label: source_label.into(),
        fetched_at_ms,
        servers: rows
            .iter()
            .filter(|row| bounded(row))
            .take(MAX_CACHE_ROWS)
            .cloned()
            .collect(),
        history,
    };
    loop {
        let text = serde_json::to_string(&saved).ok()?;
        if text.len() <= MAX_CACHE_BYTES {
            return Some(text);
        }
        // Keep the same total byte cap even when all proofs are at the limit.
        // Evict the least recent historical claim before trimming display rows.
        if !saved.history.is_empty() {
            saved.history.remove(0);
        } else {
            saved.servers.pop()?;
        }
    }
}

fn remember_proof(history: &mut Vec<DirectoryServer>, row: &DirectoryServer, now_ms: i64) {
    if !bounded(row) {
        return;
    }
    let claim = match row.verification(now_ms) {
        crate::DirectoryVerification::Verified(proof)
        | crate::DirectoryVerification::Stale(proof) => proof,
        _ => return,
    };
    if let Some(index) = history.iter().position(|held| {
        held.endpoint == row.endpoint
            && crate::verification::verified_claim(held)
                .is_some_and(|(older, _)| older.public_key == claim.public_key)
    }) {
        let old = crate::verification::verified_claim(&history[index])
            .expect("history was verified")
            .0;
        if old.issued_at_ms > claim.issued_at_ms {
            return;
        }
        history.remove(index);
    }
    let mut held = row.clone();
    // Only fields actually authenticated by this verifier belong in history.
    held.description.clear();
    held.listeners.clear();
    held.users_online = None;
    held.uptime_pct = None;
    held.reachable = false;
    history.push(held);
    if history.len() > MAX_CACHE_ROWS {
        history.remove(0);
    }
}

/// Reject unknown schema/source, malformed or oversized input before exposure.
/// Saved health is historical; callers must label it as such or hide it.
pub fn decode(source_key: &str, text: &str) -> Option<CachedListing> {
    if text.len() > MAX_CACHE_BYTES {
        return None;
    }
    let saved: CachedListing = serde_json::from_str(text).ok()?;
    if saved.version != 1
        || saved.source_key != source_key
        || saved.source_key.len() > 256
        || saved.source_label.len() > 256
        || saved.source_key.is_empty()
        || saved.fetched_at_ms < 0
        || saved.servers.len() > MAX_CACHE_ROWS
        || saved.history.len() > MAX_CACHE_ROWS
        || !saved.history.iter().all(bounded)
        || !saved.servers.iter().all(bounded)
    {
        return None;
    }
    Some(saved)
}

/// Reject generation rollback for the same exact endpoint and signing key.
/// A fresh signature from another key remains a separate self-certified claim;
/// this cache is not a key-pinning or reputation system. Invalid new proofs
/// stay visibly invalid instead of being masked by an older successful check.
pub fn retain_newer_at(
    previous: &[DirectoryServer],
    incoming: &mut [DirectoryServer],
    now_ms: i64,
) {
    for row in incoming {
        let next = match row.verification(now_ms) {
            crate::DirectoryVerification::Verified(proof)
            | crate::DirectoryVerification::Stale(proof) => proof,
            _ => continue,
        };
        let older = previous
            .iter()
            .filter(|held| held.endpoint == row.endpoint)
            .filter_map(|held| {
                let claim = match held.verification(now_ms) {
                    crate::DirectoryVerification::Verified(proof)
                    | crate::DirectoryVerification::Stale(proof) => proof,
                    _ => return None,
                };
                (claim.public_key == next.public_key && claim.issued_at_ms > next.issued_at_ms)
                    .then_some((held, claim.issued_at_ms))
            })
            .max_by_key(|(_, issued)| *issued);
        if let Some((held, _)) = older {
            row.name.clone_from(&held.name);
            row.proof.clone_from(&held.proof);
        }
    }
}
