//! Wave 5 multi-peer simulation harness: swarm fetches under adversarial
//! conditions — many seeders, dead and wrong-fingerprint sources (lossy
//! links), and actively corrupting peers (corruption injection).
//!
//! The malicious peers are implemented inline against the raw
//! `rabbithole-net` QUIC surface: they speak just enough of the peer wire
//! (framed request in, framed `PeerResponseHeader` out) to look plausible,
//! then misbehave in the body. The invariant under test is the peer wire's
//! core promise: an adversarial peer can waste a fetcher's time, but never
//! lands a wrong byte — and a swarm with any honest capacity still
//! completes exactly.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rabbithole_identity::IdentityKey;
use rabbithole_net::quic::QuicListener;
use rabbithole_net::tls::{CertFingerprint, TlsIdentity};
use rabbithole_net::{read_framed, write_framed, BulkRecv, BulkSend, Listener, NetError};
use rabbithole_swarm::cap::CapToken;
use rabbithole_swarm::peer::{
    fetch_range, PeerError, PeerRequest, PeerResponseHeader, PeerServer, SeedStore, STATUS_OK,
};
use rabbithole_swarm::scheduler::{fetch_swarm, fetch_swarm_resumable, rhstate_path, SourcePeer};

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn token_for(key: &IdentityKey, root: [u8; 32]) -> Vec<u8> {
    CapToken::issue(key, root, "sim-tester", now() + 300)
        .unwrap()
        .to_bytes()
}

fn source_of(p: &PeerServer) -> SourcePeer {
    SourcePeer {
        endpoint: format!("127.0.0.1:{}", p.addr.port()),
        cert_fp: p.fingerprint.0,
    }
}

/// Write `body` under `dir` and start `n` honest seeders all serving it.
/// One shared [`SeedStore`], so the Bao outboard is computed once.
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
            PeerServer::start(
                "127.0.0.1:0".parse().unwrap(),
                key.public().0,
                seeds.clone(),
            )
            .await
            .unwrap(),
        );
    }
    (root, peers)
}

/// How a malicious peer sabotages the response body (the header is always
/// a well-formed `STATUS_OK` with the true file size — the lie is below it).
#[derive(Clone, Copy)]
enum Sabotage {
    /// A plausibly-sized body of garbage bytes instead of a Bao stream.
    Garbage,
    /// A few bytes, then the stream dies — a mid-transfer link drop.
    Truncate,
}

/// A raw QUIC endpoint speaking just enough peer wire to inject corruption.
/// Mirrors `PeerServer`'s accept loop (control stream first, then requests
/// as bulk bi-streams) so a fetcher can't tell it apart until the body.
struct MaliciousPeer {
    addr: SocketAddr,
    fingerprint: CertFingerprint,
    /// Requests that got as far as our lying response.
    served: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl MaliciousPeer {
    async fn start(size: u64, mode: Sabotage) -> MaliciousPeer {
        let tls = TlsIdentity::self_signed(&["peer".into()]).unwrap();
        let fingerprint = tls.fingerprint();
        let mut listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), &tls).unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::new(AtomicU64::new(0));
        let counter = served.clone();
        let task = tokio::spawn(async move {
            while let Ok(conn) = listener.accept().await {
                let counter = counter.clone();
                tokio::spawn(async move {
                    let Some(bulk) = conn.bulk() else { return };
                    let _conn = conn;
                    while let Ok((send, recv)) = bulk.accept().await {
                        let counter = counter.clone();
                        tokio::spawn(async move {
                            let _ = sabotage_stream(send, recv, size, mode, counter).await;
                        });
                    }
                });
            }
        });
        MaliciousPeer {
            addr,
            fingerprint,
            served,
            task,
        }
    }

    fn source(&self) -> SourcePeer {
        SourcePeer {
            endpoint: format!("127.0.0.1:{}", self.addr.port()),
            cert_fp: self.fingerprint.0,
        }
    }
}

impl Drop for MaliciousPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn sabotage_stream(
    mut send: BulkSend,
    mut recv: BulkRecv,
    size: u64,
    mode: Sabotage,
    served: Arc<AtomicU64>,
) -> Result<(), NetError> {
    use tokio::io::AsyncWriteExt;

    let bytes = read_framed(&mut recv, 8192).await?;
    let Ok(req) = postcard::from_bytes::<PeerRequest>(&bytes) else {
        return Ok(());
    };
    served.fetch_add(1, Ordering::SeqCst);

    // A perfectly valid framed header claiming success and the true size.
    let header = postcard::to_allocvec(&PeerResponseHeader {
        status: STATUS_OK,
        size,
    })
    .unwrap();
    write_framed(&mut send, &header).await?;

    match mode {
        Sabotage::Garbage => {
            // Roughly the size a real Bao stream for this range would be,
            // but patterned junk: every leaf fails its hash check.
            let n = req.len.min(size) as usize + 4096;
            let junk: Vec<u8> = (0..n)
                .map(|i| (i.wrapping_mul(31) % 251) as u8 ^ 0x5A)
                .collect();
            send.write_all(&junk).await?;
        }
        Sabotage::Truncate => {
            send.write_all(&[0xEE; 16]).await?;
        }
    }
    send.shutdown().await?;
    Ok(())
}

