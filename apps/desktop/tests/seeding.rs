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
use rabbithole_desktop_lib::swarm::{run_download, run_download_sharing, Route, SourceMode, SwarmEvent, Wanted};
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
            // Two sources: Alice, and the burrow beside her (it sends
            // proved ranges too); every unit came from one of them.
            assert_eq!(per_source.len(), 2, "Alice and the burrow: {per_source:?}");
            assert!(per_source.iter().any(|(l, _)| l == "the burrow"));
            assert_eq!(per_source.iter().map(|(_, n)| n).sum::<u64>(), 4);
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

#[tokio::test]
async fn a_download_shares_as_it_goes_and_the_burrow_steps_in_when_peers_cannot() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Sharing Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for who in ["admin", "alice", "bob", "carol"] {
        burrow.shared.auth.create_account(who, "pw-pw-pw", Role::Admin).await.unwrap();
    }
    let body = payload(3 * 1024 * 1024 + 77);
    let src = work.path().join("tapes.zip");
    std::fs::write(&src, &body).unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = admin
        .transfer_upload("warez", None, "tapes.zip", &src, "application/zip", "")
        .await
        .unwrap();
    let root = *blake3::hash(&body).as_bytes();
    let size = body.len() as u64;
    let want = Wanted { root, size, node_id: Some(node.id), max_sources: 4, mode: SourceMode::Auto };

    // Alice has it from the burrow, and offers it.
    let mut alice = login(&burrow, "alice").await;
    let alices = work.path().join("alice.zip");
    run_download(&mut alice, &want, &alices, |_| {}).await.unwrap();
    let mut alice_seeder = Seeder::default();
    alice_seeder.share(&mut alice, root, size, "tapes.zip", &alices).await.unwrap();

    // Bob downloads with sharing on: offered from the start, fetched from
    // Alice, and seeded whole at the end from the proofs he kept (no second
    // pass over the file, no proofs file left beside it).
    let mut bob = login(&burrow, "bob").await;
    let bobs = work.path().join("bob.zip");
    let mut bob_seeder = Seeder::default();
    let share = bob_seeder.begin(&mut bob, root, size, "tapes.zip").await.unwrap();
    let seeds = share.seeds.clone();
    assert_eq!(bob_seeder.files(), 1, "set to share as it comes in");
    let route = run_download_sharing(&mut bob, &want, &bobs, Some(share), |_| {}).await.unwrap();
    assert_eq!(route, Route::Swarm);
    assert_eq!(std::fs::read(&bobs).unwrap(), body);
    assert!(seeds.holds_whole(&root));
    assert!(!rabbithole_swarm::proofs_path(&bobs).exists());
    // Sharing it again afterwards is the same one file.
    bob_seeder.share(&mut bob, root, size, "tapes.zip", &bobs).await.unwrap();
    assert_eq!(bob_seeder.files(), 1);

    // Alice's machine drops off without withdrawing her advert. Carol still
    // gets the file whole, from Bob.
    alice_seeder.stop(None).await;
    let mut carol = login(&burrow, "carol").await;
    let carols = work.path().join("carol.zip");
    let mut events = Vec::new();
    let route = run_download(&mut carol, &want, &carols, |e| events.push(e)).await.unwrap();
    assert_eq!(route, Route::Swarm);
    assert_eq!(std::fs::read(&carols).unwrap(), body);
    match events.last() {
        Some(SwarmEvent::Done { per_source, .. }) => {
            assert_eq!(per_source.iter().map(|(_, n)| n).sum::<u64>(), 4);
        }
        other => panic!("expected Done, got {other:?}"),
    }

    // Bob drops off too. Both adverts still stand, but no peer answers: the
    // burrow, one of the sources all along, sends every unit, each proved,
    // and it is still the file asked for.
    bob_seeder.stop(None).await;
    let again = work.path().join("again.zip");
    let mut events = Vec::new();
    let route = run_download(&mut carol, &want, &again, |e| events.push(e)).await.unwrap();
    assert_eq!(route, Route::Swarm);
    assert_eq!(std::fs::read(&again).unwrap(), body);
    match events.last() {
        Some(SwarmEvent::Done { per_source, .. }) => {
            let burrows: u64 = per_source.iter().filter(|(l, _)| l == "the burrow").map(|(_, n)| n).sum();
            assert_eq!(burrows, 4, "the burrow stepped in: {per_source:?}");
        }
        other => panic!("expected Done, got {other:?}"),
    }

    burrow.shutdown().await;
}

