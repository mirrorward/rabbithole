//! Wave 13 end-to-end tests: the moderation suite — report queues,
//! quarantine-for-review, and the blake3 hash-deny list.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    report_action, report_state, subject_kind, DenyHashAdd, DenyHashList, DenyHashListRequest,
    DenyHashRemove, QuarantineClear, QuarantineSet, ReportAck, ReportCreate, ReportList,
    ReportListRequest, ReportResolve,
};
use rabbithole_proto::board::PostCreate;
use rabbithole_proto::filelib::FileUpload;
use rabbithole_proto::session::ServerNotice;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Moderated Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn start(dir: &std::path::Path) -> Burrow {
    let burrow = Burrow::start(test_config(dir)).await.unwrap();
    for (login, role) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("mo", Role::Moderator),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    burrow
}

/// A user files a report; the moderator sees it in the queue, claims it,
/// and resolves it. Duplicates dedupe, non-moderators are refused on every
/// moderator op, and the moderator gets a push notice for the new report.
#[tokio::test]
async fn report_queue_flow() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut mo = login(&burrow, "mo").await;
    let mut alice = login(&burrow, "alice").await;

    // Alice reports a post; an identical re-report dedupes to the same id.
    let subject = [7u8; 32].to_vec();
    let ack: ReportAck = alice
        .request(&ReportCreate::new(
            subject_kind::POST,
            subject.clone(),
            "spam",
        ))
        .await
        .unwrap();
    assert!(!ack.deduped);
    let again: ReportAck = alice
        .request(&ReportCreate::new(
            subject_kind::POST,
            subject.clone(),
            "still spam",
        ))
        .await
        .unwrap();
    assert!(again.deduped);
    assert_eq!(again.id, ack.id);

    // Every moderator op is refused for alice.
    assert!(matches!(
        alice
            .request::<_, ReportList>(&ReportListRequest::new(None, 0, 10))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice
            .request_ack(&ReportResolve::new(ack.id, report_action::RESOLVE, "no"))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice
            .request_ack(&QuarantineSet::new(subject_kind::POST, subject.clone(), ""))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice.request_ack(&DenyHashAdd::new([9u8; 32], "")).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice.request_ack(&DenyHashRemove::new([9u8; 32])).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice.request::<_, DenyHashList>(&DenyHashListRequest).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // The moderator was pushed a notice about the new report.
    let notice = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let frame = mo.next_push().await.unwrap().expect("push stream open");
            if let Some(Ok(n)) = frame.decode::<ServerNotice>() {
                if n.from == "moderation" {
                    return n;
                }
            }
        }
    })
    .await
    .expect("moderation notice arrives");
    assert!(notice.text.contains(&format!("#{}", ack.id)), "{notice:?}");

    // The queue shows one open report; mo claims then resolves it.
    let queue: ReportList = mo
        .request(&ReportListRequest::new(Some(report_state::OPEN), 0, 10))
        .await
        .unwrap();
    assert_eq!(queue.total, 1);
    assert_eq!(queue.reports.len(), 1);
    let r = &queue.reports[0];
    assert_eq!(r.id, ack.id);
    assert_eq!(
        (r.subject_kind, r.subject_ref.as_slice()),
        (subject_kind::POST, &subject[..])
    );
    assert_eq!(r.reason, "spam");
    assert_eq!(r.resolver, "");

    mo.request_ack(&ReportResolve::new(ack.id, report_action::CLAIM, ""))
        .await
        .unwrap();
    // A second claim clashes with the state machine.
    assert!(matches!(
        mo.request_ack(&ReportResolve::new(ack.id, report_action::CLAIM, ""))
            .await,
        Err(ClientError::Refused(ErrorCode::AlreadyExists))
    ));
    mo.request_ack(&ReportResolve::new(
        ack.id,
        report_action::RESOLVE,
        "handled",
    ))
    .await
    .unwrap();

    let resolved: ReportList = mo
        .request(&ReportListRequest::new(Some(report_state::RESOLVED), 0, 10))
        .await
        .unwrap();
    assert_eq!(resolved.total, 1);
    assert_eq!(resolved.reports[0].resolver, "mo");
    assert_eq!(resolved.reports[0].resolution, "handled");
    assert!(resolved.reports[0].resolved_at_unix.is_some());
    // The open queue is empty again, and a fresh report is possible.
    let open: ReportList = mo
        .request(&ReportListRequest::new(Some(report_state::OPEN), 0, 10))
        .await
        .unwrap();
    assert_eq!(open.total, 0);
    let fresh: ReportAck = alice
        .request(&ReportCreate::new(subject_kind::POST, subject, "again"))
        .await
        .unwrap();
    assert!(!fresh.deduped);

    burrow.shutdown().await;
}

