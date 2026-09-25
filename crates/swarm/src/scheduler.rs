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
//! Sources need not hold the whole file. Each worker first asks its source
//! which units it holds (a [`HaveMap`](crate::peer::HaveMap); a source that
//! predates the question holds the whole file) and takes only those. A
//! source holding none of what is left (a partial seed still fetching) is
//! asked again every few seconds, and let go after a minute without
//! anything new; one that says it does not hold a unit after all
//! ([`STATUS_NOT_HELD`]) loses that unit, not its place. Every unit is
//! verified block by block against the root whoever sent it, and the whole
//! file again at the end.
//!
//! A resumable fetch keeps the proof of every unit it verifies (the Bao
//! parent hashes, in `<dest>.obao`), so the units it has can be served on
//! while the rest comes in ([`fetch_swarm_sharing`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bao_tree::io::outboard::PreOrderOutboard;
use serde::{Deserialize, Serialize};

use crate::peer::{
    fetch_proved, open_proofs, proofs_path, proven_ranges, HaveMap, PeerError, PeerSource,
    RangeSource, SeedStore, Sharing, HAVE_UNIT, PEER_REQUEST_MAX, STATUS_BUSY, STATUS_DENIED,
    STATUS_NOT_FOUND, STATUS_NOT_HELD,
};

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
    /// A source that does not allow what it sends to be passed on carried
    /// part of this file ([`RangeSource::shareable`]), so the file as a
    /// whole is not offered on either.
    pub borrowed: bool,
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
// A unit is what a have-map counts: one bit per unit fetched.
const _: () = assert!(UNIT_SIZE == HAVE_UNIT);

/// The on-disk resume record (`<dest>.rhstate`, postcard): which units of
/// which root have already been fetched and verified. The bytes live in the
/// partial destination file itself; this is just the map of what's real.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RhState {
    pub root: [u8; 32],
    pub size: u64,
    /// Offsets of completed units.
    pub done: Vec<u64>,
    /// Offsets of units a source lent rather than gave
    /// ([`RangeSource::shareable`]): they are on disk and count as done,
    /// but they are never offered on, this run or any that resumes it.
    pub borrowed: Vec<u64>,
}

/// The conventional state-file path for a destination.
pub fn rhstate_path(dest: &Path) -> PathBuf {
    let mut os = dest.as_os_str().to_owned();
    os.push(".rhstate");
    PathBuf::from(os)
}

fn load_rhstate(path: &Path, root: &[u8; 32], size: u64) -> Option<RhState> {
    let bytes = std::fs::read(path).ok()?;
    let mut state: RhState = postcard::from_bytes(&bytes).ok()?;
    // A state for a different root or size describes some other download.
    if state.root != *root || state.size != size {
        return None;
    }
    // Only offsets a unit of this file can start at count.
    state.done.retain(|&off| off < size && off % UNIT_SIZE == 0);
    Some(state)
}

/// Shared scheduler state: pending units and the in-flight set (offsets),
/// for endgame duplication.
struct WorkState {
    pending: Vec<(u64, u64)>,
    in_flight: HashSet<u64>,
    /// Which source is fetching which unit right now (offset, source), so a
    /// source with several lanes is never asked for one unit twice at once.
    working: HashSet<(u64, usize)>,
    /// Units verified-and-written (offsets) — endgame duplicates check this
    /// so both copies don't double-count.
    done: HashSet<u64>,
    /// When resumable: persist `done` here as units land (see
    /// [`WorkState::landed`]).
    persist_to: Option<(PathBuf, [u8; 32], u64)>,
    /// Units landed since the record was last written, and when it was.
    unpersisted: u32,
    persisted_at: std::time::Instant,
    /// The fetch was dropped: no worker writes another byte or record.
    stopped: bool,
    /// Units each source landed, by its place in the source list (two
    /// entries may share an endpoint): kept here rather than returned by the
    /// workers, so the count stands for a worker stopped early.
    served: Vec<u64>,
    /// When resumable: the proof of every unit that lands is kept here.
    proofs: Option<PreOrderOutboard<std::fs::File>>,
    /// When sharing: units whose proof is kept are served from here on.
    sharing: Option<Sharing>,
    /// A unit could not be written here (a full disk, a removed file): the
    /// fetch fails with this, not as if no source could serve.
    local: Option<std::io::Error>,
    /// Units a source lent rather than gave: never offered on.
    borrowed: HashSet<u64>,
}

/// What a fetch keeps as it goes: its resume record, the proofs of what
/// landed, and the partial seed it shares them through.
#[derive(Default)]
struct Keep {
    state_path: Option<PathBuf>,
    proofs: Option<PreOrderOutboard<std::fs::File>>,
    sharing: Option<Sharing>,
}

/// The resume record is rewritten after this many units, or this long,
/// whichever comes first: every unit would rewrite the whole record for each
/// megabyte, which for a large file is more than the file. A record a little
/// behind only means those units are fetched again after a crash.
const PERSIST_EVERY_UNITS: u32 = 32;
const PERSIST_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

impl WorkState {
    /// A unit just landed: write the record when it is due.
    fn landed(&mut self) {
        self.unpersisted += 1;
        if self.unpersisted >= PERSIST_EVERY_UNITS || self.persisted_at.elapsed() >= PERSIST_EVERY {
            self.persist();
        }
    }

    /// Unit `off` is done: out of both queues, whoever held it (a copy that
    /// failed may have put it back after the other copy landed it). Tells
    /// `complete` when nothing is left.
    fn settle(&mut self, off: u64, complete: &tokio::sync::Notify) {
        self.in_flight.remove(&off);
        self.pending.retain(|(o, _)| *o != off);
        if self.pending.is_empty() && self.in_flight.is_empty() {
            complete.notify_one();
        }
    }

