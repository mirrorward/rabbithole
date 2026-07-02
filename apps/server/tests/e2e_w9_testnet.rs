//! Wave 9 end-to-end tests: a three-burrow CI testnet exercising the S2S
//! federation layer as it exists **today** — authenticated peering with
//! admin approval, plus signed-catalog announce/get/verify where sync is
//! strictly **dialer-pull** (see `apps/server/src/federation.rs`). Building
//! on the two-burrow harness in `e2e_w9_federation.rs` / `e2e_w9_catalog.rs`,
//! this file proves at three-node scale:
//!
//! - **full mesh**: with all six directed edges approved and dialed, every
//!   burrow's federated-search surface sees all three libraries with correct
//!   provenance (server name, key, generation, local flag);
//! - **no transitive relay**: on an A–B–C chain, C's catalog reaches B but
//!   never A — a burrow serves only its *own* signed catalog over
//!   `MT_CATALOG_GET`, so catalogs travel exactly one hop. That is the
//!   current contract, asserted here so a future flood-fill changes this
//!   test deliberately;
//! - **partition + rejoin**: killing the middle node leaves the outer nodes
//!   serving their retained catalogs without errors (peer catalogs are
//!   in-memory and are not evicted on disconnect — also the current
//!   contract); a replacement burrow on the same data dir keeps its identity,
//!   its approvals, and its catalog generation chain, and re-peering
//!   converges on the fresher generation;
//! - **dupe storms are idempotent**: re-announcing/replaying the same
//!   catalog generation — over redials or by direct ingest — never
//!   duplicates stored state, never regresses the generation, and never
//!   duplicates search rows;
//! - **stale replays are refused mesh-wide**: once every holder has a newer
//!   generation, replaying an older (or equal) generation is refused by each
//!   of them independently.
//!
//! Honest scope note: there is **no board-event flood-fill over S2S yet**,
//! so a "dupe storm" here targets what exists — catalog re-sync idempotence
//! (generation staleness in `fed_catalog::ingest_peer_catalog`) — plus the
//! in-server [`DedupStore`] gate that will back event propagation when it
//! lands (it is not yet wired into the S2S path).
//!
//! Determinism is by real readiness signals: `dial_peer` completes the
//! handshake *and* the catalog sync before returning `Connected`, so there
//! are no sleeps or polls; the only timer is a bounded timeout around a dial
//! to a dead peer, which must simply not produce a session.

