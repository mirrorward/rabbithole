//! The Looking Glass announce, end to end against a stand-in coordinator.
//!
//! The unit tests in `burrow::announce` pin the document and its signature.
//! What they can't show is that the burrow *sends* it: correct HTTP framing, a
//! `Content-Length` that matches the body, and a signature that still verifies
//! after the bytes have been through a socket.
//!
//! So this stands up a one-shot HTTP listener, points a burrow's tracker list
//! at it, and checks what actually arrives — validating it the way
//! `tracker.rabbit.direct` would: re-canonicalize the received descriptor and
//! verify the signature against the key the descriptor itself names.

use std::time::Duration;

use burrow::announce::{announce_url, canonical_json, signed_body};
use rabbithole_identity::{IdentityKey, PublicKey, Signature};
use rabbithole_server_core::config::{ServerConfig, DEFAULT_TRACKER};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Accept one connection, read the request, answer 200, return the raw request.
async fn one_shot_tracker(listener: TcpListener) -> String {
    let (mut sock, _) = listener.accept().await.expect("accept");
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = sock.read(&mut buf).await.expect("read");
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        // Stop once the body named by Content-Length has arrived; the client
        // keeps the socket open waiting for our response.
        let text = String::from_utf8_lossy(&raw).to_string();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if body.len() >= len {
                break;
            }
        }
    }
    sock.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"ok\":true,\"a\":1}",
    )
    .await
    .ok();
    let _ = sock.shutdown().await;
    String::from_utf8_lossy(&raw).to_string()
}

fn advertised_cfg(tracker: &str) -> ServerConfig {
    ServerConfig {
        name: "Wonderland".into(),
        advertise_host: "wonderland.example".into(),
        announce_sysop: "alice".into(),
        announce_description: "Down the rabbit hole.".into(),
        announce_trackers: vec![tracker.to_string()],
        quic_addr: "0.0.0.0:4653".parse().unwrap(),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn the_burrow_posts_an_announce_a_coordinator_would_accept() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(one_shot_tracker(listener));

    let key = IdentityKey::from_seed(&[9u8; 32]);
    let cfg = advertised_cfg(&format!("http://127.0.0.1:{port}"));
    let body = signed_body(&cfg, &key, 1_700_000_000_000).expect("a body to send");
    let url = announce_url(&cfg.announce_trackers[0]).expect("a url");
    assert_eq!(url, format!("http://127.0.0.1:{port}/api/announce"));

    // Drive the same POST path the announce loop uses.
    let (_status, raw) = tokio::time::timeout(Duration::from_secs(10), async {
        let posted = burrow::announce::post_announce_for_test(&url, &body).await;
        let raw = server.await.expect("listener");
        (posted, raw)
    })
    .await
    .expect("the exchange completes");

    let (head, wire_body) = raw.split_once("\r\n\r\n").expect("a framed request");
    assert!(
        head.starts_with(&format!(
            "POST /api/announce HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
        )),
        "method, path and Host are right:\n{head}"
    );
    assert!(
        head.contains("Content-Type: application/json\r\n"),
        "the coordinator parses JSON:\n{head}"
    );
    assert!(
        head.contains(&format!("Content-Length: {}\r\n", wire_body.len())),
        "the declared length matches the body actually sent:\n{head}"
    );

    // Now validate exactly as the coordinator does, from the received bytes.
    let received: Value = serde_json::from_str(wire_body).expect("valid JSON arrived");
    let descriptor = &received["descriptor"];
    let sig_hex = received["signature"].as_str().expect("a signature");

    let announced_key = descriptor["publicKey"].as_str().expect("a key");
    let vk = PublicKey(hex::decode(announced_key).unwrap().try_into().unwrap());
    let sig = Signature(hex::decode(sig_hex).unwrap().try_into().unwrap());
    assert!(
        vk.verify(canonical_json(descriptor).as_bytes(), &sig),
        "the signature verifies after a round trip through the socket"
    );

    assert_eq!(descriptor["name"], "alice@wonderland.example");
    assert_eq!(descriptor["description"], "Down the rabbit hole.");
    assert_eq!(
        descriptor["endpoints"]["quic"],
        "quic://wonderland.example:4653"
    );
    assert_eq!(descriptor["timestamp"], 1_700_000_000_000i64);
    assert_eq!(descriptor["ttl"], 120);
}

#[tokio::test]
async fn a_burrow_that_opted_out_sends_nothing_at_all() {
    // Not "sends an announce marked private" — sends nothing. The opt-out has
    // to hold even if a coordinator ignores flags it doesn't understand.
    let mut cfg = advertised_cfg(DEFAULT_TRACKER);
    cfg.announce_enabled = false;
    assert!(
        signed_body(&cfg, &IdentityKey::from_seed(&[9u8; 32]), 1).is_none(),
        "there is no body to POST"
    );
}

/// Reach the real `tracker.rabbit.direct` over TLS and confirm it *rejects* a
/// tampered announce.
///
/// Deliberately a rejection: a valid announce would publish a listing for a
/// burrow that doesn't exist onto a public directory. A 4xx here still proves
/// the whole client path — DNS, TLS, request framing, endpoint, JSON shape —
/// because the coordinator had to parse the document to refuse it.
///
/// Ignored by default: it needs the network, and CI shouldn't depend on a
/// third-party service being up. Run it with
/// `cargo test -p burrow --test e2e_announce -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires network access to tracker.rabbit.direct"]
async fn the_real_coordinator_refuses_a_tampered_announce() {
    let key = IdentityKey::from_seed(&[3u8; 32]);
    let mut cfg = advertised_cfg(DEFAULT_TRACKER);
    cfg.advertise_host = "example.invalid".into();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let mut body: Value = serde_json::from_str(&signed_body(&cfg, &key, now).unwrap()).unwrap();
    // Flip one signature byte. Everything else stays well-formed, so the
    // coordinator has to get all the way to signature verification to say no.
    let mut sig = hex::decode(body["signature"].as_str().unwrap()).unwrap();
    sig[0] ^= 0xff;
    body["signature"] = Value::String(hex::encode(sig));

    let url = announce_url(DEFAULT_TRACKER).unwrap();
    let result =
        burrow::announce::post_announce_for_test(&url, &serde_json::to_string(&body).unwrap())
            .await;

    let err = result.expect_err("a tampered signature must not be accepted");
    let text = err.to_string();
    println!("tracker said: {text}");
    assert!(
        text.contains(" 400") || text.contains(" 401") || text.contains(" 403"),
        "expected a validation refusal, got: {text}"
    );
}

#[test]
fn the_standard_coordinator_is_the_default() {
    let cfg = ServerConfig::default();
    assert!(
        cfg.announce_enabled,
        "a burrow nobody can find is a burrow nobody joins"
    );
    assert_eq!(cfg.announce_trackers, vec![DEFAULT_TRACKER.to_string()]);
    assert_eq!(
        announce_url(DEFAULT_TRACKER).unwrap(),
        "https://tracker.rabbit.direct/api/announce"
    );
    assert!(
        signed_body(&cfg, &IdentityKey::from_seed(&[1u8; 32]), 1).is_none(),
        "but inert until advertise_host names somewhere reachable"
    );
}
