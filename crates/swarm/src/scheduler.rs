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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

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

/// A live progress event: one verified work unit just landed. Emitted the
/// moment it's persisted, so a UI can show per-source throughput + a chunk map
/// as the swarm fills — the streaming counterpart to the terminal [`FetchReport`].
#[derive(Debug, Clone)]
pub struct UnitDone {
    /// The source (peer endpoint) that served this unit.
    pub endpoint: String,
    /// Byte offset of the unit that just completed.
    pub offset: u64,
    /// Units verified so far (across all sources).
    pub done_units: u64,
    /// Total units in the file.
    pub total_units: u64,
}

/// A live-progress sink threaded into the swarm workers.
pub type ProgressSink = tokio::sync::mpsc::UnboundedSender<UnitDone>;

/// Work-unit size: 1 MiB balances distribution granularity against
/// per-request overhead (and stays under [`PEER_REQUEST_MAX`]).
pub const UNIT_SIZE: u64 = 1024 * 1024;
const _: () = assert!(UNIT_SIZE <= PEER_REQUEST_MAX);

/// The on-disk resume record (`<dest>.rhstate`, postcard): which units of
/// which root have already been fetched and verified. The bytes live in the
/// partial destination file itself; this is just the map of what's real.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RhState {
    pub root: [u8; 32],
    pub size: u64,
    /// Offsets of completed units.
    pub done: Vec<u64>,
}

/// The conventional state-file path for a destination.
pub fn rhstate_path(dest: &Path) -> PathBuf {
    let mut os = dest.as_os_str().to_owned();
    os.push(".rhstate");
    PathBuf::from(os)
}

fn load_rhstate(path: &Path, root: &[u8; 32], size: u64) -> Option<RhState> {
    let bytes = std::fs::read(path).ok()?;
    let state: RhState = postcard::from_bytes(&bytes).ok()?;
    // A state for a different root or size describes some other download.
    (state.root == *root && state.size == size).then_some(state)
}

/// Shared scheduler state: pending units and the in-flight set (offsets),
/// for endgame duplication.
struct WorkState {
    pending: Vec<(u64, u64)>,
    in_flight: HashSet<u64>,
    /// Units verified-and-written (offsets) — endgame duplicates check this
    /// so both copies don't double-count.
    done: HashSet<u64>,
    /// When resumable: persist `done` here after every unit.
    persist_to: Option<(PathBuf, [u8; 32], u64)>,
}

