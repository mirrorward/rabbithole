//! Wave 9.x end-to-end tests: signed-catalog sync and cross-server file
//! search over the S2S federation peering session. Extends the two-burrow
//! harness from `e2e_w9_federation.rs`. We prove that:
//!
//! - a burrow's catalog advertises only its publicly-listable files (drop-box
//!   contents stay hidden) and is Ed25519-signed;
//! - a **non-approved** peer gets no catalog (the pending handshake is
//!   refused before any catalog frame, and direct ingest refuses too);
//! - after approval, a dial syncs the peer's catalog: verified against the
//!   pinned key, stored with its generation;
//! - `fed-search` over ctl finds a remote file with correct provenance
//!   (which server, which generation) and blake3-dedupes identical bytes
//!   offered by several servers;
//! - tampered / impersonated / malformed / stale catalogs are rejected;
//! - the local catalog only bumps its generation when the library changes,
//!   supersedes its predecessor, and the chain survives a restart.
//!
//! Determinism is by real readiness signals: `dial_peer` performs the catalog
//! sync *before* returning `Connected`, so there are no sleeps or polls.

use burrow::federation::{dial_peer, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_federation::{Catalog, CatalogEntry};
use rabbithole_identity::IdentityKey;
use rabbithole_server_core::{PeerState, ServerConfig};
use serde_json::json;

fn fed_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Catalog Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// A dial target aimed at `b`, pinning its live cert + identity.
fn target_for(b: &Burrow) -> DialTarget {
    DialTarget {
        addr: b.federation_addr.expect("federation enabled").to_string(),
        server_name: "localhost".into(),
        fingerprint: b.fingerprint,
        expected_key: Some(b.shared.server_key),
    }
}

/// Add a library file with a fixed blake3 hash (the catalog reads the tree,
/// not the blob bytes, so a synthetic hash is fine here).
async fn add_file(b: &Burrow, area: &str, folder: Option<&str>, name: &str, hash: u8, size: i64) {
    b.shared
        .files
        .add_file(
            area,
            folder,
            name,
            &[hash; 32],
            size,
            "application/zip",
            "disk",
            "",
            "op@warren",
            1,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn catalog_sync_and_federated_search_with_provenance() {
    let work = tempfile::tempdir().unwrap();
    let a = Burrow::start(ServerConfig {
        name: "Warren A".into(),
        ..fed_config(&work.path().join("a"))
    })
    .await
    .unwrap();
    let b = Burrow::start(ServerConfig {
        name: "Warren B".into(),
        ..fed_config(&work.path().join("b"))
    })
    .await
    .unwrap();
    let a_key = a.shared.server_key;
    let b_key = b.shared.server_key;

    // A's public library: one visible file, plus a drop box whose contents
    // must never be advertised.
    a.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    add_file(&a, "warez", None, "cool-demo.zip", 7, 1234).await;
    a.shared
        .files
        .mkdir("warez", None, "inbox", true)
        .await
        .unwrap();
    add_file(&a, "warez", Some("inbox"), "secret.zip", 9, 55).await;

    // B dials A before A has approved B: authenticated but refused — and no
    // catalog crosses the wire (a non-approved peer can't fetch).
    let outcome = dial_peer(b.shared.clone(), target_for(&a)).await.unwrap();
    assert_eq!(outcome, DialOutcome::Pending(a_key));
    assert!(
        b.shared.catalogs.peer_catalog(&a_key).is_none(),
        "no catalog for a pending (unapproved) peering"
    );

    // A's admin approves B via the audited ctl path; B redials and the
    // connected dial syncs A's catalog before returning.
    let b_key_hex = hex::encode(b_key);
    let resp =
        burrow::ctl::handle(&a.shared, &json!({"cmd": "peer-approve", "key": b_key_hex})).await;
    assert_eq!(resp["ok"], json!(true), "approval accepted: {resp}");
    let outcome = dial_peer(b.shared.clone(), target_for(&a)).await.unwrap();
    assert_eq!(outcome, DialOutcome::Connected(a_key));
    assert_eq!(a.shared.peers.state(&b_key), Some(PeerState::Connected));

    let stored = b
        .shared
        .catalogs
        .peer_catalog(&a_key)
        .expect("catalog synced on the connected dial");
    assert_eq!(stored.catalog.server_key, a_key);
    assert_eq!(stored.catalog.generation, 1);
    let names: Vec<&str> = stored
        .catalog
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, vec!["cool-demo.zip"], "drop-box contents excluded");
    assert_eq!(stored.catalog.entries[0].hash, [7u8; 32]);
    assert_eq!(stored.catalog.entries[0].area, "warez");
    assert_eq!(stored.catalog.entries[0].path, "");

    // B offers the same bytes under another name: fed-search must dedupe by
    // blake3 hash and report both servers as sources with provenance.
    b.shared
        .files
        .create_area("mirror", "Mirror", "")
        .await
        .unwrap();
    add_file(&b, "mirror", None, "cool-demo-copy.zip", 7, 1234).await;

    let resp = burrow::ctl::handle(&b.shared, &json!({"cmd": "fed-search", "terms": "cool"})).await;
    assert_eq!(resp["ok"], json!(true), "fed-search ok: {resp}");
    let rows = resp["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "identical bytes collapse to one match");
    let row = &rows[0];
    assert_eq!(row["hash"], json!(hex::encode([7u8; 32])));
    assert_eq!(row["size"], json!(1234));
    let sources = row["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2, "both warrens offer the file");
    let from_a = sources
        .iter()
        .find(|s| s["server_key"] == json!(hex::encode(a_key)))
        .expect("remote source present");
    assert_eq!(from_a["server"], json!("Warren A"));
    assert_eq!(from_a["generation"], json!(1));
    assert_eq!(from_a["area"], json!("warez"));
    assert_eq!(from_a["name"], json!("cool-demo.zip"));
    assert_eq!(from_a["local"], json!(false));
    let from_b = sources
        .iter()
        .find(|s| s["server_key"] == json!(hex::encode(b_key)))
        .expect("local source present");
    assert_eq!(from_b["local"], json!(true));
    assert_eq!(from_b["name"], json!("cool-demo-copy.zip"));

    // fed-catalogs reflects what B holds: its own catalog plus A's at gen 1.
    let resp = burrow::ctl::handle(&b.shared, &json!({"cmd": "fed-catalogs"})).await;
    let rows = resp["data"].as_array().unwrap();
    assert!(rows
        .iter()
        .any(|r| r["local"] == json!(true) && r["entries"] == json!(1)));
    assert!(rows.iter().any(|r| r["key"] == json!(hex::encode(a_key))
        && r["generation"] == json!(1)
        && r["entries"] == json!(1)));

    // A's library grows; a redial pulls the superseding generation.
    add_file(&a, "warez", None, "more-cool.zip", 8, 999).await;
    let outcome = dial_peer(b.shared.clone(), target_for(&a)).await.unwrap();
    assert_eq!(outcome, DialOutcome::Connected(a_key));
    let fresher = b.shared.catalogs.peer_catalog(&a_key).unwrap();
    assert_eq!(fresher.catalog.generation, 2);
    assert_eq!(fresher.catalog.entries.len(), 2);
    assert!(
        fresher.supersedes(&stored),
        "gen 2 links back to gen 1's catalog id"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn tampered_impersonated_and_stale_catalogs_are_rejected() {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(fed_config(&work.path().join("b")))
        .await
        .unwrap();

    let peer = IdentityKey::from_seed(&[42u8; 32]);
    let peer_key = peer.public().0;
    let entry = |name: &str, hash: u8| CatalogEntry::new(name, 10, [hash; 32], "warez", "");

    let gen1 = Catalog::new(peer_key, 1, None)
        .with_entry(entry("zap.zip", 1))
        .sign(&peer)
        .unwrap();

    // 1. A catalog from a non-approved key is refused outright.
    let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &gen1.to_bytes())
        .expect_err("non-approved peer must be refused");
    assert!(err.to_string().contains("non-approved"), "{err}");

    // Admin approves the peer; the same catalog now ingests.
    assert!(!b.shared.peers.approve(&peer_key));
    burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &gen1.to_bytes()).unwrap();
    assert_eq!(
        b.shared
            .catalogs
            .peer_catalog(&peer_key)
            .unwrap()
            .catalog
            .generation,
        1
    );

    // 2. Tampered: entries mutated after signing — signature check fails.
    let mut tampered = Catalog::new(peer_key, 2, Some(gen1.catalog_id().unwrap()))
        .with_entry(entry("zap.zip", 1))
        .sign(&peer)
        .unwrap();
    tampered.catalog.entries.push(entry("evil.exe", 66));
    let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &tampered.to_bytes())
        .expect_err("tampered catalog must be rejected");
    assert!(err.to_string().contains("rejected"), "{err}");

    // 3. Impersonation: signed by a different key — the pinned peer key does
    //    not match, so verification fails.
    let impostor = IdentityKey::from_seed(&[66u8; 32]);
    let forged = Catalog::new([0u8; 32], 2, Some(gen1.catalog_id().unwrap()))
        .with_entry(entry("free-stuff.zip", 3))
        .sign(&impostor)
        .unwrap();
    let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &forged.to_bytes())
        .expect_err("catalog signed by another key must be rejected");
    assert!(err.to_string().contains("rejected"), "{err}");

    // 4. Garbage bytes never panic, only error.
    let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &[0xff; 7])
        .expect_err("garbage must be rejected");
    assert!(err.to_string().contains("malformed"), "{err}");

    // 5. Stale: re-offering the stored generation (or older) is refused…
    let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &gen1.to_bytes())
        .expect_err("same generation is stale");
    assert!(err.to_string().contains("stale"), "{err}");

    // …while a properly-signed fresher generation supersedes.
    let gen2 = Catalog::new(peer_key, 2, Some(gen1.catalog_id().unwrap()))
        .with_entry(entry("zap.zip", 1))
        .with_entry(entry("pow.zip", 2))
        .sign(&peer)
        .unwrap();
    burrow::fed_catalog::ingest_peer_catalog(&b.shared, peer_key, &gen2.to_bytes()).unwrap();
    let stored = b.shared.catalogs.peer_catalog(&peer_key).unwrap();
    assert_eq!(stored.catalog.generation, 2);

    // The verified catalog is searchable with provenance intact.
    let resp = burrow::ctl::handle(&b.shared, &json!({"cmd": "fed-search", "terms": "pow"})).await;
    let rows = resp["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let src = &rows[0]["sources"][0];
    assert_eq!(src["server_key"], json!(hex::encode(peer_key)));
    assert_eq!(src["generation"], json!(2));

    b.shutdown().await;
}

#[tokio::test]
async fn local_catalog_generation_is_content_driven_and_survives_restart() {
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("a");
    let a = Burrow::start(fed_config(&dir)).await.unwrap();

    a.shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();
    add_file(&a, "pub", None, "one.zip", 1, 11).await;

    // First build signs generation 1; an unchanged library reuses it (same
    // id, no generation churn).
    let gen1 = burrow::fed_catalog::local_catalog(&a.shared).await.unwrap();
    assert_eq!(gen1.catalog.generation, 1);
    assert_eq!(gen1.catalog.prev_id, None);
    let again = burrow::fed_catalog::local_catalog(&a.shared).await.unwrap();
    assert_eq!(again, gen1, "unchanged library: identical signed catalog");

    // A library change bumps the generation and links the chain.
    add_file(&a, "pub", None, "two.zip", 2, 22).await;
    let gen2 = burrow::fed_catalog::local_catalog(&a.shared).await.unwrap();
    assert_eq!(gen2.catalog.generation, 2);
    assert!(gen2.supersedes(&gen1));

    // Restart on the same data dir: the persisted chain is reloaded, so an
    // unchanged library still reports generation 2 with the same id — a peer
    // holding gen 2 is never shown a "fresh" gen 1.
    a.shutdown().await;
    let a2 = Burrow::start(fed_config(&dir)).await.unwrap();
    let reloaded = burrow::fed_catalog::local_catalog(&a2.shared)
        .await
        .unwrap();
    assert_eq!(reloaded.catalog.generation, 2);
    assert_eq!(
        reloaded.catalog_id().unwrap(),
        gen2.catalog_id().unwrap(),
        "same content, same id across the restart"
    );

    a2.shutdown().await;
}
