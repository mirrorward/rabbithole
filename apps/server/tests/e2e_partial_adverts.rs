//! Partial seeds on the coordinator: a session holding part of a file
//! advertises it apart from whole ones, and only a client that asks for
//! partial sources is shown it, so a client from before them never meets a
//! peer that lacks units. Asking for all sources, a session is never shown
//! itself.

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_proto::swarm::AdvertEntry;
use rabbithole_server_core::{Role, ServerConfig};

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

fn names(list: &rabbithole_proto::swarm::SourceList) -> Vec<String> {
    let mut v: Vec<String> = list.sources.iter().map(|s| s.screen_name.clone()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn partial_seeds_are_found_only_by_those_who_ask_for_them() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Parts Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().to_path_buf(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for who in ["ann", "bob", "cy"] {
        burrow
            .shared
            .auth
            .create_account(who, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let root = [3u8; 32];
    let entry = AdvertEntry::new(root, 5 << 20, "tape.bin", "application/octet-stream");

    let mut ann = login(&burrow, "ann").await;
    let mut bob = login(&burrow, "bob").await;
    let mut cy = login(&burrow, "cy").await;
    // Ann holds all of it; Bob only part, still downloading.
    ann.swarm_advertise(vec![entry.clone()], 0).await.unwrap();
    let ack = bob
        .swarm_advertise_partial(vec![entry.clone()], 0)
        .await
        .unwrap()
        .expect("this burrow takes partial adverts");
    assert_eq!(ack.accepted, 1);

    // A client that asks for whole sources (as every client before partial
    // seeds does) sees Ann only; one that asks for all sees both.
    assert_eq!(names(&cy.swarm_find(root).await.unwrap()), ["ann"]);
    assert_eq!(
        names(&cy.swarm_find_all(root).await.unwrap()),
        ["ann", "bob"]
    );

    // Asking for all sources, nobody is shown themselves. (The older query
    // lists everyone, as it always has.)
    assert_eq!(names(&bob.swarm_find_all(root).await.unwrap()), ["ann"]);
    assert_eq!(names(&ann.swarm_find_all(root).await.unwrap()), ["bob"]);
    assert_eq!(names(&ann.swarm_find(root).await.unwrap()), ["ann"]);

    // Bob's download finishes: advertised whole, everyone sees him.
    bob.swarm_advertise(vec![entry.clone()], 0).await.unwrap();
    assert_eq!(names(&cy.swarm_find(root).await.unwrap()), ["ann", "bob"]);
    // A late partial re-announce does not demote him.
    bob.swarm_advertise_partial(vec![entry], 0).await.unwrap();
    assert_eq!(names(&cy.swarm_find(root).await.unwrap()), ["ann", "bob"]);

    burrow.shutdown().await;
}
