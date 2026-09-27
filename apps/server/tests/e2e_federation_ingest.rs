//! RH-68: real authenticated QUIC links exercise shared ingest enforcement.
use burrow::{
    federation::{dial_peer, DialTarget},
    Burrow,
};
use rabbithole_federation::{PeerHello, PeerHelloAck};
use rabbithole_identity::{IdentityKey, Signature};
use rabbithole_net::{quic::QuicTransport, tls::ServerAuth, Connection, Transport};
use rabbithole_proto::{Family, Frame, FrameKind, Payload, RequestId, PROTOCOL_VERSION};
use rabbithole_server_core::ServerConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize)]
struct Hello {
    hello: PeerHello,
    nonce: [u8; 32],
}
#[derive(Deserialize)]
struct Ack {
    ack: PeerHelloAck,
    nonce: [u8; 32],
    proof: Signature,
}
#[derive(Serialize)]
struct Proof {
    proof: Signature,
}

fn frame<T: Serialize>(kind: FrameKind, message_type: u16, value: &T) -> Frame {
    Frame {
        version: PROTOCOL_VERSION,
        kind,
        family: Family::FEDERATION,
        message_type,
        id: RequestId::PUSH,
        error: None,
        payload: Payload(postcard::to_allocvec(value).unwrap()),
    }
}
async fn connect(peer: &Burrow, server: &Burrow) -> Box<dyn Connection> {
    let mut conn = QuicTransport::new("localhost", ServerAuth::Pinned(server.fingerprint))
        .connect(&server.federation_addr.unwrap().to_string())
        .await
        .unwrap();
    let nonce = [42; 32];
    conn.send(frame(
        FrameKind::Request,
        1,
        &Hello {
            hello: PeerHello {
                server_key: peer.shared.server_key,
                server_name: "fixture".into(),
                origin: peer.shared.origin_name(),
                protocol_version: 2,
                software: "test".into(),
            },
            nonce,
        },
    ))
    .await
    .unwrap();
    let response = conn.recv().await.unwrap().unwrap();
    let ack: Ack = postcard::from_bytes(&response.payload.0).unwrap();
    let mut transcript = b"rhp-fed-s2s-auth-v2".to_vec();
    transcript.extend_from_slice(&peer.shared.server_key);
    transcript.extend_from_slice(&server.shared.server_key);
    for origin in [peer.shared.origin_name(), server.shared.origin_name()] {
        transcript.extend_from_slice(&(origin.len() as u16).to_be_bytes());
        transcript.extend_from_slice(origin.as_bytes());
    }
    transcript.extend_from_slice(&nonce);
    transcript.extend_from_slice(&ack.nonce);
    assert!(rabbithole_identity::PublicKey(ack.ack.server_key).verify(&transcript, &ack.proof));
    let key = IdentityKey::from_seed(&peer.shared.server_signing_seed);
    conn.send(frame(
        FrameKind::Request,
        3,
        &Proof {
            proof: key.sign(&transcript),
        },
    ))
    .await
    .unwrap();
    let welcome = conn.recv().await.unwrap().unwrap();
    assert_eq!(welcome.message_type, 4);
    assert!(postcard::from_bytes::<bool>(&welcome.payload.0).unwrap());
    conn
}
async fn closed(conn: &mut dyn Connection) {
    let received = tokio::time::timeout(Duration::from_secs(5), conn.recv())
        .await
        .expect("refusal closes promptly");
    assert!(
        !matches!(received, Ok(Some(_))),
        "unexpected application response"
    );
}
async fn node(path: &std::path::Path) -> Burrow {
    Burrow::start(ServerConfig {
        data_dir: path.into(),
        federation_enabled: true,
        federation_origin: path.file_name().unwrap().to_str().unwrap().into(),
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_enabled: false,
        federation_ingest_frames_burst: 2,
        federation_ingest_frames_per_sec: 0,
        ..Default::default()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn noisy_peer_and_reconnect_share_debt_while_healthy_peer_keeps_working() {
    let dir = tempfile::tempdir().unwrap();
    let server = node(&dir.path().join("server")).await;
    let noisy = node(&dir.path().join("noisy")).await;
    let healthy = node(&dir.path().join("healthy")).await;
    for peer in [&noisy, &healthy] {
        server.shared.peers.seed_approved(
            peer.shared.server_key,
            "fixture",
            Some(peer.shared.origin_name()),
        );
    }
    let mut one = connect(&noisy, &server).await;
    let mut two = connect(&noisy, &server).await;
    // Unknown messages are forward-compatible but still spend ingest work.
    one.send(frame(FrameKind::Request, 65000, &()))
        .await
        .unwrap();
    one.send(frame(FrameKind::Request, 65000, &()))
        .await
        .unwrap();
    one.send(frame(FrameKind::Request, 65000, &()))
        .await
        .unwrap();
    closed(one.as_mut()).await;
    two.send(frame(FrameKind::Request, 65000, &()))
        .await
        .unwrap();
    closed(two.as_mut()).await;
    let mut reconnected = connect(&noisy, &server).await;
    reconnected
        .send(frame(FrameKind::Request, 65000, &()))
        .await
        .unwrap();
    closed(reconnected.as_mut()).await;
    let mut good = connect(&healthy, &server).await;
    // A legitimate empty catalog announcement has a deterministic reply.
    good.send(frame(FrameKind::Request, 5, &([0u8; 32], 0u64)))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), good.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.message_type, 5);
    good.close().await;
    noisy.shutdown().await;
    healthy.shutdown().await;
    server.shutdown().await;
}

#[tokio::test]
async fn live_deny_closes_idle_link_overrides_dial_and_preserves_approval_and_pin() {
    let dir = tempfile::tempdir().unwrap();
    let server = node(&dir.path().join("server")).await;
    let peer = node(&dir.path().join("peer")).await;
    let key = peer.shared.server_key;
    server
        .shared
        .peers
        .seed_approved(key, "fixture", Some(peer.shared.origin_name()));
    let mut conn = connect(&peer, &server).await;
    assert_eq!(server.shared.fed_flood.resolve("peer"), Some(key));
    server
        .shared
        .config
        .set_key(
            "federation_denied_keys",
            &format!("[\"{}\"]", hex::encode(key)),
        )
        .unwrap();
    closed(conn.as_mut()).await;
    assert!(server.shared.peers.is_approved_origin(&key, "peer"));
    assert_eq!(server.shared.fed_flood.resolve("peer"), Some(key));
    // Deny takes precedence even over a configured/explicit outbound dial.
    let result = dial_peer(
        server.shared.clone(),
        DialTarget {
            addr: peer.federation_addr.unwrap().to_string(),
            server_name: "localhost".into(),
            fingerprint: peer.fingerprint,
            expected_key: Some(key),
            expected_origin: "peer".into(),
        },
    )
    .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("explicitly denied"));
    server
        .shared
        .config
        .set_key("federation_denied_keys", "[]")
        .unwrap();
    let mut resumed = connect(&peer, &server).await;
    resumed
        .send(frame(FrameKind::Request, 5, &([0u8; 32], 0u64)))
        .await
        .unwrap();
    assert_eq!(resumed.recv().await.unwrap().unwrap().message_type, 5);
    resumed.close().await;
    peer.shutdown().await;
    server.shutdown().await;
}

