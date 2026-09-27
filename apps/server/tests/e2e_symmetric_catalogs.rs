//! RH-73: one configured direction carries both catalogs and later mutations
//! alongside board traffic. Uses the production 30-second content cadence.
use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::config::FederationPeer;
use rabbithole_server_core::{PeerState, ServerConfig, ServerEvent};
use serde_json::json;

async fn start(dir: &std::path::Path, origin: &str) -> Burrow {
    let b = Burrow::start(ServerConfig {
        name: origin.into(),
        federation_origin: origin.into(),
        data_dir: dir.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        federation_board_subscribe: vec!["shared".into()],
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    b.shared
        .files
        .create_area("public", "Public", "")
        .await
        .unwrap();
    b.shared
        .files
        .mkdir("public", None, "drop", true)
        .await
        .unwrap();
    b.shared
        .boards
        .create_board("shared", "Shared", "", 2, None, 0)
        .await
        .unwrap();
    b
}

async fn add(b: &Burrow, name: &str, hash: u8) -> i64 {
    b.shared
        .files
        .add_file(
            "public",
            None,
            name,
            &[hash; 32],
            3,
            "text/plain",
            "disk",
            "",
            "operator",
            1,
        )
        .await
        .unwrap()
        .id
}

async fn expect_catalog(on: &Burrow, peer: &Burrow, generation: u64, names: &[&str]) {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let Some(catalog) = on.shared.catalogs.peer_catalog(&peer.shared.server_key) {
                let mut actual: Vec<_> = catalog
                    .catalog
                    .entries
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect();
                actual.sort();
                if catalog.catalog.generation >= generation && actual == names {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("live session should converge without redial");
}

async fn post(b: &Burrow, subject: &str) -> [u8; 32] {
    let row = b
        .shared
        .boards
        .post(
            "shared",
            None,
            &format!("author@{}", b.shared.origin_name()),
            &[7; 32],
            subject,
            "body",
            "text/plain",
            1,
        )
        .await
        .unwrap();
    b.shared.bus.publish(ServerEvent::BoardPost {
        board: row.board_slug,
        id: row.event_id,
        root: row.root_id,
    });
    row.event_id
}

async fn expect_post(on: &Burrow, id: [u8; 32]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while on.shared.boards.post_by_id(&id).await.unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("board traffic remains routable alongside catalog requests");
}

#[tokio::test]
async fn one_configured_direction_synchronizes_both_libraries_and_silent_changes_with_board_traffic(
) {
    let work = tempfile::tempdir().unwrap();
    let a = start(&work.path().join("a"), "alpha").await;
    let b = start(&work.path().join("b"), "beta").await;
    let a_file = add(&a, "alpha.txt", 1).await;
    let b_file = add(&b, "beta.txt", 2).await;
    let approval = burrow::ctl::handle(&b.shared, &json!({
        "cmd": "peer-approve", "key": hex::encode(a.shared.server_key), "origin": a.shared.origin_name()
    })).await;
    assert_eq!(approval["ok"], true, "{approval}");
    // Restart the prepared A with one configured target. No manual dial is
    // made: its background dialer initiates, and B has no outbound target.
    let mut config = a.shared.config.read();
    config.federation_peers = vec![FederationPeer {
        name: "Beta".into(),
        origin: b.shared.origin_name(),
        addr: b.federation_addr.unwrap().to_string(),
        server_name: "localhost".into(),
        key: hex::encode(b.shared.server_key),
        fingerprint: b.fingerprint.to_hex(),
    }];
    a.shutdown().await;
    let a = Burrow::start(config).await.unwrap();
    assert_eq!(a.shared.config.read().federation_peers.len(), 1);
    assert!(b.shared.config.read().federation_peers.is_empty());
    tokio::join!(
        expect_catalog(&a, &b, 1, &["beta.txt"]),
        expect_catalog(&b, &a, 1, &["alpha.txt"])
    );
    let a_link = a.shared.s2s.link(&b.shared.server_key).unwrap();
    let b_link = b.shared.s2s.link(&a.shared.server_key).unwrap();

    // Both changes bypass file-added broadcasts: periodic content comparison
    // must remove a newly private file and publish a renamed public file.
    a.shared.files.move_to(a_file, Some("drop")).await.unwrap();
    b.shared.files.rename(b_file, "beta-new.txt").await.unwrap();
    let post_a = post(&a, "from alpha").await;
    let post_b = post(&b, "from beta").await;
    tokio::join!(
        expect_catalog(&a, &b, 2, &["beta-new.txt"]),
        expect_catalog(&b, &a, 2, &[]),
        expect_post(&a, post_b),
        expect_post(&b, post_a)
    );
    assert_eq!(
        a.shared.peers.state(&b.shared.server_key),
        Some(PeerState::Connected)
    );
    assert_eq!(
        b.shared.peers.state(&a.shared.server_key),
        Some(PeerState::Connected)
    );
    assert!(
        std::sync::Arc::ptr_eq(&a_link, &a.shared.s2s.link(&b.shared.server_key).unwrap()),
        "A retained the original transport link"
    );
    assert!(
        std::sync::Arc::ptr_eq(&b_link, &b.shared.s2s.link(&a.shared.server_key).unwrap()),
        "B retained the original transport link"
    );
    let from_a =
        burrow::ctl::handle(&a.shared, &json!({"cmd":"fed-search", "terms":"beta-new"})).await;
    assert_eq!(from_a["data"].as_array().unwrap().len(), 1);
    let from_b =
        burrow::ctl::handle(&b.shared, &json!({"cmd":"fed-search", "terms":"alpha"})).await;
    assert!(
        from_b["data"].as_array().unwrap().is_empty(),
        "withdrawn private file absent from search"
    );
    a.shutdown().await;
    b.shutdown().await;
}
