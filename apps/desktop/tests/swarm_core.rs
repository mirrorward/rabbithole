//! Slice 1 of the native swarm backend: prove the in-process swarm engine does
//! a REAL multi-source, Bao-verified fetch from *inside the desktop crate's dep
//! graph* (swarm + quinn/rustls linked alongside tauri's wry/webkit). No server,
//! no Tauri, no webview — the pure engine driven from the real desktop build, so
//! it verifies fully headlessly. Mirrors the harness in `crates/swarm/tests/sim.rs`,
//! kept small (few peers, few units) so it runs reliably rather than as a soak.

use std::path::Path;
use std::sync::Arc;

use rabbithole_identity::IdentityKey;
use rabbithole_swarm::cap::CapToken;
use rabbithole_swarm::peer::{PeerServer, SeedStore};
use rabbithole_swarm::scheduler::{fetch_swarm_resumable, rhstate_path, SourcePeer};

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn source_of(p: &PeerServer) -> SourcePeer {
    SourcePeer {
        endpoint: format!("127.0.0.1:{}", p.addr.port()),
        cert_fp: p.fingerprint.0,
    }
}

/// Write `body` and start `n` honest seeders all serving it from one `SeedStore`.
async fn honest_swarm(
    key: &IdentityKey,
    body: &[u8],
    dir: &Path,
    n: usize,
) -> ([u8; 32], Vec<PeerServer>) {
    let path = dir.join("seed.bin");
    std::fs::write(&path, body).unwrap();
    let root = *blake3::hash(body).as_bytes();
    let seeds = Arc::new(SeedStore::new());
    seeds.add(root, &path).unwrap();
    let mut peers = Vec::with_capacity(n);
    for _ in 0..n {
        peers.push(
            PeerServer::start("127.0.0.1:0".parse().unwrap(), key.public().0, seeds.clone())
                .await
                .unwrap(),
        );
    }
    (root, peers)
}

/// The core promise, exercised from the desktop crate: three honest seeders and
/// one dead endpoint deliver a multi-unit file byte-exact, work spreads across
/// the swarm, and the resumable state is cleaned up on success.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_core_fetches_multisource() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[42; 32]);
    // ~3 MiB + tail → four 1 MiB work units, so the swarm has something to split.
    let body = payload(3 * 1024 * 1024 + 17);
    let (root, seeders) = honest_swarm(&key, &body, dir.path(), 3).await;
    let token = CapToken::issue(&key, root, "desktop-test", now() + 300)
        .unwrap()
        .to_bytes();

    // Three live seeders plus one dead endpoint (nothing listening): the fetch
    // must route around it.
    let mut sources: Vec<SourcePeer> = seeders.iter().map(source_of).collect();
    sources.push(SourcePeer {
        endpoint: "127.0.0.1:1".into(),
        cert_fp: [0; 32],
    });

    let dest = dir.path().join("out.bin");
    let report = fetch_swarm_resumable(&sources, &token, root, body.len() as u64, &dest)
        .await
        .expect("swarm fetch completes");

    // Byte-exact reassembly (every 16 KiB block was Bao-verified against `root`).
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), body, "reassembled exactly");
    // Every unit accounted for exactly once.
    let total: u64 = report.per_source.iter().map(|(_, n)| n).sum();
    assert_eq!(total, 4, "four units, each served once: {:?}", report.per_source);
    // The dead endpoint carried nothing; a live seeder did.
    assert!(
        report
            .per_source
            .iter()
            .any(|(e, n)| e != "127.0.0.1:1" && *n > 0),
        "a live seeder served units: {:?}",
        report.per_source
    );
    // Resume state removed on success (a completed fetch caches nothing; the
    // `.rhstate` file exists only to resume an *interrupted* fetch — that path
    // is covered by crates/swarm's own sim tests).
    assert!(!rhstate_path(&dest).exists(), "resume state cleaned up");
}
