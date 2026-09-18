//! Opt-in seeding, end to end against a real burrow.
//!
//! One person downloads a file (from the burrow: nobody was seeding) and,
//! having opted in, offers it. A second person then downloads the same file,
//! and this time it comes from the first person's machine, verified block by
//! block, with a capability the burrow signed for that second person.

#![cfg_attr(rustfmt, rustfmt_skip)]

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_desktop_lib::seeding::Seeder;
use rabbithole_desktop_lib::swarm::{run_download, Route, SourceMode, SwarmEvent, Wanted};
use rabbithole_server_core::{Role, ServerConfig};

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 249) as u8).collect()
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "desktop-test",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

#[tokio::test]
async fn a_download_shared_by_one_person_serves_the_next() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Seeding Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for who in ["admin", "alice", "bob"] {
        burrow.shared.auth.create_account(who, "pw-pw-pw", Role::Admin).await.unwrap();
    }

    let body = payload(3 * 1024 * 1024 + 99);
    let src = work.path().join("album.zip");
    std::fs::write(&src, &body).unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = admin
        .transfer_upload("warez", None, "album.zip", &src, "application/zip", "")
        .await
        .unwrap();
    let root = *blake3::hash(&body).as_bytes();
    let want = Wanted { root, size: body.len() as u64, node_id: Some(node.id), max_sources: 4, mode: SourceMode::Auto };

    // Alice downloads. Nobody is seeding, so it comes from the burrow.
    let mut alice = login(&burrow, "alice").await;
    let alices_copy = work.path().join("alice").join("album.zip");
    std::fs::create_dir_all(alices_copy.parent().unwrap()).unwrap();
    assert_eq!(run_download(&mut alice, &want, &alices_copy, |_| {}).await.unwrap(), Route::Origin);

    // She opted in, so her copy is now on offer to this burrow's swarm.
    let mut seeder = Seeder::default();
    seeder.share(&mut alice, root, body.len() as u64, "album.zip", &alices_copy).await.unwrap();
    assert_eq!(seeder.files(), 1);
    // Sharing the same file twice is one file on offer.
    seeder.share(&mut alice, root, body.len() as u64, "album.zip", &alices_copy).await.unwrap();
    assert_eq!(seeder.files(), 1);

    // Bob downloads the same file. Now there is a peer: it comes from Alice.
    let mut bob = login(&burrow, "bob").await;
    let bobs_copy = work.path().join("bob.zip");
    let mut events = Vec::new();
    let route = run_download(&mut bob, &want, &bobs_copy, |e| events.push(e)).await.unwrap();
    assert_eq!(route, Route::Swarm, "a peer has it now");
    assert_eq!(std::fs::read(&bobs_copy).unwrap(), body, "and it is the same file, verified");
    match events.last() {
        Some(SwarmEvent::Done { per_source, .. }) => {
            assert_eq!(per_source.len(), 1, "one source: Alice");
            assert_ne!(per_source[0].0, "the burrow");
        }
        other => panic!("expected Done, got {other:?}"),
    }

    // Alice changes her mind. Her adverts go, her endpoint closes, and the
    // next download quietly goes back to the burrow.
    seeder.stop(Some(&mut alice)).await;
    assert_eq!(seeder.files(), 0);
    let again = work.path().join("again.zip");
    assert_eq!(run_download(&mut bob, &want, &again, |_| {}).await.unwrap(), Route::Origin);

    burrow.shutdown().await;
}
