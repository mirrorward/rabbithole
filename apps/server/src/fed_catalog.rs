//! Signed-catalog building, verification, and federated search (Wave 9.x).
//!
//! This module is the burrow-side glue over the [`rabbithole_federation`]
//! catalog primitives: it turns the local file library into a
//! [`SignedCatalog`], keeps the latest **verified** catalog per approved peer,
//! and runs [`federated_search`] across all of them.
//!
//! # What is advertised
//!
//! Only *publicly-listable* files: for every file area, [`local_catalog`]
//! walks the tree via [`FileService::manifest`] (which already skips drop-box
//! folders and aliases) and keeps a file only if the **anonymous public
//! subject** — a bare guest with no class or account grants — holds
//! `SEE | FILE_LIST` on both the area and the file's ACL resource path
//! (`files/<area>/<path>`). Files without a content blob are skipped (there
//! is nothing to hash or fetch).
//!
//! # Rebuild trigger: on demand, content-compared
//!
//! The catalog is rebuilt **on demand** — whenever a peer asks for it or a
//! federated search runs — rather than on a file-event debounce. Entries are
//! collected in a deterministic order and compared with the last signed
//! catalog: if nothing changed, the existing signed catalog (same id, same
//! generation) is reused; if the listing changed, the generation is bumped
//! and `prev_id` links to the previous catalog id, giving peers the
//! staleness/supersedes chain from [`SignedCatalog::supersedes`]. Rebuilds
//! are serialized behind an async mutex so concurrent requests can't
//! double-bump the generation. At BBS scale a walk of the library per fetch
//! is cheap and keeps the design free of timers (deterministic tests, no
//! debounce races).
//!
//! # Persistence
//!
//! The last signed *local* catalog is persisted to
//! `<data_dir>/federation/catalog.bin` and reloaded on boot (discarded if it
//! doesn't verify under the current server identity), so the generation
//! chain survives restarts — a peer holding generation N is never shown a
//! "fresh" generation 1. Verified **peer** catalogs are in-memory only; a
//! restarted server re-pulls them on the next dial (persisting the peer cache
//! is a documented follow-up, not needed for correctness).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use rabbithole_federation::{
    dedupe_by_hash, Catalog, CatalogEntry, DedupedMatch, SearchQuery, SearchResult, SignedCatalog,
};
use rabbithole_identity::{IdentityKey, PublicKey};
use rabbithole_server_core::{Caps, Role, Subject};

use crate::Shared;

/// The "anyone" subject used to decide what is publicly listable: a bare
/// guest with no class mask and no per-account grants. Only what this subject
/// may `SEE | FILE_LIST` is advertised to peers.
fn public_subject() -> Subject {
    Subject {
        account_id: 0,
        role: Role::Guest,
        class_id: None,
        class_mask: 0,
        grant_mask: 0,
        revoke_mask: 0,
    }
}

/// In-memory catalog state: the last locally-built signed catalog plus the
/// latest verified catalog per peer (keyed by the peer's Ed25519 server key).
#[derive(Default)]
pub struct CatalogState {
    /// The last built+signed local catalog. An async mutex: it is held across
    /// the (awaiting) rebuild so concurrent callers serialize and can't
    /// double-bump the generation.
    local: tokio::sync::Mutex<Option<SignedCatalog>>,
    /// Latest verified catalog per approved peer.
    peers: parking_lot::RwLock<HashMap<[u8; 32], SignedCatalog>>,
}

impl CatalogState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Boot-time load: seed the local catalog from `<data_dir>` if a persisted
    /// copy exists and still verifies under this server's identity key
    /// (otherwise it is discarded and the chain restarts at generation 1).
    pub fn load(data_dir: &Path, server_key: &[u8; 32]) -> Self {
        let state = Self::default();
        let path = catalog_path(data_dir);
        if let Ok(bytes) = std::fs::read(&path) {
            match SignedCatalog::from_bytes(&bytes) {
                Some(signed) if signed.verify(&PublicKey(*server_key)).is_ok() => {
                    // Freshly-constructed mutex: try_lock cannot fail here.
                    if let Ok(mut guard) = state.local.try_lock() {
                        *guard = Some(signed);
                    }
                }
                _ => {
                    tracing::warn!(
                        path = %path.display(),
                        "persisted local catalog unreadable or key changed; starting fresh"
                    );
                }
            }
        }
        state
    }

    /// The latest verified catalog from `peer`, if any.
    pub fn peer_catalog(&self, key: &[u8; 32]) -> Option<SignedCatalog> {
        self.peers.read().get(key).cloned()
    }

    /// All stored peer catalogs, sorted by server key for stable output.
    pub fn peer_catalogs(&self) -> Vec<SignedCatalog> {
        let mut v: Vec<SignedCatalog> = self.peers.read().values().cloned().collect();
        v.sort_by_key(|c| c.catalog.server_key);
        v
    }

    /// Whether an announced `generation` from `peer` is fresher than what we
    /// hold — i.e. worth a full fetch.
    pub fn wants(&self, key: &[u8; 32], generation: u64) -> bool {
        match self.peers.read().get(key) {
            Some(cur) => generation > cur.catalog.generation,
            None => true,
        }
    }
}

fn catalog_path(data_dir: &Path) -> PathBuf {
    data_dir.join("federation").join("catalog.bin")
}

