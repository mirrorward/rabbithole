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
use rabbithole_server_core::{PeerState, ServerConfig};
use serde_json::json;

fn fed_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Federating Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
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
