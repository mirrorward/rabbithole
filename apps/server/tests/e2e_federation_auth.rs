//! RH-118: real QUIC peers exercise the inbound federation AUTH boundary.
//! Each refusal is awaited on the protocol; there are no negative sleeps.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_federation::{PeerHello, PeerHelloAck};
use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use rabbithole_net::quic::QuicTransport;
use rabbithole_net::tls::ServerAuth;
use rabbithole_net::{Connection, Transport};
use rabbithole_proto::{Family, Frame, FrameKind, Payload, RequestId, PROTOCOL_VERSION};
use rabbithole_server_core::ServerConfig;
use serde::{Deserialize, Serialize};

const HELLO: u16 = 1;
const ACK: u16 = 2;
const PROOF: u16 = 3;
const WELCOME: u16 = 4;

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

#[derive(Deserialize)]
struct Welcome {
    connected: bool,
}

fn frame<T: Serialize>(message_type: u16, message: &T) -> Frame {
    Frame {
        version: PROTOCOL_VERSION,
        kind: FrameKind::Request,
        family: Family::FEDERATION,
        message_type,
        id: RequestId::PUSH,
        error: None,
        payload: Payload(postcard::to_allocvec(message).unwrap()),
    }
}

async fn connect(server: &Burrow) -> Box<dyn Connection> {
    tokio::time::timeout(
        Duration::from_secs(5),
        QuicTransport::new("localhost", ServerAuth::Pinned(server.fingerprint))
            .connect(&server.federation_addr.unwrap().to_string()),
    )
    .await
    .expect("QUIC connection bounded")
    .unwrap()
}

async fn recv(conn: &mut dyn Connection) -> Option<Frame> {
    tokio::time::timeout(Duration::from_secs(5), conn.recv())
        .await
        .expect("peer answers or closes promptly")
        .unwrap()
}

fn hello(key: &IdentityKey) -> Hello {
    Hello {
        hello: PeerHello {
            server_key: key.public().0,
            server_name: "Authentication fixture".into(),
            origin: "fixture.example".into(),
            protocol_version: 2,
            software: "e2e".into(),
        },
        nonce: [7; 32],
    }
}

/// Reconstruct the documented v2 transcript, and verify the server's proof
/// before generating the peer's valid or deliberately invalid signature.
fn proof(key: &IdentityKey, hello: &Hello, ack_frame: &Frame, valid: bool) -> Frame {
    assert_eq!(ack_frame.family, Family::FEDERATION);
    assert_eq!(ack_frame.message_type, ACK);
    let ack: Ack = postcard::from_bytes(&ack_frame.payload.0).unwrap();
    let mut transcript = b"rhp-fed-s2s-auth-v2".to_vec();
    transcript.extend_from_slice(&hello.hello.server_key);
    transcript.extend_from_slice(&ack.ack.server_key);
    for origin in [&hello.hello.origin, &ack.ack.origin] {
        transcript.extend_from_slice(&(origin.len() as u16).to_be_bytes());
        transcript.extend_from_slice(origin.as_bytes());
    }
    transcript.extend_from_slice(&hello.nonce);
    transcript.extend_from_slice(&ack.nonce);
    assert!(PublicKey(ack.ack.server_key).verify(&transcript, &ack.proof));
    let signature = if valid {
        key.sign(&transcript)
    } else {
        IdentityKey::from_seed(&[99; 32]).sign(&transcript)
    };
    frame(PROOF, &Proof { proof: signature })
}

async fn attempt(server: &Burrow, key: &IdentityKey, valid: bool) -> Option<bool> {
    let mut conn = connect(server).await;
    let hello = hello(key);
    conn.send(frame(HELLO, &hello)).await.unwrap();
    let ack = recv(conn.as_mut()).await.expect("challenge admitted");
    conn.send(proof(key, &hello, &ack, valid)).await.unwrap();
    let result = recv(conn.as_mut()).await.map(|frame| {
        assert_eq!(frame.message_type, WELCOME);
        postcard::from_bytes::<Welcome>(&frame.payload.0)
            .unwrap()
            .connected
    });
    conn.close().await;
    result
}

async fn server(path: &std::path::Path) -> Burrow {
    Burrow::start(ServerConfig {
        name: "Limited federation".into(),
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_origin: "source.example".into(),
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_conn_per_min: 0, // isolate the failures-only AUTH budget
        ratelimit_auth_per_min: 1,
        ratelimit_auth_burst: 2,
        ..ServerConfig::default()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn invalid_proofs_exhaust_ip_while_successes_and_disabled_limits_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path()).await;
    let key = IdentityKey::from_seed(&[8; 32]);

    for _ in 0..3 {
        assert_eq!(
            attempt(&server, &key, true).await,
            Some(false),
            "pending proofs are free"
        );
    }
    server
        .shared
        .peers
        .seed_approved(key.public().0, "fixture", Some("fixture.example".into()));
    for _ in 0..3 {
        assert_eq!(
            attempt(&server, &key, true).await,
            Some(true),
            "approved proofs are free"
        );
    }
    for _ in 0..2 {
        assert_eq!(
            attempt(&server, &key, false).await,
            None,
            "invalid proof closes without Welcome"
        );
    }

    // New source ports and even an approved claimed identity cannot bypass
    // the exhausted IP bucket. Refusal precedes any signed HelloAck.
    let mut conn = connect(&server).await;
    let _ = conn.send(frame(HELLO, &hello(&key))).await;
    assert!(recv(conn.as_mut()).await.is_none());
    conn.close().await;

    for knob in ["ratelimit_enabled", "ratelimit_auth_per_min"] {
        server
            .shared
            .config
            .set_key(
                knob,
                if knob == "ratelimit_enabled" {
                    "false"
                } else {
                    "0"
                },
            )
            .unwrap();
        for _ in 0..3 {
            assert_eq!(attempt(&server, &key, false).await, None);
            assert_eq!(attempt(&server, &key, true).await, Some(true));
        }
        server
            .shared
            .config
            .set_key(
                knob,
                if knob == "ratelimit_enabled" {
                    "true"
                } else {
                    "1"
                },
            )
            .unwrap();
    }
    server.shutdown().await;
}

#[tokio::test]
async fn previously_issued_challenge_cannot_bypass_an_exhausted_ip_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path()).await;
    let key = IdentityKey::from_seed(&[8; 32]);
    let mut waiting = connect(&server).await;
    let hello = hello(&key);
    waiting.send(frame(HELLO, &hello)).await.unwrap();
    let ack = recv(waiting.as_mut())
        .await
        .expect("challenge issued before failures");

    for _ in 0..2 {
        assert_eq!(attempt(&server, &key, false).await, None);
    }
    waiting.send(proof(&key, &hello, &ack, true)).await.unwrap();
    assert!(
        recv(waiting.as_mut()).await.is_none(),
        "no Welcome after concurrent exhaustion"
    );
    assert!(server.shared.peers.pending().is_empty());
    waiting.close().await;
    server.shutdown().await;
}
