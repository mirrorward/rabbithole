//! Wave 9 end-to-end tests: **board-event flood-fill over the S2S federation
//! transport**. Where `e2e_w9_catalog.rs` / `e2e_w9_testnet.rs` prove signed
//! *file catalogs* sync one hop (dialer-pull), this file proves signed *board
//! posts* gossip across the mesh: a subscription-driven `ihave → pull →
//! events` exchange that relays events **unchanged**, verifying the origin
//! signature on ingest and re-flooding to the next hop.
//!
//! It proves, over the real QUIC federation endpoint:
//!
//! - **multi-hop relay**: on an `A ← B ← C` chain (A subscribes to B, B to C),
//!   a post authored on C is verifiably ingested on A *through* B — the origin
//!   signature is intact end-to-end, and A never peered with C;
//! - **signature-verified ingestion**: the ingested event verifies under C's
//!   origin key and *fails* under any other key (no forgery slips through);
//! - **loop-safety / no duplication**: under a full A/B/C mesh with everyone
//!   subscribed to everyone, a single post lands exactly once on every burrow
//!   and a re-flood storm never duplicates it — the run simply terminates,
//!   which a relay loop could not.
//!
//! Determinism is by real readiness signals: the harness subscribes to the
//! destination burrow's event bus and awaits the `BoardPost` its ingest
//! re-fires (bounded by a timeout), so there are no blind sleeps — the
//! subscribe → offer → pull → deliver sequence is driven to completion by the
//! flood itself.

use std::time::Duration;

