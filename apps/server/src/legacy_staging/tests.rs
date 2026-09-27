use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn target(name: &str) -> Target {
    Target {
        protocol: Protocol::Zmodem,
        account: 1,
        area: 1,
        parent: None,
        name: name.into(),
    }
}
fn offer() -> Offer {
    Offer {
        length: Some(10),
        mtime: Some(7),
    }
}
fn open(path: &Path, time: &Arc<AtomicU64>) -> Staging {
    let time = time.clone();
    Staging::open_with_clock(
        path.to_owned(),
        [3; 32],
        Arc::new(move || time.load(Ordering::SeqCst)),
    )
    .unwrap()
}

#[tokio::test]
async fn restart_validates_prefix_and_truncates_uncommitted_tail() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let (lease, bytes) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert!(bytes.is_empty());
    lease.append(0, b"hello").await.unwrap();
    drop(lease);
    let id = target_id(&target("a")).unwrap();
    OpenOptions::new()
        .append(true)
        .open(s.0.path(&id, "part"))
        .unwrap()
        .write_all(b"junk")
        .unwrap();
    drop(s);
    let s = open(dir.path(), &time);
    let (lease, bytes) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert_eq!(bytes, b"hello");
    assert_eq!(fs::metadata(s.0.path(&id, "part")).unwrap().len(), 5);
    lease.append(5, b"world").await.unwrap();
    drop(lease);
    drop(s);
    let s = open(dir.path(), &time);
    let (lease, bytes) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert_eq!(bytes, b"helloworld");
    lease.discard().await.unwrap();
    assert!(!s.0.path(&id, "meta").exists());
    assert!(!s.0.path(&id, "part").exists());
}

#[tokio::test]
async fn account_destination_and_offer_metadata_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    lease.append(0, b"hello").await.unwrap();
    assert!(s.claim(target("a"), offer(), 10).await.is_err());
    for other in [
        Target {
            account: 2,
            ..target("a")
        },
        Target {
            area: 2,
            ..target("a")
        },
        Target {
            parent: Some(4),
            ..target("a")
        },
        target("b"),
    ] {
        let (lease, data) = s.claim(other, offer(), 10).await.unwrap();
        assert!(data.is_empty());
        lease.discard().await.unwrap();
    }
    drop(lease);
    let (lease, data) = s
        .claim(
            target("a"),
            Offer {
                mtime: Some(8),
                ..offer()
            },
            10,
        )
        .await
        .unwrap();
    assert!(
        data.is_empty(),
        "different file metadata never inherits a prefix"
    );
    lease.append(0, b"abc").await.unwrap();
    drop(lease);
    let (_, data) = s
        .claim(
            target("a"),
            Offer {
                length: Some(9),
                mtime: Some(8),
            },
            9,
        )
        .await
        .unwrap();
    assert!(data.is_empty());
}

#[tokio::test]
async fn expiry_is_absolute_and_peeking_does_not_refresh_it() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    lease.append(0, b"hello").await.unwrap();
    drop(lease);
    time.store(100 + TTL - 1, Ordering::SeqCst);
    let (lease, data) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert_eq!(data, b"hello");
    lease.append(5, &[]).await.unwrap();
    drop(lease);
    time.store(100 + TTL, Ordering::SeqCst);
    let (_, data) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert!(data.is_empty());
}

#[tokio::test]
async fn tampered_metadata_payload_and_oversized_records_are_not_resumed() {
    for corruption in ["meta", "part", "oversize"] {
        let dir = tempfile::tempdir().unwrap();
        let time = Arc::new(AtomicU64::new(100));
        let s = open(dir.path(), &time);
        let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
        lease.append(0, b"hello").await.unwrap();
        drop(lease);
        let id = target_id(&target("a")).unwrap();
        let path =
            s.0.path(&id, if corruption == "part" { "part" } else { "meta" });
        if corruption == "oversize" {
            fs::write(path, vec![0; MAX_META as usize + 1]).unwrap();
        } else {
            let mut data = fs::read(&path).unwrap();
            data[0] ^= 1;
            fs::write(path, data).unwrap();
        }
        drop(s);
        let s = open(dir.path(), &time);
        let (_, data) = s.claim(target("a"), offer(), 10).await.unwrap();
        assert!(data.is_empty(), "{corruption}");
    }
}

#[tokio::test]
async fn active_claims_reserve_capacity_and_disk_failures_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let mut held = Vec::new();
    for n in 0..4 {
        held.push(
            s.claim(
                target(&format!("{n}")),
                Offer {
                    length: None,
                    mtime: None,
                },
                MAX_FILE,
            )
            .await
            .unwrap()
            .0,
        );
    }
    assert!(
        s.claim(target("fifth"), offer(), 10).await.is_err(),
        "reserve caps, not only the zero bytes currently written"
    );
    held.pop();
    let (lease, _) = s.claim(target("fifth"), offer(), 10).await.unwrap();
    lease.append(0, b"hello").await.unwrap();
    let id = target_id(&target("fifth")).unwrap();
    fs::create_dir(s.0.path(&id, "tmp")).unwrap();
    assert!(lease.append(5, b"world").await.is_err());
    fs::remove_dir(s.0.path(&id, "tmp")).unwrap();
    drop(lease);
    assert!(
        s.claim(target("fifth"), offer(), 10).await.is_err(),
        "ambiguous IO blocks further staging until reload"
    );
    drop(held);
    drop(s);
    let s = open(dir.path(), &time);
    let (_, data) = s.claim(target("fifth"), offer(), 10).await.unwrap();
    assert_eq!(
        data, b"hello",
        "failed metadata replacement retains the old committed prefix"
    );
}