impl WorkState {
    /// Write the resume record (atomically: tmp + rename). Called under the
    /// scheduler lock right after a unit lands, so a kill at any instant
    /// leaves a state file that matches bytes actually on disk.
    fn persist(&self) {
        let Some((path, root, size)) = &self.persist_to else {
            return;
        };
        let state = RhState {
            root: *root,
            size: *size,
            done: self.done.iter().copied().collect(),
        };
        let bytes = postcard::to_allocvec(&state).expect("state serializes");
        let tmp = path.with_extension("rhstate.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Fetch `root` (`size` bytes) into `dest` from every reachable source
/// concurrently. Fails only when no source can make progress. Fresh fetch:
/// truncates `dest` and keeps no resume state — see [`fetch_swarm_resumable`].
pub async fn fetch_swarm(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
) -> Result<FetchReport, PeerError> {
    fetch_swarm_inner(sources, token, root, size, dest, HashSet::new(), None, None).await
}

/// [`fetch_swarm`], but interruption-proof: completed units are recorded in
/// `<dest>.rhstate` as they land, a matching state file on entry skips the
/// units it lists, the reassembled file is hash-verified whole against
/// `root` (so a stale or corrupted partial can't slip through), and the
/// state file is removed on success.
pub async fn fetch_swarm_resumable(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
) -> Result<FetchReport, PeerError> {
    resumable(sources, token, root, size, dest, None).await
}

/// [`fetch_swarm_resumable`] plus a live [`UnitDone`] stream on `progress` —
/// one event per verified unit as it lands, for a live UI roster + chunk map.
pub async fn fetch_swarm_resumable_with_progress(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    progress: ProgressSink,
) -> Result<FetchReport, PeerError> {
    resumable(sources, token, root, size, dest, Some(progress)).await
}

async fn resumable(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    progress: Option<ProgressSink>,
) -> Result<FetchReport, PeerError> {
    let state_path = rhstate_path(dest);
    let done: HashSet<u64> = load_rhstate(&state_path, &root, size)
        .map(|s| s.done.into_iter().collect())
        .unwrap_or_default();
    let report = fetch_swarm_inner(
        sources,
        token,
        root,
        size,
        dest,
        done,
        Some(state_path.clone()),
        progress,
    )
    .await?;
    // The resume trusted prior units from disk; verify the whole file.
    let path = dest.to_path_buf();
    let ok = tokio::task::spawn_blocking(move || -> Result<bool, std::io::Error> {
        use std::io::Read;
        let mut f = std::fs::File::open(&path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(*hasher.finalize().as_bytes() == root)
    })
    .await
    .map_err(|e| PeerError::Verify(e.to_string()))??;
    if !ok {
        // A prior partial lied; the caller should remove dest and restart.
        return Err(PeerError::Verify(
            "assembled file does not hash to the root (stale partial?)".into(),
        ));
    }
    let _ = std::fs::remove_file(&state_path);
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn fetch_swarm_inner(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    done: HashSet<u64>,
    state_path: Option<PathBuf>,
    progress: Option<ProgressSink>,
) -> Result<FetchReport, PeerError> {
    // Total units in the file (for progress denominators).
    let total_units = size.div_ceil(UNIT_SIZE);
    if sources.is_empty() {
        return Err(PeerError::BadRequest);
    }
    // Pre-size the destination so workers can write units at any offset —
    // without truncating an existing partial when resuming.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(state_path.is_none())
        .write(true)
        .open(dest)?;
    file.set_len(size)?;
    drop(file);
    if size == 0 {
        return Ok(FetchReport::default());
    }

    // Units back-to-front so `pop()` hands them out front-to-back; already-
    // done units (a resume) never enter the queue.
    let mut pending: Vec<(u64, u64)> = Vec::new();
    let mut offset = 0;
    while offset < size {
        if !done.contains(&offset) {
            pending.push((offset, (size - offset).min(UNIT_SIZE)));
        }
        offset += UNIT_SIZE;
    }
    pending.reverse();
    let state = Arc::new(Mutex::new(WorkState {
        pending,
        in_flight: HashSet::new(),
        done,
        persist_to: state_path.map(|p| (p, root, size)),
    }));

    let mut workers = Vec::new();
    for source in sources {
        let source = source.clone();
        let state = state.clone();
        let token = token.to_vec();
        let dest = dest.to_path_buf();
        let progress = progress.clone();
        workers.push(tokio::spawn(async move {
            worker(source, state, token, root, dest, progress, total_units).await
        }));
    }

    let mut per_source = Vec::new();
    for w in workers {
        if let Ok((endpoint, units)) = w.await {
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
    Ok(FetchReport {
        bytes: size,
        per_source,
    })
}

/// One source's worker: pull units until none are left (normal or endgame),
/// or until this source fails one. Returns (endpoint, units it completed).
#[allow(clippy::too_many_arguments)]
async fn worker(
    source: SourcePeer,
    state: Arc<Mutex<WorkState>>,
    token: Vec<u8>,
    root: [u8; 32],
    dest: std::path::PathBuf,
    progress: Option<ProgressSink>,
    total_units: u64,
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
                    s.persist();
                    // Emit the live progress event under the lock, so
                    // `done_units` is a consistent snapshot.
                    if let Some(tx) = progress.as_ref() {
                        let _ = tx.send(UnitDone {
                            endpoint: source.endpoint.clone(),
                            offset: off,
                            done_units: s.done.len() as u64,
                            total_units,
                        });
                    }
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
    async fn resume_skips_done_units_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[8; 32]);
        let body = payload(4 * 1024 * 1024); // four units
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();
        let peer = seeding_peer(&key, root, &src).await;
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();

        // Simulate an interrupted fetch: units 0 and 2 already on disk,
        // recorded in the .rhstate file.
        let dest = dir.path().join("out.bin");
        let mut partial = vec![0u8; body.len()];
        partial[0..UNIT_SIZE as usize].copy_from_slice(&body[0..UNIT_SIZE as usize]);
        let u2 = 2 * UNIT_SIZE as usize;
        partial[u2..u2 + UNIT_SIZE as usize].copy_from_slice(&body[u2..u2 + UNIT_SIZE as usize]);
        std::fs::write(&dest, &partial).unwrap();
        let state = RhState {
            root,
            size: body.len() as u64,
            done: vec![0, 2 * UNIT_SIZE],
        };
        std::fs::write(rhstate_path(&dest), postcard::to_allocvec(&state).unwrap()).unwrap();

        let report = fetch_swarm_resumable(
            &[peer_source(&peer)],
            &token,
            root,
            body.len() as u64,
            &dest,
        )
        .await
        .unwrap();
        // Only the two missing units moved; the file is whole and verified,
        // and the state file is gone.
        let fetched: u64 = report.per_source.iter().map(|(_, n)| n).sum();
        assert_eq!(fetched, 2, "resume fetched only the missing units");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!rhstate_path(&dest).exists(), "state removed on success");
    }

    #[tokio::test]
    async fn corrupted_partial_fails_the_final_verify() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[10; 32]);
        let body = payload(2 * 1024 * 1024);
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();
        let peer = seeding_peer(&key, root, &src).await;
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();

        // A lying partial: unit 0 marked done but its bytes are garbage.
        let dest = dir.path().join("out.bin");
        let mut partial = vec![0u8; body.len()];
        partial[..UNIT_SIZE as usize].fill(0xAB);
        std::fs::write(&dest, &partial).unwrap();
        let state = RhState {
            root,
            size: body.len() as u64,
            done: vec![0],
        };
        std::fs::write(rhstate_path(&dest), postcard::to_allocvec(&state).unwrap()).unwrap();

        let err = fetch_swarm_resumable(
            &[peer_source(&peer)],
            &token,
            root,
            body.len() as u64,
            &dest,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, PeerError::Verify(_)),
            "whole-file check catches the stale unit: {err}"
        );
    }

    #[tokio::test]
    async fn mismatched_rhstate_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[11; 32]);
        let body = payload(1024 * 1024 + 5);
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();
        let peer = seeding_peer(&key, root, &src).await;
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();

        // A state file for some OTHER root must not mask units here.
        let dest = dir.path().join("out.bin");
        let state = RhState {
            root: [0xEE; 32],
            size: body.len() as u64,
            done: vec![0],
        };
        std::fs::write(rhstate_path(&dest), postcard::to_allocvec(&state).unwrap()).unwrap();

        let report = fetch_swarm_resumable(
            &[peer_source(&peer)],
            &token,
            root,
            body.len() as u64,
            &dest,
        )
        .await
        .unwrap();
        let fetched: u64 = report.per_source.iter().map(|(_, n)| n).sum();
        assert_eq!(fetched, 2, "foreign state ignored; full fetch ran");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
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
