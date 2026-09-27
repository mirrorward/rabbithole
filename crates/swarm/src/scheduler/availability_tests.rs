use super::availability::*;
use super::*;
use crate::peer::{BaoPiece, PEER_BLOCK_BYTES};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

fn map(bits: u8) -> HaveMap {
    HaveMap {
        unit: UNIT_SIZE,
        bits: vec![bits],
    }
}

#[test]
fn counts_are_per_source_not_lane_and_claims_are_not_verified() {
    let (reporter, observer) = availability_channel();
    let tracker = &reporter.tracker;
    tracker.begin([7; 32], 3 * UNIT_SIZE + 1);
    tracker.initialize(&[4, 2, 1], &HashSet::from([0]));
    tracker.queried(0, 0, Ok(Some(map(0b0101))));
    tracker.queried(1, 0, Ok(Some(map(0b0110))));
    tracker.queried(2, 0, Ok(None));
    let snapshot = observer.snapshot(0, 8);
    assert_eq!(snapshot.live_sources, 3);
    assert_eq!(snapshot.probing_sources, 0);
    assert_eq!(
        snapshot
            .units
            .iter()
            .map(|u| u.claimed_sources)
            .collect::<Vec<_>>(),
        vec![1, 1, 2, 0]
    );
    assert!(snapshot.units.iter().all(|u| u.assumed_sources == 1));
    assert_eq!(snapshot.units[0].local, LocalUnit::Resumed);
    assert!(snapshot.units[1..]
        .iter()
        .all(|u| u.local == LocalUnit::Missing));
    tracker.landed(UNIT_SIZE);
    assert_eq!(observer.snapshot(1, 1).units[0].local, LocalUnit::Verified);
    tracker.leave_lane(0);
    assert_eq!(observer.snapshot(0, 1).units[0].claimed_sources, 1);
    for _ in 0..3 {
        tracker.leave_lane(0);
    }
    assert_eq!(observer.snapshot(0, 1).units[0].claimed_sources, 0);
}

#[test]
fn map_changes_and_negative_replies_invalidate_older_queries() {
    let (reporter, observer) = availability_channel();
    let tracker = &reporter.tracker;
    tracker.begin([8; 32], 3 * UNIT_SIZE);
    tracker.initialize(&[1], &HashSet::new());
    tracker.queried(0, 0, Ok(Some(map(0b001))));
    tracker.queried(0, 0, Ok(Some(map(0b110)))); // loses 0, gains 1 and 2
    assert_eq!(
        observer
            .snapshot(0, 3)
            .units
            .iter()
            .map(|u| u.claimed_sources)
            .collect::<Vec<_>>(),
        vec![0, 1, 1]
    );
    let stale = tracker.query_generation(0).unwrap();
    tracker.lacks(0, UNIT_SIZE, UNIT_SIZE);
    tracker.queried(0, stale, Ok(Some(map(0b111))));
    assert_eq!(
        observer
            .snapshot(0, 3)
            .units
            .iter()
            .map(|u| u.claimed_sources)
            .collect::<Vec<_>>(),
        vec![0, 0, 1]
    );
    let fresh = tracker.query_generation(0).unwrap();
    tracker.queried(0, fresh, Ok(Some(map(0b011)))); // genuinely newer gain
    tracker.queried(0, fresh, Ok(None)); // unanswered partial never means all
    let snapshot = observer.snapshot(0, 3);
    assert_eq!(
        snapshot
            .units
            .iter()
            .map(|u| u.claimed_sources)
            .collect::<Vec<_>>(),
        vec![1, 1, 0]
    );
    assert!(snapshot.units.iter().all(|u| u.assumed_sources == 0));
    assert_eq!(snapshot.units[0].stale_sources, 1);
    tracker.leave_lane(0);
    tracker.queried(0, fresh, Ok(Some(map(0b111))));
    assert_eq!(observer.snapshot(0, 3).live_sources, 0);
    assert!(observer
        .snapshot(0, 3)
        .units
        .iter()
        .all(|u| u.claimed_sources == 0));
}