#[tokio::test]
async fn initial_metadata_failure_also_suspends_admission() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let id = target_id(&target("a")).unwrap();
    fs::create_dir(s.0.path(&id, "tmp")).unwrap();
    assert!(s.claim(target("a"), offer(), 10).await.is_err());
    fs::remove_dir(s.0.path(&id, "tmp")).unwrap();
    assert!(s.claim(target("b"), offer(), 10).await.is_err());
    drop(s);
    let s = open(dir.path(), &time);
    assert!(s
        .claim(target("a"), offer(), 10)
        .await
        .unwrap()
        .1
        .is_empty());
}

#[tokio::test]
async fn entry_file_and_contiguous_offset_limits_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    assert!(s
        .claim(
            target("too-big"),
            Offer {
                length: Some(MAX_FILE + 1),
                mtime: None
            },
            MAX_FILE
        )
        .await
        .is_err());
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert!(lease.append(1, b"x").await.is_err());
    assert!(lease.append(0, b"elevenbytes").await.is_err());
    lease.append(0, b"ok").await.unwrap();
    drop(lease);
    for n in 1..MAX_RECORDS {
        drop(
            s.claim(target(&format!("entry-{n}")), offer(), 10)
                .await
                .unwrap(),
        );
    }
    assert!(s.claim(target("overflow"), offer(), 10).await.is_err());
    time.store(100 + TTL, Ordering::SeqCst);
    assert!(
        s.claim(target("overflow"), offer(), 10).await.is_ok(),
        "lazy expiry frees capacity"
    );
}

#[tokio::test]
async fn failed_discard_cannot_resurrect_rejected_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    lease.append(0, b"hello").await.unwrap();
    let id = target_id(&target("a")).unwrap();
    let part = s.0.path(&id, "part");
    fs::remove_file(&part).unwrap();
    fs::create_dir(&part).unwrap();
    assert!(lease.discard().await.is_err());
    assert!(!s.0.path(&id, "meta").exists());
    fs::remove_dir(&part).unwrap();
    drop(lease);
    assert!(s.claim(target("a"), offer(), 10).await.is_err());
    drop(s);
    let s = open(dir.path(), &time);
    assert!(s
        .claim(target("a"), offer(), 10)
        .await
        .unwrap()
        .1
        .is_empty());
}

#[tokio::test]
async fn queued_claim_observes_storage_failure_before_mutating() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    lease.append(0, b"hello").await.unwrap();
    let id = target_id(&target("a")).unwrap();
    fs::create_dir(s.0.path(&id, "tmp")).unwrap();

    // Pause after the append has failed, while it still owns the index lock.
    // Start another claim at that lock boundary before publishing the failure.
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    *s.0.failure_pause.lock() = Some((entered, gate));
    let append = tokio::spawn(async move { lease.append(5, b"world").await });
    waiting.await.unwrap();
    let (started, queued) = tokio::sync::oneshot::channel();
    *s.0.claim_started.lock() = Some(started);
    let other = s.clone();
    let claim = tokio::spawn(async move { other.claim(target("b"), offer(), 10).await });
    queued.await.unwrap();
    release.send(()).unwrap();
    assert!(append.await.unwrap().is_err());
    assert!(claim.await.unwrap().is_err());
    let other_id = target_id(&target("b")).unwrap();
    assert!(!s.0.path(&other_id, "meta").exists());
    assert!(!s.0.path(&other_id, "part").exists());
    assert!(s.0.index.lock().claimed.is_empty());
}

#[tokio::test]
async fn cancellation_retains_claim_until_blocking_checkpoint_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(dir.path(), &time);
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    *s.0.append_pause.lock() = Some((entered, gate));
    let task = tokio::spawn(async move { lease.append(0, b"hello").await });
    waiting.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(s.claim(target("a"), offer(), 10).await.is_err());
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !s.0.index.lock().claimed.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (_, data) = s.claim(target("a"), offer(), 10).await.unwrap();
    assert_eq!(data, b"hello");
}

#[cfg(unix)]
#[tokio::test]
async fn staged_symlinks_are_unlinked_without_touching_external_files() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let time = Arc::new(AtomicU64::new(100));
    let s = open(&dir.path().join("stage"), &time);
    let (lease, _) = s.claim(target("a"), offer(), 10).await.unwrap();
    lease.append(0, b"hello").await.unwrap();
    drop(lease);
    let id = target_id(&target("a")).unwrap();
    let part = s.0.path(&id, "part");
    let external = dir.path().join("external");
    fs::write(&external, b"private").unwrap();
    fs::remove_file(&part).unwrap();
    symlink(&external, &part).unwrap();
    drop(s);
    let s = open(&dir.path().join("stage"), &time);
    assert!(s
        .claim(target("a"), offer(), 10)
        .await
        .unwrap()
        .1
        .is_empty());
    assert_eq!(fs::read(external).unwrap(), b"private");
    let link = dir.path().join("linked-root");
    symlink(dir.path().join("stage"), &link).unwrap();
    assert!(Staging::open_with_clock(link, [3; 32], Arc::new(|| 100)).is_err());
}
