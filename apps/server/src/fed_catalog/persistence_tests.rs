use super::*;

const LOCAL_SEED: [u8; 32] = [71; 32];

fn state(dir: &Path, registry: &PeerRegistry) -> CatalogState {
    let state = CatalogState::load(dir, &LOCAL_SEED);
    state.load_peers(registry);
    state
}

fn catalog(identity: &IdentityKey, generation: u64) -> SignedCatalog {
    Catalog::new(identity.public().0, generation, None)
        .with_entry(CatalogEntry::new("demo.zip", 10, [1; 32], "pub", ""))
        .sign(identity)
        .unwrap()
}

fn put(
    state: &CatalogState,
    registry: &PeerRegistry,
    signed: SignedCatalog,
) -> Result<SignedCatalog> {
    let fetch = state.begin_fetch(registry, signed.catalog.server_key)?;
    state.store_verified(registry, fetch, signed)
}

fn path(dir: &Path) -> PathBuf {
    dir.join("federation/peer_catalogs.bin")
}

#[test]
fn cache_replaces_existing_generation_atomically_and_rejects_delayed_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(PeerRegistry::new());
    let identity = IdentityKey::from_seed(&[1; 32]);
    let key = identity.public().0;
    registry.seed_approved(key, "peer", Some("peer.example".into()));
    let state = Arc::new(state(dir.path(), &registry));
    put(&state, &registry, catalog(&identity, 1)).unwrap();
    let older = state.begin_fetch(&registry, key).unwrap();
    let delayed = catalog(&identity, 2);
    let release = Arc::new(std::sync::Barrier::new(2));
    let thread = {
        let state = state.clone();
        let registry = registry.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            release.wait();
            state.store_verified(&registry, older, delayed)
        })
    };
    put(&state, &registry, catalog(&identity, 3)).unwrap();
    let good_bytes = std::fs::read(path(dir.path())).unwrap();
    release.wait();
    assert!(thread
        .join()
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("stale catalog"));
    assert_eq!(std::fs::read(path(dir.path())).unwrap(), good_bytes);
    let loaded = CatalogState::load(dir.path(), &LOCAL_SEED);
    loaded.load_peers(&registry);
    assert_eq!(loaded.peer_catalog(&key), Some(catalog(&identity, 3)));
    assert!(!loaded.wants(&key, 3));
    assert!(loaded.wants(&key, 4));
    assert!(put(&loaded, &registry, catalog(&identity, 2)).is_err());
}

#[test]
fn revoked_and_changed_pin_payloads_stay_hidden_but_watermarks_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let registry = PeerRegistry::new();
    let identity = IdentityKey::from_seed(&[2; 32]);
    let key = identity.public().0;
    registry.seed_approved(key, "peer", Some("old.example".into()));
    let first = state(dir.path(), &registry);
    put(&first, &registry, catalog(&identity, 7)).unwrap();

    let different = PeerRegistry::new();
    different.seed_approved(key, "peer", Some("new.example".into()));
    let changed = state(dir.path(), &different);
    assert!(changed.peer_catalog(&key).is_none());
    assert!(!changed.wants(&key, 7));
    let missing = state(dir.path(), &PeerRegistry::new());
    assert!(missing.peer_catalogs().is_empty());
    assert!(!missing.wants(&key, 7));

    first.revoke_peer(&registry, &key).unwrap();
    let revoked = state(dir.path(), &registry);
    assert!(revoked.peer_catalog(&key).is_none());
    assert!(!revoked.wants(&key, 7));
    registry.approve_origin(&key, "old.example".into());
    assert!(put(&revoked, &registry, catalog(&identity, 7)).is_err());
    put(&revoked, &registry, catalog(&identity, 8)).unwrap();
    assert_eq!(
        state(dir.path(), &registry).peer_catalog(&key),
        Some(catalog(&identity, 8))
    );
}

#[test]
fn write_failure_keeps_previous_generation_and_revoke_still_hides_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let registry = PeerRegistry::new();
    let identity = IdentityKey::from_seed(&[3; 32]);
    let key = identity.public().0;
    registry.seed_approved(key, "peer", Some("peer.example".into()));
    let state = state(dir.path(), &registry);
    let first = catalog(&identity, 1);
    put(&state, &registry, first.clone()).unwrap();
    let before = std::fs::read(path(dir.path())).unwrap();
    // Block staging before rename, leaving the previous final file intact.
    let blocked = path(dir.path()).with_extension("tmp");
    std::fs::create_dir(&blocked).unwrap();
    assert!(put(&state, &registry, catalog(&identity, 2)).is_err());
    assert_eq!(state.peer_catalog(&key), Some(first));
    assert_eq!(std::fs::read(path(dir.path())).unwrap(), before);
    assert!(state.wants(&key, 2), "failed writes never advance memory");
    assert!(state.revoke_peer(&registry, &key).is_err());
    assert!(!registry.is_approved(&key));
    assert!(state.peer_catalog(&key).is_none());
    assert!(!state.wants(&key, 1));
    assert!(state.persist_current().is_err());
    std::fs::remove_dir(blocked).unwrap();
    state.persist_current().unwrap();
    registry.approve_origin(&key, "peer.example".into());
    let loaded = CatalogState::load(dir.path(), &LOCAL_SEED);
    loaded.load_peers(&registry);
    assert!(loaded.peer_catalog(&key).is_none());
    assert!(!loaded.wants(&key, 1));
}

