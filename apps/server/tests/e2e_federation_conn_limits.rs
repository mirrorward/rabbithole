//! RH-166: shared connection admission before federation authentication.
//! Real QUIC proves refusals close promptly; controlled clock hooks avoid
//! waiting for per-minute refill. No public network or extra loopback alias.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use burrow::federation::{dial_peer, spawn_federation, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_federation::PeerHello;
use rabbithole_identity::IdentityKey;
use rabbithole_net::quic::QuicTransport;
use rabbithole_net::tls::{CertFingerprint, ServerAuth, TlsIdentity};
use rabbithole_net::{Connection, NetError, Transport};
use rabbithole_proto::{Family, Frame, FrameKind, Payload, RequestId, PROTOCOL_VERSION};
use rabbithole_server_core::ratelimit::{self, class, Scope};
use rabbithole_server_core::{PeerState, ServerConfig};
use serde::Serialize;

fn config(dir: &Path, origin: &str, burst: u32) -> ServerConfig {
    ServerConfig {
        data_dir: dir.to_owned(),
        name: origin.into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_origin: origin.into(),
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_conn_per_min: 1,
        ratelimit_conn_burst: burst,
        ratelimit_auth_per_min: 1,
        ratelimit_auth_burst: 1,
        ..Default::default()
    }
}

