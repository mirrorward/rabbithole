//! Wave 4.2 end-to-end tests: bulk transfers — multi-chunk upload/download,
//! whole-file integrity, resume, folder pipelining, and upload permissions.
//! Most run over WebSocket (the ranged control-frame path); one runs over
//! QUIC to cover the dedicated bulk-stream data plane.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::FolderCreate;
use rabbithole_proto::transfer::{
    FileChunkPut, TransferAbort, TransferOpen, TransferTicket, UploadFinish,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Transfer Warren".into(),
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

/// Log in over QUIC (which offers dedicated bulk streams, unlike WS).
async fn login_quic(burrow: &Burrow, user: &str) -> Client {
    let fp = burrow.fingerprint.to_hex();
    let mut c = Client::connect(
        &format!("127.0.0.1:{}", burrow.quic_addr.port()),
        Some("localhost"),
        Some(&fp),
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

/// A deterministic multi-chunk payload (larger than the 256 KiB chunk).
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn root(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

#[tokio::test]
async fn bulk_transfer_over_quic_dedicated_streams() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();

    // Over QUIC the client uses dedicated bulk streams (bytes off the control
    // channel), so this covers the path WS can't exercise.
    let mut admin = login_quic(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();

    let body = payload(600 * 1024);
    let src = work.path().join("big.bin");
    std::fs::write(&src, &body).unwrap();
    let node = admin
        .transfer_upload(
            "warez",
            None,
            "big.bin",
            &src,
            "application/octet-stream",
            "over quic",
        )
        .await
        .unwrap();
    assert_eq!(node.size, body.len() as i64);

    // Fresh download over a bulk stream.
    let dst = work.path().join("got.bin");
    let n = admin.transfer_download(node.id, &dst).await.unwrap();
    assert_eq!(n, body.len() as u64);
    assert_eq!(std::fs::read(&dst).unwrap(), body, "bulk download matches");

    // Resume a partial download over a bulk stream.
    let resume_dst = work.path().join("resume.bin");
    std::fs::write(&resume_dst, &body[..150 * 1024]).unwrap();
    let n = admin.transfer_download(node.id, &resume_dst).await.unwrap();
    assert_eq!(n, body.len() as u64);
    assert_eq!(
        std::fs::read(&resume_dst).unwrap(),
        body,
        "bulk resume matches"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn upload_download_roundtrip_and_resume() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for (n, r) in [("admin", Role::Admin), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", r)
            .await
            .unwrap();
    }

    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    admin
        .folder_create(&FolderCreate::new("warez", None, "iso"))
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;

    // Upload a ~600 KiB file (spans 3 chunks) and confirm the node.
    let body = payload(600 * 1024);
    let src = work.path().join("big.bin");
    std::fs::write(&src, &body).unwrap();
    let node = alice
        .transfer_upload(
            "warez",
            Some("iso".into()),
            "big.bin",
            &src,
            "application/octet-stream",
            "a big one",
        )
        .await
        .unwrap();
    assert_eq!(node.size, body.len() as i64);
    assert_eq!(node.path, "iso/big.bin");

    // Download it fresh; bytes must match exactly.
    let dst = work.path().join("got.bin");
    let n = alice.transfer_download(node.id, &dst).await.unwrap();
    assert_eq!(n, body.len() as u64);
    assert_eq!(std::fs::read(&dst).unwrap(), body, "downloaded bytes match");

    // Resume: pre-seed a destination with the correct first 100 KiB, then
    // download — the client resumes from the partial and still verifies.
    let resume_dst = work.path().join("resume.bin");
    std::fs::write(&resume_dst, &body[..100 * 1024]).unwrap();
    let n = alice.transfer_download(node.id, &resume_dst).await.unwrap();
    assert_eq!(n, body.len() as u64);
    assert_eq!(
        std::fs::read(&resume_dst).unwrap(),
        body,
        "resumed download matches"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn folder_download_pipelines_a_subtree() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;

    // Build a tree: docs/ , docs/sub/ with a file in each.
    admin.area_create("docs", "Docs", "").await.unwrap();
    admin
        .folder_create(&FolderCreate::new("docs", None, "sub"))
        .await
        .unwrap();

    let a = work.path().join("a.bin");
    std::fs::write(&a, payload(300 * 1024)).unwrap();
    admin
        .transfer_upload("docs", None, "a.bin", &a, "application/octet-stream", "")
        .await
        .unwrap();
    let b = work.path().join("b.bin");
    std::fs::write(&b, payload(400 * 1024)).unwrap();
    admin
        .transfer_upload(
            "docs",
            Some("sub".into()),
            "b.bin",
            &b,
            "application/octet-stream",
            "",
        )
        .await
        .unwrap();

    // Download the whole area into a local dir, preserving structure.
    let dest = work.path().join("pulled");
    let count = admin.folder_download("docs", None, &dest).await.unwrap();
    assert_eq!(count, 2, "both files fetched in one manifest round trip");
    assert_eq!(
        std::fs::read(dest.join("a.bin")).unwrap(),
        payload(300 * 1024)
    );
    assert_eq!(
        std::fs::read(dest.join("sub").join("b.bin")).unwrap(),
        payload(400 * 1024)
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn upload_quota_is_enforced() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        upload_quota_bytes: 400 * 1024, // room for one 300 KiB file, not two
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    // Alice needs an area to upload into; make her area via an admin.
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    login(&burrow, "admin")
        .await
        .area_create("pub", "Public", "")
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let a = work.path().join("a.bin");
    std::fs::write(&a, payload(300 * 1024)).unwrap();
    // First upload fits under the quota.
    alice
        .transfer_upload("pub", None, "a.bin", &a, "application/octet-stream", "")
        .await
        .unwrap();
    // Second upload would exceed it — refused at ticket issue.
    let b = work.path().join("b.bin");
    std::fs::write(&b, payload(300 * 1024)).unwrap();
    assert!(matches!(
        alice
            .transfer_upload("pub", None, "b.bin", &b, "application/octet-stream", "")
            .await,
        Err(ClientError::Refused(ErrorCode::TooLarge))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn upload_requires_permission_and_verifies_hash() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("pub", "Public", "").await.unwrap();

    // Guests can't upload (no FILE_UPLOAD): TransferOpen is refused.
    let mut guest = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("Dormouse".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    let src = work.path().join("x.bin");
    std::fs::write(&src, payload(300 * 1024)).unwrap();
    assert!(matches!(
        guest
            .transfer_upload("pub", None, "x.bin", &src, "application/octet-stream", "")
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Admin upload + download roundtrips a small file too.
    let small = work.path().join("small.txt");
    std::fs::write(&small, b"just a little file").unwrap();
    let node = admin
        .transfer_upload("pub", None, "small.txt", &small, "text/plain", "")
        .await
        .unwrap();
    let out = work.path().join("small.out");
    admin.transfer_download(node.id, &out).await.unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"just a little file");

    burrow.shutdown().await;
}

#[tokio::test]
async fn chunk_upload_rejects_overrun_sparse_offset_and_short_finish() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("pub", "Public", "").await.unwrap();

    let overrun: TransferTicket = admin
        .request(&TransferOpen::upload(
            "pub",
            None,
            "over.bin",
            1,
            root(b"x"),
        ))
        .await
        .unwrap();
    assert!(matches!(
        admin
            .request_ack(&FileChunkPut::new(
                overrun.transfer_id,
                0,
                true,
                b"xx".to_vec(),
            ))
            .await,
        Err(ClientError::Refused(ErrorCode::TooLarge))
    ));

    let sparse: TransferTicket = admin
        .request(&TransferOpen::upload(
            "pub",
            None,
            "sparse.bin",
            4,
            root(b"data"),
        ))
        .await
        .unwrap();
    assert!(matches!(
        admin
            .request_ack(&FileChunkPut::new(
                sparse.transfer_id,
                u64::MAX,
                true,
                vec![1],
            ))
            .await,
        Err(ClientError::Refused(ErrorCode::TooLarge))
    ));

    let short: TransferTicket = admin
        .request(&TransferOpen::upload(
            "pub",
            None,
            "short.bin",
            4,
            root(b"abc"),
        ))
        .await
        .unwrap();
    admin
        .request_ack(&FileChunkPut::new(
            short.transfer_id,
            0,
            true,
            b"abc".to_vec(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        admin
            .request::<_, rabbithole_proto::filelib::NodeReply>(&UploadFinish::new(
                short.transfer_id,
            ))
            .await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn upload_quota_is_rechecked_when_concurrent_tickets_finish() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        upload_quota_bytes: 4,
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut first_client = login(&burrow, "admin").await;
    first_client.area_create("pub", "Public", "").await.unwrap();
    let mut second_client = login(&burrow, "admin").await;

    // Both tickets fit when opened, but only one can fit when committed.
    let first: TransferTicket = first_client
        .request(&TransferOpen::upload(
            "pub",
            None,
            "one.bin",
            3,
            root(b"one"),
        ))
        .await
        .unwrap();
    let second: TransferTicket = second_client
        .request(&TransferOpen::upload(
            "pub",
            None,
            "two.bin",
            3,
            root(b"two"),
        ))
        .await
        .unwrap();
    first_client
        .request_ack(&FileChunkPut::new(
            first.transfer_id,
            0,
            true,
            b"one".to_vec(),
        ))
        .await
        .unwrap();
    second_client
        .request_ack(&FileChunkPut::new(
            second.transfer_id,
            0,
            true,
            b"two".to_vec(),
        ))
        .await
        .unwrap();

    let first_finish = UploadFinish::new(first.transfer_id);
    let second_finish = UploadFinish::new(second.transfer_id);
    let (first_result, second_result) = tokio::join!(
        first_client.request::<_, rabbithole_proto::filelib::NodeReply>(&first_finish),
        second_client.request::<_, rabbithole_proto::filelib::NodeReply>(&second_finish),
    );
    let outcomes = [first_result, second_result];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(ClientError::Refused(ErrorCode::TooLarge))))
            .count(),
        1
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn transfer_abort_is_bound_to_the_opening_session() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for (name, role) in [
        ("admin", Role::Admin),
        ("alice", Role::User),
        ("bob", Role::User),
    ] {
        burrow
            .shared
            .auth
            .create_account(name, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    login(&burrow, "admin")
        .await
        .area_create("pub", "Public", "")
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let mut alice_other_session = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let ticket: TransferTicket = alice
        .request(&TransferOpen::upload(
            "pub",
            None,
            "owned.bin",
            3,
            root(b"own"),
        ))
        .await
        .unwrap();

    for attacker in [&mut bob, &mut alice_other_session] {
        assert!(matches!(
            attacker
                .request_ack(&TransferAbort::new(ticket.transfer_id))
                .await,
            Err(ClientError::Refused(ErrorCode::Forbidden))
        ));
    }

    // The unauthorized aborts did not disturb the owner's transfer.
    alice
        .request_ack(&FileChunkPut::new(
            ticket.transfer_id,
            0,
            true,
            b"own".to_vec(),
        ))
        .await
        .unwrap();
    alice
        .request::<_, rabbithole_proto::filelib::NodeReply>(&UploadFinish::new(ticket.transfer_id))
        .await
        .unwrap();

    burrow.shutdown().await;
}