#[test]
fn retention_evicts_oldest_payload_but_preserves_its_replay_watermark_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let registry = PeerRegistry::new();
    let state = state(dir.path(), &registry);
    let mut keys = Vec::new();
    for byte in 0..=cache::MAX_CACHED_CATALOGS {
        let identity = IdentityKey::from_seed(&[byte as u8; 32]);
        let key = identity.public().0;
        registry.seed_approved(key, "peer", Some(format!("peer{byte}.example")));
        put(&state, &registry, catalog(&identity, 1)).unwrap();
        keys.push(key);
    }
    assert_eq!(state.peer_catalogs().len(), cache::MAX_CACHED_CATALOGS);
    assert!(state.peer_catalog(&keys[0]).is_none());
    assert!(!state.wants(&keys[0], 1));
    assert!(state.peer_catalog(keys.last().unwrap()).is_some());
    let reloaded = CatalogState::load(dir.path(), &LOCAL_SEED);
    reloaded.load_peers(&registry);
    assert_eq!(reloaded.peer_catalogs().len(), cache::MAX_CACHED_CATALOGS);
    assert!(reloaded.peer_catalog(&keys[0]).is_none());
    assert!(!reloaded.wants(&keys[0], 1));
}

#[test]
fn pending_fetch_admission_is_bounded_without_allocating_unknown_revokes() {
    let state = CatalogState::new();
    let registry = PeerRegistry::new();
    for index in 0..=cache::MAX_TRACKED_PEERS {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&(index as u64).to_le_bytes());
        registry.seed_approved(key, "peer", Some(format!("peer{index}.example")));
        assert_eq!(
            state.begin_fetch(&registry, key).is_ok(),
            index < cache::MAX_TRACKED_PEERS
        );
    }
    assert_eq!(state.peers.read().len(), cache::MAX_TRACKED_PEERS);
    assert!(!state.revoke_peer(&registry, &[255; 32]).unwrap());
    assert_eq!(state.peers.read().len(), cache::MAX_TRACKED_PEERS);
}

#[test]
fn byte_retention_limit_preserves_watermarks_and_catalog_input_is_bounded() {
    let state = CatalogState::new();
    let registry = PeerRegistry::new();
    let identity = IdentityKey::from_seed(&[3; 32]);
    let key = identity.public().0;
    registry.seed_approved(key, "peer", Some("peer.example".into()));
    let mut oversized = Catalog::new(key, 1, None);
    oversized.entries.push(CatalogEntry::new(
        "x".repeat(cache::MAX_CATALOG_BYTES),
        1,
        [1; 32],
        "pub",
        "",
    ));
    assert!(put(&state, &registry, oversized.sign(&identity).unwrap()).is_err());
    assert!(state.peer_catalog(&key).is_none());
    assert!(state.wants(&key, 1));

    // Accounted sizes come from actual encoded catalogs on the production
    // path. Exercise byte-driven eviction separately from the count cap.
    let signed = Arc::new(catalog(&identity, 1));
    let mut peers = HashMap::new();
    for index in 0..9u8 {
        peers.insert(
            [index; 32],
            PeerCatalog {
                generation: Some(1),
                signed: Some(signed.clone()),
                bytes: cache::MAX_CATALOG_BYTES,
                order: u64::from(index),
                ..PeerCatalog::default()
            },
        );
    }
    cache::trim(&mut peers);
    assert!(peers[&[0; 32]].signed.is_none());
    assert_eq!(peers[&[0; 32]].generation, Some(1));
    assert_eq!(
        peers.values().filter(|peer| peer.signed.is_some()).count(),
        8
    );
}

#[tokio::test]
async fn failed_revoke_cannot_interleave_between_approval_flush_and_pin_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let config = rabbithole_server_core::ServerConfig {
        data_dir: dir.path().to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ..rabbithole_server_core::ServerConfig::default()
    };
    let b = crate::Burrow::start(config.clone()).await.unwrap();
    let identity = IdentityKey::from_seed(&[9; 32]);
    let key = identity.public().0;
    crate::federation::approve_peer(&b.shared, key, Some("peer.example".into())).unwrap();
    ingest_peer_catalog(&b.shared, key, &catalog(&identity, 7).to_bytes()).unwrap();
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    *b.shared.catalogs.after_approval_flush.lock() = Some(ApprovalPause {
        reached: reached.clone(),
        release: release.clone(),
    });
    let approval = {
        let shared = b.shared.clone();
        std::thread::spawn(move || {
            crate::federation::approve_peer(&shared, key, Some("peer.example".into()))
        })
    };
    reached.wait();
    // The cache mutation guard has been released, but the complete operator
    // transaction still owns serialization until its approval file is saved.
    let mutation_released = b.shared.catalogs.mutation.try_lock().is_some();
    let operator_held = b.shared.catalogs.operator_change.try_lock().is_none();
    std::fs::create_dir(path(dir.path()).with_extension("tmp")).unwrap();
    let revocation = {
        let shared = b.shared.clone();
        std::thread::spawn(move || crate::federation::revoke_peer(&shared, key))
    };
    release.wait();
    assert!(approval.join().unwrap().is_ok());
    assert!(
        revocation.join().unwrap().is_err(),
        "cache failure is reported"
    );
    assert!(mutation_released && operator_held);
    assert!(!b.shared.peers.is_approved(&key));
    assert!(b.shared.catalogs.peer_catalog(&key).is_none());
    b.shutdown().await;
    let restarted = crate::Burrow::start(config).await.unwrap();
    assert!(!restarted.shared.peers.is_approved(&key));
    assert!(restarted.shared.catalogs.peer_catalog(&key).is_none());
    assert!(!restarted.shared.catalogs.wants(&key, 7));
    restarted.shutdown().await;
}