#[tokio::test]
async fn sharing_a_download_nobody_else_has_goes_straight_to_the_burrow() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Lonely Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for who in ["admin", "dana"] {
        burrow.shared.auth.create_account(who, "pw-pw-pw", Role::Admin).await.unwrap();
    }
    let body = payload(2 * 1024 * 1024 + 3);
    let src = work.path().join("solo.bin");
    std::fs::write(&src, &body).unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = admin.transfer_upload("warez", None, "solo.bin", &src, "application/octet-stream", "").await.unwrap();
    let root = *blake3::hash(&body).as_bytes();
    let size = body.len() as u64;

    // Dana shares as she downloads; nobody else has the file. Her own
    // endpoint is not a source for her, so the burrow sends it at once.
    let mut dana = login(&burrow, "dana").await;
    let mut seeder = Seeder::default();
    let share = seeder.begin(&mut dana, root, size, "solo.bin").await.unwrap();
    let seeds = share.seeds.clone();
    let dest = work.path().join("dana.bin");
    let started = std::time::Instant::now();
    let want = Wanted { root, size, node_id: Some(node.id), max_sources: 4, mode: SourceMode::Auto };
    let route = run_download_sharing(&mut dana, &want, &dest, Some(share), |_| {}).await.unwrap();
    assert_eq!(route, Route::Origin);
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "no wait on herself");
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert!(!rabbithole_swarm::proofs_path(&dest).exists());
    // Then it is hers to offer, whole.
    seeder.share(&mut dana, root, size, "solo.bin", &dest).await.unwrap();
    assert!(seeds.holds_whole(&root));

    // With peers only, she gets "nobody has it" at once, not after a wait.
    let mut seeder2 = Seeder::default();
    let share = seeder2.begin(&mut dana, [7; 32], size, "nothing.bin").await.unwrap();
    let want = Wanted { root: [7; 32], size, node_id: None, max_sources: 4, mode: SourceMode::PeersOnly };
    let started = std::time::Instant::now();
    assert!(run_download_sharing(&mut dana, &want, &work.path().join("x.bin"), Some(share), |_| {}).await.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(3));

    burrow.shutdown().await;
}

#[tokio::test]
async fn the_burrow_fills_what_the_peers_do_not_hold_unit_by_unit() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Mixed Warren".into(),
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
    let body = payload(4 * 1024 * 1024 + 55); // five units
    let src = work.path().join("mix.bin");
    std::fs::write(&src, &body).unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = admin.transfer_upload("warez", None, "mix.bin", &src, "application/octet-stream", "").await.unwrap();
    let root = *blake3::hash(&body).as_bytes();
    let size = body.len() as u64;

    // Alice holds only the first two units so far, with their proofs, and
    // offers them.
    let mut alice = login(&burrow, "alice").await;
    let hers = work.path().join("alice.bin");
    std::fs::write(&hers, &body).unwrap();
    let proofs = rabbithole_swarm::proofs_path(&hers);
    rabbithole_swarm::write_outboard(&hers, root, &proofs).unwrap();
    let seeds = std::sync::Arc::new(rabbithole_swarm::SeedStore::new());
    seeds.add_partial(root, size, &hers, &proofs, [0, 1]).unwrap();
    let peer = rabbithole_swarm::PeerServer::start("127.0.0.1:0".parse().unwrap(), alice.server.server_key, seeds)
        .await
        .unwrap();
    alice.swarm_contact(peer.addr.port(), peer.fingerprint.0).await.unwrap();
    let entry = rabbithole_proto::swarm::AdvertEntry::new(root, size, "mix.bin", "application/octet-stream");
    alice.swarm_advertise_partial(vec![entry], 0).await.unwrap().unwrap();

    // Bob downloads: from Alice what she holds, and from the burrow the rest,
    // every unit proved against the root.
    let mut bob = login(&burrow, "bob").await;
    let dest = work.path().join("bob.bin");
    let want = Wanted { root, size, node_id: Some(node.id), max_sources: 4, mode: SourceMode::Auto };
    let mut events = Vec::new();
    let route = run_download(&mut bob, &want, &dest, |e| events.push(e)).await.unwrap();
    assert_eq!(route, Route::Swarm, "peers and the burrow together");
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    match events.last() {
        Some(SwarmEvent::Done { per_source, .. }) => {
            let burrows: u64 = per_source.iter().filter(|(l, _)| l == "the burrow").map(|(_, n)| n).sum();
            let total: u64 = per_source.iter().map(|(_, n)| n).sum();
            assert_eq!(total, 5, "{per_source:?}");
            assert!(burrows >= 3, "the burrow sent what Alice lacks: {per_source:?}");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    burrow.shutdown().await;
}
