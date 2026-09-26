//! Private peer-cache format and bounded atomic storage. The local signature
//! authenticates watermarks and provenance as well as retained peer payloads.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rabbithole_federation::{is_valid_server_name, SignedCatalog};
use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

use super::PeerCatalog;

pub(super) const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_CACHED_CATALOGS: usize = 64;
pub(super) const MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_TRACKED_PEERS: usize = 4096;
const MAX_FILE_BYTES: usize = MAX_CACHED_BYTES + 4 * 1024 * 1024;
const CONTEXT: &[u8] = b"rabbithole-peer-catalog-cache-v1\0";
const SIGNATURE_BYTES: usize = 64;

pub(super) struct PeerCache {
    path: PathBuf,
    identity: IdentityKey,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u8,
    peers: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    key: [u8; 32],
    origin: String,
    generation: u64,
    order: u64,
    signed: Option<SignedCatalog>,
}

impl PeerCache {
    pub(super) fn new(data_dir: &Path, seed: &[u8; 32]) -> Self {
        Self {
            path: data_dir.join("federation").join("peer_catalogs.bin"),
            identity: IdentityKey::from_seed(seed),
        }
    }

    pub(super) fn read(&self) -> Result<HashMap<[u8; 32], PeerCatalog>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES || bytes.len() < SIGNATURE_BYTES {
            bail!("peer cache size is invalid");
        }
        let (signature, body) = bytes.split_at(SIGNATURE_BYTES);
        let signature = Signature(signature.try_into().expect("fixed signature length"));
        let signed_bytes = [CONTEXT, body].concat();
        // Authenticate before deserializing counts/strings from a corrupt file.
        if !self.identity.public().verify(&signed_bytes, &signature) {
            bail!("peer cache signature is invalid or local identity changed");
        }
        let snapshot: Snapshot = postcard::from_bytes(body).context("decode peer cache")?;
        if snapshot.version != 1 || snapshot.peers.len() > MAX_TRACKED_PEERS {
            bail!("unsupported or over-limit peer cache");
        }
        let mut peers = HashMap::new();
        let mut payload_count = 0;
        let mut payload_bytes = 0;
        for record in snapshot.peers {
            if !is_valid_server_name(&record.origin) || peers.contains_key(&record.key) {
                bail!("invalid or duplicate peer cache provenance");
            }
            let size = if let Some(signed) = &record.signed {
                signed
                    .verify(&PublicKey(record.key))
                    .context("peer cache catalog signature")?;
                if signed.catalog.generation != record.generation {
                    bail!("peer cache generation mismatch");
                }
                let size = signed.to_bytes().len();
                if size > MAX_CATALOG_BYTES {
                    bail!("peer cache catalog is too large");
                }
                payload_count += 1;
                payload_bytes += size;
                size
            } else {
                0
            };
            peers.insert(
                record.key,
                PeerCatalog {
                    revision: Arc::new(()),
                    origin: Some(record.origin),
                    generation: Some(record.generation),
                    signed: record.signed.map(Arc::new),
                    bytes: size,
                    order: record.order,
                },
            );
        }
        if payload_count > MAX_CACHED_CATALOGS || payload_bytes > MAX_CACHED_BYTES {
            bail!("peer cache payload retention limit exceeded");
        }
        Ok(peers)
    }

    pub(super) fn write(&self, peers: &HashMap<[u8; 32], PeerCatalog>) -> Result<()> {
        // This clone/encoding runs outside the catalog and registry locks.
        let mut records = Vec::new();
        for (key, peer) in peers {
            let Some(generation) = peer.generation else {
                continue;
            };
            let origin = peer
                .origin
                .clone()
                .context("catalog has no pinned origin")?;
            records.push(Record {
                key: *key,
                origin,
                generation,
                order: peer.order,
                signed: peer.signed.as_deref().cloned(),
            });
        }
        records.sort_by_key(|record| record.key);
        let body = postcard::to_allocvec(&Snapshot {
            version: 1,
            peers: records,
        })?;
        let signature = self.identity.sign(&[CONTEXT, body.as_slice()].concat());
        if SIGNATURE_BYTES + body.len() > MAX_FILE_BYTES {
            bail!("peer cache file limit exceeded");
        }
        let parent = self.path.parent().expect("cache path has a parent");
        std::fs::create_dir_all(parent)?;
        let tmp = self.path.with_extension("tmp");
        let write = || -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(&signature.0)?;
            file.write_all(&body)?;
            file.sync_all()?;
            drop(file);
            // Same-directory replacement: std::fs::rename replaces an existing
            // file on Windows as well as Unix. Never delete the old cache first.
            std::fs::rename(&tmp, &self.path)
        };
        if let Err(error) = write() {
            let _ = std::fs::remove_file(&tmp);
            return Err(error).context("replace peer catalog cache");
        }
        Ok(())
    }
}