#[test]
fn snapshot_pages_and_hostile_maps_are_bounded() {
    let (reporter, observer) = availability_channel();
    let tracker = &reporter.tracker;
    tracker.begin([9; 32], (AVAILABILITY_PAGE_MAX as u64 + 10) * UNIT_SIZE);
    tracker.initialize(&[1], &HashSet::new());
    for unit in [0, 1, PEER_BLOCK_BYTES - 1, u64::MAX] {
        tracker.queried(
            0,
            0,
            Ok(Some(HaveMap {
                unit,
                bits: vec![255; 2],
            })),
        );
        let page = observer.snapshot(0, usize::MAX);
        assert_eq!(page.units.len(), AVAILABILITY_PAGE_MAX);
        assert!(page.units.iter().all(|u| u.local == LocalUnit::Missing));
    }
    tracker.queried(
        0,
        0,
        Ok(Some(HaveMap {
            unit: PEER_BLOCK_BYTES,
            bits: vec![255; (1 << 20) + 1],
        })),
    );
    assert_eq!(
        observer
            .snapshot(AVAILABILITY_PAGE_MAX as u64 + 9, usize::MAX)
            .units
            .len(),
        1
    );
    assert!(observer.snapshot(u64::MAX, usize::MAX).units.is_empty());
    assert!(observer.snapshot(0, 0).units.is_empty());
}

#[tokio::test]
async fn observers_finish_on_errors_and_cancellation_before_a_worker_starts() {
    let (reporter, mut observer) = availability_channel();
    assert_eq!(observer.snapshot(0, 1).phase, AvailabilityPhase::Waiting);
    drop(reporter);
    assert!(observer.changed().await);
    assert_eq!(observer.snapshot(0, 1).phase, AvailabilityPhase::Cancelled);
    assert!(!observer.changed().await);

    let dir = tempfile::tempdir().unwrap();
    let (reporter, observer) = availability_channel();
    let missing = dir.path().join("missing/target");
    let result =
        fetch_swarm_from_with_availability(&[], [2; 32], 1, &missing, None, None, reporter).await;
    assert!(result.is_err());
    assert_eq!(observer.snapshot(0, 1).phase, AvailabilityPhase::Failed);

    let (reporter, observer) = availability_channel();
    let sources = Vec::new();
    let future =
        fetch_swarm_from_with_availability(&sources, [3; 32], 1, &missing, None, None, reporter);
    drop(future);
    assert_eq!(observer.snapshot(0, 1).phase, AvailabilityPhase::Cancelled);
}

struct Controlled {
    data: PathBuf,
    proofs: PathBuf,
    root: [u8; 32],
    size: u64,
    label: String,
    map: Mutex<Option<HaveMap>>,
    lanes: usize,
    have_calls: AtomicUsize,
    active_queries: AtomicUsize,
    most_queries: AtomicUsize,
    asks: Mutex<Vec<u64>>,
    entered: Semaphore,
    gate: Option<Arc<Semaphore>>,
    corrupt: bool,
}

impl Controlled {
    fn new(
        dir: &Path,
        label: &str,
        body: &[u8],
        map: Option<HaveMap>,
        lanes: usize,
        gate: Option<Arc<Semaphore>>,
        corrupt: bool,
    ) -> Self {
        let data = dir.join(format!("{label}.data"));
        let proofs = dir.join(format!("{label}.proofs"));
        std::fs::write(&data, body).unwrap();
        let root = *blake3::hash(body).as_bytes();
        crate::peer::write_outboard(&data, root, &proofs).unwrap();
        Self {
            data,
            proofs,
            root,
            size: body.len() as u64,
            label: label.into(),
            map: Mutex::new(map),
            lanes,
            have_calls: AtomicUsize::new(0),
            active_queries: AtomicUsize::new(0),
            most_queries: AtomicUsize::new(0),
            asks: Mutex::new(Vec::new()),
            entered: Semaphore::new(0),
            gate,
            corrupt,
        }
    }
}

