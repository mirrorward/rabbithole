//! Wave 4.3 end-to-end tests: the transfer *rate policy* (per-account
//! concurrency cap, per-transfer bandwidth cap, and session-scoped ticket
//! cleanup) and the client-side persistent transfer-queue driver.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::transfer::{TransferOpen, TransferTicket};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_client::transfers::{self as tq, NewTransfer, TransferQueue};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Rate Warren".into(),
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

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Upload a file as admin and return its node id, for download tests.
async fn seed_file(admin: &mut Client, work: &std::path::Path, body: &[u8]) -> i64 {
    let src = work.join("seed.bin");
    std::fs::write(&src, body).unwrap();
    admin
        .transfer_upload(
            "warez",
            None,
            "seed.bin",
            &src,
            "application/octet-stream",
            "",
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn concurrency_cap_refuses_extra_opens() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        max_concurrent_transfers: 1,
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = seed_file(&mut admin, work.path(), &payload(300 * 1024)).await;

    // Hold one download ticket open (never request its chunks): it occupies
    // the single per-account slot.
    let _held: TransferTicket = admin.request(&TransferOpen::download(node)).await.unwrap();
    // A second open is refused — the account is at its concurrency cap.
    let refused = admin
        .request::<TransferOpen, TransferTicket>(&TransferOpen::download(node))
        .await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::RateLimited))),
        "second concurrent open should be rate-limited, got {refused:?}"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn completed_download_frees_the_concurrency_slot() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        max_concurrent_transfers: 1,
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = seed_file(&mut admin, work.path(), &payload(300 * 1024)).await;

    // Two full downloads back-to-back under a cap of 1: the second only
    // succeeds because the first retired its ticket when it finished.
    let dst1 = work.path().join("a.bin");
    assert_eq!(
        admin.transfer_download(node, &dst1).await.unwrap(),
        300 * 1024
    );
    let dst2 = work.path().join("b.bin");
    assert_eq!(
        admin.transfer_download(node, &dst2).await.unwrap(),
        300 * 1024,
        "slot must free when the first download finishes"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn session_close_frees_a_held_transfer_slot() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        max_concurrent_transfers: 1,
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = seed_file(&mut admin, work.path(), &payload(200 * 1024)).await;

    // Session A holds the only slot with an un-drained download ticket.
    let mut a = login(&burrow, "admin").await;
    let _held: TransferTicket = a.request(&TransferOpen::download(node)).await.unwrap();

    // Session B is refused while A holds the slot.
    let mut b = login(&burrow, "admin").await;
    let dst = work.path().join("late.bin");
    assert!(matches!(
        b.transfer_download(node, &dst).await,
        Err(ClientError::Refused(ErrorCode::RateLimited))
    ));

    // Close A: teardown retires its ticket. B then succeeds (poll to absorb
    // the async disconnect handling).
    a.close().await;
    let mut ok = false;
    for _ in 0..100 {
        match b.transfer_download(node, &dst).await {
            Ok(n) => {
                assert_eq!(n, 200 * 1024);
                ok = true;
                break;
            }
            Err(ClientError::Refused(ErrorCode::RateLimited)) => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert!(ok, "B should get the slot once A's session is torn down");

    burrow.shutdown().await;
}

#[tokio::test]
async fn bandwidth_cap_preserves_integrity() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        transfer_rate_bytes_per_sec: 4 * 1024 * 1024, // 4 MiB/s — a light cap
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("admin", "pw-pw-pw", Role::Admin)
        .await
        .unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();

    let body = payload(600 * 1024);
    let node = seed_file(&mut admin, work.path(), &body).await;
    let dst = work.path().join("got.bin");
    let n = admin.transfer_download(node, &dst).await.unwrap();
    assert_eq!(n, body.len() as u64);
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        body,
        "throttled download is byte-exact"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn queue_driver_drains_downloads_and_uploads() {
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
    // Admin seeds a file to download and creates the area to upload into.
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let down_body = payload(500 * 1024);
    let node = seed_file(&mut admin, work.path(), &down_body).await;

    // A local file to upload through the queue.
    let up_src = work.path().join("upme.bin");
    let up_body = payload(400 * 1024);
    std::fs::write(&up_src, &up_body).unwrap();

    // Build the client-side queue and enqueue one download and one upload.
    let store = rabbithole_store_client::open_in_memory().unwrap();
    let q = TransferQueue(&store);
    let dst = work.path().join("pulled.bin");
    q.enqueue(
        &NewTransfer {
            direction: tq::DIR_DOWNLOAD,
            endpoint: "unused".into(),
            node_id: Some(node),
            local_path: dst.to_string_lossy().into_owned(),
            priority: 10,
            ..Default::default()
        },
        1_000,
    )
    .unwrap();
    q.enqueue(
        &NewTransfer {
            direction: tq::DIR_UPLOAD,
            endpoint: "unused".into(),
            area: Some("warez".into()),
            name: Some("upme.bin".into()),
            local_path: up_src.to_string_lossy().into_owned(),
            size: up_body.len() as i64,
            mime: "application/octet-stream".into(),
            comment: "via queue".into(),
            ..Default::default()
        },
        1_000,
    )
    .unwrap();

    // Drain over alice's session.
    let mut alice = login(&burrow, "alice").await;
    let report = rabbithole_core::queue::drain(&mut alice, &store, || 2_000)
        .await
        .unwrap();
    assert_eq!(report.completed, 2);
    assert_eq!(report.failed, 0);

    // The download landed byte-exact and both items are marked DONE.
    assert_eq!(std::fs::read(&dst).unwrap(), down_body);
    for item in q.all().unwrap() {
        assert_eq!(item.state, tq::DONE, "queue item #{} not done", item.id);
    }

    // The upload is now a real node in the library with its comment intact.
    let listed = alice.folder_list("warez", None).await.unwrap();
    let up = listed
        .iter()
        .find(|n| n.name == "upme.bin")
        .expect("uploaded file present in area");
    assert_eq!(up.size, up_body.len() as i64);
    assert_eq!(up.comment, "via queue");

    burrow.shutdown().await;
}
