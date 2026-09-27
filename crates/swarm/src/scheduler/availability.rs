//! Fetch-local availability, not an assertion that a peer's bytes are valid.
//!
//! One entry means one caller-supplied source, regardless of its lane count.
//! Callers passing duplicate source entries must deduplicate them first: a
//! human-readable `RangeSource::label` is not a trustworthy source identity.
//! Maps never cross the coordinator wire or change the verification anchor.

use super::{Holds, UNIT_SIZE};
use crate::peer::SourceRejection;
use crate::peer::{HaveMap, PeerError, PEER_BLOCK_BYTES, STATUS_DENIED, STATUS_NOT_FOUND};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// Maximum units returned by one snapshot, even when the caller asks for more.
pub const AVAILABILITY_PAGE_MAX: usize = 4096;

/// At most this many source/reason pairs are retained for one invocation.
pub const SOURCE_DIAGNOSTICS_MAX: usize = 32;

/// Evidence of rejected bytes, without remote labels, paths or error text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceDiagnostic {
    /// Zero-based position in this invocation's source list; labels are not identities.
    pub source_index: usize,
    pub first_offset: u64,
    pub reason: SourceRejection,
    pub occurrences: u64,
}

/// A bounded cumulative snapshot, retained even when another source succeeds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceDiagnostics {
    pub rejections: Vec<SourceDiagnostic>,
    /// Further rejected responses whose source/reason did not fit the bound.
    pub omitted: u64,
}

/// Local bytes and remote claims are deliberately separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalUnit {
    Missing,
    /// Listed by the resume record; not reverified in this invocation yet.
    Resumed,
    /// Bao-verified in this invocation, or covered by its final whole-file hash.
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityPhase {
    Waiting,
    Fetching,
    Verifying,
    Complete,
    Failed,
    Cancelled,
}

impl AvailabilityPhase {
    fn terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Cancelled)
    }
}

/// Availability of one 1 MiB scheduling unit (the final unit may be shorter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitAvailability {
    pub index: u64,
    /// Live sources whose latest HaveMap claims the entire unit.
    pub claimed_sources: usize,
    /// Live sources treated as whole-file sources for old-peer compatibility
    /// or because their initial availability query failed. Still unverified.
    pub assumed_sources: usize,
    /// Subset of `claimed_sources` whose last refresh failed. A stale map
    /// remains a scheduling hint; a NOT_HELD reply removes its unit immediately.
    pub stale_sources: usize,
    pub local: LocalUnit,
}

/// A bounded page from a consistent point-in-time view of one fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilitySnapshot {
    pub root: Option<[u8; 32]>,
    pub size: u64,
    pub unit_size: u64,
    pub total_units: u64,
    pub revision: u64,
    pub phase: AvailabilityPhase,
    pub sources: usize,
    pub live_sources: usize,
    /// Sources still awaiting their first availability result.
    pub probing_sources: usize,
    pub start_unit: u64,
    pub units: Vec<UnitAvailability>,
}

/// The producer is consumed by one fetch and cannot be cloned or reused for
/// another root. Dropping its fetch publishes cancellation to its observers.
pub struct AvailabilityReporter {
    pub(super) tracker: Tracker,
}

impl Drop for AvailabilityReporter {
    fn drop(&mut self) {
        self.tracker.finish(AvailabilityPhase::Cancelled);
    }
}

/// A read-only, cloneable handle that remains readable after a fetch ends.
/// Notifications coalesce: a slow consumer cannot accumulate an event queue.
#[derive(Clone)]
pub struct AvailabilityObserver {
    tracker: Tracker,
    updates: watch::Receiver<u64>,
}

/// Create an observer and its one-use producer for
/// [`super::fetch_swarm_from_with_availability`]. Snapshots remain available on
/// both success and failure, without changing `FetchReport` or existing APIs.
pub fn availability_channel() -> (AvailabilityReporter, AvailabilityObserver) {
    let tracker = Tracker::new();
    let updates = tracker.0.changed.subscribe();
    (
        AvailabilityReporter {
            tracker: tracker.clone(),
        },
        AvailabilityObserver { tracker, updates },
    )
}

impl AvailabilityObserver {
    /// Source verification failures only. Storage, worker and transport failures
    /// are not evidence that a source supplied invalid bytes.
    pub fn diagnostics(&self) -> SourceDiagnostics {
        self.tracker
            .0
            .state
            .lock()
            .expect("not poisoned")
            .diagnostics
            .clone()
    }