#[tokio::test]
async fn byte_and_event_budgets_close_the_real_session_before_work() {
    for bytes in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let server = node(&dir.path().join("server")).await;
        let peer = node(&dir.path().join("peer")).await;
        server.shared.peers.seed_approved(
            peer.shared.server_key,
            "fixture",
            Some(peer.shared.origin_name()),
        );
        let mut conn = connect(&peer, &server).await;
        let request = if bytes {
            server
                .shared
                .config
                .set_key("federation_ingest_bytes_burst", "16")
                .unwrap();
            let mut request = frame(FrameKind::Request, 65000, &());
            request.payload = Payload(vec![0; 17]);
            request
        } else {
            server
                .shared
                .config
                .set_key("federation_ingest_events_burst", "1")
                .unwrap();
            let events = rabbithole_federation::PushEvents {
                board: "missing".into(),
                events: vec![
                    rabbithole_federation::FedEvent {
                        id: [1; 32],
                        bytes: vec![255],
                    },
                    rabbithole_federation::FedEvent {
                        id: [2; 32],
                        bytes: vec![255],
                    },
                ],
            };
            frame(
                FrameKind::Reply,
                11,
                &(events, vec![peer.shared.server_key; 2]),
            )
        };
        conn.send(request).await.unwrap();
        closed(conn.as_mut()).await;
        assert!(server.shared.peers.is_approved(&peer.shared.server_key));
        peer.shutdown().await;
        server.shutdown().await;
    }
}

#[tokio::test]
async fn initial_catalog_budget_refusal_is_not_downgraded_to_success() {
    let dir = tempfile::tempdir().unwrap();
    let server = node(&dir.path().join("server")).await;
    let peer = node(&dir.path().join("peer")).await;
    server.shared.peers.seed_approved(
        peer.shared.server_key,
        "fixture",
        Some(peer.shared.origin_name()),
    );
    peer.shared
        .config
        .set_key("federation_ingest_frames_burst", "0")
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        dial_peer(
            peer.shared.clone(),
            DialTarget {
                addr: server.federation_addr.unwrap().to_string(),
                server_name: "localhost".into(),
                fingerprint: server.fingerprint,
                expected_key: Some(server.shared.server_key),
                expected_origin: "server".into(),
            },
        ),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().to_string().contains("frames budget"));
    assert_ne!(
        peer.shared.peers.state(&server.shared.server_key),
        Some(rabbithole_server_core::PeerState::Connected)
    );
    peer.shutdown().await;
    server.shutdown().await;
}