#[async_trait::async_trait]
impl RangeSource for Controlled {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn lanes(&self) -> usize {
        self.lanes
    }
    async fn have(&self) -> Result<Option<HaveMap>, PeerError> {
        self.have_calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active_queries.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_queries.fetch_max(active, Ordering::SeqCst);
        // Keep a query pending long enough for wrongly independent lane
        // queries to overlap, without depending on a wall-clock sleep.
        struct Query<'a>(&'a AtomicUsize);
        impl Drop for Query<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _query = Query(&self.active_queries);
        tokio::task::yield_now().await;
        Ok(self.map.lock().unwrap().clone())
    }
    async fn bao(&self, offset: u64, len: u64) -> Result<Vec<BaoPiece>, PeerError> {
        self.asks.lock().unwrap().push(offset);
        self.entered.add_permits(1);
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        let mut stream = crate::peer::encode_proved(
            &self.data,
            &self.proofs,
            self.root,
            self.size,
            offset,
            len,
        )?;
        if self.corrupt {
            *stream.last_mut().unwrap() ^= 1;
        }
        Ok(vec![BaoPiece {
            offset,
            len,
            size: self.size,
            stream,
        }])
    }
}

fn body(units: usize) -> Vec<u8> {
    (0..units * UNIT_SIZE as usize)
        .map(|i| (i % 251) as u8)
        .collect()
}

async fn observe_until(
    observer: &mut AvailabilityObserver,
    what: &str,
    condition: impl Fn(&AvailabilitySnapshot) -> bool,
) -> AvailabilitySnapshot {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let snapshot = observer.snapshot(0, 20);
            if condition(&snapshot) {
                return snapshot;
            }
            assert!(
                observer.changed().await,
                "fetch ended before {what}: {snapshot:?}"
            );
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test]
async fn one_probe_per_source_refreshes_maps_while_all_its_lanes_are_busy() {
    let dir = tempfile::tempdir().unwrap();
    let data = body(4);
    let gate = Arc::new(Semaphore::new(0));
    let source = Arc::new(Controlled::new(
        dir.path(),
        "dynamic",
        &data,
        Some(map(0b1111)),
        4,
        Some(gate),
        false,
    ));
    let sources: Vec<Arc<dyn RangeSource>> = vec![source.clone()];
    let dest = dir.path().join("out");
    let (reporter, mut observer) = availability_channel();
    let (root, size) = (source.root, source.size);
    let task = tokio::spawn(async move {
        fetch_swarm_from_with_availability(&sources, root, size, &dest, None, None, reporter).await
    });
    observe_until(&mut observer, "initial map", |s| {
        s.units.len() == 4
            && s.probing_sources == 0
            && s.units.iter().all(|u| u.claimed_sources == 1)
    })
    .await;
    tokio::time::timeout(Duration::from_secs(12), source.entered.acquire_many(4))
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert_eq!(
        source.asks.lock().unwrap().len(),
        4,
        "every lane is blocked in its byte stream"
    );
    *source.map.lock().unwrap() = Some(map(0b1000));
    observe_until(
        &mut observer,
        "a refresh during blocked byte streams",
        |s| {
            s.units.len() == 4 && s.units[0].claimed_sources == 0 && s.units[3].claimed_sources == 1
        },
    )
    .await;
    assert!(source.have_calls.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        source.most_queries.load(Ordering::SeqCst),
        1,
        "one query per source, independent of elapsed ticks"
    );
    *source.map.lock().unwrap() = Some(map(0b0101));
    observe_until(&mut observer, "lost and gained units", |s| {
        s.units.len() == 4
            && s.units[0].claimed_sources == 1
            && s.units[2].claimed_sources == 1
            && s.units[3].claimed_sources == 0
    })
    .await;
    let snapshot = observer.snapshot(0, 4);
    assert!(snapshot.units.iter().all(|u| u.local == LocalUnit::Missing));
    task.abort();
    let _ = task.await;
    assert_eq!(observer.snapshot(0, 4).phase, AvailabilityPhase::Cancelled);
    assert_eq!(observer.snapshot(0, 4).live_sources, 0);
}