    /// Write the resume record (atomically: tmp + rename). Called under the
    /// scheduler lock after units land, so a kill at any instant leaves a
    /// state file that lists only bytes actually on disk.
    fn write_record(&self) {
        let Some((path, root, size)) = &self.persist_to else {
            return;
        };
        let state = RhState {
            root: *root,
            size: *size,
            done: self.done.iter().copied().collect(),
            borrowed: self.borrowed.iter().copied().collect(),
        };
        let bytes = postcard::to_allocvec(&state).expect("state serializes");
        let tmp = path.with_extension("rhstate.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn persist(&mut self) {
        self.write_record();
        self.unpersisted = 0;
        self.persisted_at = std::time::Instant::now();
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
    fetch_swarm_inner(
        &peers(sources, token, root),
        root,
        size,
        dest,
        HashSet::new(),
        HashSet::new(),
        Keep::default(),
        None,
    )
    .await
}

/// Peers on the peer wire as sources, under one capability for `root`.
fn peers(sources: &[SourcePeer], token: &[u8], root: [u8; 32]) -> Vec<Arc<dyn RangeSource>> {
    sources
        .iter()
        .map(|p| {
            Arc::new(PeerSource {
                endpoint: p.endpoint.clone(),
                cert_fp: p.cert_fp,
                token: token.to_vec(),
                root,
            }) as Arc<dyn RangeSource>
        })
        .collect()
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
    resumable(&peers(sources, token, root), root, size, dest, None, None).await
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
    resumable(
        &peers(sources, token, root),
        root,
        size,
        dest,
        Some(progress),
        None,
    )
    .await
}

/// [`fetch_swarm_resumable`], sharing as it goes: from the start `seeds`
/// serves, from `dest`, every unit that has landed with its proof (a partial
/// seed, which peers find through the same advert as a whole one), and once
/// the file is whole and checked it serves the whole file. A fetch that
/// fails or is dropped stops sharing its part.
pub async fn fetch_swarm_sharing(
    sources: &[SourcePeer],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    progress: Option<ProgressSink>,
    seeds: Arc<SeedStore>,
) -> Result<FetchReport, PeerError> {
    resumable(
        &peers(sources, token, root),
        root,
        size,
        dest,
        progress,
        Some(seeds),
    )
    .await
}

/// A resumable fetch from any sources: peers on the peer wire, a burrow
/// serving a file it stores, the burrow a file is sent from, together,
/// each unit taken from one that holds it and checked against the root
/// whoever sent it. Shares as it goes through `seeds`, if given (see
/// [`fetch_swarm_sharing`]).
pub async fn fetch_swarm_from(
    sources: &[Arc<dyn RangeSource>],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    progress: Option<ProgressSink>,
    seeds: Option<Arc<SeedStore>>,
) -> Result<FetchReport, PeerError> {
    resumable(sources, root, size, dest, progress, seeds).await
}

/// Stops sharing a fetch's part when the fetch ends without the whole file
/// (a whole seed that replaced it is left alone).
struct PartialGuard {
    seeds: Option<Arc<SeedStore>>,
    root: [u8; 32],
}

impl Drop for PartialGuard {
    fn drop(&mut self) {
        if let Some(seeds) = &self.seeds {
            seeds.remove_partial(&self.root);
        }
    }
}

async fn resumable(
    sources: &[Arc<dyn RangeSource>],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    progress: Option<ProgressSink>,
    seeds: Option<Arc<SeedStore>>,
) -> Result<FetchReport, PeerError> {
    // An empty file has nothing to fetch: it is the empty file, if that is
    // what the root says.
    if size == 0 {
        if root != *blake3::hash(&[]).as_bytes() {
            return Err(PeerError::Verify(
                "an empty file with a root that is not".into(),
            ));
        }
        std::fs::File::create(dest)?;
        return Ok(FetchReport::default());
    }
    let state_path = rhstate_path(dest);
    let proofs_at = proofs_path(dest);
    let (done, borrowed): (HashSet<u64>, HashSet<u64>) = load_rhstate(&state_path, &root, size)
        .map(|s| {
            (
                s.done.into_iter().collect(),
                s.borrowed.into_iter().collect(),
            )
        })
        .unwrap_or_default();
    // The proof of each unit is kept beside the file, so what has landed
    // can be served on, before and after a resume.
    // Without a proofs file (it cannot be made there) the fetch still runs;
    // it just has nothing to share.
    let proofs = if size > 0 {
        open_proofs(&proofs_at, root, size).ok()
    } else {
        None
    };
    let sharing = match (&seeds, &proofs) {
        (Some(seeds), Some(proofs)) => {
            // Units from before a resume are shared only where their proof
            // was kept; their bytes are checked against it as they are
            // served.
            let proven = proven_ranges(proofs, size)?;
            let held: Vec<u64> = done
                .iter()
                // What another source lent stays lent across a resume: it
                // is on disk, and it is still not this fetch's to pass on.
                .filter(|off| !borrowed.contains(off))
                .filter(|&&off| {
                    let end = off + (size - off).min(UNIT_SIZE);
                    proven.iter().any(|&(s, e)| s <= off && end <= e)
                })
                .map(|off| off / HAVE_UNIT)
                .collect();
            seeds.add_partial(root, size, dest, &proofs_at, held)
        }
        _ => None,
    };
    let _guard = PartialGuard {
        seeds: sharing.as_ref().and(seeds.clone()),
        root,
    };
    let report = fetch_swarm_inner(
        sources,
        root,
        size,
        dest,
        done,
        borrowed,
        Keep {
            state_path: Some(state_path.clone()),
            proofs,
            sharing,
        },
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
        // A prior partial lied (a completed unit's on-disk bytes are corrupt, or
        // a stale `.rhstate` for a since-changed file). Remove BOTH the state and
        // the destination so the next call starts clean — otherwise the state
        // keeps marking the corrupt unit "done", it's skipped forever, and every
        // retry re-hashes to the same failure. Self-healing beats a permanent poison.
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&proofs_at);
        let _ = std::fs::remove_file(dest);
        return Err(PeerError::Verify(
            "assembled file does not hash to the root (stale partial removed; retry)".into(),
        ));
    }
    let _ = std::fs::remove_file(&state_path);
    // Whole and checked: seeded whole from here on, from the proofs already
    // kept (no second pass over the file) — unless part of it came from a
    // source whose bytes are not this fetch's to pass on.
    if let Some(seeds) = &seeds {
        if size > 0 && !report.borrowed {
            let _ = seeds.add_proved(root, size, dest, &proofs_at);
        }
    }
    let _ = std::fs::remove_file(&proofs_at);
    Ok(report)
}

/// The worker tasks of one fetch, stopped when the fetch is dropped: a
/// caller that gives up (a timeout, a cancel, a fallback to another route)
/// must not leave workers still fetching and writing its file.
struct Workers {
    handles: Vec<tokio::task::JoinHandle<(String, u64)>>,
    state: Arc<Mutex<WorkState>>,
}

impl Drop for Workers {
    fn drop(&mut self) {
        for worker in &self.handles {
            worker.abort();
        }
        // A worker running on another thread finishes the unit it is
        // writing, which it does holding this lock, and writes nothing
        // after it: once this returns, the file and its record are left
        // as they are, and the caller may remove them.
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // The record catches up with what landed, so a resume skips it.
        if state.unpersisted > 0 {
            state.persist();
        }
        state.stopped = true;
        state.persist_to = None;
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_swarm_inner(
    sources: &[Arc<dyn RangeSource>],
    root: [u8; 32],
    size: u64,
    dest: &Path,
    done: HashSet<u64>,
    borrowed: HashSet<u64>,
    keep: Keep,
    progress: Option<ProgressSink>,
) -> Result<FetchReport, PeerError> {
    let state_path = keep.state_path;
    // Total units in the file (for progress denominators).
    let total_units = size.div_ceil(UNIT_SIZE);
    if sources.is_empty() {
        return Err(PeerError::BadRequest);
    }
    // Nothing to fetch — return BEFORE touching `dest`, so a `size == 0` (e.g. a
    // caller whose size derivation failed) can never `set_len(0)` and wipe an
    // existing partial. An empty transfer has no bytes to move.
    if size == 0 {
        return Ok(FetchReport::default());
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
    let units_left = pending.len().max(1);
    let state = Arc::new(Mutex::new(WorkState {
        pending,
        in_flight: HashSet::new(),
        working: HashSet::new(),
        done,
        persist_to: state_path.map(|p| (p, root, size)),
        stopped: false,
        served: vec![0; sources.len()],
        unpersisted: 0,
        persisted_at: std::time::Instant::now(),
        proofs: keep.proofs,
        sharing: keep.sharing,
        local: None,
        borrowed,
    }));
    // Told by the worker that lands the last unit, so a worker still stuck
    // on a slow peer does not hold back a file that is already whole.
    let complete = Arc::new(tokio::sync::Notify::new());

    let mut workers = Workers {
        handles: Vec::new(),
        state: state.clone(),
    };
    // A worker per lane of each source (never more than there are units).
    for (index, source) in sources.iter().enumerate() {
        for _ in 0..source.lanes().clamp(1, units_left) {
            let source = source.clone();
            let state = state.clone();
            let dest = dest.to_path_buf();
            let progress = progress.clone();
            let complete = complete.clone();
            workers.handles.push(tokio::spawn(async move {
                worker(
                    index,
                    source,
                    state,
                    root,
                    size,
                    dest,
                    progress,
                    total_units,
                    complete,
                )
                .await
            }));
        }
    }
    drop(progress);

    {
        let joined = async {
            for w in workers.handles.iter_mut() {
                let _ = w.await;
            }
        };
        tokio::select! {
            _ = joined => {}
            _ = complete.notified() => {}
        }
    }

    let mut state = state.lock().expect("not poisoned");
    if let Some(e) = state.local.take() {
        return Err(PeerError::Io(e));
    }
    let per_source = sources
        .iter()
        .zip(&state.served)
        .map(|(s, units)| (s.label(), *units))
        .collect();
    if !state.pending.is_empty() || !state.in_flight.is_empty() {
        return Err(PeerError::Verify(format!(
            "no source could serve {} remaining unit(s)",
            state.pending.len() + state.in_flight.len()
        )));
    }
    Ok(FetchReport {
        bytes: size,
        per_source,
        borrowed: !state.borrowed.is_empty(),
    })
}

/// How long a worker whose source holds none of what is left waits before
/// asking it again, and how long without anything new before letting it go
/// (by the clock: a source slow to answer does not stretch it).
const HAVE_REFRESH: Duration = Duration::from_secs(3);
const IDLE_LIMIT: Duration = Duration::from_secs(60);
/// Units a source may say it does not hold after all (against its own
/// have-map) before it is let go.
const MAX_MISSES: u32 = 8;

/// What a worker knows of which units its source holds.
enum Holds {
    /// The whole file (a whole seed, or one that predates the question).
    All,
    Map(HaveMap),
}

impl Holds {
    fn covers(&self, offset: u64, len: u64) -> bool {
        match self {
            Holds::All => true,
            Holds::Map(map) => map.covers(offset, len),
        }
    }

    /// The source does not hold `[offset, offset + len)` after all.
    fn lacks(&mut self, offset: u64, len: u64, size: u64) {
        if let Holds::All = self {
            *self = Holds::Map(HaveMap::whole(size));
        }
        if let Holds::Map(map) = self {
            for i in offset / map.unit..=(offset + len.max(1) - 1) / map.unit {
                map.clear(i);
            }
        }
    }
}

/// A worker's next move.
enum Next {
    Unit {
        off: u64,
        len: u64,
        endgame: bool,
    },
    /// Units remain, but none this source holds.
    Wait,
    /// What is left that this source holds is being fetched by another of
    /// its lanes: wait for that to land or come back.
    Sibling,
    Done,
}

/// How often a lane whose source is already fetching all that is left looks
/// again.
const SIBLING_WAIT: Duration = Duration::from_millis(200);

/// The next pending unit source `index` holds, in order; else a unit in
/// flight elsewhere that it holds (endgame); else wait or finish. Never a
/// unit the same source is fetching already, on another lane.
fn next_unit(s: &mut WorkState, holds: &Holds, size: u64, index: usize) -> Next {
    let mut i = s.pending.len();
    while i > 0 {
        i -= 1;
        let (off, len) = s.pending[i];
        if s.done.contains(&off) {
            s.pending.remove(i);
            continue;
        }
        if holds.covers(off, len) && !s.working.contains(&(off, index)) {
            s.pending.remove(i);
            s.in_flight.insert(off);
            s.working.insert((off, index));
            return Next::Unit {
                off,
                len,
                endgame: false,
            };
        }
    }
    let mut sibling = false;
    let mut straggler = None;
    for &off in &s.in_flight {
        if s.done.contains(&off) || !holds.covers(off, (size - off).min(UNIT_SIZE)) {
            continue;
        }
        if s.working.contains(&(off, index)) {
            sibling = true;
        } else {
            straggler = Some(off);
            break;
        }
    }
    if let Some(off) = straggler {
        s.working.insert((off, index));
        // Every unit is UNIT_SIZE but maybe the last; the peer clamps.
        return Next::Unit {
            off,
            len: UNIT_SIZE,
            endgame: true,
        };
    }
    if s.pending.is_empty() && s.in_flight.is_empty() {
        Next::Done
    } else if sibling
        || s.pending
            .iter()
            .any(|&(off, _)| s.working.contains(&(off, index)))
    {
        Next::Sibling
    } else {
        Next::Wait
    }
}

/// Hand unit `off` back for another source to fetch (unless it is done, or
/// this was an endgame copy the original holder still has). Source `index`
/// is no longer fetching it, so its other lanes may take it on.
fn give_back(
    s: &mut WorkState,
    off: u64,
    len: u64,
    index: usize,
    endgame: bool,
    complete: &tokio::sync::Notify,
) {
    s.working.remove(&(off, index));
    if s.done.contains(&off) {
        s.settle(off, complete);
    } else if !endgame {
        s.in_flight.remove(&off);
        s.pending.push((off, len));
    }
}

/// One source's worker: pull the units its source holds until none are left
/// (normal or endgame), or until the source fails one. Returns (endpoint,
/// units it completed).
#[allow(clippy::too_many_arguments)]
async fn worker(
    index: usize,
    source: Arc<dyn RangeSource>,
    state: Arc<Mutex<WorkState>>,
    root: [u8; 32],
    size: u64,
    dest: std::path::PathBuf,
    progress: Option<ProgressSink>,
    total_units: u64,
    complete: Arc<tokio::sync::Notify>,
) -> (String, u64) {
    use std::io::{Seek, SeekFrom, Write};
    let mut completed = 0u64;
    {
        let s = state.lock().expect("not poisoned");
        if s.stopped || (s.pending.is_empty() && s.in_flight.is_empty()) {
            return (source.label(), 0);
        }
    }
    let label = source.label();
    let mut holds = match source.have().await {
        Ok(Some(map)) => Holds::Map(map),
        Ok(None) => Holds::All,
        // No capability for it here, or it has none of this file: not a
        // source at all.
        Err(PeerError::Refused(STATUS_DENIED | STATUS_NOT_FOUND)) => return (label, 0),
        // Unreachable, or unclear: the first unit will tell.
        Err(_) => Holds::All,
    };
    let mut idle_since: Option<std::time::Instant> = None;
    let mut misses = 0u32;
    loop {
        let next = {
            let mut s = state.lock().expect("not poisoned");
            if s.stopped {
                break;
            }
            next_unit(&mut s, &holds, size, index)
        };
        let (off, len, endgame) = match next {
            Next::Done => break,
            Next::Unit { off, len, endgame } => (off, len, endgame),
            Next::Sibling => {
                tokio::time::sleep(SIBLING_WAIT).await;
                continue;
            }
            Next::Wait => {
                // A partial seed may have more by now: ask again, a while
                // later, and let it go after a minute without anything new.
                let since = *idle_since.get_or_insert_with(std::time::Instant::now);
                if matches!(holds, Holds::All) || since.elapsed() >= IDLE_LIMIT {
                    break;
                }
                tokio::time::sleep(HAVE_REFRESH).await;
                match source.have().await {
                    Ok(Some(map)) => holds = Holds::Map(map),
                    // A known partial seed that did not answer this time
                    // keeps its last map (it is not taken to hold it all).
                    Ok(None) => {}
                    Err(_) => break,
                }
                continue;
            }
        };

        // What this unit must come to: every unit is whole but the last.
        let want = (size - off).min(UNIT_SIZE);
        match fetch_proved(source.as_ref(), root, off, len).await {
            // A verified answer of another length is still not this unit:
            // the peer is serving some other size, and is retired below.
            Ok(proved) if proved.bytes.len() as u64 == want => {
                let write: Result<bool, std::io::Error> = (|| {
                    let mut s = state.lock().expect("not poisoned");
                    // Under the same lock as the write, so no other lane of
                    // this source can take the unit it just fetched.
                    s.working.remove(&(off, index));
                    if s.stopped {
                        return Err(std::io::Error::other("the fetch was dropped"));
                    }
                    if s.done.contains(&off) {
                        return Ok(false); // endgame race: other copy won
                    }
                    let mut f = std::fs::OpenOptions::new().write(true).open(&dest)?;
                    f.seek(SeekFrom::Start(off))?;
                    f.write_all(&proved.bytes)?;
                    // Keep the proof, so the unit can be served on; shared
                    // only once both the bytes and their proof are down, and
                    // never what another burrow lent this fetch.
                    let kept = s.proofs.as_mut().map(|proofs| proved.keep(proofs).is_ok());
                    if !source.shareable() {
                        s.borrowed.insert(off);
                    } else if kept == Some(true) {
                        if let Some(sharing) = &s.sharing {
                            sharing.mark(off / HAVE_UNIT);
                        }
                    }
                    s.done.insert(off);
                    s.served[index] += 1;
                    s.landed();
                    s.settle(off, &complete);
                    // Emit the live progress event under the lock, so
                    // `done_units` is a consistent snapshot.
                    if let Some(tx) = progress.as_ref() {
                        let _ = tx.send(UnitDone {
                            endpoint: label.clone(),
                            offset: off,
                            done_units: s.done.len() as u64,
                            total_units,
                        });
                    }
                    Ok(true)
                })();
                match write {
                    Ok(true) => {
                        completed += 1;
                        idle_since = None;
                    }
                    // The other copy landed it first.
                    Ok(false) => state.lock().expect("not poisoned").settle(off, &complete),
                    Err(e) => {
                        // Local IO failure: put the unit back, note it for
                        // the fetch's error, and stop.
                        let mut s = state.lock().expect("not poisoned");
                        if !s.stopped && s.local.is_none() {
                            s.local = Some(e);
                        }
                        give_back(&mut s, off, len, index, endgame, &complete);
                        break;
                    }
                }
            }
            // Not now: its session is busy, or it is not ready. Nothing is
            // learned about what it holds, and it costs it no strike — the
            // unit simply goes to whoever is free.
            Err(PeerError::Refused(STATUS_BUSY)) => {
                give_back(
                    &mut state.lock().expect("not poisoned"),
                    off,
                    len,
                    index,
                    endgame,
                    &complete,
                );
                let since = *idle_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= IDLE_LIMIT {
                    break;
                }
                tokio::time::sleep(SIBLING_WAIT).await;
            }
            // It holds the file but not this unit (its map was behind, or
            // the unit would not prove out there): the unit goes to another
            // source, and this one stays for the rest.
            Err(PeerError::Refused(STATUS_NOT_HELD)) => {
                holds.lacks(off, want, size);
                give_back(
                    &mut state.lock().expect("not poisoned"),
                    off,
                    len,
                    index,
                    endgame,
                    &complete,
                );
                misses += 1;
                if misses > MAX_MISSES {
                    break;
                }
            }
            Ok(_) | Err(_) => {
                // This source failed: hand the unit back and retire it.
                give_back(
                    &mut state.lock().expect("not poisoned"),
                    off,
                    len,
                    index,
                    endgame,
                    &complete,
                );
                break;
            }
        }
    }
    (label, completed)
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

    /// A seed of part of a file (`body` on disk with every proof), sharing
    /// only the units in `held`, served on a peer endpoint of its own.
    async fn partial_peer(
        key: &IdentityKey,
        dir: &Path,
        name: &str,
        body: &[u8],
        held: &[u64],
    ) -> PeerServer {
        use bao_tree::io::outboard::PreOrderMemOutboard;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let root = *blake3::hash(body).as_bytes();
        let outboard = PreOrderMemOutboard::create(body, crate::peer::PEER_BLOCK);
        let proofs = proofs_path(&path);
        std::fs::write(&proofs, &outboard.data).unwrap();
        let seeds = Arc::new(SeedStore::new());
        seeds
            .add_partial(
                root,
                body.len() as u64,
                &path,
                &proofs,
                held.iter().copied(),
            )
            .unwrap();
        PeerServer::start("127.0.0.1:0".parse().unwrap(), key.public().0, seeds)
            .await
            .unwrap()
    }

    async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !ok() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
    }

    /// No one holds the whole file: one peer has the first two units, the
    /// other the rest. A fetch from the first shares those two as soon as
    /// they land, while it waits for the rest; a second fetcher takes them
    /// from it and the rest from the other peer, each unit verified.
    #[tokio::test]
    async fn each_unit_comes_from_whoever_holds_it_and_a_fetch_shares_as_it_goes() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[11; 32]);
        let body = payload(4 * 1024 * 1024 + 99); // five units
        let root = *blake3::hash(&body).as_bytes();
        let size = body.len() as u64;
        let token = CapToken::issue(&key, root, "t", now() + 600)
            .unwrap()
            .to_bytes();
        let front = partial_peer(&key, dir.path(), "front.bin", &body, &[0, 1]).await;
        let back = partial_peer(&key, dir.path(), "back.bin", &body, &[2, 3, 4]).await;

        // Ann fetches from the front half's peer, sharing as she goes.
        let ann_seeds = Arc::new(SeedStore::new());
        let ann_peer = PeerServer::start(
            "127.0.0.1:0".parse().unwrap(),
            key.public().0,
            ann_seeds.clone(),
        )
        .await
        .unwrap();
        let ann_dest = dir.path().join("ann.bin");
        let ann = tokio::spawn({
            let (sources, token, dest, seeds) = (
                vec![peer_source(&front)],
                token.clone(),
                ann_dest.clone(),
                ann_seeds.clone(),
            );
            async move { fetch_swarm_sharing(&sources, &token, root, size, &dest, None, seeds).await }
        });
        wait_for("Ann shares the first two units", || {
            ann_seeds
                .have(&root)
                .is_some_and(|m| m.covers(0, 2 * UNIT_SIZE))
        })
        .await;
        assert!(!ann_seeds.have(&root).unwrap().covers(2 * UNIT_SIZE, 1));

        // Bob takes the first two from Ann, the rest from the back peer.
        let bob_dest = dir.path().join("bob.bin");
        let report = fetch_swarm_resumable(
            &[peer_source(&ann_peer), peer_source(&back)],
            &token,
            root,
            size,
            &bob_dest,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&bob_dest).unwrap(), body, "whole and exact");
        assert_eq!(report.per_source[0].1, 2, "Ann's two units");
        assert_eq!(report.per_source[1].1, 3, "the back peer's three");