    /// Page by unit index, not byte offset. Pages are capped and out-of-range
    /// requests return an empty page; callers can inspect `total_units`.
    pub fn snapshot(&self, start_unit: u64, limit: usize) -> AvailabilitySnapshot {
        let state = self.tracker.0.state.lock().expect("not poisoned");
        let total_units = state.size.div_ceil(UNIT_SIZE);
        let end = start_unit
            .saturating_add(limit.min(AVAILABILITY_PAGE_MAX) as u64)
            .min(total_units);
        let mut units = Vec::with_capacity(end.saturating_sub(start_unit) as usize);
        for index in start_unit..end {
            let offset = index * UNIT_SIZE;
            let len = (state.size - offset).min(UNIT_SIZE);
            let mut unit = UnitAvailability {
                index,
                claimed_sources: 0,
                assumed_sources: 0,
                stale_sources: 0,
                local: if state.verified.contains(&index)
                    || state.phase == AvailabilityPhase::Complete
                {
                    LocalUnit::Verified
                } else if state.resumed.contains(&index) {
                    LocalUnit::Resumed
                } else {
                    LocalUnit::Missing
                },
            };
            for source in state.sources.iter().filter(|s| s.live()) {
                match &source.holds {
                    Some(Holds::All) => unit.assumed_sources += 1,
                    Some(Holds::Map(map)) if map.covers(offset, len) => {
                        unit.claimed_sources += 1;
                        unit.stale_sources += usize::from(source.stale);
                    }
                    _ => {}
                }
            }
            units.push(unit);
        }
        AvailabilitySnapshot {
            root: state.root,
            size: state.size,
            unit_size: UNIT_SIZE,
            total_units,
            revision: state.revision,
            phase: state.phase,
            sources: state.sources.len(),
            live_sources: state.sources.iter().filter(|s| s.live()).count(),
            probing_sources: state
                .sources
                .iter()
                .filter(|s| s.live() && !s.initialized)
                .count(),
            start_unit,
            units,
        }
    }

    /// Wait for a newer revision. Returns false once the terminal revision has
    /// been observed; the snapshot can still be read after that.
    pub async fn changed(&mut self) -> bool {
        if self
            .tracker
            .0
            .state
            .lock()
            .expect("not poisoned")
            .phase
            .terminal()
            && !self.updates.has_changed().unwrap_or(false)
        {
            return false;
        }
        self.updates.changed().await.is_ok()
    }
}

struct Source {
    holds: Option<Holds>,
    lanes: usize,
    initialized: bool,
    retired: bool,
    stale: bool,
    /// A negative range reply invalidates any older in-flight map reply.
    generation: u64,
}

impl Source {
    fn live(&self) -> bool {
        self.lanes > 0 && !self.retired
    }
}

struct State {
    root: Option<[u8; 32]>,
    size: u64,
    revision: u64,
    phase: AvailabilityPhase,
    sources: Vec<Source>,
    resumed: HashSet<u64>,
    verified: HashSet<u64>,
    diagnostics: SourceDiagnostics,
}