#[tokio::test]
async fn mixed_partial_sources_finish_and_a_lying_source_never_verifies_a_unit() {
    let dir = tempfile::tempdir().unwrap();
    let data = body(3);
    let gate = Arc::new(Semaphore::new(0));
    let bad = Arc::new(Controlled::new(
        dir.path(),
        "liar",
        &data,
        Some(map(0b111)),
        1,
        None,
        true,
    ));
    let front = Arc::new(Controlled::new(
        dir.path(),
        "front",
        &data,
        Some(map(0b001)),
        1,
        Some(gate.clone()),
        false,
    ));
    let back = Arc::new(Controlled::new(
        dir.path(),
        "back",
        &data,
        Some(map(0b110)),
        1,
        Some(gate.clone()),
        false,
    ));
    let sources: Vec<Arc<dyn RangeSource>> = vec![bad.clone(), front, back];
    let dest = dir.path().join("out");
    let output = dest.clone();
    let (root, size) = (bad.root, bad.size);
    let (reporter, mut observer) = availability_channel();
    let task = tokio::spawn(async move {
        fetch_swarm_from_with_availability(&sources, root, size, &dest, None, None, reporter).await
    });
    observe_until(
        &mut observer,
        "liar retired and both partial maps known",
        |s| s.units.len() == 3 && s.live_sources == 2 && s.probing_sources == 0,
    )
    .await;
    assert!(!bad.asks.lock().unwrap().is_empty());
    assert!(observer
        .snapshot(0, 3)
        .units
        .iter()
        .all(|u| u.local == LocalUnit::Missing));
    assert!(observer
        .snapshot(0, 3)
        .units
        .iter()
        .all(|u| u.claimed_sources == 1));
    gate.add_permits(3);
    let report = tokio::time::timeout(Duration::from_secs(12), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        report.per_source,
        vec![("liar".into(), 0), ("front".into(), 1), ("back".into(), 2)]
    );
    assert_eq!(std::fs::read(output).unwrap(), data);
    let snapshot = observer.snapshot(0, 3);
    assert_eq!(snapshot.phase, AvailabilityPhase::Complete);
    assert!(snapshot
        .units
        .iter()
        .all(|u| u.local == LocalUnit::Verified));
}

#[tokio::test]
async fn resume_records_are_not_claimed_verified_until_the_whole_file_passes() {
    for corrupt_resume in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let data = body(3);
        let gate = Arc::new(Semaphore::new(0));
        let source = Arc::new(Controlled::new(
            dir.path(),
            "whole",
            &data,
            None,
            2,
            Some(gate.clone()),
            false,
        ));
        let (root, size) = (source.root, source.size);
        let sources: Vec<Arc<dyn RangeSource>> = vec![source];
        let dest = dir.path().join("out");
        let mut previous = data.clone();
        if corrupt_resume {
            previous[0] ^= 1;
        }
        std::fs::write(&dest, previous).unwrap();
        std::fs::write(
            rhstate_path(&dest),
            postcard::to_allocvec(&RhState {
                root,
                size,
                done: vec![0],
                borrowed: vec![],
            })
            .unwrap(),
        )
        .unwrap();
        let output = dest.clone();
        let (reporter, mut observer) = availability_channel();
        let task = tokio::spawn(async move {
            fetch_swarm_from_with_availability(&sources, root, size, &dest, None, None, reporter)
                .await
        });
        observe_until(&mut observer, "resume record exposed as unverified", |s| {
            s.units.len() == 3 && s.units[0].local == LocalUnit::Resumed && s.probing_sources == 0
        })
        .await;
        assert!(observer
            .snapshot(1, 2)
            .units
            .iter()
            .all(|u| u.local == LocalUnit::Missing));
        gate.add_permits(2);
        let result = tokio::time::timeout(Duration::from_secs(12), task)
            .await
            .unwrap()
            .unwrap();
        let snapshot = observer.snapshot(0, 3);
        if corrupt_resume {
            assert!(result.is_err());
            assert_eq!(snapshot.phase, AvailabilityPhase::Failed);
            assert!(snapshot.units.iter().all(|u| u.local == LocalUnit::Missing));
            assert!(!output.exists(), "poisoned partial is removed as before");
        } else {
            assert!(result.is_ok());
            assert_eq!(snapshot.phase, AvailabilityPhase::Complete);
            assert!(snapshot
                .units
                .iter()
                .all(|u| u.local == LocalUnit::Verified));
            assert_eq!(std::fs::read(output).unwrap(), data);
        }
    }
}