        // Ann's fetch is still waiting for the rest; stopping it stops her
        // sharing her part.
        ann.abort();
        let _ = ann.await;
        wait_for("Ann's part is no longer shared", || {
            ann_seeds.have(&root).is_none()
        })
        .await;
        // What she had stays for a resume: the record and the proofs.
        assert!(rhstate_path(&ann_dest).exists() && proofs_path(&ann_dest).exists());

        // Resumed from the back peer, she shares her first two units at
        // once (their proofs were kept), finishes, and seeds the whole file.
        let ann_again = tokio::spawn({
            let (sources, token, dest, seeds) = (
                vec![peer_source(&back)],
                token.clone(),
                ann_dest.clone(),
                ann_seeds.clone(),
            );
            async move { fetch_swarm_sharing(&sources, &token, root, size, &dest, None, seeds).await }
        });
        wait_for("Ann shares her resumed units", || {
            ann_seeds
                .have(&root)
                .is_some_and(|m| m.covers(0, 2 * UNIT_SIZE))
        })
        .await;
        let report = ann_again.await.unwrap().unwrap();
        assert_eq!(report.per_source[0].1, 3, "only the three she lacked");
        assert_eq!(std::fs::read(&ann_dest).unwrap(), body);
        assert_eq!(
            ann_seeds.have(&root),
            Some(crate::peer::HaveMap::whole(size))
        );
        assert!(!proofs_path(&ann_dest).exists() && !rhstate_path(&ann_dest).exists());