struct Shared {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

#[derive(Clone)]
pub(super) struct Tracker(Arc<Shared>);

impl Tracker {
    pub(super) fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                root: None,
                size: 0,
                revision: 0,
                phase: AvailabilityPhase::Waiting,
                sources: Vec::new(),
                resumed: HashSet::new(),
                verified: HashSet::new(),
                diagnostics: SourceDiagnostics::default(),
            }),
            changed,
        }))
    }

    fn publish(&self, state: &mut State) {
        state.revision = state.revision.saturating_add(1);
        self.0.changed.send_replace(state.revision);
    }

    pub(super) fn begin(&self, root: [u8; 32], size: u64) {
        let mut state = self.0.state.lock().expect("not poisoned");
        state.root = Some(root);
        state.size = size;
        state.phase = AvailabilityPhase::Fetching;
        self.publish(&mut state);
    }

    pub(super) fn initialize(&self, lanes: &[usize], resumed: &HashSet<u64>) {
        let mut state = self.0.state.lock().expect("not poisoned");
        state.sources = lanes
            .iter()
            .map(|&lanes| Source {
                holds: None,
                lanes,
                initialized: false,
                retired: false,
                stale: false,
                generation: 0,
            })
            .collect();
        state.resumed = resumed.iter().map(|off| off / UNIT_SIZE).collect();
        self.publish(&mut state);
    }

    pub(super) fn verifying(&self) {
        let mut state = self.0.state.lock().expect("not poisoned");
        state.phase = AvailabilityPhase::Verifying;
        self.publish(&mut state);
    }

    pub(super) fn finish(&self, phase: AvailabilityPhase) {
        let mut state = self.0.state.lock().expect("not poisoned");
        if state.phase.terminal() {
            return;
        }
        state.phase = phase;
        for source in &mut state.sources {
            source.retired = true;
            source.generation = source.generation.saturating_add(1);
        }
        self.publish(&mut state);
    }

    pub(super) fn stop_sources(&self) {
        let mut state = self.0.state.lock().expect("not poisoned");
        for source in &mut state.sources {
            source.retired = true;
            source.generation = source.generation.saturating_add(1);
        }
        self.publish(&mut state);
    }

    pub(super) fn invalidate_local(&self) {
        let mut state = self.0.state.lock().expect("not poisoned");
        state.resumed.clear();
        state.verified.clear();
        self.publish(&mut state);
    }

    pub(super) fn landed(&self, off: u64) {
        let mut state = self.0.state.lock().expect("not poisoned");
        state.verified.insert(off / UNIT_SIZE);
        self.publish(&mut state);
    }

    pub(super) fn rejected(&self, index: usize, offset: u64, reason: SourceRejection) {
        let mut state = self.0.state.lock().expect("not poisoned");
        if state.phase.terminal() || index >= state.sources.len() {
            return;
        }
        let diagnostics = &mut state.diagnostics;
        if let Some(found) = diagnostics
            .rejections
            .iter_mut()
            .find(|d| d.source_index == index && d.reason == reason)
        {
            found.occurrences = found.occurrences.saturating_add(1);
        } else if diagnostics.rejections.len() < SOURCE_DIAGNOSTICS_MAX {
            diagnostics.rejections.push(SourceDiagnostic {
                source_index: index,
                first_offset: offset,
                reason,
                occurrences: 1,
            });
        } else {
            diagnostics.omitted = diagnostics.omitted.saturating_add(1);
        }
        self.publish(&mut state);
    }

    pub(super) fn leave_lane(&self, index: usize) {
        let mut state = self.0.state.lock().expect("not poisoned");
        let source = &mut state.sources[index];
        source.lanes = source.lanes.saturating_sub(1);
        if source.lanes == 0 {
            source.retired = true;
            source.generation = source.generation.saturating_add(1);
            self.publish(&mut state);
        }
    }

    pub(super) fn query_generation(&self, index: usize) -> Option<u64> {
        let state = self.0.state.lock().expect("not poisoned");
        let source = &state.sources[index];
        source.live().then_some(source.generation)
    }

    /// Apply only a query started after the last contrary range reply. A
    /// later fresh query may legitimately report that the peer gained a unit.
    pub(super) fn queried(
        &self,
        index: usize,
        generation: u64,
        result: Result<Option<HaveMap>, PeerError>,
    ) {
        let mut state = self.0.state.lock().expect("not poisoned");
        let size = state.size;
        let source = &mut state.sources[index];
        if !source.live() || source.generation != generation {
            return;
        }
        match result {
            Ok(Some(mut map)) if map.unit > 0 && map.unit % PEER_BLOCK_BYTES == 0 => {
                // Match the peer-wire frame cap even for custom RangeSource
                // implementations; bits beyond this file are irrelevant.
                let needed = size.div_ceil(map.unit).div_ceil(8).min(1 << 20) as usize;
                map.bits.truncate(needed);
                source.holds = Some(Holds::Map(map));
                source.stale = false;
            }
            Ok(None) => {
                // A known partial never becomes whole because a re-ask got
                // no answer (the compatibility result from old peers).
                if source.holds.is_none() {
                    source.holds = Some(Holds::All);
                } else if matches!(source.holds, Some(Holds::Map(_))) {
                    source.stale = true;
                }
            }
            Err(PeerError::Refused(STATUS_DENIED | STATUS_NOT_FOUND)) => {
                source.retired = true;
            }
            _ => {
                source.stale = true;
                if source.holds.is_none() {
                    source.holds = Some(Holds::All);
                }
            }
        }
        source.initialized = true;
        self.publish(&mut state);
    }

    pub(super) fn lacks(&self, index: usize, off: u64, len: u64) {
        let mut state = self.0.state.lock().expect("not poisoned");
        let size = state.size;
        let source = &mut state.sources[index];
        if let Some(holds) = &mut source.holds {
            holds.lacks(off, len, size);
        }
        source.generation = source.generation.saturating_add(1);
        self.publish(&mut state);
    }

    pub(super) async fn ready(&self, index: usize) -> bool {
        let mut updates = self.0.changed.subscribe();
        loop {
            {
                let state = self.0.state.lock().expect("not poisoned");
                let source = &state.sources[index];
                if !source.live() {
                    return false;
                }
                if source.initialized {
                    return true;
                }
            }
            if updates.changed().await.is_err() {
                return false;
            }
        }
    }

    /// Scheduler lock is acquired before this lock; observers never acquire
    /// the scheduler lock, so there is no inverse lock order.
    pub(super) fn with_holds<T>(&self, index: usize, f: impl FnOnce(Option<&Holds>) -> T) -> T {
        let state = self.0.state.lock().expect("not poisoned");
        let source = &state.sources[index];
        f(source.live().then_some(source.holds.as_ref()).flatten())
    }
}
