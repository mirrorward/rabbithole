//! Wave 5.2 end-to-end tests: the swarm coordinator — advertise
//! (list-without-upload), find-sources, withdraw, TTL soft state, the
//! per-account advert cap, session-scoped cleanup, and the origin-server
//! fallback flag.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::swarm::{AdvertEntry, SourceList};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Swarm Warren".into(),
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

fn entry(root: u8, name: &str, size: u64) -> AdvertEntry {
    AdvertEntry::new([root; 32], size, name, "application/octet-stream")
}

#[tokio::test]
async fn advertise_find_withdraw_roundtrip() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    let mut alice = login(&burrow, "alice").await;
    let ack = alice
        .swarm_advertise(vec![entry(1, "a.bin", 100), entry(2, "b.bin", 200)], 0)
        .await
        .unwrap();
    assert_eq!(ack.accepted, 2);
    assert_eq!(ack.total, 2);
    assert!(ack.ttl_secs > 0, "server granted a concrete TTL");

    // Bob sees alice as a source, with her metadata.
    let mut bob = login(&burrow, "bob").await;
    let list: SourceList = bob.swarm_find([1; 32]).await.unwrap();
    assert!(!list.server_has, "server never stored these bytes");
    assert_eq!(list.sources.len(), 1);
    assert_eq!(list.sources[0].screen_name, "alice");
    assert_eq!(list.sources[0].name, "a.bin");
    assert_eq!(list.sources[0].size, 100);

    // Withdraw one root: it vanishes, the other stays.
    alice.swarm_withdraw(vec![[1; 32]]).await.unwrap();
    assert!(bob.swarm_find([1; 32]).await.unwrap().sources.is_empty());
    assert_eq!(bob.swarm_find([2; 32]).await.unwrap().sources.len(), 1);

    // Withdraw-all clears the rest.
    alice.swarm_withdraw(vec![]).await.unwrap();
    assert!(bob.swarm_find([2; 32]).await.unwrap().sources.is_empty());

    burrow.shutdown().await;
}

#[tokio::test]
async fn readvertise_refreshes_and_cap_is_enforced() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        swarm_adverts_max: 2,
        ..test_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;

    // Two adverts fill the cap; the third is refused (accepted=0).
    let ack = alice
        .swarm_advertise(vec![entry(1, "a", 1), entry(2, "b", 2)], 0)
        .await
        .unwrap();
    assert_eq!(ack.accepted, 2);
    let ack = alice
        .swarm_advertise(vec![entry(3, "c", 3)], 0)
        .await
        .unwrap();
    assert_eq!(ack.accepted, 0, "cap of 2 refuses a third advert");
    assert_eq!(ack.total, 2);

    // Re-announcing an existing root is a refresh, not a new slot.
    let ack = alice
        .swarm_advertise(vec![entry(1, "a-renamed", 1)], 0)
        .await
        .unwrap();
    assert_eq!(ack.accepted, 1);
    assert_eq!(ack.total, 2);
    let list = alice.swarm_find([1; 32]).await.unwrap();
    assert_eq!(list.sources[0].name, "a-renamed", "metadata refreshed");

    burrow.shutdown().await;
}

#[tokio::test]
async fn ttl_expiry_prunes_sources() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;

    // Request a 1-second TTL (the server clamps requested values into
    // [1, max]; 1 stays 1) and watch the advert lapse.
    let ack = alice
        .swarm_advertise(vec![entry(9, "brief", 9)], 1)
        .await
        .unwrap();
    assert_eq!(ack.ttl_secs, 1);
    assert_eq!(alice.swarm_find([9; 32]).await.unwrap().sources.len(), 1);

    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    assert!(
        alice.swarm_find([9; 32]).await.unwrap().sources.is_empty(),
        "advert expired after its TTL"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn sources_vanish_when_the_session_closes() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for n in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }

    let mut alice = login(&burrow, "alice").await;
    alice
        .swarm_advertise(vec![entry(5, "gone", 5)], 0)
        .await
        .unwrap();

    let mut bob = login(&burrow, "bob").await;
    assert_eq!(bob.swarm_find([5; 32]).await.unwrap().sources.len(), 1);

    // Alice disconnects; her adverts must vanish (poll for async teardown).
    alice.close().await;
    let mut gone = false;
    for _ in 0..100 {
        if bob.swarm_find([5; 32]).await.unwrap().sources.is_empty() {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(gone, "disconnected peer's adverts were dropped");

    burrow.shutdown().await;
}

#[tokio::test]
async fn guests_cannot_advertise_but_can_find() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
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

    // No SWARM_ADVERTISE on the guest role.
    assert!(matches!(
        guest.swarm_advertise(vec![entry(1, "x", 1)], 0).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    // But FILE_LIST lets them look.
    let list = guest.swarm_find([1; 32]).await.unwrap();
    assert!(list.sources.is_empty());

    burrow.shutdown().await;
}

#[tokio::test]
async fn find_reports_when_the_origin_server_has_the_file() {
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

    // Upload a real file; its blob root is now served by the origin.
    let body: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    let src = work.path().join("seed.bin");
    std::fs::write(&src, &body).unwrap();
    admin
        .transfer_upload(
            "pub",
            None,
            "seed.bin",
            &src,
            "application/octet-stream",
            "",
        )
        .await
        .unwrap();
    let (root, size) = Client::hash_file(&src).unwrap();

    let list = admin.swarm_find(root).await.unwrap();
    assert!(list.server_has, "origin's blob store holds the file");
    assert_eq!(list.server_size, size);
    assert!(list.sources.is_empty(), "no peer adverts for it");

    burrow.shutdown().await;
}
