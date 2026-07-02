//! Multi-peer swarm simulation (Wave 5.5): drive the real peer wire and
//! scheduler against a mix of honest, dead, and *adversarial* peers to prove
//! the end-to-end security and liveness properties hold together — not just
//! in the per-unit unit tests.
//!
//! The adversarial peer speaks the wire by hand and returns `STATUS_OK`
//! followed by a garbage body. It exists to prove the load-bearing claim:
//! an untrusted source can waste time but never corrupt the result, and the
//! scheduler routes around it to honest peers.

use std::path::Path;
use std::sync::Arc;

use rabbithole_identity::IdentityKey;
use rabbithole_net::tls::TlsIdentity;
use rabbithole_net::{read_framed, write_framed, Listener};
use rabbithole_swarm::peer::{PeerRequest, PeerResponseHeader, STATUS_OK};
use rabbithole_swarm::{fetch_swarm, CapToken, PeerServer, SeedStore, SourcePeer};

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 7 + 3) % 251) as u8).collect()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn honest_peer(key: &IdentityKey, root: [u8; 32], path: &Path) -> PeerServer {
    let seeds = Arc::new(SeedStore::new());
    seeds.add(root, path).unwrap();
    PeerServer::start("127.0.0.1:0".parse().unwrap(), key.public().0, seeds)
        .await
        .unwrap()
}

fn source(p: &PeerServer) -> SourcePeer {
    SourcePeer {
        endpoint: format!("127.0.0.1:{}", p.addr.port()),
        cert_fp: p.fingerprint.0,
    }
}

/// A peer that answers every request with `STATUS_OK` + `len` bytes of
/// garbage. Returns its (endpoint, fingerprint) and a shutdown handle.
struct AdversarialPeer {
    endpoint: String,
    cert_fp: [u8; 32],
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AdversarialPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn adversarial_peer(size: u64) -> AdversarialPeer {
    use rabbithole_net::quic::QuicListener;
    let tls = TlsIdentity::self_signed(&["evil".into()]).unwrap();
    let cert_fp = tls.fingerprint().0;
    let mut listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), &tls).unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let task = tokio::spawn(async move {
        while let Ok(conn) = listener.accept().await {
            tokio::spawn(async move {
                let Some(bulk) = conn.bulk() else { return };
                let _conn = conn;
                while let Ok((mut send, mut recv)) = bulk.accept().await {
                    use tokio::io::AsyncWriteExt;
                    let Ok(bytes) = read_framed(&mut recv, 8192).await else {
                        continue;
                    };
                    let Ok(req) = postcard::from_bytes::<PeerRequest>(&bytes) else {
                        continue;
                    };
                    // Claim success, then serve lies.
                    let header = PeerResponseHeader {
                        status: STATUS_OK,
                        size,
                    };
                    let _ = write_framed(&mut send, &postcard::to_allocvec(&header).unwrap()).await;
                    let garbage = vec![0xEEu8; req.len.min(64 * 1024) as usize];
                    let _ = send.write_all(&garbage).await;
                    let _ = send.shutdown().await;
                }
            });
        }
    });
    AdversarialPeer {
        endpoint,
        cert_fp,
        task,
    }
}

#[tokio::test]
async fn swarm_survives_adversarial_and_dead_peers() {
    let dir = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[71; 32]);
    // ~4 MiB + tail: five 1 MiB units.
    let body = payload(4 * 1024 * 1024 + 512);
    let src = dir.path().join("seed.bin");
    std::fs::write(&src, &body).unwrap();
    let root = *blake3::hash(&body).as_bytes();

    let honest_a = honest_peer(&key, root, &src).await;
    let honest_b = honest_peer(&key, root, &src).await;
    let evil = adversarial_peer(body.len() as u64).await;
    let token = CapToken::issue(&key, root, "tester", now() + 60)
        .unwrap()
        .to_bytes();

    // Mix: two honest peers, one lying peer, one dead endpoint.
    let sources = vec![
        source(&honest_a),
        SourcePeer {
            endpoint: evil.endpoint.clone(),
            cert_fp: evil.cert_fp,
        },
        SourcePeer {
            endpoint: "127.0.0.1:2".into(),
            cert_fp: [0; 32],
        },
        source(&honest_b),
    ];

    let dest = dir.path().join("out.bin");
    let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
        .await
        .unwrap();

    // Correct bytes despite the liar and the corpse.
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body,
        "assembled file is exact"
    );
    // The adversary contributed nothing that stuck: honest peers did the work.
    let honest_units: u64 = report
        .per_source
        .iter()
        .filter(|(e, _)| *e == sources[0].endpoint || *e == sources[3].endpoint)
        .map(|(_, n)| n)
        .sum();
    assert_eq!(honest_units, 5, "all units delivered by honest peers");

    drop(evil);
}

#[tokio::test]
async fn swarm_fails_closed_when_only_liars_remain() {
    let dir = tempfile::tempdir().unwrap();
    let body = payload(2 * 1024 * 1024);
    let root = *blake3::hash(&body).as_bytes();
    let evil = adversarial_peer(body.len() as u64).await;

    // The token needn't be valid to the adversary — it ignores it — but the
    // fetcher still can't get good bytes from a liar, so the fetch fails
    // rather than writing corruption.
    let sources = vec![SourcePeer {
        endpoint: evil.endpoint.clone(),
        cert_fp: evil.cert_fp,
    }];
    let dest = dir.path().join("out.bin");
    let result = fetch_swarm(&sources, &[1, 2, 3], root, body.len() as u64, &dest).await;
    assert!(
        result.is_err(),
        "a swarm of only liars must fail, never write bad bytes"
    );

    drop(evil);
}