/// A quarantined post vanishes for regular users (thread list AND thread
/// fetch) but stays visible to moderators; clearing the quarantine brings
/// it back.
#[tokio::test]
async fn quarantined_post_hidden_from_users_visible_to_moderators() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    burrow
        .shared
        .boards
        .create_board("general", "General", "", 2, None, 0)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let mut mo = login(&burrow, "mo").await;

    let post = alice
        .post(&PostCreate::new("general", "hot take", "…"))
        .await
        .unwrap();
    assert_eq!(bob.threads("general", 50).await.unwrap().len(), 1);

    // Mo quarantines the post.
    mo.request_ack(&QuarantineSet::new(
        subject_kind::POST,
        post.id.to_vec(),
        "pending review",
    ))
    .await
    .unwrap();

    // Bob no longer sees the thread anywhere; mo still does.
    assert_eq!(bob.threads("general", 50).await.unwrap().len(), 0);
    assert!(matches!(
        bob.thread(post.id, 50).await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));
    let mo_threads = mo.threads("general", 50).await.unwrap();
    assert_eq!(mo_threads.len(), 1);
    assert_eq!(mo_threads[0].root.id, post.id);
    assert_eq!(mo.thread(post.id, 50).await.unwrap().len(), 1);

    // Clearing restores it for everyone; a second clear is NotFound.
    mo.request_ack(&QuarantineClear::new(subject_kind::POST, post.id.to_vec()))
        .await
        .unwrap();
    assert_eq!(bob.threads("general", 50).await.unwrap().len(), 1);
    assert!(matches!(
        mo.request_ack(&QuarantineClear::new(subject_kind::POST, post.id.to_vec()))
            .await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    burrow.shutdown().await;
}

/// A quarantined file disappears from listings/search/downloads for users
/// but not for moderators.
#[tokio::test]
async fn quarantined_file_hidden_on_list_and_download() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    burrow
        .shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let mut mo = login(&burrow, "mo").await;

    let bytes = b"suspicious payload".to_vec();
    let blob = *blake3::hash(&bytes).as_bytes();
    let node = alice
        .file_upload(&FileUpload::new("pub", None, "sus.bin", bytes))
        .await
        .unwrap();
    assert_eq!(node.blob_id, Some(blob));
    assert_eq!(bob.folder_list("pub", None).await.unwrap().len(), 1);

    mo.request_ack(&QuarantineSet::new(
        subject_kind::FILE,
        blob.to_vec(),
        "reported",
    ))
    .await
    .unwrap();

    // Hidden from bob: listing, metadata, search, and the bytes themselves.
    assert_eq!(bob.folder_list("pub", None).await.unwrap().len(), 0);
    assert!(matches!(
        bob.node_get(node.id).await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));
    assert_eq!(bob.file_search(None, "sus", 10).await.unwrap().len(), 0);
    assert!(matches!(
        bob.file_download(node.id).await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));
    // Fully visible to mo.
    assert_eq!(mo.folder_list("pub", None).await.unwrap().len(), 1);
    assert_eq!(mo.file_download(node.id).await.unwrap().node.id, node.id);

    burrow.shutdown().await;
}

/// Deny-hashed content is refused at upload (inline finalize) and DM
/// attachment; removing the hash lifts the ban. The deny list is
/// moderator-readable.
#[tokio::test]
async fn deny_hashed_upload_refused() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    burrow
        .shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let mut mo = login(&burrow, "mo").await;

    let bad = b"known-bad bytes".to_vec();
    let hash = *blake3::hash(&bad).as_bytes();
    mo.request_ack(&DenyHashAdd::new(hash, "test ban"))
        .await
        .unwrap();
    let listed: DenyHashList = mo.request(&DenyHashListRequest).await.unwrap();
    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].hash, hash);
    assert_eq!(listed.entries[0].added_by, "mo");

    // The denied bytes are refused; different bytes sail through.
    assert!(matches!(
        alice
            .file_upload(&FileUpload::new("pub", None, "bad.bin", bad.clone()))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    alice
        .file_upload(&FileUpload::new("pub", None, "good.bin", b"fine".to_vec()))
        .await
        .unwrap();

    // The denied blob can't ride a DM attachment either (it is already in
    // the blob store from an earlier era — simulate by putting it directly).
    let blob_id = burrow.shared.blobs.put(&bad).unwrap();
    assert_eq!(blob_id.0, hash);
    let mut dm = rabbithole_proto::dm::DmSend::new("mo", "psst");
    dm.attachments = vec![hash];
    assert!(matches!(
        alice.dm_send(&dm).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Removing the hash lifts the refusal.
    mo.request_ack(&DenyHashRemove::new(hash)).await.unwrap();
    alice
        .file_upload(&FileUpload::new("pub", None, "bad.bin", bad))
        .await
        .unwrap();

    burrow.shutdown().await;
}