use burrow::federation::{dial_peer, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_server_core::{PeerState, SeenKey, ServerConfig};
use serde_json::{json, Value};

fn fed_config(name: &str, dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: name.into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn start(name: &str, dir: &std::path::Path) -> Burrow {
    Burrow::start(fed_config(name, dir)).await.unwrap()
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
async fn add_file(b: &Burrow, area: &str, name: &str, hash: u8) {
    b.shared
        .files
        .add_file(
            area,
            None,
            name,
            &[hash; 32],
            100 + hash as i64,
            "application/zip",
            "disk",
            "",
            "op@warren",
            1,
        )
        .await
        .unwrap();
}

/// Give `b` a public library of exactly one file.
async fn publish_one(b: &Burrow, name: &str, hash: u8) {
    b.shared
        .files
        .create_area("stash", "Stash", "")
        .await
        .unwrap();
    add_file(b, "stash", name, hash).await;
}

/// Admin-approve `key` on `on` through the audited ctl path.
async fn approve(on: &Burrow, key: [u8; 32]) {
    let resp = burrow::ctl::handle(
        &on.shared,
        &json!({"cmd": "peer-approve", "key": hex::encode(key)}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "peer-approve accepted: {resp}");
}

/// Dial `listener` from `dialer` and require a live (approved) session.
async fn connect(dialer: &Burrow, listener: &Burrow) {
    let outcome = dial_peer(dialer.shared.clone(), target_for(listener))
        .await
        .unwrap();
    assert_eq!(outcome, DialOutcome::Connected(listener.shared.server_key));
}

/// Run `fed-search` over ctl and return the deduped result rows.
async fn fed_search(b: &Burrow, terms: &str) -> Vec<Value> {
    let resp = burrow::ctl::handle(&b.shared, &json!({"cmd": "fed-search", "terms": terms})).await;
    assert_eq!(resp["ok"], json!(true), "fed-search ok: {resp}");
    resp["data"].as_array().unwrap().to_vec()
}

/// The sorted file names a burrow's federated search surfaces for `terms`.
/// Every test file has a distinct hash, so each row carries one source.
async fn search_names(b: &Burrow, terms: &str) -> Vec<String> {
    let mut names: Vec<String> = fed_search(b, terms)
        .await
        .iter()
        .map(|row| {
            let sources = row["sources"].as_array().unwrap();
            assert_eq!(sources.len(), 1, "distinct hashes: one source per row");
            sources[0]["name"].as_str().unwrap().to_string()
        })
        .collect();
    names.sort();
    names
}

/// The single source record for the row named `name`.
fn source_of<'a>(rows: &'a [Value], name: &str) -> &'a Value {
    rows.iter()
        .map(|row| &row["sources"][0])
        .find(|s| s["name"] == json!(name))
        .unwrap_or_else(|| panic!("no search row for {name}"))
}

/// Full mesh A/B/C: all six directed edges approved and dialed. Because
/// catalog sync is dialer-pull, full mutual visibility needs every directed
/// edge — after which each burrow's fed-search sees all three libraries with
/// correct provenance.
#[tokio::test]
async fn three_server_full_mesh() {
    let work = tempfile::tempdir().unwrap();
    let a = start("Warren A", &work.path().join("a")).await;
    let b = start("Warren B", &work.path().join("b")).await;
    let c = start("Warren C", &work.path().join("c")).await;
    publish_one(&a, "alpha-pack.zip", 1).await;
    publish_one(&b, "bravo-pack.zip", 2).await;
    publish_one(&c, "charlie-pack.zip", 3).await;

    let (a_key, b_key, c_key) = (
        a.shared.server_key,
        b.shared.server_key,
        c.shared.server_key,
    );

    // Every listener approves every dialer (audited ctl path), then all six
    // directed edges dial: each dialer pulls each listener's catalog.
    for (on, keys) in [
        (&a, [b_key, c_key]),
        (&b, [a_key, c_key]),
        (&c, [a_key, b_key]),
    ] {
        for key in keys {
            approve(on, key).await;
        }
    }
    for (dialer, listener) in [(&a, &b), (&a, &c), (&b, &a), (&b, &c), (&c, &a), (&c, &b)] {
        connect(dialer, listener).await;
    }
    assert_eq!(a.shared.peers.state(&b_key), Some(PeerState::Connected));
    assert_eq!(a.shared.peers.state(&c_key), Some(PeerState::Connected));
    assert_eq!(b.shared.peers.state(&c_key), Some(PeerState::Connected));

    let all = vec![
        "alpha-pack.zip".to_string(),
        "bravo-pack.zip".to_string(),
        "charlie-pack.zip".to_string(),
    ];
    for srv in [&a, &b, &c] {
        assert_eq!(
            search_names(srv, "pack").await,
            all,
            "{} sees the whole mesh",
            srv.shared.config.read().name
        );
        // Three catalogs held: its own plus one per peer, one entry each.
        let resp = burrow::ctl::handle(&srv.shared, &json!({"cmd": "fed-catalogs"})).await;
        let cats = resp["data"].as_array().unwrap();
        assert_eq!(cats.len(), 3, "own catalog + two peers: {resp}");
        assert!(cats
            .iter()
            .all(|r| r["generation"] == json!(1) && r["entries"] == json!(1)));
    }

    // Provenance, as seen from A: remote rows carry the advertising server's
    // name, key, and generation; the local row is flagged local.
    let rows = fed_search(&a, "pack").await;
    let from_c = source_of(&rows, "charlie-pack.zip");
    assert_eq!(from_c["server"], json!("Warren C"));
    assert_eq!(from_c["server_key"], json!(hex::encode(c_key)));
    assert_eq!(from_c["generation"], json!(1));
    assert_eq!(from_c["area"], json!("stash"));
    assert_eq!(from_c["local"], json!(false));
    let from_b = source_of(&rows, "bravo-pack.zip");
    assert_eq!(from_b["server"], json!("Warren B"));
    assert_eq!(from_b["local"], json!(false));
    let own = source_of(&rows, "alpha-pack.zip");
    assert_eq!(own["local"], json!(true));

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// Chain topology A–B–C, then partition B out and rejoin a replacement.
/// Documents today's contracts: catalogs travel one hop (no transitive
/// relay), retained peer catalogs keep serving through a partition, and a
/// same-data-dir replacement resumes identity, approvals, and the catalog
/// generation chain.
#[tokio::test]
async fn partition_rejoin() {
    let work = tempfile::tempdir().unwrap();
    let b_dir = work.path().join("b");
    let a = start("Warren A", &work.path().join("a")).await;
    let b = start("Warren B", &b_dir).await;
    let c = start("Warren C", &work.path().join("c")).await;
    publish_one(&a, "alpha-pack.zip", 1).await;
    publish_one(&b, "bravo-pack.zip", 2).await;
    publish_one(&c, "charlie-pack.zip", 3).await;

    let (a_key, b_key, c_key) = (
        a.shared.server_key,
        b.shared.server_key,
        c.shared.server_key,
    );

    // Chain edges only: A<->B and B<->C. No A<->C relationship exists.
    approve(&a, b_key).await;
    approve(&b, a_key).await;
    approve(&b, c_key).await;
    approve(&c, b_key).await;
    connect(&a, &b).await;
    connect(&b, &a).await;
    connect(&b, &c).await;
    connect(&c, &b).await;

    // The middle node sees everything; the outer nodes see one hop only.
    // C's catalog reached B but NOT A: a burrow serves only its own signed
    // catalog, so there is no transitive relay today. This assertion pins
    // that contract.
    assert_eq!(
        search_names(&b, "pack").await,
        ["alpha-pack.zip", "bravo-pack.zip", "charlie-pack.zip"]
    );
    assert_eq!(
        search_names(&a, "pack").await,
        ["alpha-pack.zip", "bravo-pack.zip"],
        "no transitive relay: C's catalog must not reach A through B"
    );
    assert_eq!(
        search_names(&c, "pack").await,
        ["bravo-pack.zip", "charlie-pack.zip"]
    );
    assert!(a.shared.catalogs.peer_catalog(&c_key).is_none());
    assert!(c.shared.catalogs.peer_catalog(&a_key).is_none());

    // Remember what A holds of B before the partition (generation 1).
    let b_gen1_at_a = a.shared.catalogs.peer_catalog(&b_key).unwrap();
    assert_eq!(b_gen1_at_a.catalog.generation, 1);

    // ---- Partition: the middle node goes away. ----
    let dead_target = target_for(&b);
    b.shutdown().await;

    // The outer nodes keep serving without errors. Retained peer catalogs
    // are in-memory and not evicted on disconnect — the partition does not
    // shrink the search surface (current contract).
    assert_eq!(
        search_names(&a, "pack").await,
        ["alpha-pack.zip", "bravo-pack.zip"]
    );
    assert_eq!(
        search_names(&c, "pack").await,
        ["bravo-pack.zip", "charlie-pack.zip"]
    );

    // Dialing the dead node never yields a session: it either errors or
    // hangs past a bounded window (its accept loop is gone).
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        dial_peer(a.shared.clone(), dead_target),
    )
    .await;
    match res {
        Err(_) | Ok(Err(_)) => {} // timed out, or the dial itself errored
        Ok(Ok(outcome)) => panic!("dial to a dead peer must not succeed: {outcome:?}"),
    }

    // ---- Rejoin: a replacement burrow on the same data dir. ----
    let b2 = start("Warren B", &b_dir).await;
    assert_eq!(b2.shared.server_key, b_key, "identity persisted on disk");
    assert!(
        b2.shared.peers.is_approved(&a_key) && b2.shared.peers.is_approved(&c_key),
        "admin approvals reloaded from disk"
    );

    // Its library grew while it was away: the persisted catalog chain must
    // resume at generation 2, not restart at 1.
    add_file(&b2, "stash", "bravo-extra-pack.zip", 4).await;

    // Re-peer all four chain edges. A and C never restarted, so their
    // in-memory approvals of B still stand; B' pulls their catalogs afresh
    // (its peer-catalog store is in-memory only and starts empty).
    connect(&a, &b2).await;
    connect(&c, &b2).await;
    connect(&b2, &a).await;
    connect(&b2, &c).await;

    let b_gen2_at_a = a.shared.catalogs.peer_catalog(&b_key).unwrap();
    assert_eq!(
        b_gen2_at_a.catalog.generation, 2,
        "generation chain survived the outage"
    );
    assert!(
        b_gen2_at_a.supersedes(&b_gen1_at_a),
        "gen 2 links back to the pre-partition catalog id"
    );

    // Converged: everyone sees the fresher B library; still no relay of the
    // far end across the chain.
    assert_eq!(
        search_names(&b2, "pack").await,
        [
            "alpha-pack.zip",
            "bravo-extra-pack.zip",
            "bravo-pack.zip",
            "charlie-pack.zip"
        ]
    );
    assert_eq!(
        search_names(&a, "pack").await,
        ["alpha-pack.zip", "bravo-extra-pack.zip", "bravo-pack.zip"]
    );
    assert_eq!(
        search_names(&c, "pack").await,
        ["bravo-extra-pack.zip", "bravo-pack.zip", "charlie-pack.zip"]
    );

    a.shutdown().await;
    b2.shutdown().await;
    c.shutdown().await;
}

/// A dupe storm against the catalog path: repeated redials re-announce the
/// same generation, and direct replays re-offer the same signed bytes. The
/// stored peer catalog never duplicates, the generation never regresses,
/// and the search surface stays byte-for-byte stable.
#[tokio::test]
async fn dupe_storm_announce() {
    let work = tempfile::tempdir().unwrap();
    let a = start("Warren A", &work.path().join("a")).await;
    let b = start("Warren B", &work.path().join("b")).await;
    publish_one(&a, "storm-pack.zip", 7).await;

    let (a_key, b_key) = (a.shared.server_key, b.shared.server_key);
    approve(&a, b_key).await;
    connect(&b, &a).await;

    let first = b.shared.catalogs.peer_catalog(&a_key).unwrap();
    assert_eq!(first.catalog.generation, 1);
    let first_id = first.catalog_id().unwrap();
    let baseline = fed_search(&b, "storm").await;
    assert_eq!(baseline.len(), 1);

    // Announce storm: eight redials, each re-announcing generation 1. The
    // dialer's `wants` check makes every re-sync a no-op fetch.
    for _ in 0..8 {
        connect(&b, &a).await;
    }
    let after = b.shared.catalogs.peer_catalog(&a_key).unwrap();
    assert_eq!(after.catalog.generation, 1, "generation did not churn");
    assert_eq!(after.catalog_id().unwrap(), first_id, "same signed catalog");
    assert_eq!(
        b.shared.catalogs.peer_catalogs().len(),
        1,
        "one peer, one stored catalog — the storm did not duplicate rows"
    );

    // Replay storm: force-feeding the identical signed bytes bypassing the
    // announce short-circuit is refused as stale every single time.
    let replay = first.to_bytes();
    for _ in 0..5 {
        let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, a_key, &replay)
            .expect_err("same-generation replay is a dupe");
        assert!(err.to_string().contains("stale"), "{err}");
    }
    assert_eq!(
        fed_search(&b, "storm").await,
        baseline,
        "search results unchanged by the storm"
    );

    // The generation is monotonic under real change + continued replay: a
    // fresher catalog supersedes, after which the old bytes stay refused.
    add_file(&a, "stash", "storm-pack-2.zip", 8).await;
    connect(&b, &a).await;
    assert_eq!(
        b.shared
            .catalogs
            .peer_catalog(&a_key)
            .unwrap()
            .catalog
            .generation,
        2
    );
    let err = burrow::fed_catalog::ingest_peer_catalog(&b.shared, a_key, &replay)
        .expect_err("gen-1 replay against a gen-2 store");
    assert!(err.to_string().contains("stale"), "{err}");
    assert_eq!(fed_search(&b, "storm").await.len(), 2, "no duplicate rows");

    // The in-server dupe gate that will back S2S event flood-fill when it
    // lands (there is no board-event propagation over S2S yet — catalog sync
    // dedupes by generation instead): first sighting acts, replays drop.
    let key = SeenKey::Event([9u8; 32]);
    assert!(b.shared.dedup.check_and_record(key.clone(), 1_000));
    assert!(!b.shared.dedup.check_and_record(key.clone(), 1_001));
    assert!(b.shared.dedup.seen(&key));

    a.shutdown().await;
    b.shutdown().await;
}

/// Three-node staleness: once every holder in the mesh has C's generation 2,
/// replaying the (properly signed) generation 1 — or generation 2 itself —
/// is refused by each holder independently, and provenance stays at gen 2.
#[tokio::test]
async fn stale_generation_refused_meshwide() {
    let work = tempfile::tempdir().unwrap();
    let a = start("Warren A", &work.path().join("a")).await;
    let b = start("Warren B", &work.path().join("b")).await;
    let c = start("Warren C", &work.path().join("c")).await;
    publish_one(&c, "relic-pack.zip", 5).await;

    let (a_key, b_key, c_key) = (
        a.shared.server_key,
        b.shared.server_key,
        c.shared.server_key,
    );
    approve(&c, a_key).await;
    approve(&c, b_key).await;
    connect(&a, &c).await;
    connect(&b, &c).await;

    // Both holders synced the same signed generation-1 catalog.
    let gen1_at_a = a.shared.catalogs.peer_catalog(&c_key).unwrap();
    let gen1_at_b = b.shared.catalogs.peer_catalog(&c_key).unwrap();
    assert_eq!(gen1_at_a, gen1_at_b, "identical signed catalog at both");
    assert_eq!(gen1_at_a.catalog.generation, 1);

    // C's library grows; both holders re-sync to generation 2.
    add_file(&c, "stash", "relic-pack-2.zip", 6).await;
    connect(&a, &c).await;
    connect(&b, &c).await;

    let old = gen1_at_a.to_bytes();
    for holder in [&a, &b] {
        let stored = holder.shared.catalogs.peer_catalog(&c_key).unwrap();
        assert_eq!(stored.catalog.generation, 2);

        // The gen-1 replay is authentic (properly signed by C) but old:
        // refused on staleness alone.
        let err = burrow::fed_catalog::ingest_peer_catalog(&holder.shared, c_key, &old)
            .expect_err("old-generation replay must be refused");
        assert!(err.to_string().contains("stale"), "{err}");

        // Re-offering the current generation is refused too (idempotence).
        let err =
            burrow::fed_catalog::ingest_peer_catalog(&holder.shared, c_key, &stored.to_bytes())
                .expect_err("same-generation replay must be refused");
        assert!(err.to_string().contains("stale"), "{err}");

        // The store and the search surface still speak generation 2.
        assert_eq!(
            holder
                .shared
                .catalogs
                .peer_catalog(&c_key)
                .unwrap()
                .catalog
                .generation,
            2
        );
        let rows = fed_search(holder, "relic").await;
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let src = &row["sources"][0];
            assert_eq!(src["server"], json!("Warren C"));
            assert_eq!(src["generation"], json!(2));
        }
    }

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}
