//! Cross-server file search over signed catalogs.
//!
//! A [`SearchQuery`] describes what a user is looking for: a set of terms
//! (case-insensitive substring on the file name), optional size and MIME-type
//! filters, and pagination. [`search_catalog`] runs that query against a single
//! [`Catalog`] as a pure, allocation-only function — a federation service runs
//! it once per peer catalog it holds.
//!
//! Matching is deliberately simple and total: every term must be a
//! case-insensitive substring of the entry name (AND semantics; an empty term
//! list matches everything), and the size/MIME predicates must all hold. The
//! per-catalog results are wrapped in a [`SearchResult`] tagged with which
//! server produced them and at which catalog generation, so a caller merging
//! results from many peers (see [`crate::dedupe`]) always knows each match's
//! provenance.

use serde::{Deserialize, Serialize};

use crate::catalog::{Catalog, CatalogEntry};

/// A cross-server search request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Case-insensitive substrings; **all** must appear in an entry's name for
    /// it to match. Empty = match every entry (subject to the filters below).
    pub terms: Vec<String>,
    /// Inclusive minimum file size in bytes, if set.
    pub min_size: Option<u64>,
    /// Inclusive maximum file size in bytes, if set.
    pub max_size: Option<u64>,
    /// MIME-type prefix filter (e.g. `"image/"` or `"application/zip"`), if
    /// set. Compared case-insensitively against the entry's `mime`.
    pub mime_prefix: Option<String>,
    /// Number of leading matches to skip (pagination).
    pub offset: usize,
    /// Maximum matches to return. `0` means "no limit".
    pub limit: usize,
}

impl SearchQuery {
    /// A query from a whitespace-free term list; no filters, no pagination.
    pub fn new(terms: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            terms: terms.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Builder: constrain the size range (inclusive bounds).
    pub fn with_size_range(mut self, min: Option<u64>, max: Option<u64>) -> Self {
        self.min_size = min;
        self.max_size = max;
        self
    }

    /// Builder: constrain the MIME-type prefix.
    pub fn with_mime_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.mime_prefix = Some(prefix.into());
        self
    }

    /// Builder: set pagination (`limit` of `0` = unbounded).
    pub fn with_page(mut self, offset: usize, limit: usize) -> Self {
        self.offset = offset;
        self.limit = limit;
        self
    }

    /// Whether `entry` satisfies this query's predicates (ignores pagination).
    pub fn matches(&self, entry: &CatalogEntry) -> bool {
        if let Some(min) = self.min_size {
            if entry.size < min {
                return false;
            }
        }
        if let Some(max) = self.max_size {
            if entry.size > max {
                return false;
            }
        }
        if let Some(prefix) = &self.mime_prefix {
            if !entry
                .mime
                .to_lowercase()
                .starts_with(&prefix.to_lowercase())
            {
                return false;
            }
        }
        let name = entry.name.to_lowercase();
        self.terms.iter().all(|t| name.contains(&t.to_lowercase()))
    }
}

/// One matched catalog entry (kept as an owned copy so results outlive the
/// catalog they came from).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    /// The advertised file that matched.
    pub entry: CatalogEntry,
}

/// The matches from a single server's catalog, tagged with provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// The advertising server's public identity key.
    pub server_key: [u8; 32],
    /// The generation of the catalog these matches came from.
    pub generation: u64,
    /// The matched entries, in catalog order, after pagination.
    pub matches: Vec<Match>,
}

impl SearchResult {
    /// Run `query` against `catalog` and wrap the hits with the catalog's
    /// server key and generation.
    pub fn from_catalog(catalog: &Catalog, query: &SearchQuery) -> Self {
        Self {
            server_key: catalog.server_key,
            generation: catalog.generation,
            matches: search_catalog(catalog, query),
        }
    }
}

/// Run `query` against `catalog`, returning the matching entries in catalog
/// order after applying the query's `offset`/`limit` pagination.
///
/// Pure and total: no I/O, no panics. A `limit` of `0` returns every match
/// past `offset`.
pub fn search_catalog(catalog: &Catalog, query: &SearchQuery) -> Vec<Match> {
    let hits = catalog
        .entries
        .iter()
        .filter(|e| query.matches(e))
        .skip(query.offset);
    let mapped = hits.map(|e| Match { entry: e.clone() });
    if query.limit == 0 {
        mapped.collect()
    } else {
        mapped.take(query.limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn entry(name: &str, size: u64, mime: &str) -> CatalogEntry {
        CatalogEntry::new(name, size, [1u8; 32], "warez", "").with_mime(mime)
    }

    fn catalog() -> Catalog {
        Catalog::new([7u8; 32], 4, None)
            .with_entry(entry("Cool-Demo.ZIP", 100, "application/zip"))
            .with_entry(entry("cool-track.mp3", 5_000, "audio/mpeg"))
            .with_entry(entry("photo.png", 2_000, "image/png"))
            .with_entry(entry("readme.txt", 10, "text/plain"))
    }

    #[test]
    fn substring_match_is_case_insensitive() {
        let q = SearchQuery::new(["cool"]);
        let hits = search_catalog(&catalog(), &q);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entry.name, "Cool-Demo.ZIP");
        assert_eq!(hits[1].entry.name, "cool-track.mp3");
    }

    #[test]
    fn empty_terms_match_everything() {
        let hits = search_catalog(&catalog(), &SearchQuery::default());
        assert_eq!(hits.len(), 4);
    }

    #[test]
    fn all_terms_must_match() {
        let q = SearchQuery::new(["cool", "demo"]);
        let hits = search_catalog(&catalog(), &q);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.name, "Cool-Demo.ZIP");
    }

    #[test]
    fn size_filters_are_inclusive() {
        let q = SearchQuery::default().with_size_range(Some(100), Some(2_000));
        let hits: Vec<_> = search_catalog(&catalog(), &q)
            .into_iter()
            .map(|m| m.entry.name)
            .collect();
        assert_eq!(hits, vec!["Cool-Demo.ZIP", "photo.png"]);
    }

    #[test]
    fn mime_prefix_filter() {
        let q = SearchQuery::default().with_mime_prefix("image/");
        let hits = search_catalog(&catalog(), &q);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.name, "photo.png");
    }

    #[test]
    fn pagination_offset_and_limit() {
        let q = SearchQuery::default().with_page(1, 2);
        let hits: Vec<_> = search_catalog(&catalog(), &q)
            .into_iter()
            .map(|m| m.entry.name)
            .collect();
        assert_eq!(hits, vec!["cool-track.mp3", "photo.png"]);
    }

    #[test]
    fn limit_zero_is_unbounded() {
        let q = SearchQuery::default().with_page(2, 0);
        assert_eq!(search_catalog(&catalog(), &q).len(), 2);
    }

    #[test]
    fn search_result_carries_provenance() {
        let cat = catalog();
        let res = SearchResult::from_catalog(&cat, &SearchQuery::new(["cool"]));
        assert_eq!(res.server_key, [7u8; 32]);
        assert_eq!(res.generation, 4);
        assert_eq!(res.matches.len(), 2);
    }

    #[test]
    fn query_roundtrips_through_postcard() {
        let q = SearchQuery::new(["a", "b"])
            .with_size_range(Some(1), Some(9))
            .with_mime_prefix("image/")
            .with_page(3, 7);
        let back: SearchQuery = postcard::from_bytes(&postcard::to_allocvec(&q).unwrap()).unwrap();
        assert_eq!(q, back);
    }
}