/// Build (or reuse) this server's signed public catalog.
///
/// Collects the publicly-listable entries, and if they match the last signed
/// catalog returns it unchanged (stable id and generation). Otherwise signs a
/// fresh catalog at `generation + 1` linked to the previous id, persists it,
/// and returns it.
pub async fn local_catalog(shared: &Shared) -> Result<SignedCatalog> {
    let entries = public_entries(shared).await?;

    let mut cur = shared.catalogs.local.lock().await;
    if let Some(existing) = cur.as_ref() {
        if existing.catalog.entries == entries {
            return Ok(existing.clone());
        }
    }

    let (generation, prev_id) = match cur.as_ref() {
        Some(prev) => (
            prev.catalog.generation + 1,
            Some(prev.catalog_id().map_err(|e| anyhow!("catalog id: {e}"))?),
        ),
        None => (1, None),
    };
    let mut catalog = Catalog::new(shared.server_key, generation, prev_id)
        .with_issued_at(chrono::Utc::now().timestamp_millis());
    catalog.entries = entries;
    let key = IdentityKey::from_seed(&shared.server_signing_seed);
    let signed = catalog
        .sign(&key)
        .map_err(|e| anyhow!("catalog sign: {e}"))?;

    // Persist best-effort so the generation chain survives a restart.
    let path = catalog_path(&shared.config.read().data_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, signed.to_bytes()) {
        tracing::warn!(path = %path.display(), "could not persist local catalog: {e}");
    }

    tracing::info!(
        generation,
        entries = signed.catalog.entries.len(),
        "local federation catalog rebuilt"
    );
    *cur = Some(signed.clone());
    Ok(signed)
}

/// Collect the publicly-listable files across every area, in a deterministic
/// `(area, path, name)` order so identical libraries yield identical catalog
/// bytes (hence stable ids across rebuilds).
async fn public_entries(shared: &Shared) -> Result<Vec<CatalogEntry>> {
    let public = public_subject();
    let need = Caps::SEE | Caps::FILE_LIST;
    let mut entries = Vec::new();

    let areas = shared
        .files
        .areas()
        .await
        .map_err(|e| anyhow!("areas: {e}"))?;
    for area in areas {
        if !shared
            .perms
            .allows(&public, &format!("files/{}", area.slug), need)
        {
            continue; // the whole area is not publicly listable
        }
        // `manifest` walks recursively and already skips drop-box folders
        // (their contents stay hidden) and aliases.
        let files = shared
            .files
            .manifest(&area.slug, None)
            .await
            .map_err(|e| anyhow!("manifest {}: {e}", area.slug))?;
        for (node, _rel) in files {
            let Some(hash) = node.blob_id else {
                continue; // no content blob: nothing to hash or fetch
            };
            if !shared
                .perms
                .allows(&public, &format!("files/{}/{}", area.slug, node.path), need)
            {
                continue; // hidden or unlisted for the public subject
            }
            let folder = node
                .path
                .strip_suffix(&node.name)
                .map(|p| p.trim_end_matches('/').to_string())
                .unwrap_or_default();
            entries.push(
                CatalogEntry::new(
                    node.name.clone(),
                    node.size.max(0) as u64,
                    hash,
                    area.slug.clone(),
                    folder,
                )
                .with_mime(node.mime.clone())
                // `created_at` is unix seconds in the store; the catalog
                // speaks unix milliseconds.
                .with_timestamp(node.created_at.saturating_mul(1000)),
            );
        }
    }

    entries.sort_by(|a, b| {
        (a.area.as_str(), a.path.as_str(), a.name.as_str()).cmp(&(
            b.area.as_str(),
            b.path.as_str(),
            b.name.as_str(),
        ))
    });
    Ok(entries)
}

/// Verify and store a catalog received from `peer_key` — the Ed25519 key the
/// peering handshake proved live possession of. Refuses:
///
/// - catalogs from peers that are not admin-approved;
/// - bytes that don't decode;
/// - signatures that don't verify under the pinned peer key (covers both
///   tampering and impersonation — a catalog naming a different server key is
///   a `KeyMismatch`);
/// - stale generations (must be strictly newer than what we hold).
pub fn ingest_peer_catalog(
    shared: &Shared,
    peer_key: [u8; 32],
    bytes: &[u8],
) -> Result<SignedCatalog> {
    if !shared.peers.is_approved(&peer_key) {
        bail!("refusing catalog from non-approved peer");
    }
    let signed = SignedCatalog::from_bytes(bytes).ok_or_else(|| anyhow!("malformed catalog"))?;
    signed
        .verify(&PublicKey(peer_key))
        .map_err(|e| anyhow!("catalog rejected: {e}"))?;

    let mut peers = shared.catalogs.peers.write();
    if let Some(cur) = peers.get(&peer_key) {
        if signed.catalog.generation <= cur.catalog.generation {
            bail!(
                "stale catalog: generation {} <= stored {}",
                signed.catalog.generation,
                cur.catalog.generation
            );
        }
    }
    peers.insert(peer_key, signed.clone());
    Ok(signed)
}

/// Run `query` across the local catalog plus every stored (verified) peer
/// catalog, collapsing identical files with [`dedupe_by_hash`]. Every source
/// in the result carries provenance: the advertising server's key and the
/// catalog generation it came from.
pub async fn federated_search(shared: &Shared, query: &SearchQuery) -> Result<Vec<DedupedMatch>> {
    let local = local_catalog(shared).await?;
    let mut results = vec![SearchResult::from_catalog(&local.catalog, query)];
    for cat in shared.catalogs.peer_catalogs() {
        results.push(SearchResult::from_catalog(&cat.catalog, query));
    }
    Ok(dedupe_by_hash(&results))
}

/// Display name for a source server key: the burrow's own name for the local
/// key, the peer registry's name when known, else the key hex.
pub fn server_display_name(shared: &Shared, key: &[u8; 32]) -> String {
    if *key == shared.server_key {
        return shared.config.read().name;
    }
    match shared.peers.get(key) {
        Some(rec) if !rec.name.is_empty() => rec.name,
        _ => hex::encode(key),
    }
}
