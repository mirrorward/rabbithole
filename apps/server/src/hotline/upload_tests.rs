use super::*;
use crate::Burrow;
use rabbithole_server_core::ServerConfig;
use std::future::{poll_fn, Future};
use std::task::Poll;

async fn fixture() -> (tempfile::TempDir, Burrow, i64) {
    let dir = tempfile::tempdir().unwrap();
    let b = Burrow::start(ServerConfig {
        data_dir: dir.path().to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    let account = AccountsRepo(&b.shared.pool)
        .create("alice", None, "Alice", Role::User as u8, None)
        .await
        .unwrap();
    b.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    b.shared
        .files
        .mkdir("warez", None, "drop", false)
        .await
        .unwrap();
    (dir, b, account.id)
}

async fn ticket(shared: &Arc<Shared>, account: i64, name: &str, bytes: &[u8]) -> UploadTicket {
    let area = shared.files.area("warez").await.unwrap();
    let parent = shared
        .files
        .node_by_path("warez", "drop")
        .await
        .unwrap()
        .unwrap();
    let target = Target {
        protocol: Protocol::Hotline,
        account,
        area: area.id,
        parent: Some(parent.id),
        name: name.into(),
    };
    let (lease, data) = shared
        .upload_staging
        .claim_hotline(target.clone(), false, 4096)
        .await
        .unwrap();
    assert!(data.is_empty());
    assert!(lease.bind_hotline_length(bytes.len() as u64).await.unwrap());
    lease.append(0, bytes).await.unwrap();
    UploadTicket {
        account_id: account,
        uploader: "alice@home".into(),
        area: "warez".into(),
        folder: Some("drop".into()),
        name: name.into(),
        declared_total: None,
        lease,
        target,
        data: bytes.to_vec(),
        expires: Instant::now() + UPLOAD_REF_TTL,
    }
}

#[tokio::test]
async fn expired_reference_releases_claim_before_same_target_resume() {
    let (_dir, b, account) = fixture().await;
    let bytes = b"saved prefix";
    let t = ticket(&b.shared, account, "lease.bin", bytes).await;
    let target = t.target.clone();
    let reference = b.shared.hotline.stage_upload(t);
    assert!(b
        .shared
        .upload_staging
        .claim_hotline(target.clone(), true, 4096)
        .await
        .is_err());
    b.shared
        .hotline
        .uploads
        .lock()
        .get_mut(&reference)
        .unwrap()
        .expires = Instant::now() - Duration::from_secs(1);
    b.shared.hotline.prune_uploads();
    assert!(b.shared.hotline.take_upload(reference).is_none());
    let (lease, prefix) = b
        .shared
        .upload_staging
        .claim_hotline(target, true, 4096)
        .await
        .unwrap();
    assert_eq!(prefix, bytes);
    lease.discard().await.unwrap();
    drop(lease);
    b.shutdown().await;
}

#[tokio::test]
async fn auxiliary_forks_share_one_budget_without_allocating_their_claimed_sizes() {
    let (_dir, b, account) = fixture().await;
    let mut t = ticket(&b.shared, account, "auxiliary.bin", b"prefix").await;
    let first_size = (MAX_HTXF_UPLOAD / 2 + 1) as u32;
    let mut header = FlatHeader { fork_count: 2 }.encode().to_vec();
    header.extend_from_slice(
        &ForkHeader {
            fork_type: *b"MACR",
            data_size: first_size,
        }
        .encode(),
    );
    let second_header = ForkHeader {
        fork_type: *b"MACR",
        data_size: (MAX_HTXF_UPLOAD / 2) as u32,
    }
    .encode();
    let mut input = std::io::Cursor::new(header)
        .chain(tokio::io::repeat(0).take(u64::from(first_size)))
        .chain(std::io::Cursor::new(second_header));
    assert!(
        receive_htxf_inner(&mut input, &b.shared, &mut t).await,
        "aggregate overflow must be rejected before attempting the absent second payload"
    );
    assert!(b
        .shared
        .files
        .node_by_path("warez", "drop/auxiliary.bin")
        .await
        .unwrap()
        .is_none());
    t.lease.discard().await.unwrap();
    drop(t);
    b.shutdown().await;
}

#[tokio::test]
async fn folder_replacement_during_commit_wait_cannot_redirect_hotline_upload() {
    let (_dir, b, account) = fixture().await;
    let bytes = b"fully checkpointed content";
    let t = ticket(&b.shared, account, "race.bin", bytes).await;
    let old_parent = t.target.parent.unwrap();
    let replacement = {
        let guard = crate::upload_gate::commit_lock(&b.shared).await;
        let publication = finalize_htxf_upload(&b.shared, &t, bytes.to_vec(), None);
        tokio::pin!(publication);
        poll_fn(|cx| {
            assert!(publication.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        b.shared.files.rename(old_parent, "moved").await.unwrap();
        let replacement = b
            .shared
            .files
            .mkdir("warez", None, "drop", false)
            .await
            .unwrap();
        assert_ne!(replacement.id, old_parent);
        drop(guard);
        assert!(
            publication.await,
            "changed destination is a terminal refusal"
        );
        replacement
    };
    for path in ["drop/race.bin", "moved/race.bin"] {
        assert!(b
            .shared
            .files
            .node_by_path("warez", path)
            .await
            .unwrap()
            .is_none());
    }
    t.lease.discard().await.unwrap();
    let fresh = ticket(&b.shared, account, "race.bin", bytes).await;
    assert!(finalize_htxf_upload(&b.shared, &fresh, bytes.to_vec(), None).await);
    let node = b
        .shared
        .files
        .node_by_path("warez", "drop/race.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.parent_id, Some(replacement.id));
    assert_eq!(node.blob_id, Some(*blake3::hash(bytes).as_bytes()));
    fresh.lease.discard().await.unwrap();
    drop(fresh);
    drop(t);
    b.shutdown().await;
}

#[tokio::test]
async fn upload_permission_revoked_during_commit_wait_is_rechecked() {
    let (_dir, b, account) = fixture().await;
    let bytes = b"permission may change after negotiation";
    let t = ticket(&b.shared, account, "revoked.bin", bytes).await;
    assert!(
        current_upload_allowed(&b.shared, account, "warez", Some("drop"))
            .await
            .unwrap()
    );
    {
        let guard = crate::upload_gate::commit_lock(&b.shared).await;
        let publication = finalize_htxf_upload(&b.shared, &t, bytes.to_vec(), None);
        tokio::pin!(publication);
        poll_fn(|cx| {
            assert!(publication.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        AccountsRepo(&b.shared.pool)
            .admin_set("alice", Some(Role::Guest as u8), None, None)
            .await
            .unwrap();
        drop(guard);
        assert!(publication.await, "revocation is a terminal refusal");
    }
    assert!(b
        .shared
        .files
        .node_by_path("warez", "drop/revoked.bin")
        .await
        .unwrap()
        .is_none());
    assert!(
        !current_upload_allowed(&b.shared, account, "warez", Some("drop"))
            .await
            .unwrap()
    );
    t.lease.discard().await.unwrap();
    drop(t);
    b.shutdown().await;
}
