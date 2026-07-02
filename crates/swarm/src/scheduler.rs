//! Multi-source fetch scheduling (Wave 5.4).
//!
//! Splits a file into fixed work units and drains them through one worker
//! per source. Scheduling is **work-stealing**: each worker pulls the next
//! unit the moment it finishes its last, so faster peers naturally carry
//! more of the file — that *is* the per-source speed assignment, with no
//! rate estimation to go stale. A failing source pushes its unit back and
//! retires; the fetch survives as long as one source can serve. When the
//! queue drains, idle workers enter **endgame** and duplicate units still
//! in flight elsewhere (verified writes are idempotent, so first-done wins
//! and a stalled peer can't hold the tail hostage).
//!
//! Rarest-first ordering operates *across* files (which root to fetch
//! next, from the coordinator's source counts); within one file every unit
//! is equally available from every source that has the file, so there is
//! nothing to order by rarity here.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::peer::{fetch_range, PeerError, PEER_REQUEST_MAX};

/// One fetchable source for a root (from a `SourceList` entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePeer {
    pub endpoint: String,
    pub cert_fp: [u8; 32],
}

/// How a swarm fetch went: total bytes plus per-source unit counts
/// (endpoint, units served) — the visibility a UI needs.
#[derive(Debug, Clone, Default)]
pub struct FetchReport {
    pub bytes: u64,
    pub per_source: Vec<(String, u64)>,
}

/// Work-unit size: 1 MiB balances distribution granularity against
/// per-request overhead (and stays under [`PEER_REQUEST_MAX`]).
pub const UNIT_SIZE: u64 = 1024 * 1024;
const _: () = assert!(UNIT_SIZE <= PEER_REQUEST_MAX);

/// Shared scheduler state: pending units and the in-flight set (offsets),
/// for endgame duplication.
struct WorkState {
    pending: Vec<(u64, u64)>,
    in_flight: HashSet<u64>,
    /// Units verified-and-written (offsets) — endgame duplicates check this
    /// so both copies don't double-count.
    done: HashSet<u64>,
}

/// Fetch `root` (`size` bytes) into `dest` from every reachable source
/// concurrently. Fails only when no source can make progress.
pub async fn fetch_swarm(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
) -> Result<FetchReport, PeerError> {
    if sources.is_empty() {
        return Err(PeerError::BadRequest);
    }
    // Pre-size the destination so workers can write units at any offset.
    let file = std::fs::File::create(dest)?;
    file.set_len(size)?;
    drop(file);
    if size == 0 {
        return Ok(FetchReport::default());
    }

    // Units back-to-front so `pop()` hands them out front-to-back.
    let mut pending: Vec<(u64, u64)> = Vec::new();
    let mut offset = 0;
    while offset < size {
        pending.push((offset, (size - offset).min(UNIT_SIZE)));
        offset += UNIT_SIZE;
    }
    pending.reverse();
    let state = Arc::new(Mutex::new(WorkState {
        pending,
        in_flight: HashSet::new(),
        done: HashSet::new(),
    }));

    let mut workers = Vec::new();
    for source in sources {
        let source = source.clone();
        let state = state.clone();
        let token = token.to_vec();
        let dest = dest.to_path_buf();
        workers.push(tokio::spawn(async move {
            worker(source, state, token, root, dest).await
        }));
    }

    let mut per_source = Vec::new();
    let mut served_total = 0u64;
    for w in workers {
        if let Ok((endpoint, units)) = w.await {
            served_total += units;
            per_source.push((endpoint, units));
        }
    }

    let state = state.lock().expect("not poisoned");
    if !state.pending.is_empty() || !state.in_flight.is_empty() {
        return Err(PeerError::Verify(format!(
            "no source could serve {} remaining unit(s)",
            state.pending.len() + state.in_flight.len()
        )));
    }
    debug_assert!(served_total >= state.done.len() as u64);
    Ok(FetchReport {
        bytes: size,
        per_source,
    })
}

