//! The download path the app takes when nobody is seeding.
//!
//! On most burrows, most of the time, no peer advertises a given file. The
//! shell used to treat that as a failure, so downloading in the app simply did
//! not work unless someone happened to be sharing. This proves the fix against
//! a real burrow: discovery finds no peers, the fetch goes to the burrow
//! itself, the bytes are the file's, and the progress events are the ones the
//! Transfers row already understands.

#![cfg_attr(rustfmt, rustfmt_skip)]

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_desktop_lib::swarm::{run_download, Route, SourceMode, SwarmEvent, Wanted, ORIGIN_SOURCE};
use rabbithole_server_core::{Role, ServerConfig};

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
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
async fn with_nobody_seeding_the_file_comes_from_the_burrow() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Origin Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    burrow.shared.auth.create_account("admin", "pw-pw-pw", Role::Admin).await.unwrap();

    // A file a little over two swarm units, so progress has something to say.
    let body = payload(2 * 1024 * 1024 + 4321);
    let src = work.path().join("big.bin");
    std::fs::write(&src, &body).unwrap();
    let mut admin = login(&burrow, "admin").await;
    admin.area_create("warez", "Warez", "").await.unwrap();
    let node = admin
        .transfer_upload("warez", None, "big.bin", &src, "application/octet-stream", "")
        .await
        .unwrap();
    let root = *blake3::hash(&body).as_bytes();

    let want = |mode| Wanted { root, size: body.len() as u64, node_id: Some(node.id), max_sources: 4, mode };

    // The default. Nobody is seeding; it must work anyway.
    let dest = work.path().join("got.bin");
    let mut events = Vec::new();
    let route = run_download(&mut admin, &want(SourceMode::Auto), &dest, |e| events.push(e))
        .await
        .expect("no peers is not a failure");
    assert_eq!(route, Route::Origin);
    assert_eq!(std::fs::read(&dest).unwrap(), body, "the bytes are the file's");
    assert!(matches!(events.first(), Some(SwarmEvent::Opened { total_units: 3, source_count: 1 })), "{events:?}");
    let chunks = events.iter().filter(|e| matches!(e, SwarmEvent::Chunk { .. })).count();
    assert_eq!(chunks, 3, "one progress event per unit: {events:?}");
    match events.last() {
        Some(SwarmEvent::Done { bytes, per_source }) => {
            assert_eq!(*bytes, body.len() as u64);
            assert_eq!(per_source, &vec![(ORIGIN_SOURCE.to_string(), 3)]);
        }
        other => panic!("expected Done last, got {other:?}"),
    }

    // Peers only means it: with nobody seeding, this one does fail, and says why.
    let refused = run_download(&mut admin, &want(SourceMode::PeersOnly), &work.path().join("no.bin"), |_| {})
        .await
        .expect_err("peers only, and there are none");
    assert!(refused.to_string().contains("peers only"), "{refused}");
    assert!(!work.path().join("no.bin").exists(), "nothing was written");

    // The burrow only, asked for outright.
    let direct = work.path().join("direct.bin");
    let route = run_download(&mut admin, &want(SourceMode::OriginOnly), &direct, |_| {}).await.unwrap();
    assert_eq!(route, Route::Origin);
    assert_eq!(std::fs::read(&direct).unwrap(), body);

    // A half-finished swarm attempt must not poison the origin's resume: its
    // file has units at arbitrary offsets, which a length-based resume would
    // mistake for a contiguous partial.
    let tainted = work.path().join("tainted.bin");
    std::fs::write(&tainted, vec![0xAA; 1024 * 1024 + 7]).unwrap();
    std::fs::write(rabbithole_swarm::scheduler::rhstate_path(&tainted), b"stale").unwrap();
    run_download(&mut admin, &want(SourceMode::Auto), &tainted, |_| {}).await.unwrap();
    assert_eq!(std::fs::read(&tainted).unwrap(), body, "started clean, verified whole");

    // A node id is only a number. Ask for one root while naming a node that
    // holds another: the fetch must refuse to keep what it got.
    let other = payload(4096);
    let other_src = work.path().join("other.bin");
    std::fs::write(&other_src, &other).unwrap();
    let other_node = admin
        .transfer_upload("warez", None, "other.bin", &other_src, "application/octet-stream", "")
        .await
        .unwrap();
    let wrong = work.path().join("wrong.bin");
    let mismatched = Wanted { root, size: body.len() as u64, node_id: Some(other_node.id), max_sources: 4, mode: SourceMode::OriginOnly };
    let refused = run_download(&mut admin, &mismatched, &wrong, |_| {}).await.expect_err("wrong content");
    assert!(refused.to_string().contains("different file"), "{refused}");
    assert!(!wrong.exists(), "the wrong file is not left on disk");

    burrow.shutdown().await;
}