        // Whole now: a third fetcher gets every unit from her.
        let cy = dir.path().join("cy.bin");
        let report = fetch_swarm(&[peer_source(&ann_peer)], &token, root, size, &cy)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&cy).unwrap(), body);
        assert_eq!(report.per_source[0].1, 5);
    }

    /// A caller that drops the fetch (a timeout, a cancel) stops its
    /// workers: nothing goes on fetching and writing its file.
    #[tokio::test]
    async fn dropping_a_fetch_stops_its_workers() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[8; 32]);
        let body = payload(24 * 1024 * 1024);
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();
        let peer = seeding_peer(&key, root, &src).await;
        let token = CapToken::issue(&key, root, "t", now() + 600)
            .unwrap()
            .to_bytes();
        let dest = dir.path().join("out.bin");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sources = [peer_source(&peer)];
        let fetch = fetch_swarm_resumable_with_progress(
            &sources,
            &token,
            root,
            body.len() as u64,
            &dest,
            tx,
        );
        tokio::select! {
            _ = fetch => panic!("24 units fetched before the first was reported"),
            first = rx.recv() => assert!(first.is_some()),
        }
        // The fetch is dropped: its worker, the last holder of the progress
        // channel, is stopped, so the channel closes with the file unfinished.
        let mut after = 0;
        let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while rx.recv().await.is_some() {
                after += 1;
            }
        })
        .await;
        assert!(
            closed.is_ok(),
            "the worker went on after the fetch was dropped"
        );
        assert!(after < 4, "{after} more units after the fetch was dropped");
        peer.stop();
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
    async fn corrupt_partial_self_heals_on_next_resume() {
        // A completed unit's on-disk bytes get corrupted while `.rhstate` still
        // lists that offset as done. The verify-fail must remove BOTH the state
        // and the destination so the *next* resume starts clean and succeeds —
        // otherwise the poison is permanent (the corrupt unit is skipped forever).
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[9; 32]);
        let body = payload(2 * 1024 * 1024 + 5); // three units
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();
        let peer = seeding_peer(&key, root, &src).await;
        let sources = vec![peer_source(&peer)];
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();
        let dest = dir.path().join("out.bin");

        // First fetch completes cleanly.
        fetch_swarm_resumable(&sources, &token, root, body.len() as u64, &dest)
            .await
            .unwrap();
        assert!(!rhstate_path(&dest).exists());

        // Simulate a corrupted first unit + a stale state file that still trusts it.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(&dest).unwrap();
            f.seek(SeekFrom::Start(0)).unwrap();
            f.write_all(&[0xFF; 4096]).unwrap();
        }
        let state = RhState {
            root,
            size: body.len() as u64,
            done: vec![0, UNIT_SIZE, 2 * UNIT_SIZE],
            borrowed: Vec::new(),
        };
        std::fs::write(rhstate_path(&dest), postcard::to_allocvec(&state).unwrap()).unwrap();

        // The poisoned resume fails its whole-file verify, but self-heals: state
        // + dest are removed, so a follow-up fetch re-downloads and succeeds.
        assert!(
            fetch_swarm_resumable(&sources, &token, root, body.len() as u64, &dest)
                .await
                .is_err()
        );
        assert!(!rhstate_path(&dest).exists(), "poisoned state removed");
        fetch_swarm_resumable(&sources, &token, root, body.len() as u64, &dest)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body, "recovered byte-exact");
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
            borrowed: Vec::new(),
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
            borrowed: Vec::new(),
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
    async fn a_resume_record_with_offsets_no_unit_starts_at_is_read_without_them() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[12; 32]);
        let body = payload(2 * 1024 * 1024 + 5); // three units
        let src = dir.path().join("seed.bin");
        std::fs::write(&src, &body).unwrap();
        let root = *blake3::hash(&body).as_bytes();
        let peer = seeding_peer(&key, root, &src).await;
        let token = CapToken::issue(&key, root, "tester", now() + 60)
            .unwrap()
            .to_bytes();
        let dest = dir.path().join("out.bin");
        let size = body.len() as u64;
        // Past the end, not on a unit boundary, and near the top of u64:
        // none of them is a unit of this file.
        let state = RhState {
            root,
            size,
            done: vec![u64::MAX - 1, size, 7, 3 * UNIT_SIZE],
            borrowed: Vec::new(),
        };
        std::fs::write(rhstate_path(&dest), postcard::to_allocvec(&state).unwrap()).unwrap();
        let seeds = Arc::new(SeedStore::new());
        let report = fetch_swarm_sharing(
            &[peer_source(&peer)],
            &token,
            root,
            size,
            &dest,
            None,
            seeds.clone(),
        )
        .await
        .unwrap();
        assert_eq!(report.per_source[0].1, 3, "every unit fetched");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(seeds.holds_whole(&root));
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
            borrowed: Vec::new(),
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

    /// A source serving a file from disk with several lanes, counting the
    /// asks for each unit and the most at once. Its first ask fails when
    /// told to.
    struct Laned {
        data: PathBuf,
        proofs: PathBuf,
        root: [u8; 32],
        size: u64,
        lanes: usize,
        fail_first: std::sync::atomic::AtomicBool,
        asks: Mutex<std::collections::HashMap<u64, u32>>,
        now: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
    }

    impl Laned {
        fn new(dir: &Path, body: &[u8], lanes: usize) -> Self {
            let data = dir.join("laned.bin");
            let proofs = dir.join("laned.obao");
            std::fs::write(&data, body).unwrap();
            let root = *blake3::hash(body).as_bytes();
            crate::peer::write_outboard(&data, root, &proofs).unwrap();
            Laned {
                data,
                proofs,
                root,
                size: body.len() as u64,
                lanes,
                fail_first: false.into(),
                asks: Mutex::new(Default::default()),
                now: 0.into(),
                most: 0.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl RangeSource for Laned {
        fn label(&self) -> String {
            "laned".into()
        }

        async fn have(&self) -> Result<Option<HaveMap>, PeerError> {
            Ok(None)
        }

        async fn bao(&self, offset: u64, len: u64) -> Result<Vec<crate::BaoPiece>, PeerError> {
            use std::sync::atomic::Ordering::SeqCst;
            *self.asks.lock().unwrap().entry(offset).or_default() += 1;
            let now = self.now.fetch_add(1, SeqCst) + 1;
            self.most.fetch_max(now, SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.now.fetch_sub(1, SeqCst);
            if self.fail_first.swap(false, SeqCst) {
                return Err(PeerError::Refused(crate::peer::STATUS_BAD_REQUEST));
            }
            let stream = crate::peer::encode_proved(
                &self.data,
                &self.proofs,
                self.root,
                self.size,
                offset,
                len,
            )?;
            Ok(vec![crate::BaoPiece {
                offset,
                len,
                size: self.size,
                stream,
            }])
        }

        fn lanes(&self) -> usize {
            self.lanes
        }
    }

    #[tokio::test]
    async fn a_source_with_lanes_is_never_asked_for_one_unit_twice_at_once() {
        let dir = tempfile::tempdir().unwrap();
        // One unit: one ask, however many lanes (no endgame copies onto the
        // same source).
        let small = payload(700_000);
        let one = Arc::new(Laned::new(dir.path(), &small, 4));
        let dest = dir.path().join("one.out");
        let sources: Vec<Arc<dyn RangeSource>> = vec![one.clone()];
        fetch_swarm_from(&sources, one.root, one.size, &dest, None, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), small);
        assert_eq!(one.asks.lock().unwrap().values().sum::<u32>(), 1);

        // Three units over four lanes: each asked once, at once.
        let sub = dir.path().join("three");
        std::fs::create_dir(&sub).unwrap();
        let body = payload(2 * UNIT_SIZE as usize + 5);
        let three = Arc::new(Laned::new(&sub, &body, 4));
        let dest = dir.path().join("three.out");
        let sources: Vec<Arc<dyn RangeSource>> = vec![three.clone()];
        fetch_swarm_from(&sources, three.root, three.size, &dest, None, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let asks = three.asks.lock().unwrap().clone();
        assert_eq!(asks.len(), 3);
        assert!(asks.values().all(|&n| n == 1), "{asks:?}");
        assert_eq!(three.most.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn what_a_lending_source_sends_is_never_offered_on() {
        let dir = tempfile::tempdir().unwrap();
        let body = payload(UNIT_SIZE as usize + 40);
        let lender = Arc::new(Laned::new(dir.path(), &body, 1));
        struct Lending(Arc<Laned>);
        #[async_trait::async_trait]
        impl RangeSource for Lending {
            fn label(&self) -> String {
                "another burrow".into()
            }
            async fn have(&self) -> Result<Option<HaveMap>, PeerError> {
                self.0.have().await
            }
            async fn bao(&self, offset: u64, len: u64) -> Result<Vec<crate::BaoPiece>, PeerError> {
                self.0.bao(offset, len).await
            }
            fn shareable(&self) -> bool {
                false
            }
        }
        let seeds = Arc::new(SeedStore::default());
        let dest = dir.path().join("borrowed.out");
        let sources: Vec<Arc<dyn RangeSource>> = vec![Arc::new(Lending(lender.clone()))];
        let report = fetch_swarm_from(
            &sources,
            lender.root,
            lender.size,
            &dest,
            None,
            Some(seeds.clone()),
        )
        .await
        .unwrap();
        // The file is here and whole, and it is nobody else's to be given.
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(report.borrowed);
        assert!(!seeds.holds_whole(&lender.root));
        assert!(seeds.have(&lender.root).is_none());
    }

    #[tokio::test]
    async fn a_resumed_fetch_still_does_not_offer_on_what_was_lent() {
        let dir = tempfile::tempdir().unwrap();
        let body = payload(3 * UNIT_SIZE as usize + 9);
        let lender = Arc::new(Laned::new(dir.path(), &body, 1));
        struct Lending(Arc<Laned>, bool);
        #[async_trait::async_trait]
        impl RangeSource for Lending {
            fn label(&self) -> String {
                "another burrow".into()
            }
            async fn have(&self) -> Result<Option<HaveMap>, PeerError> {
                self.0.have().await
            }
            async fn bao(&self, offset: u64, len: u64) -> Result<Vec<crate::BaoPiece>, PeerError> {
                // The lending phase can land one unit, but cannot finish the
                // file before the test observes progress and cancels it.
                if !self.1 && offset != 0 {
                    std::future::pending::<()>().await;
                }
                self.0.bao(offset, len).await
            }
            fn shareable(&self) -> bool {
                self.1
            }
        }
        let dest = dir.path().join("half.out");
        // Cancel only after a verified borrowed unit lands. A fixed delay can
        // expire before any disk work finishes on a slower CI runner.
        {
            let sources: Vec<Arc<dyn RangeSource>> = vec![Arc::new(Lending(lender.clone(), false))];
            let (progress, mut updates) = tokio::sync::mpsc::unbounded_channel();
            let mut fetch = Box::pin(fetch_swarm_from(
                &sources,
                lender.root,
                lender.size,
                &dest,
                Some(progress),
                None,
            ));
            let landed = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    unit = updates.recv() => unit.expect("the fetch reports its first unit"),
                    result = &mut fetch => panic!("partial fetch finished before cancellation: {result:?}"),
                }
            })
            .await
            .expect("the first borrowed unit lands before the safety deadline");
            assert_eq!(landed.offset, 0);
            assert_eq!(landed.done_units, 1);
            assert_eq!(landed.total_units, 4);
            // Dropping the actual future stops its worker and flushes the
            // partial resume record before it is inspected below.
            drop(fetch);
        }
        let record =
            load_rhstate(&rhstate_path(&dest), &lender.root, lender.size).expect("a resume record");
        assert_eq!(record.done, vec![0], "exactly the first unit landed");
        assert_eq!(
            record.borrowed, record.done,
            "and all of it was lent: {record:?}"
        );

        // It finishes from a source that gives rather than lends, with
        // sharing on: what was lent before is still not offered, and nor is
        // the file as a whole.
        let seeds = Arc::new(SeedStore::default());
        let sources: Vec<Arc<dyn RangeSource>> = vec![Arc::new(Lending(lender.clone(), true))];
        let report = fetch_swarm_from(
            &sources,
            lender.root,
            lender.size,
            &dest,
            None,
            Some(seeds.clone()),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(report.per_source[0].1, 3, "only missing units were fetched");
        assert!(report.borrowed, "the lent units are remembered");
        assert!(!seeds.holds_whole(&lender.root));
        assert!(seeds.have(&lender.root).is_none());
    }

    #[tokio::test]
    async fn a_lane_that_fails_leaves_its_unit_to_the_sources_other_lanes() {
        let dir = tempfile::tempdir().unwrap();
        let body = payload(UNIT_SIZE as usize + 77);
        let source = Arc::new(Laned::new(dir.path(), &body, 2));
        source
            .fail_first
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dest = dir.path().join("out");
        let sources: Vec<Arc<dyn RangeSource>> = vec![source.clone()];
        let report = fetch_swarm_from(&sources, source.root, source.size, &dest, None, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(report.per_source, vec![("laned".to_string(), 2)]);
        // The failed lane's unit went to the other lane: that unit asked
        // for twice, one after the other, three asks in all.
        assert_eq!(source.asks.lock().unwrap().values().sum::<u32>(), 3);
    }
}
