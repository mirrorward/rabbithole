//! Wave 9 end-to-end tests: server-to-server (S2S) federation peering wired
//! into `burrow`. Two burrows are brought up; one dials the other over the
//! real QUIC federation endpoint. We prove that:
//!
//! - the handshake mutually authenticates each server's Ed25519 identity;
//! - an unknown peer stays **pending** (refused) until an admin approves it
//!   through the audited `ctl` path;
//! - approval transitions the peer to **connected** on both sides;
//! - approved peer keys persist across a restart;
//! - a peer presenting an unexpected key is rejected;
//! - federation is off by default.
//!
//! The registry logic is unit-tested in `rabbithole-server-core`; here we
//! exercise the live socket transport + approval + lifecycle. Determinism is
//! by real readiness signals (`dial_peer` awaits the full handshake, and the
//! listener updates the registry before its final `Welcome`) — no sleeps.

use burrow::federation::{dial_peer, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_server_core::{config::FederationPeer, PeerState, ServerConfig};
use serde_json::json;

fn fed_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Federating Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_origin: dir.file_name().unwrap().to_string_lossy().into_owned(),
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// A dial target aimed at `b`, pinning its live cert + identity.
fn target_for(b: &Burrow) -> DialTarget {
    DialTarget {
        addr: b.federation_addr.expect("federation enabled").to_string(),
        server_name: "localhost".into(),
        fingerprint: b.fingerprint,
        expected_key: Some(b.shared.server_key),
        expected_origin: b.shared.origin_name(),
    }
}

#[tokio::test]
async fn unknown_peer_pending_until_approved_then_connected() {
    let work = tempfile::tempdir().unwrap();
    let a = Burrow::start(fed_config(&work.path().join("a")))
        .await
        .unwrap();
    let b = Burrow::start(fed_config(&work.path().join("b")))
        .await
        .unwrap();
    let a_key = a.shared.server_key;
    let b_key = b.shared.server_key;
    assert_ne!(a_key, b_key, "the two burrows have distinct identities");

    // A dials B, which has never heard of A: authenticated but not approved.
    let outcome = dial_peer(a.shared.clone(), target_for(&b)).await.unwrap();
    assert_eq!(outcome, DialOutcome::Pending(b_key));

    // B recorded A as an authenticated, pending peer (refused for now).
    assert_eq!(b.shared.peers.state(&a_key), Some(PeerState::Pending));
    assert!(!b.shared.peers.is_approved(&a_key));
    let pending = b.shared.peers.pending();
    assert_eq!(pending.len(), 1, "exactly one pending peer");
    assert_eq!(pending[0].server_key, a_key);
    let a_key_hex = pending[0].key_hex();

    // A, having chosen to dial B, trusts B — but no session is live yet.
    assert!(a.shared.peers.is_approved(&b_key));
    assert_eq!(a.shared.peers.state(&b_key), Some(PeerState::Disconnected));

    // Admin approves A on B via the audited ctl path (owner-only socket).
    let resp =
        burrow::ctl::handle(&b.shared, &json!({"cmd": "peer-approve", "key": a_key_hex})).await;
    assert_eq!(resp["ok"], json!(true), "approval accepted: {resp}");
    assert!(b.shared.peers.is_approved(&a_key));

    // peer-list reflects the approval.
    let list = burrow::ctl::handle(&b.shared, &json!({"cmd": "peer-list"})).await;
    let peers = list["data"].as_array().unwrap();
    assert!(peers
        .iter()
        .any(|p| p["key"] == json!(a_key_hex) && p["approved"] == json!(true)));

    // A dials again; now B approves and the session goes live both ways.
    let outcome = dial_peer(a.shared.clone(), target_for(&b)).await.unwrap();
    assert_eq!(outcome, DialOutcome::Connected(b_key));
    assert_eq!(b.shared.peers.state(&a_key), Some(PeerState::Connected));
    assert_eq!(a.shared.peers.state(&b_key), Some(PeerState::Connected));

    // Revocation is a live-session boundary, not merely a future-dial gate.
    let revoked = burrow::ctl::handle(
        &b.shared,
        &json!({"cmd": "peer-revoke", "key": hex::encode(a_key)}),
    )
    .await;
    assert_eq!(revoked["ok"], json!(true));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if a.shared.peers.state(&b_key) == Some(PeerState::Disconnected) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("revoking a live peer closes the session");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn approved_peer_persists_across_restart() {
    let work = tempfile::tempdir().unwrap();
    let b_dir = work.path().join("b");
    let a = Burrow::start(fed_config(&work.path().join("a")))
        .await
        .unwrap();
    let a_key = a.shared.server_key;

    let b = Burrow::start(fed_config(&b_dir)).await.unwrap();
    dial_peer(a.shared.clone(), target_for(&b)).await.unwrap();
    let a_key_hex = b.shared.peers.pending()[0].key_hex();
    let resp =
        burrow::ctl::handle(&b.shared, &json!({"cmd": "peer-approve", "key": a_key_hex})).await;
    assert_eq!(resp["ok"], json!(true));
    b.shutdown().await;

    // Restart B on the same data dir: the approval is reloaded from disk.
    let b2 = Burrow::start(fed_config(&b_dir)).await.unwrap();
    assert!(
        b2.shared.peers.is_approved(&a_key),
        "approved key survived the restart"
    );

    // A fresh dial now connects immediately, no re-approval needed.
    let outcome = dial_peer(a.shared.clone(), target_for(&b2)).await.unwrap();
    assert_eq!(outcome, DialOutcome::Connected(b2.shared.server_key));
    assert_eq!(b2.shared.peers.state(&a_key), Some(PeerState::Connected));

    a.shutdown().await;
    b2.shutdown().await;
}

#[tokio::test]
async fn failed_peer_approval_persistence_leaves_no_origin_authority() {
    let work = tempfile::tempdir().unwrap();
    let b_dir = work.path().join("b");
    let a = Burrow::start(fed_config(&work.path().join("a")))
        .await
        .unwrap();
    let b = Burrow::start(fed_config(&b_dir)).await.unwrap();
    let a_key = a.shared.server_key;
    let a_origin = a.shared.origin_name();

    dial_peer(a.shared.clone(), target_for(&b)).await.unwrap();
    let approval_path = b_dir.join("federation").join("approved_peers.json");
    std::fs::create_dir_all(&approval_path).unwrap();
    let response = burrow::ctl::handle(
        &b.shared,
        &json!({
            "cmd": "peer-approve",
            "key": hex::encode(a_key),
            "origin": a_origin,
        }),
    )
    .await;
    assert_eq!(
        response["ok"],
        json!(false),
        "approval must fail: {response}"
    );
    assert!(!b.shared.peers.is_approved(&a_key));
    assert_eq!(b.shared.fed_flood.resolve(&a.shared.origin_name()), None);

    b.shutdown().await;
    let b2 = Burrow::start(fed_config(&b_dir)).await.unwrap();
    assert!(!b2.shared.peers.is_approved(&a_key));
    assert_eq!(
        b2.shared.fed_flood.resolve(&a.shared.origin_name()),
        None,
        "a failed peer approval must not authorize later relayed content after restart"
    );

    a.shutdown().await;
    b2.shutdown().await;
}

#[tokio::test]
async fn configured_peer_revoke_requires_config_removal_and_restart() {
    let work = tempfile::tempdir().unwrap();
    let data_dir = work.path().join("configured");
    let key = [0x31u8; 32];
    let mut config = fed_config(&data_dir);
    config.federation_peers = vec![FederationPeer {
        name: "configured-peer".into(),
        origin: "configured.example".into(),
        addr: "127.0.0.1:1".into(),
        server_name: "localhost".into(),
        key: hex::encode(key),
        fingerprint: hex::encode([0x42u8; 32]),
    }];

    let server = Burrow::start(config.clone()).await.unwrap();
    assert!(server
        .shared
        .peers
        .is_approved_origin(&key, "configured.example"));
    let response = burrow::ctl::handle(
        &server.shared,
        &json!({"cmd": "peer-revoke", "key": hex::encode(key)}),
    )
    .await;
    assert_eq!(response["ok"], json!(false));
    assert!(response["error"]
        .as_str()
        .unwrap()
        .contains("remove it from configuration and restart"));
    assert!(server
        .shared
        .peers
        .is_approved_origin(&key, "configured.example"));
    server.shutdown().await;

    let restarted = Burrow::start(config).await.unwrap();
    assert!(restarted
        .shared
        .peers
        .is_approved_origin(&key, "configured.example"));
    restarted.shutdown().await;
}

#[tokio::test]
async fn peer_with_unexpected_key_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let a = Burrow::start(fed_config(&work.path().join("a")))
        .await
        .unwrap();
    let b = Burrow::start(fed_config(&work.path().join("b")))
        .await
        .unwrap();

    // Pin the wrong identity: the handshake must refuse the connection.
    let mut target = target_for(&b);
    target.expected_key = Some([0x42u8; 32]);
    assert!(
        dial_peer(a.shared.clone(), target).await.is_err(),
        "a peer presenting an unexpected key is rejected"
    );

    let mut target = target_for(&b);
    target.expected_origin = "victim.example".into();
    assert!(
        dial_peer(a.shared.clone(), target).await.is_err(),
        "a peer cannot substitute an unapproved federation origin"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn federation_off_by_default() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        name: "Quiet Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().join("srv"),
        ..ServerConfig::default()
    };
    assert!(!cfg.federation_enabled, "federation defaults off");
    let burrow = Burrow::start(cfg).await.unwrap();
    assert!(
        burrow.federation_addr.is_none(),
        "no federation listener when disabled"
    );
    burrow.shutdown().await;
}
