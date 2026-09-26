//! RH-71: persisted verified peer catalogs keep restart search useful without
//! restoring revoked approval or accepting stale generations after reapproval.

use std::path::{Path, PathBuf};

use burrow::{fed_catalog, federation, Burrow};
use rabbithole_federation::{Catalog, CatalogEntry, SignedCatalog};
use rabbithole_identity::IdentityKey;
use rabbithole_server_core::ServerConfig;
use serde_json::{json, Value};

fn config(dir: &Path) -> ServerConfig {
    ServerConfig {
        name: "Cache Fixture".into(),
        data_dir: dir.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_enabled: false,
        ..ServerConfig::default()
    }
}

fn catalog(identity: &IdentityKey, generation: u64) -> SignedCatalog {
    Catalog::new(identity.public().0, generation, None)
        .with_entry(CatalogEntry::new(
            "demo.zip", 12, [42; 32], "pub", "releases",
        ))
        .sign(identity)
        .unwrap()
}

fn approve(b: &Burrow, identity: &IdentityKey, origin: &str) {
    federation::approve_peer(&b.shared, identity.public().0, Some(origin.into())).unwrap();
}

async fn search(b: &Burrow) -> Vec<Value> {
    let response =
        burrow::ctl::handle(&b.shared, &json!({"cmd":"fed-search", "terms":"demo"})).await;
    assert_eq!(response["ok"], true, "{response}");
    response["data"].as_array().unwrap().clone()
}

fn cache_path(dir: &Path) -> PathBuf {
    dir.join("federation/peer_catalogs.bin")
}

#[tokio::test]
async fn restart_keeps_approved_mirror_provenance_and_revoked_generation_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let a = IdentityKey::from_seed(&[1; 32]);
    let b = IdentityKey::from_seed(&[2; 32]);
    let source = Burrow::start(config(dir.path())).await.unwrap();
    approve(&source, &a, "a.example");
    approve(&source, &b, "b.example");
    fed_catalog::ingest_peer_catalog(&source.shared, a.public().0, &catalog(&a, 7).to_bytes())
        .unwrap();
    fed_catalog::ingest_peer_catalog(&source.shared, b.public().0, &catalog(&b, 4).to_bytes())
        .unwrap();
    let both = search(&source).await;
    assert_eq!(both.len(), 1, "identical hashes are still deduplicated");
    assert_eq!(both[0]["sources"].as_array().unwrap().len(), 2);
    federation::revoke_peer(&source.shared, a.public().0).unwrap();
    source.shutdown().await;

    let restarted = Burrow::start(config(dir.path())).await.unwrap();
    assert!(!restarted.shared.peers.is_approved(&a.public().0));
    let found = search(&restarted).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        found[0]["sources"][0]["server_key"],
        hex::encode(b.public().0)
    );
    assert_eq!(found[0]["sources"][0]["generation"], 4);
    assert_eq!(found[0]["sources"][0]["area"], "pub");
    assert_eq!(found[0]["sources"][0]["path"], "releases");
    approve(&restarted, &a, "a.example");
    assert!(fed_catalog::ingest_peer_catalog(
        &restarted.shared,
        a.public().0,
        &catalog(&a, 7).to_bytes()
    )
    .is_err());
    fed_catalog::ingest_peer_catalog(&restarted.shared, a.public().0, &catalog(&a, 8).to_bytes())
        .unwrap();
    restarted.shutdown().await;
    let again = Burrow::start(config(dir.path())).await.unwrap();
    assert_eq!(
        again
            .shared
            .catalogs
            .peer_catalog(&a.public().0)
            .unwrap()
            .catalog
            .generation,
        8
    );
    assert_eq!(
        search(&again).await[0]["sources"].as_array().unwrap().len(),
        2
    );
    again.shutdown().await;
}

#[tokio::test]
async fn current_pins_override_an_older_valid_snapshot_and_corruption_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let identity = IdentityKey::from_seed(&[3; 32]);
    let source = Burrow::start(config(dir.path())).await.unwrap();
    approve(&source, &identity, "peer.example");
    let signed = catalog(&identity, 5);
    fed_catalog::ingest_peer_catalog(&source.shared, identity.public().0, &signed.to_bytes())
        .unwrap();
    let old_cache = std::fs::read(cache_path(dir.path())).unwrap();
    federation::revoke_peer(&source.shared, identity.public().0).unwrap();
    source.shutdown().await;
    // Even a previously valid cache is not approval: the current pin file is.
    std::fs::write(cache_path(dir.path()), &old_cache).unwrap();
    let restarted = Burrow::start(config(dir.path())).await.unwrap();
    assert!(search(&restarted).await.is_empty());
    assert!(!restarted.shared.catalogs.wants(&identity.public().0, 5));
    approve(&restarted, &identity, "peer.example");
    assert!(restarted
        .shared
        .catalogs
        .peer_catalog(&identity.public().0)
        .is_none());
    restarted.shutdown().await;
    let again = Burrow::start(config(dir.path())).await.unwrap();
    assert!(
        search(&again).await.is_empty(),
        "reapproval flushed the cleared payload"
    );
    assert!(fed_catalog::ingest_peer_catalog(
        &again.shared,
        identity.public().0,
        &signed.to_bytes()
    )
    .is_err());
    again.shutdown().await;

    std::fs::write(cache_path(dir.path()), b"truncated cache").unwrap();
    let corrupt = Burrow::start(config(dir.path())).await.unwrap();
    assert!(search(&corrupt).await.is_empty());
    assert!(corrupt.shared.peers.is_approved(&identity.public().0));
    fed_catalog::ingest_peer_catalog(
        &corrupt.shared,
        identity.public().0,
        &catalog(&identity, 6).to_bytes(),
    )
    .unwrap();
    assert_eq!(
        search(&corrupt).await.len(),
        1,
        "a fresh verified fetch recovers a bad cache"
    );
    corrupt.shutdown().await;
}

#[tokio::test]
async fn failed_cache_revoke_is_reported_and_withdrawal_still_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let identity = IdentityKey::from_seed(&[4; 32]);
    let source = Burrow::start(config(dir.path())).await.unwrap();
    approve(&source, &identity, "peer.example");
    fed_catalog::ingest_peer_catalog(
        &source.shared,
        identity.public().0,
        &catalog(&identity, 1).to_bytes(),
    )
    .unwrap();
    let blocked = cache_path(dir.path()).with_extension("tmp");
    std::fs::create_dir(&blocked).unwrap();
    assert!(federation::revoke_peer(&source.shared, identity.public().0).is_err());
    assert!(search(&source).await.is_empty());
    assert!(!source.shared.peers.is_approved(&identity.public().0));
    assert!(federation::approve_peer(
        &source.shared,
        identity.public().0,
        Some("peer.example".into())
    )
    .is_err());
    assert!(!source.shared.peers.is_approved(&identity.public().0));
    source.shutdown().await;

    let restarted = Burrow::start(config(dir.path())).await.unwrap();
    assert!(!restarted.shared.peers.is_approved(&identity.public().0));
    assert!(search(&restarted).await.is_empty());
    std::fs::remove_dir(blocked).unwrap();
    approve(&restarted, &identity, "peer.example");
    assert!(fed_catalog::ingest_peer_catalog(
        &restarted.shared,
        identity.public().0,
        &catalog(&identity, 1).to_bytes()
    )
    .is_err());
    fed_catalog::ingest_peer_catalog(
        &restarted.shared,
        identity.public().0,
        &catalog(&identity, 2).to_bytes(),
    )
    .unwrap();
    restarted.shutdown().await;
}
