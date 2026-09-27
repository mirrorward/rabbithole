//! RH-66: controlled missed live broadcasts recover over the original QUIC
//! session. No reconnect, new subscription, or navigation triggers the repair.
use burrow::{
    federation::{dial_peer, DialOutcome, DialTarget},
    Burrow,
};
use rabbithole_server_core::{
    events::{EventBody, SignedEvent},
    PeerState, ServerConfig, ServerEvent,
};
use rabbithole_store_server::repo4::PostRow;
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::broadcast;

async fn server(path: &std::path::Path, origin: &str) -> Burrow {
    let b = Burrow::start(ServerConfig {
        name: origin.into(),
        federation_origin: origin.into(),
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        federation_board_subscribe: vec!["shared".into()],
        federation_history_reoffer_secs: 5,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for board in ["shared", "quiet"] {
        b.shared
            .boards
            .create_board(board, board, "", 2, None, 0)
            .await
            .unwrap();
    }
    b
}
async fn post(b: &Burrow, board: &str, subject: &str, at: i64) -> PostRow {
    b.shared
        .boards
        .post(
            board,
            None,
            &format!("author@{}", b.shared.origin_name()),
            &[7; 32],
            subject,
            "original",
            "text/plain",
            at,
        )
        .await
        .unwrap()
}
fn publish(b: &Burrow, row: &PostRow) {
    b.shared.bus.publish(ServerEvent::BoardPost {
        board: row.board_slug.clone(),
        id: row.event_id,
        root: row.root_id,
    });
}
async fn wait_posts<const N: usize>(b: &Burrow, ids: [[u8; 32]; N]) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let mut complete = true;
            for id in &ids {
                complete &= b.shared.boards.post_by_id(id).await.unwrap().is_some();
            }
            if complete {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("bounded recovery on the live link");
}
async fn wait_followup(b: &Burrow, id: &[u8; 32]) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(f) = b.shared.boards.followup_by_id(id).await.unwrap() {
                if f.applied {
                    if let Some(post) = b.shared.boards.post_by_id(&f.target_id).await.unwrap() {
                        let signed: SignedEvent = postcard::from_bytes(&f.event_blob).unwrap();
                        let applied = match signed.body {
                            EventBody::Edit { body, subject, .. } => {
                                post.body == body && post.subject == subject
                            }
                            EventBody::Tombstone { .. } => post.tombstoned,
                            _ => false,
                        };
                        if applied {
                            return;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("stored edit/tombstone was periodically offered");
}
async fn wait_pushes<const N: usize>(
    rx: &mut broadcast::Receiver<ServerEvent>,
    expected: [[u8; 32]; N],
) -> HashMap<[u8; 32], usize> {
    let mut counts = HashMap::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !expected.iter().all(|id| counts.contains_key(id)) {
            match rx
                .recv()
                .await
                .expect("fixture event bus must retain evidence")
            {
                ServerEvent::BoardPost { id, .. } | ServerEvent::BoardEvent { id, .. } => {
                    *counts.entry(id).or_default() += 1;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("durable delivery also broadcasts exactly once");
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::BoardPost { id, .. } | ServerEvent::BoardEvent { id, .. } = event {
            *counts.entry(id).or_default() += 1;
        }
    }
    counts
}

#[tokio::test]
async fn missed_posts_edits_and_tombstones_recover_without_redial_or_duplicate_broadcasts() {
    let work = tempfile::tempdir().unwrap();
    let a = server(&work.path().join("a"), "alpha").await;
    let b = server(&work.path().join("b"), "beta").await;
    for (on, peer) in [(&a, &b), (&b, &a)] {
        let result = burrow::ctl::handle(&on.shared, &json!({"cmd": "peer-approve", "key": hex::encode(peer.shared.server_key), "origin": peer.shared.origin_name()})).await;
        assert_eq!(result["ok"], true, "{result}");
    }
    let connected = dial_peer(
        a.shared.clone(),
        DialTarget {
            addr: b.federation_addr.unwrap().to_string(),
            server_name: "localhost".into(),
            fingerprint: b.fingerprint,
            expected_key: Some(b.shared.server_key),
            expected_origin: b.shared.origin_name(),
        },
    )
    .await
    .unwrap();
    assert_eq!(connected, DialOutcome::Connected(b.shared.server_key));
    let a_original = post(&a, "shared", "will edit", 1).await;
    let b_original = post(&b, "shared", "will retract", 1).await;
    publish(&a, &a_original);
    publish(&b, &b_original);
    tokio::join!(
        wait_posts(&a, [b_original.event_id]),
        wait_posts(&b, [a_original.event_id])
    );
    // The handshake verdict precedes the spawned session task. Exchanging
    // the baseline posts proves both links and subscriptions are active.
    let original_a = a.shared.s2s.link(&b.shared.server_key).unwrap();
    let original_b = b.shared.s2s.link(&a.shared.server_key).unwrap();
    let mut a_events = a.shared.bus.subscribe();
    let mut b_events = b.shared.bus.subscribe();

    // Mint real durable events without publishing their live notifications.
    // This models an application-level missed broadcast, not QUIC packet loss.
    let a_missed = post(&a, "shared", "missed alpha", 2).await;
    let b_missed = post(&b, "shared", "missed beta", 2).await;
    let quiet = post(&a, "quiet", "never subscribed", 2).await;
    let (_, edit) = a
        .shared
        .boards
        .edit(
            a_original.event_id,
            "author@alpha",
            &[7; 32],
            "edited",
            "recovered edit",
            "text/plain",
            3,
        )
        .await
        .unwrap();
    let tombstone = b
        .shared
        .boards
        .tombstone(b_original.event_id, "author@beta", &[7; 32], 3)
        .await
        .unwrap();
    tokio::join!(
        wait_posts(&a, [b_missed.event_id]),
        wait_posts(&b, [a_missed.event_id]),
        wait_followup(&b, &edit),
        wait_followup(&a, &tombstone)
    );
    assert_eq!(
        b.shared
            .boards
            .post_by_id(&a_original.event_id)
            .await
            .unwrap()
            .unwrap()
            .body,
        "recovered edit"
    );
    assert!(
        a.shared
            .boards
            .post_by_id(&b_original.event_id)
            .await
            .unwrap()
            .unwrap()
            .tombstoned
    );
    for (on, origin, row) in [(&a, &b, &b_missed), (&b, &a, &a_missed)] {
        let stored = on
            .shared
            .boards
            .post_by_id(&row.event_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.event_blob, row.event_blob,
            "original signed bytes preserved"
        );
        let signed: SignedEvent = postcard::from_bytes(&stored.event_blob).unwrap();
        signed.verify(&origin.shared.server_key).unwrap();
    }
    for (on, origin, id) in [(&a, &b, tombstone), (&b, &a, edit)] {
        let stored = on.shared.boards.followup_by_id(&id).await.unwrap().unwrap();
        let source = origin
            .shared
            .boards
            .followup_by_id(&id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.event_blob, source.event_blob);
        let signed: SignedEvent = postcard::from_bytes(&stored.event_blob).unwrap();
        signed.verify(&origin.shared.server_key).unwrap();
    }
    // A second missed event in each direction forces another recovery pass;
    // all earlier IDs are offered again but do not produce duplicate pushes.
    let a_next = post(&a, "shared", "next alpha", 4).await;
    let b_next = post(&b, "shared", "next beta", 4).await;
    tokio::join!(
        wait_posts(&a, [b_next.event_id]),
        wait_posts(&b, [a_next.event_id])
    );
    let (a_counts, b_counts) = tokio::join!(
        wait_pushes(
            &mut a_events,
            [b_missed.event_id, tombstone, b_next.event_id]
        ),
        wait_pushes(&mut b_events, [a_missed.event_id, edit, a_next.event_id])
    );
    for id in [b_missed.event_id, tombstone, b_next.event_id] {
        assert_eq!(a_counts.get(&id), Some(&1));
    }
    for id in [a_missed.event_id, edit, a_next.event_id] {
        assert_eq!(b_counts.get(&id), Some(&1));
    }
    assert!(b
        .shared
        .boards
        .post_by_id(&quiet.event_id)
        .await
        .unwrap()
        .is_none());
    assert!(Arc::ptr_eq(
        &original_a,
        &a.shared.s2s.link(&b.shared.server_key).unwrap()
    ));
    assert!(Arc::ptr_eq(
        &original_b,
        &b.shared.s2s.link(&a.shared.server_key).unwrap()
    ));
    assert_eq!(
        a.shared.peers.state(&b.shared.server_key),
        Some(PeerState::Connected)
    );
    assert_eq!(
        b.shared.peers.state(&a.shared.server_key),
        Some(PeerState::Connected)
    );
    a.shutdown().await;
    b.shutdown().await;
}