fn ipv4_scope() -> Scope {
    Scope::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

async fn dial(a: &Burrow, b: &Burrow) -> anyhow::Result<DialOutcome> {
    tokio::time::timeout(
        Duration::from_secs(5),
        dial_peer(
            a.shared.clone(),
            DialTarget {
                addr: b.federation_addr.unwrap().to_string(),
                server_name: "localhost".into(),
                fingerprint: b.fingerprint,
                expected_key: Some(b.shared.server_key),
                expected_origin: b.shared.origin_name(),
            },
        ),
    )
    .await
    .expect("application handshake succeeds or is refused promptly")
}

#[derive(Serialize)]
struct Hello {
    hello: PeerHello,
    nonce: [u8; 32],
}

/// Open a real control stream and send a valid Hello, but leave the proof
/// outstanding. No application reply at all is allowed on admission refusal.
async fn greeting(
    addr: SocketAddr,
    fingerprint: CertFingerprint,
) -> (Box<dyn Connection>, Option<Frame>) {
    let mut conn = tokio::time::timeout(
        Duration::from_secs(5),
        QuicTransport::new("localhost", ServerAuth::Pinned(fingerprint)).connect(&addr.to_string()),
    )
    .await
    .expect("QUIC handshake bounded")
    .unwrap();
    let hello = Hello {
        hello: PeerHello {
            server_key: IdentityKey::from_seed(&[9; 32]).public().0,
            server_name: "Admission fixture".into(),
            origin: "fixture.example".into(),
            protocol_version: 2,
            software: "e2e".into(),
        },
        nonce: [7; 32],
    };
    let _ = conn
        .send(Frame {
            version: PROTOCOL_VERSION,
            kind: FrameKind::Request,
            family: Family::FEDERATION,
            message_type: 1,
            id: RequestId::PUSH,
            error: None,
            payload: Payload(postcard::to_allocvec(&hello).unwrap()),
        })
        .await;
    let reply = match tokio::time::timeout(Duration::from_secs(2), conn.recv())
        .await
        .expect("admission responds or closes promptly, without a graceful-close delay")
    {
        Ok(reply) => reply,
        Err(NetError::Closed | NetError::Quic(_) | NetError::Io(_)) => None,
        Err(error) => panic!("unexpected transport/protocol error: {error}"),
    };
    if let Some(frame) = &reply {
        assert_eq!(frame.family, Family::FEDERATION);
        assert_eq!(frame.message_type, 2, "HelloAck");
    }
    (conn, reply)
}

#[tokio::test]
async fn pending_and_approved_peers_each_cost_one_connection_but_no_auth_failure() {
    let dir = tempfile::tempdir().unwrap();
    // A refuses all inbound connections; its outbound dial must still work.
    let a = Burrow::start(config(&dir.path().join("a"), "a.example", 0))
        .await
        .unwrap();
    let b = Burrow::start(config(&dir.path().join("b"), "b.example", 2))
        .await
        .unwrap();
    assert_eq!(
        dial(&a, &b).await.unwrap(),
        DialOutcome::Pending(b.shared.server_key)
    );
    assert_eq!(
        b.shared.peers.state(&a.shared.server_key),
        Some(PeerState::Pending)
    );
    b.shared
        .peers
        .seed_approved(a.shared.server_key, "a", Some(a.shared.origin_name()));
    assert_eq!(
        dial(&a, &b).await.unwrap(),
        DialOutcome::Connected(b.shared.server_key)
    );
    assert!(b.shared.rate_probe(ipv4_scope(), class::AUTH));

    let (mut refused, reply) = greeting(b.federation_addr.unwrap(), b.fingerprint).await;
    assert!(reply.is_none(), "third connection gets no HelloAck");
    refused.close().await;
    assert!(!b.shared.rate_probe(ipv4_scope(), class::CONN));
    assert!(
        b.shared.rate_allow(ipv4_scope(), class::AUTH),
        "admission refusal did not charge AUTH"
    );
    assert!(!b.shared.rate_allow(ipv4_scope(), class::AUTH));
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn incomplete_handshake_spends_admission_other_ip_works_and_refill_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let b = Burrow::start(config(dir.path(), "b.example", 1))
        .await
        .unwrap();
    let tls = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
    let (ipv6, task) = spawn_federation(b.shared.clone(), "[::1]:0".parse().unwrap(), &tls)
        .await
        .unwrap();
    let (mut incomplete, reply) = greeting(b.federation_addr.unwrap(), b.fingerprint).await;
    assert!(reply.is_some());
    let (mut refused, reply) = greeting(b.federation_addr.unwrap(), b.fingerprint).await;
    assert!(reply.is_none());
    refused.close().await;

    // Same shared limiter, actual ::1 source, same claimed identity/origin.
    let (mut unrelated, reply) = greeting(ipv6, tls.fingerprint()).await;
    assert!(reply.is_some(), "another source IP remains admitted");
    unrelated.close().await;
    incomplete.close().await;
    assert!(b.shared.rate_probe(ipv4_scope(), class::AUTH));
    assert!(
        b.shared.peers.pending().is_empty(),
        "no incomplete proof creates a peer"
    );

    // Controlled admission tests cover the exact refill threshold; here the
    // explicit-clock expiry hook restores a full bucket before a real retry.
    b.shared
        .ratelimit
        .sweep(ratelimit::now_ms().saturating_add(120_001));
    let (mut recovered, reply) = greeting(b.federation_addr.unwrap(), b.fingerprint).await;
    assert!(reply.is_some(), "refilled IP is admitted again");
    recovered.close().await;
    assert!(!b.shared.rate_probe(ipv4_scope(), class::CONN));
    assert!(b.shared.rate_allow(ipv4_scope(), class::AUTH));
    assert!(!b.shared.rate_allow(ipv4_scope(), class::AUTH));
    task.abort();
    let _ = task.await;
    b.shutdown().await;
}

#[tokio::test]
async fn live_master_and_class_disables_preserve_valid_peering() {
    let dir = tempfile::tempdir().unwrap();
    let a = Burrow::start(config(&dir.path().join("a"), "a.example", 0))
        .await
        .unwrap();
    let b = Burrow::start(config(&dir.path().join("b"), "b.example", 0))
        .await
        .unwrap();
    let (mut refused, reply) = greeting(b.federation_addr.unwrap(), b.fingerprint).await;
    assert!(reply.is_none(), "nonzero rate with zero burst refuses");
    refused.close().await;

    b.shared
        .config
        .set_key("ratelimit_enabled", "false")
        .unwrap();
    assert_eq!(
        dial(&a, &b).await.unwrap(),
        DialOutcome::Pending(b.shared.server_key)
    );
    b.shared
        .peers
        .seed_approved(a.shared.server_key, "a", Some(a.shared.origin_name()));
    assert_eq!(
        dial(&a, &b).await.unwrap(),
        DialOutcome::Connected(b.shared.server_key)
    );
    b.shared
        .config
        .set_key("ratelimit_enabled", "true")
        .unwrap();
    b.shared
        .config
        .set_key("ratelimit_conn_per_min", "0")
        .unwrap();
    assert_eq!(
        dial(&a, &b).await.unwrap(),
        DialOutcome::Connected(b.shared.server_key)
    );
    assert!(b.shared.rate_probe(ipv4_scope(), class::AUTH));
    b.shared
        .config
        .set_key("ratelimit_conn_per_min", "1")
        .unwrap();
    let (mut refused, reply) = greeting(b.federation_addr.unwrap(), b.fingerprint).await;
    assert!(
        reply.is_none(),
        "restoring the live policy resumes admission enforcement"
    );
    refused.close().await;
    a.shutdown().await;
    b.shutdown().await;
}