/// Ten seeders, ~8 MiB: the fetch completes byte-exact, every unit is
/// counted exactly once, and the work actually spreads across the swarm.
#[tokio::test]
#[ignore = "heavy multi-peer QUIC soak/adversarial test; reliable locally but flaky under constrained CI cross-binary parallelism. Run with: cargo test -p rabbithole-swarm --test sim -- --ignored --test-threads=1"]
async fn ten_peer_swarm() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[21; 32]);
    // 8 MiB plus a tail → nine work units across ten seeders.
    let body = payload(8 * 1024 * 1024 + 4321);
    let (root, peers) = honest_swarm(&key, &body, dir.path(), 10).await;
    let sources: Vec<SourcePeer> = peers.iter().map(source_of).collect();
    let token = token_for(&key, root);

    let dest = dir.path().join("out.bin");
    let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
        .await
        .unwrap();
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), body, "reassembled exactly");
    let total: u64 = report.per_source.iter().map(|(_, n)| n).sum();
    assert_eq!(
        total, 9,
        "each unit exactly once, endgame duplicates uncounted: {:?}",
        report.per_source
    );
    let active = report.per_source.iter().filter(|(_, n)| *n > 0).count();
    assert!(
        active >= 3,
        "work spread across the swarm: {:?}",
        report.per_source
    );
}

/// Ten sources where seven are hopeless — four dead endpoints (nothing
/// listening) and three live endpoints pinned to the wrong fingerprint —
/// still complete via the three real seeders.
#[tokio::test]
#[ignore = "heavy multi-peer QUIC soak/adversarial test; reliable locally but flaky under constrained CI cross-binary parallelism. Run with: cargo test -p rabbithole-swarm --test sim -- --ignored --test-threads=1"]
async fn flaky_majority() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[22; 32]);
    let body = payload(4 * 1024 * 1024 + 99); // five units
    let (root, live) = honest_swarm(&key, &body, dir.path(), 3).await;
    let token = token_for(&key, root);

    // Four dead endpoints: nothing listens on these ports.
    let mut sources: Vec<SourcePeer> = (1..=4)
        .map(|port| SourcePeer {
            endpoint: format!("127.0.0.1:{port}"),
            cert_fp: [port as u8; 32],
        })
        .collect();
    // Three live endpoints pinned to a wrong fingerprint: the TLS handshake
    // fails, so these can never serve a byte.
    for p in &live {
        sources.push(SourcePeer {
            endpoint: format!("127.0.0.1:{}", p.addr.port()),
            cert_fp: [0xBB; 32],
        });
    }
    let live_sources: Vec<SourcePeer> = live.iter().map(source_of).collect();
    sources.extend(live_sources.iter().cloned());
    assert_eq!(sources.len(), 10);

    let dest = dir.path().join("out.bin");
    let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body, "reassembled exactly");
    let total: u64 = report.per_source.iter().map(|(_, n)| n).sum();
    assert_eq!(total, 5, "the three live seeders carried every unit");
    // Only genuinely live entries completed units. (Wrong-fingerprint
    // entries share an endpoint string with live ones but always report 0,
    // so any n > 0 row here is a correctly-pinned live source.)
    let live_endpoints: Vec<&str> = live_sources.iter().map(|s| s.endpoint.as_str()).collect();
    for (endpoint, n) in &report.per_source {
        if *n > 0 {
            assert!(
                live_endpoints.contains(&endpoint.as_str()),
                "unit credited to a hopeless source: {endpoint}"
            );
        }
    }
}