/// Evict the oldest retained payloads, keeping their generation watermarks.
/// Key ordering breaks ties deterministically after a restart.
pub(super) fn trim(peers: &mut HashMap<[u8; 32], PeerCatalog>) {
    let mut retained: Vec<_> = peers
        .iter()
        .filter(|(_, peer)| peer.signed.is_some())
        .map(|(key, peer)| (peer.order, *key, peer.bytes))
        .collect();
    retained.sort_unstable();
    let mut count = retained.len();
    let mut bytes: usize = retained.iter().map(|(_, _, size)| size).sum();
    for (_, key, size) in retained {
        if count <= MAX_CACHED_CATALOGS && bytes <= MAX_CACHED_BYTES {
            break;
        }
        let peer = peers.get_mut(&key).expect("retained peer exists");
        peer.signed = None;
        peer.bytes = 0;
        count -= 1;
        bytes -= size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_federation::{Catalog, CatalogEntry};

    fn fixture() -> ([u8; 32], HashMap<[u8; 32], PeerCatalog>) {
        let identity = IdentityKey::from_seed(&[1; 32]);
        let key = identity.public().0;
        let signed = Catalog::new(key, 3, None)
            .with_entry(CatalogEntry::new("demo.zip", 1, [4; 32], "pub", ""))
            .sign(&identity)
            .unwrap();
        let bytes = signed.to_bytes().len();
        (
            key,
            HashMap::from([(
                key,
                PeerCatalog {
                    origin: Some("peer.example".into()),
                    generation: Some(3),
                    signed: Some(Arc::new(signed)),
                    bytes,
                    order: 1,
                    ..PeerCatalog::default()
                },
            )]),
        )
    }

    #[test]
    fn corrupt_truncated_appended_and_wrong_local_identity_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PeerCache::new(dir.path(), &[71; 32]);
        let (key, peers) = fixture();
        cache.write(&peers).unwrap();
        assert_eq!(cache.read().unwrap()[&key].generation, Some(3));
        assert!(PeerCache::new(dir.path(), &[72; 32]).read().is_err());
        let good = std::fs::read(&cache.path).unwrap();
        let mut corrupt = good.clone();
        corrupt[0] ^= 1;
        let mut appended = good.clone();
        appended.push(0);
        for bytes in [
            corrupt,
            good[..good.len() - 3].to_vec(),
            appended,
            Vec::new(),
        ] {
            std::fs::write(&cache.path, bytes).unwrap();
            assert!(cache.read().is_err());
        }
    }

    #[test]
    fn locally_authenticated_cache_still_rechecks_peer_signature_key_and_generation() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PeerCache::new(dir.path(), &[71; 32]);
        let (key, peers) = fixture();
        let mut bad_signature = peers.clone();
        let mut signed = bad_signature[&key].signed.as_deref().unwrap().clone();
        signed.sig.0[0] ^= 1;
        bad_signature.get_mut(&key).unwrap().signed = Some(Arc::new(signed));
        cache.write(&bad_signature).unwrap();
        assert!(cache.read().is_err());

        let mut bad_generation = peers.clone();
        bad_generation.get_mut(&key).unwrap().generation = Some(4);
        cache.write(&bad_generation).unwrap();
        assert!(cache.read().is_err());
        let wrong_key = HashMap::from([([9; 32], peers[&key].clone())]);
        cache.write(&wrong_key).unwrap();
        assert!(cache.read().is_err());
    }

    #[test]
    fn oversized_and_unknown_version_cache_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PeerCache::new(dir.path(), &[71; 32]);
        let (_, peers) = fixture();
        cache.write(&peers).unwrap();
        File::create(&cache.path)
            .unwrap()
            .set_len((MAX_FILE_BYTES + 1) as u64)
            .unwrap();
        assert!(cache.read().is_err());
        let body = postcard::to_allocvec(&Snapshot {
            version: 2,
            peers: Vec::new(),
        })
        .unwrap();
        let signature = cache.identity.sign(&[CONTEXT, body.as_slice()].concat());
        std::fs::write(
            &cache.path,
            [signature.0.as_slice(), body.as_slice()].concat(),
        )
        .unwrap();
        assert!(cache.read().is_err());
    }
}
