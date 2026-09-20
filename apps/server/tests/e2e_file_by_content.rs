//! Which file here holds a given content, for a download running on another
//! burrow: answered only for a file this person may actually download here,
//! so a lookup by content hash tells nobody anything they could not already
//! have asked for by name.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{DenyHashAdd, DenyHashRemove, QuarantineClear, QuarantineSet};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

async fn connect(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap()
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = connect(burrow).await;
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

fn not_found<T: std::fmt::Debug>(r: Result<T, ClientError>) {
    refused(r, ErrorCode::NotFound)
}

#[tokio::test]
async fn a_file_is_found_by_its_content_only_by_someone_who_may_download_it() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Content Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().to_path_buf(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for (who, role) in [("alice", Role::User), ("mo", Role::Moderator)] {
        burrow
            .shared
            .auth
            .create_account(who, "pw-pw-pw", role)
            .await
            .unwrap();
    }

    let files = &burrow.shared.files;
    files.create_area("music", "Music", "").await.unwrap();
    files.create_area("inbox", "Inbox", "").await.unwrap();
    let secret = files.mkdir("inbox", None, "drop", true).await.unwrap();
    let body: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let blob = burrow.shared.blobs.put(&body).unwrap();
    let add = async |area: &str, parent: Option<&str>, name: &str| {
        files
            .add_file(
                area,
                parent,
                name,
                &blob.0,
                body.len() as i64,
                "application/octet-stream",
                "",
                "",
                "x@y",
                1,
            )
            .await
            .unwrap()
    };

    // Filed only in a drop box: a person who may not see into one is told
    // the same as if the burrow did not have it at all.
    let dropped = add("inbox", Some(&secret.path), "tape.bin").await;
    let mut alice = login(&burrow, "alice").await;
    not_found(alice.file_by_content(blob.0).await);

    // Filed where they may download it: that is the one they are told about.
    let open = add("music", None, "tape.bin").await;
    let found = alice.file_by_content(blob.0).await.unwrap();
    assert_eq!((found.node_id, found.size), (open.id, body.len() as u64));
    assert_ne!(found.node_id, dropped.id);
    // Asking is not downloading: the file's count moves when a ticket
    // opens, not before.
    let counted = |id| async move { files.node(id).await.unwrap().unwrap().downloads };
    assert_eq!(counted(open.id).await, 0);
    // And the ticket it is for opens.
    let ticket = alice.download_ticket(found.node_id).await.unwrap();
    assert_eq!(counted(open.id).await, 1);
    assert_eq!(ticket.size, body.len() as u64);
    alice.close_transfer(ticket.transfer_id).await.unwrap();

    // Content this burrow does not hold, and content held but quarantined,
    // read the same: nothing to see.
    not_found(alice.file_by_content([9; 32]).await);
    let mut mo = login(&burrow, "mo").await;
    mo.request_ack(&QuarantineSet::new(
        rabbithole_proto::admin::subject_kind::FILE,
        blob.0.to_vec(),
        "reported",
    ))
    .await
    .unwrap();
    not_found(alice.file_by_content(blob.0).await);
    mo.request_ack(&QuarantineClear::new(
        rabbithole_proto::admin::subject_kind::FILE,
        blob.0.to_vec(),
    ))
    .await
    .unwrap();
    assert_eq!(
        alice.file_by_content(blob.0).await.unwrap().node_id,
        open.id
    );

    // Denied content is not here either, whoever asks.
    mo.request_ack(&DenyHashAdd::new(blob.0, "no"))
        .await
        .unwrap();
    not_found(alice.file_by_content(blob.0).await);
    mo.request_ack(&DenyHashRemove::new(blob.0)).await.unwrap();
    assert!(alice.file_by_content(blob.0).await.is_ok());

    // A guest is turned away before anything is looked up: its budget can
    // be had again by connecting again.
    let mut guest = connect(&burrow).await;
    guest.auth_guest(Some("wanderer".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    refused(guest.file_by_content(blob.0).await, ErrorCode::Forbidden);

    burrow.shutdown().await;
}

/// Copies nobody else may read cannot hide the one copy they may: the
/// lookup walks past them rather than reading only the newest few.
#[tokio::test]
async fn a_readable_copy_is_found_under_a_pile_of_unreadable_ones() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Pile Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().to_path_buf(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let files = &burrow.shared.files;
    files.create_area("music", "Music", "").await.unwrap();
    files.create_area("inbox", "Inbox", "").await.unwrap();
    let drop = files.mkdir("inbox", None, "drop", true).await.unwrap();
    let body = b"one content, filed many times".to_vec();
    let blob = burrow.shared.blobs.put(&body).unwrap();
    let add = async |area: &str, parent: Option<&str>, name: &str| {
        files
            .add_file(
                area,
                parent,
                name,
                &blob.0,
                body.len() as i64,
                "application/octet-stream",
                "",
                "",
                "x@y",
                1,
            )
            .await
            .unwrap()
    };
    // The one they may have is filed first, then buried under 40 they may
    // not: more than one page of the walk, and more than the newest few.
    let open = add("music", None, "tape.bin").await;
    for i in 0..40 {
        add("inbox", Some(&drop.path), &format!("copy-{i}.bin")).await;
    }

    let mut alice = login(&burrow, "alice").await;
    assert_eq!(
        alice.file_by_content(blob.0).await.unwrap().node_id,
        open.id
    );

    burrow.shutdown().await;
}