use burrow::federation::{dial_peer, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_identity::keys::{IdentityKey, Signature};
use rabbithole_server_core::events::SignedEvent;
use rabbithole_server_core::{PeerState, ServerConfig, ServerEvent};
use serde_json::json;
use tokio::sync::broadcast;

/// A burrow with federation on and an interest in the shared board.
fn fed_config(name: &str, dir: &std::path::Path, subscribe: &[&str]) -> ServerConfig {
    ServerConfig {
        name: name.into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        federation_board_subscribe: subscribe.iter().map(|s| s.to_string()).collect(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn start(name: &str, dir: &std::path::Path, subscribe: &[&str]) -> Burrow {
    let b = Burrow::start(fed_config(name, dir, subscribe))
        .await
        .unwrap();
    // Every participant hosts the same board so ingest has somewhere to land.
    b.shared
        .boards
        .create_board("rabbit", "Rabbit", "", 0, None, 0)
        .await
        .unwrap();
    b.shared
        .boards
        .create_board("rabbit.general", "General", "", 2, Some("rabbit"), 0)
        .await
        .unwrap();
    b
}

fn target_for(b: &Burrow) -> DialTarget {
    DialTarget {
        addr: b.federation_addr.expect("federation enabled").to_string(),
        server_name: "localhost".into(),
        fingerprint: b.fingerprint,
        expected_key: Some(b.shared.server_key),
    }
}

async fn approve(on: &Burrow, key: [u8; 32]) {
    let resp = burrow::ctl::handle(
        &on.shared,
        &json!({"cmd": "peer-approve", "key": hex::encode(key)}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "peer-approve accepted: {resp}");
}

async fn connect(dialer: &Burrow, listener: &Burrow) {
    let outcome = dial_peer(dialer.shared.clone(), target_for(listener))
        .await
        .unwrap();
    assert_eq!(outcome, DialOutcome::Connected(listener.shared.server_key));
}

/// Author a post on `origin` and fire its `BoardPost` on the bus (mirroring the
/// live-post path in `handlers6`), returning the new event id.
async fn post_and_announce(origin: &Burrow, author: &str, subject: &str, body: &str) -> [u8; 32] {
    let seed = blake3::hash(author.as_bytes());
    let row = origin
        .shared
        .boards
        .post(
            "rabbit.general",
            None,
            author,
            seed.as_bytes(),
            subject,
            body,
            "text/plain",
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    origin.shared.bus.publish(ServerEvent::BoardPost {
        board: row.board_slug.clone(),
        id: row.event_id,
        root: row.root_id,
    });
    row.event_id
}

/// Wait until `dest` has ingested the post `id` (its ingest re-fires
/// `BoardPost`), bounded — returns `false` on timeout.
async fn wait_ingested(
    mut rx: broadcast::Receiver<ServerEvent>,
    dest: &Burrow,
    id: [u8; 32],
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if dest.shared.boards.post_by_id(&id).await.unwrap().is_some() {
            return true;
        }
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(ServerEvent::BoardPost { id: got, .. })) if got == id => return true,
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => return false,
            Err(_) => return dest.shared.boards.post_by_id(&id).await.unwrap().is_some(),
        }
    }
}

/// The stored signed event for `id` on `b`, or `None`.
async fn stored_event(b: &Burrow, id: &[u8; 32]) -> Option<SignedEvent> {
    let row = b.shared.boards.post_by_id(id).await.unwrap()?;
    postcard::from_bytes(&row.event_blob).ok()
}

/// A–B–C chain: a post authored on C floods through B to A, origin signature
/// intact, with no direct A↔C relationship.
#[tokio::test]
async fn multi_hop_relay_through_b() {
    let work = tempfile::tempdir().unwrap();
    let a = start("Warren A", &work.path().join("a"), &["rabbit.general"]).await;
    let b = start("Warren B", &work.path().join("b"), &["rabbit.general"]).await;
    let c = start("Warren C", &work.path().join("c"), &["rabbit.general"]).await;
    let (a_key, b_key, c_key) = (
        a.shared.server_key,
        b.shared.server_key,
        c.shared.server_key,
    );

    // Chain edges only: A↔B and B↔C. A and C never meet.
    approve(&b, a_key).await;
    approve(&a, b_key).await;
    approve(&b, c_key).await;
    approve(&c, b_key).await;
    connect(&a, &b).await;
    connect(&b, &c).await;
    assert_eq!(a.shared.peers.state(&b_key), Some(PeerState::Connected));
    assert_eq!(b.shared.peers.state(&c_key), Some(PeerState::Connected));
    assert_eq!(a.shared.peers.state(&c_key), None, "A never peered with C");

    // Subscribe to A's bus *before* posting so we can't miss the re-fire.
    let rx_a = a.shared.bus.subscribe();
    let id = post_and_announce(&c, "cottontail@warren-c", "Down the hole", "wake up").await;

    assert!(
        wait_ingested(rx_a, &a, id).await,
        "C's post must reach A through B via flood-fill"
    );

    // Origin signature intact end-to-end: verifies under C's key, and only C's.
    let ev = stored_event(&a, &id).await.expect("A stored the event");
    assert!(
        ev.verify(&c_key).is_ok(),
        "A's copy verifies under C's origin key"
    );
    assert!(
        ev.verify(&a_key).is_err() && ev.verify(&b_key).is_err(),
        "the origin signature is C's — not the relayer's, not A's"
    );
    assert_eq!(ev.origin, "warren-c", "origin attribution preserved");
    assert!(
        ev.author.starts_with("cottontail@"),
        "origin author preserved, not re-signed as local"
    );

    // A pinned C's origin key by learning it from the relayed event.
    assert_eq!(a.shared.fed_flood.resolve("warren-c"), Some(c_key));

    // Exactly one copy — the flood did not duplicate it on A or B.
    assert_eq!(a.shared.boards.thread(&id, 100).await.unwrap().len(), 1);
    assert_eq!(b.shared.boards.thread(&id, 100).await.unwrap().len(), 1);

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// Full A/B/C mesh, everyone subscribed to everything: a single post lands
/// exactly once everywhere, and a re-flood storm can't duplicate it or loop.
#[tokio::test]
async fn full_mesh_is_loop_safe() {
    let work = tempfile::tempdir().unwrap();
    let a = start("Warren A", &work.path().join("a"), &["all"]).await;
    let b = start("Warren B", &work.path().join("b"), &["all"]).await;
    let c = start("Warren C", &work.path().join("c"), &["all"]).await;
    let (a_key, b_key, c_key) = (
        a.shared.server_key,
        b.shared.server_key,
        c.shared.server_key,
    );

    for (on, keys) in [
        (&a, [b_key, c_key]),
        (&b, [a_key, c_key]),
        (&c, [a_key, b_key]),
    ] {
        for key in keys {
            approve(on, key).await;
        }
    }
    // All six directed edges live (a live session each way per pair).
    for (dialer, listener) in [(&a, &b), (&b, &c), (&c, &a), (&b, &a), (&c, &b), (&a, &c)] {
        connect(dialer, listener).await;
    }

    // Post on B; watch it converge on both A and C.
    let rx_a = a.shared.bus.subscribe();
    let rx_c = c.shared.bus.subscribe();
    let id = post_and_announce(&b, "thumper@warren-b", "Mesh", "hello all").await;

    assert!(
        wait_ingested(rx_a, &a, id).await,
        "A received the mesh post"
    );
    assert!(
        wait_ingested(rx_c, &c, id).await,
        "C received the mesh post"
    );

    // Re-flood storm: replay the announcement several times across the mesh.
    // Every id is already stored/deduped, so no burrow pulls or re-projects,
    // and the loop-safe seen-sets turn each re-offer into a no-op.
    for _ in 0..5 {
        for src in [&a, &b, &c] {
            src.shared.bus.publish(ServerEvent::BoardPost {
                board: "rabbit.general".into(),
                id,
                root: Some(id),
            });
        }
    }

    // Barrier: flood a *second*, distinct post and wait for it to arrive.
    // Because each burrow's bus delivers to its session tasks in FIFO order,
    // once the second post has floored in, the storm frames published before
    // it have all been drained — so any duplication they could cause has
    // already happened (or, as asserted, has not).
    let rx_a2 = a.shared.bus.subscribe();
    let rx_c2 = c.shared.bus.subscribe();
    let id2 = post_and_announce(&b, "thumper@warren-b", "Mesh 2", "still here").await;
    assert!(
        wait_ingested(rx_a2, &a, id2).await,
        "A received the second post"
    );
    assert!(
        wait_ingested(rx_c2, &c, id2).await,
        "C received the second post"
    );

    for (name, srv) in [("A", &a), ("B", &b), ("C", &c)] {
        assert_eq!(
            srv.shared.boards.thread(&id, 100).await.unwrap().len(),
            1,
            "{name} holds exactly one copy of the first post after the storm"
        );
        assert_eq!(
            srv.shared.boards.thread(&id2, 100).await.unwrap().len(),
            1,
            "{name} holds exactly one copy of the second post"
        );
        assert!(
            stored_event(srv, &id)
                .await
                .expect("stored")
                .verify(&b_key)
                .is_ok(),
            "{name}'s copy still verifies under B's origin key"
        );
    }

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// A locally-minted event and a wrong-key check, in-process — a fast guard on
/// the verification the ingest path performs, independent of the transport.
#[test]
fn signed_event_verification_is_key_bound() {
    let author = IdentityKey::generate();
    let origin = IdentityKey::generate();
    let ev = rabbithole_server_core::events::mint(
        "alice@home",
        &author,
        "home",
        &origin,
        1_000,
        rabbithole_server_core::events::EventBody::Post {
            board: "rabbit.general".into(),
            root: None,
            parent: None,
            subject: "s".into(),
            body: "b".into(),
            mime: "text/plain".into(),
        },
    );
    // The blob a peer would carry verifies only under the true origin key.
    let blob = postcard::to_allocvec(&ev).unwrap();
    let back: SignedEvent = postcard::from_bytes(&blob).unwrap();
    assert!(back.verify(&origin.public().0).is_ok());
    assert!(back.verify(&IdentityKey::generate().public().0).is_err());

    // A tampered signature is caught (defends the ingest verify()).
    let mut forged = back.clone();
    forged.origin_sig = Signature([0u8; 64]).0.to_vec();
    assert!(forged.verify(&origin.public().0).is_err());
}
