//! Wave 4.2 end-to-end tests: bulk transfers — multi-chunk upload/download,
//! whole-file integrity, resume, and upload permissions. Runs over WebSocket,
//! which has no bulk streams, so it exercises the ranged control-frame path.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::FolderCreate;
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

/// A deterministic multi-chunk payload (larger than the 256 KiB chunk).
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
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