/// A peer that answers a valid OK header and then streams garbage instead
/// of a Bao stream. (a) Fetching from it alone fails verification — never
/// wrong bytes. (b) In a swarm with honest peers the fetch completes
/// byte-exact; the corruptor is consulted, defeated, and credited nothing.
#[tokio::test]
#[ignore = "heavy multi-peer QUIC soak/adversarial test; reliable locally but flaky under constrained CI cross-binary parallelism. Run with: cargo test -p rabbithole-swarm --test sim -- --ignored --test-threads=1"]
async fn corrupting_peer() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[23; 32]);
    let body = payload(3 * 1024 * 1024 + 7); // four units
    let (root, honest) = honest_swarm(&key, &body, dir.path(), 3).await;
    let token = token_for(&key, root);
    let mallory = MaliciousPeer::start(body.len() as u64, Sabotage::Garbage).await;

    // (a) Alone: the garbage stream must produce an error, never bytes.
    let err = fetch_range(
        &mallory.source().endpoint,
        mallory.fingerprint.0,
        &token,
        root,
        0,
        64 * 1024,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            PeerError::Verify(_) | PeerError::Io(_) | PeerError::Net(_)
        ),
        "garbage must fail verification, got: {err}"
    );
    assert_eq!(
        mallory.served.load(Ordering::SeqCst),
        1,
        "the request reached the corruptor and it answered"
    );

    // (b) In a swarm: mallory listed first so its worker grabs a unit,
    // fails it, and hands it back to the honest seeders.
    let mut sources = vec![mallory.source()];
    sources.extend(honest.iter().map(source_of));
    let dest = dir.path().join("out.bin");
    let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body,
        "corruption never lands a byte"
    );
    let total: u64 = report.per_source.iter().map(|(_, n)| n).sum();
    assert_eq!(total, 4);
    let mallory_units = report
        .per_source
        .iter()
        .find(|(e, _)| *e == mallory.source().endpoint)
        .map(|(_, n)| *n)
        .unwrap();
    assert_eq!(mallory_units, 0, "the corruptor is credited nothing");
    assert!(
        mallory.served.load(Ordering::SeqCst) >= 2,
        "the swarm consulted the corruptor too"
    );
}

/// A peer that sends a valid header and then drops the stream after a few
/// bytes — a lossy link cut mid-transfer. Alone it errors; a swarm with
/// honest capacity absorbs it.
#[tokio::test]
#[ignore = "heavy multi-peer QUIC soak/adversarial test; reliable locally but flaky under constrained CI cross-binary parallelism. Run with: cargo test -p rabbithole-swarm --test sim -- --ignored --test-threads=1"]
async fn truncating_peer_mid_stream() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[24; 32]);
    let body = payload(2 * 1024 * 1024 + 1); // three units
    let (root, honest) = honest_swarm(&key, &body, dir.path(), 2).await;
    let token = token_for(&key, root);
    let mallory = MaliciousPeer::start(body.len() as u64, Sabotage::Truncate).await;

    let err = fetch_range(
        &mallory.source().endpoint,
        mallory.fingerprint.0,
        &token,
        root,
        0,
        64 * 1024,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            PeerError::Verify(_) | PeerError::Io(_) | PeerError::Net(_)
        ),
        "a cut stream must fail, got: {err}"
    );

    let mut sources = vec![mallory.source()];
    sources.extend(honest.iter().map(source_of));
    let dest = dir.path().join("out.bin");
    let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body, "reassembled exactly");
    let mallory_units = report
        .per_source
        .iter()
        .find(|(e, _)| *e == mallory.source().endpoint)
        .map(|(_, n)| *n)
        .unwrap();
    assert_eq!(mallory_units, 0);
}

/// The resumable path under the full zoo — honest seeders plus a garbage
/// corruptor plus a dead endpoint: completes, the whole file hash-verifies
/// against the root, and the `.rhstate` file is cleaned up.
#[tokio::test]
#[ignore = "heavy multi-peer QUIC soak/adversarial test; reliable locally but flaky under constrained CI cross-binary parallelism. Run with: cargo test -p rabbithole-swarm --test sim -- --ignored --test-threads=1"]
async fn resumable_swarm_survives_adversaries() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[25; 32]);
    let body = payload(3 * 1024 * 1024); // three units
    let (root, honest) = honest_swarm(&key, &body, dir.path(), 2).await;
    let token = token_for(&key, root);
    let mallory = MaliciousPeer::start(body.len() as u64, Sabotage::Garbage).await;

    let mut sources = vec![
        mallory.source(),
        SourcePeer {
            endpoint: "127.0.0.1:1".into(),
            cert_fp: [0; 32],
        },
    ];
    sources.extend(honest.iter().map(source_of));

    let dest = dir.path().join("out.bin");
    let report = fetch_swarm_resumable(&sources, &token, root, body.len() as u64, &dest)
        .await
        .unwrap();
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), body, "reassembled exactly");
    assert!(
        !rhstate_path(&dest).exists(),
        "resume state removed on success"
    );
}
