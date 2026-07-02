//! Blake3 dedupe across servers.
//!
//! The same file — byte-for-byte, hence the same blake3 hash — is often
//! advertised by several federated servers. [`dedupe_by_hash`] collapses the
//! per-server [`SearchResult`]s from [`crate::search`] into one
//! [`DedupedMatch`] per distinct hash, each carrying the full set of
//! [`ServerRef`] sources that offer it. That deduped view is the basis for
//! swarm-style pull fan-out ([`crate::fanout`]): a client fetches one logical
//! file while striping its chunks across every server that has it.
//!
//! This is pure set-building over in-memory results — no I/O. Output is fully
//! deterministic: deduped matches are sorted by hash, and each match's sources
//! are sorted by `(server_key, generation)`, so identical inputs always yield
//! byte-identical plans downstream.

use serde::{Deserialize, Serialize};

use crate::search::SearchResult;

/// A single server that can serve a particular file, with enough locator
/// information for the transfer layer to request it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRef {
    /// The source server's public identity key.
    pub server_key: [u8; 32],
    /// The generation of the catalog this source came from (higher = fresher,
    /// hence more likely to still hold the file).
    pub generation: u64,
    /// The file area / library slug on that server.
    pub area: String,
    /// The folder path within the area (`""` = area root).
    pub path: String,
    /// The file name on that server (may differ across servers for identical
    /// bytes).
    pub name: String,
}

/// One logical file (identified by its blake3 hash) and every server offering
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupedMatch {
    /// blake3 content hash shared by every source.
    pub hash: [u8; 32],
    /// File size in bytes (from the first-seen source; identical hashes imply
    /// identical size).
    pub size: u64,
    /// Servers offering this file, sorted by `(server_key, generation)`, each
    /// server appearing at most once.
    pub sources: Vec<ServerRef>,
}

/// Collapse per-server [`SearchResult`]s into one [`DedupedMatch`] per distinct
/// blake3 hash, gathering all sources.
///
/// A given server contributes at most one [`ServerRef`] per hash (if its
/// catalog lists the same bytes under two paths, the lexicographically-first
/// locator wins). Output is sorted by hash for determinism.
pub fn dedupe_by_hash(results: &[SearchResult]) -> Vec<DedupedMatch> {
    // Preserve first-seen order of hashes while accumulating, then sort at the
    // end. A Vec of (hash, match) keeps the code dependency-light (no HashMap
    // ordering concerns) and inputs are small (a page of results per peer).
    let mut out: Vec<DedupedMatch> = Vec::new();

    for result in results {
        for m in &result.matches {
            let e = &m.entry;
            let candidate = ServerRef {
                server_key: result.server_key,
                generation: result.generation,
                area: e.area.clone(),
                path: e.path.clone(),
                name: e.name.clone(),
            };

            match out.iter_mut().find(|d| d.hash == e.hash) {
                Some(existing) => insert_source(&mut existing.sources, candidate),
                None => out.push(DedupedMatch {
                    hash: e.hash,
                    size: e.size,
                    sources: vec![candidate],
                }),
            }
        }
    }

    for d in &mut out {
        d.sources.sort_by(|a, b| {
            a.server_key
                .cmp(&b.server_key)
                .then(a.generation.cmp(&b.generation))
        });
    }
    out.sort_by_key(|d| d.hash);
    out
}

/// Add `candidate` unless the same server is already a source; if it is, keep
/// whichever locator sorts first by `(path, name)` so the result is stable.
fn insert_source(sources: &mut Vec<ServerRef>, candidate: ServerRef) {
    if let Some(existing) = sources
        .iter_mut()
        .find(|s| s.server_key == candidate.server_key)
    {
        let better = (candidate.path.as_str(), candidate.name.as_str())
            < (existing.path.as_str(), existing.name.as_str());
        if better {
            *existing = candidate;
        }
    } else {
        sources.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CatalogEntry;
    use crate::search::Match;

    fn result(server: u8, generation: u64, entries: &[(&str, u8, u64)]) -> SearchResult {
        SearchResult {
            server_key: [server; 32],
            generation,
            matches: entries
                .iter()
                .map(|(name, hash, size)| Match {
                    entry: CatalogEntry::new(*name, *size, [*hash; 32], "warez", ""),
                })
                .collect(),
        }
    }

    #[test]
    fn collapses_identical_hash_across_servers() {
        let a = result(1, 1, &[("game.zip", 42, 1000)]);
        let b = result(2, 1, &[("game-cracked.zip", 42, 1000)]);
        let deduped = dedupe_by_hash(&[a, b]);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].hash, [42u8; 32]);
        assert_eq!(deduped[0].size, 1000);
        assert_eq!(deduped[0].sources.len(), 2);
        assert_eq!(deduped[0].sources[0].server_key, [1u8; 32]);
        assert_eq!(deduped[0].sources[1].server_key, [2u8; 32]);
    }

    #[test]
    fn distinct_hashes_stay_separate_and_sorted() {
        let a = result(1, 1, &[("b.zip", 9, 2), ("a.zip", 3, 1)]);
        let deduped = dedupe_by_hash(&[a]);
        assert_eq!(deduped.len(), 2);
        // sorted by hash ascending: [3;32] before [9;32].
        assert_eq!(deduped[0].hash, [3u8; 32]);
        assert_eq!(deduped[1].hash, [9u8; 32]);
    }

    #[test]
    fn same_server_counted_once_per_hash() {
        // One server lists the same bytes twice under different names.
        let a = result(1, 1, &[("z-copy.zip", 7, 5), ("a-orig.zip", 7, 5)]);
        let deduped = dedupe_by_hash(&[a]);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].sources.len(), 1);
        // The lexicographically-first locator wins.
        assert_eq!(deduped[0].sources[0].name, "a-orig.zip");
    }

    #[test]
    fn sources_are_sorted_deterministically() {
        let a = result(3, 1, &[("f.zip", 1, 1)]);
        let b = result(1, 1, &[("f.zip", 1, 1)]);
        let c = result(2, 1, &[("f.zip", 1, 1)]);
        let deduped = dedupe_by_hash(&[a, b, c]);
        let keys: Vec<u8> = deduped[0].sources.iter().map(|s| s.server_key[0]).collect();
        assert_eq!(keys, vec![1, 2, 3]);
    }

    #[test]
    fn empty_input_yields_no_matches() {
        assert!(dedupe_by_hash(&[]).is_empty());
        assert!(dedupe_by_hash(&[result(1, 1, &[])]).is_empty());
    }
}