/// One source's worker: pull units until none are left (normal or endgame),
/// or until this source fails one. Returns (endpoint, units it completed).
async fn worker(
    source: SourcePeer,
    state: Arc<Mutex<WorkState>>,
    token: Vec<u8>,
    root: [u8; 32],
    dest: std::path::PathBuf,
) -> (String, u64) {
    use std::io::{Seek, SeekFrom, Write};
    let mut completed = 0u64;
    loop {
        // Next pending unit, or an in-flight one to duplicate (endgame).
        let (unit, endgame) = {
            let mut s = state.lock().expect("not poisoned");
            match s.pending.pop() {
                Some(u) => {
                    s.in_flight.insert(u.0);
                    (u, false)
                }
                None => {
                    // Endgame: duplicate some straggler still in flight.
                    match s.in_flight.iter().next().copied() {
                        Some(off) => ((off, 0), true),
                        None => break, // truly done
                    }
                }
            }
        };
        let (off, len) = if endgame {
            // Recompute the unit length from the offset (all units are
            // UNIT_SIZE except possibly the last; fetch_range clamps).
            (unit.0, UNIT_SIZE)
        } else {
            unit
        };

        match fetch_range(&source.endpoint, source.cert_fp, &token, root, off, len).await {
            Ok(bytes) => {
                let write: Result<bool, std::io::Error> = (|| {
                    let mut s = state.lock().expect("not poisoned");
                    if s.done.contains(&off) {
                        return Ok(false); // endgame race: other copy won
                    }
                    let mut f = std::fs::OpenOptions::new().write(true).open(&dest)?;
                    f.seek(SeekFrom::Start(off))?;
                    f.write_all(&bytes)?;
                    s.done.insert(off);
                    s.in_flight.remove(&off);
                    Ok(true)
                })();
                match write {
                    Ok(true) => completed += 1,
                    Ok(false) => {}
                    Err(_) => {
                        // Local IO failure: put the unit back and stop.
                        let mut s = state.lock().expect("not poisoned");
                        if !endgame && !s.done.contains(&off) {
                            s.in_flight.remove(&off);
                            s.pending.push((off, len));
                        }
                        break;
                    }
                }
            }
            Err(_) => {
                // This source failed: hand the unit back (unless it was an
                // endgame duplicate — the original holder still has it) and
                // retire the source.
                if !endgame {
                    let mut s = state.lock().expect("not poisoned");
                    if !s.done.contains(&off) {
                        s.in_flight.remove(&off);
                        s.pending.push((off, len));
                    }
                }
                break;
            }
        }
    }
    (source.endpoint, completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::CapToken;
    use crate::peer::{PeerServer, SeedStore};
    use rabbithole_identity::IdentityKey;

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    async fn seeding_peer(key: &IdentityKey, root: [u8; 32], path: &Path) -> PeerServer {
        let seeds = Arc::new(SeedStore::new());
        seeds.add(root, path).unwrap();
        PeerServer::start("127.0.0.1:0".parse().unwrap(), key.public().0, seeds)
            .await
            .unwrap()
    }

    fn peer_source(p: &PeerServer) -> SourcePeer {
        SourcePeer {
            endpoint: format!("127.0.0.1:{}", p.addr.port()),
            cert_fp: p.fingerprint.0,
        }
    }

    #[tokio::test]
    async fn multi_source_fetch_spreads_work() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[6; 32]);
        // 5 MiB + tail → six units across three seeders.
        let body = payload(5 * 1024 * 1024 + 999);
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();

        let peers = [
            seeding_peer(&key, root, &src).await,
            seeding_peer(&key, root, &src).await,
            seeding_peer(&key, root, &src).await,
        ];
        let sources: Vec<SourcePeer> = peers.iter().map(peer_source).collect();
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();

        let dest = dir.path().join("out.bin");
        let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
            .await
            .unwrap();
        assert_eq!(report.bytes, body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), body, "reassembled exactly");
        let total_units: u64 = report.per_source.iter().map(|(_, n)| n).sum();
        assert_eq!(total_units, 6);
        assert!(
            report.per_source.iter().filter(|(_, n)| *n > 0).count() >= 2,
            "work spread across sources: {:?}",
            report.per_source
        );
    }

    #[tokio::test]
    async fn dead_source_is_survived() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[7; 32]);
        let body = payload(3 * 1024 * 1024);
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();

        let live = seeding_peer(&key, root, &src).await;
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();
        // One dead endpoint (nothing listens), one live.
        let sources = vec![
            SourcePeer {
                endpoint: "127.0.0.1:1".into(),
                cert_fp: [0; 32],
            },
            peer_source(&live),
        ];

        let dest = dir.path().join("out.bin");
        let report = fetch_swarm(&sources, &token, root, body.len() as u64, &dest)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        // The live peer carried everything.
        let live_units = report
            .per_source
            .iter()
            .find(|(e, _)| *e == sources[1].endpoint)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        assert_eq!(live_units, 3);
    }

    #[tokio::test]
    async fn all_sources_dead_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let body = payload(64 * 1024);
        let root = *blake3::hash(&body).as_bytes();
        let sources = vec![SourcePeer {
            endpoint: "127.0.0.1:1".into(),
            cert_fp: [0; 32],
        }];
        let dest = dir.path().join("out.bin");
        assert!(
            fetch_swarm(&sources, &[1, 2, 3], root, body.len() as u64, &dest)
                .await
                .is_err()
        );
    }
}
