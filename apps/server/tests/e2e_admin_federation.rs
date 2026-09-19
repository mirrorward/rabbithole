//! Federation peers and trusted origins over the wire: the console approves,
//! revokes and pins what `ctl` did from the burrow's own machine, with the
//! same rules and the same audit trail.

use burrow::federation::{dial_peer, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    origin_trust, peer_state, OriginList, OriginListRequest, OriginPin, PeerApprove, PeerList,
    PeerListRequest, PeerRevoke,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{PeerState, Role, ServerConfig};
use rabbithole_store_server::repo::AuditRepo;

const PW: &str = "pw-pw-pw";

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

fn target_for(b: &Burrow) -> DialTarget {
    DialTarget {
        addr: b.federation_addr.expect("federation enabled").to_string(),
        server_name: "localhost".into(),
        fingerprint: b.fingerprint,
        expected_key: Some(b.shared.server_key),
        expected_origin: b.shared.origin_name(),
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    c.auth_password(user, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

async fn until_state(burrow: &Burrow, key: &[u8; 32], wanted: PeerState) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while burrow.shared.peers.state(key) != Some(wanted) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("peer never reached {wanted:?}"));
}

#[tokio::test]
async fn an_operator_approves_and_revokes_a_peer_from_the_console() {
    let work = tempfile::tempdir().unwrap();
    let a = Burrow::start(fed_config(&work.path().join("a")))
        .await
        .unwrap();
    let b = Burrow::start(fed_config(&work.path().join("b")))
        .await
        .unwrap();
    for (login, role) in [("root", Role::Admin), ("alice", Role::User)] {
        b.shared.auth.create_account(login, PW, role).await.unwrap();
    }
    let a_key = a.shared.server_key;
    let b_key = b.shared.server_key;

    // A knocks on B, which has never heard of it: authenticated, pending.
    assert_eq!(
        dial_peer(a.shared.clone(), target_for(&b)).await.unwrap(),
        DialOutcome::Pending(b_key)
    );

    // A member sees none of this.
    let mut alice = login(&b, "alice").await;
    refused(
        alice.request::<_, PeerList>(&PeerListRequest).await,
        ErrorCode::Forbidden,
    );
    refused(
        alice.request_ack(&PeerApprove::new(a_key, None)).await,
        ErrorCode::Forbidden,
    );

    // The operator sees A waiting, with the origin it announced.
    let mut root = login(&b, "root").await;
    let list: PeerList = root.request(&PeerListRequest).await.unwrap();
    assert_eq!(list.peers.len(), 1, "{list:?}");
    let peer = &list.peers[0];
    assert_eq!(peer.key, a_key);
    assert_eq!(peer.state, peer_state::PENDING);
    assert!(!peer.approved && !peer.configured);
    assert_eq!(peer.origin.as_deref(), Some("a"));

    // A key nobody has heard of, with no origin to bind it to: refused.
    refused(
        root.request_ack(&PeerApprove::new([9u8; 32], None)).await,
        ErrorCode::BadRequest,
    );
    // The announced origin cannot be swapped for another at approval.
    refused(
        root.request_ack(&PeerApprove::new(a_key, Some("someone-else".into())))
            .await,
        ErrorCode::BadRequest,
    );

    // Approved under the origin it announced; A's next dial goes through.
    root.request_ack(&PeerApprove::new(a_key, None))
        .await
        .unwrap();
    assert!(b.shared.peers.is_approved_origin(&a_key, "a"));
    assert_eq!(
        dial_peer(a.shared.clone(), target_for(&b)).await.unwrap(),
        DialOutcome::Connected(b_key)
    );
    let list: PeerList = root.request(&PeerListRequest).await.unwrap();
    assert_eq!(list.peers[0].state, peer_state::CONNECTED);
    assert!(list.peers[0].approved);

    // The direct peer's signing key is now believed for relayed posts.
    let origins: OriginList = root.request(&OriginListRequest).await.unwrap();
    assert!(
        origins
            .origins
            .iter()
            .any(|o| o.origin == "a" && o.key == a_key && o.trust == origin_trust::DIRECT_PEER),
        "{origins:?}"
    );

    // Pinning by hand: a good name once, then the rules.
    root.request_ack(&OriginPin::new("grove.example", [7u8; 32]))
        .await
        .unwrap();
    root.request_ack(&OriginPin::new("grove.example", [7u8; 32]))
        .await
        .expect("pinning the same binding again is fine");
    refused(
        root.request_ack(&OriginPin::new("Grove Example", [6u8; 32]))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        root.request_ack(&OriginPin::new("grove.example", [8u8; 32]))
            .await,
        ErrorCode::BadRequest,
    );
    let origins: OriginList = root.request(&OriginListRequest).await.unwrap();
    assert!(origins
        .origins
        .iter()
        .any(|o| o.origin == "grove.example" && o.trust == origin_trust::OPERATOR));

    // Revoking closes the live session, and an unknown key is just unknown.
    root.request_ack(&PeerRevoke::new(a_key)).await.unwrap();
    until_state(&a, &b_key, PeerState::Disconnected).await;
    assert_eq!(b.shared.peers.state(&a_key), Some(PeerState::Pending));
    refused(
        root.request_ack(&PeerRevoke::new([9u8; 32])).await,
        ErrorCode::NotFound,
    );

    // All of it on the record, under the operator's own name.
    let audit = AuditRepo(&b.shared.pool).recent(50).await.unwrap();
    let by_root = |action: &str| {
        audit
            .iter()
            .any(|row| row.actor == "root" && row.action == action)
    };
    assert!(by_root("peer-approve"), "{audit:?}");
    assert!(by_root("peer-revoke"), "{audit:?}");
    assert!(by_root("origin-pin"), "{audit:?}");
    assert!(audit
        .iter()
        .any(|row| row.action == "peer-approve" && row.detail.starts_with("a ")));

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn a_configured_dial_target_is_not_revocable_from_the_console() {
    let work = tempfile::tempdir().unwrap();
    let a = Burrow::start(fed_config(&work.path().join("a")))
        .await
        .unwrap();
    let a_key = a.shared.server_key;
    let mut config = fed_config(&work.path().join("b"));
    config.federation_peers = vec![rabbithole_server_core::config::FederationPeer {
        name: "a".into(),
        origin: "a".into(),
        addr: a.federation_addr.unwrap().to_string(),
        server_name: "localhost".into(),
        key: hex::encode(a_key),
        fingerprint: hex::encode(a.fingerprint.0),
    }];
    let b = Burrow::start(config).await.unwrap();
    b.shared
        .auth
        .create_account("root", PW, Role::Admin)
        .await
        .unwrap();

    let mut root = login(&b, "root").await;
    let list: PeerList = root.request(&PeerListRequest).await.unwrap();
    let peer = list
        .peers
        .iter()
        .find(|p| p.key == a_key)
        .expect("the configured peer is listed");
    assert!(peer.approved && peer.configured, "{peer:?}");
    refused(
        root.request_ack(&PeerRevoke::new(a_key)).await,
        ErrorCode::BadRequest,
    );
    assert!(b.shared.peers.is_approved(&a_key), "still approved");

    a.shutdown().await;
    b.shutdown().await;
}
