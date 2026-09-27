use super::*;
use rabbithole_federation::Catalog;
use rabbithole_net::{PeerInfo, TransportKind};
use rabbithole_server_core::ServerConfig;

struct Wire {
    sent: Vec<Frame>,
    peer: PeerInfo,
}

impl Wire {
    fn new() -> Self {
        Self {
            sent: Vec::new(),
            peer: PeerInfo {
                remote_addr: "127.0.0.1:12345".parse().unwrap(),
                transport: TransportKind::Quic,
            },
        }
    }
}

#[async_trait::async_trait]
impl Connection for Wire {
    async fn send(&mut self, frame: Frame) -> std::result::Result<(), NetError> {
        self.sent.push(frame);
        Ok(())
    }
    async fn recv(&mut self) -> std::result::Result<Option<Frame>, NetError> {
        Ok(None)
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
    async fn close(&mut self) {}
}

async fn server(path: &Path, key: &IdentityKey) -> crate::Burrow {
    let b = crate::Burrow::start(ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: path.to_owned(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    b.shared
        .peers
        .seed_approved(key.public().0, "Remote", Some("remote".into()));
    b
}

fn signed(key: &IdentityKey, generation: u64) -> SignedCatalog {
    Catalog::new(key.public().0, generation, None)
        .with_issued_at(1)
        .sign(key)
        .unwrap()
}

fn announcement(signed: &SignedCatalog) -> Frame {
    frame(
        FrameKind::Push,
        MT_ANNOUNCE,
        RequestId::PUSH,
        &CatalogAnnounceMsg {
            catalog_id: signed.catalog_id().unwrap(),
            generation: signed.catalog.generation,
        },
    )
}

#[test]
fn content_checks_are_bounded_and_do_not_catch_up_in_a_storm() {
    let now = Instant::now();
    let mut state = CatalogSync::new();
    assert!(!state.check_due(now));
    state.enabled = true;
    assert!(state.check_due(now));
    assert!(!state.check_due(now + CHECK_INTERVAL - Duration::from_nanos(1)));
    assert!(state.check_due(now + CHECK_INTERVAL));
    assert!(!state.check_due(now + CHECK_INTERVAL));
    assert!(state.check_due(now + CHECK_INTERVAL * 100));
    assert!(!state.check_due(now + CHECK_INTERVAL * 100));
}

#[tokio::test]
async fn capability_is_correlated_and_old_peers_remain_quiet() {
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[41; 32]);
    let b = server(work.path(), &key).await;
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    let now = Instant::now();
    state.probe(&mut wire, now).await.unwrap();
    let id = wire.sent[0].id;
    assert_ne!(id, RequestId::PUSH);
    let wrong = frame(
        FrameKind::Reply,
        MT_SUPPORT,
        RequestId(id.0 + 1),
        &CAPABILITY,
    );
    state
        .handle(&mut wire, &b.shared, &key.public().0, "remote", &wrong, now)
        .await
        .unwrap();
    assert!(!state.enabled);
    state
        .tick(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            now + REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
    let late = frame(FrameKind::Reply, MT_SUPPORT, id, &CAPABILITY);
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &late,
            now + REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(!state.enabled);
    assert_eq!(
        wire.sent.len(),
        1,
        "old listener gets one harmless probe, no catalog traffic"
    );

    // New listener remains quiet through the complete old dialer exchange.
    let mut edge = FloodEdge::new(key.public().0, "remote".into());
    let mut listener = Wire::new();
    let legacy = frame(
        FrameKind::Request,
        MT_CATALOG_ANNOUNCE,
        RequestId::PUSH,
        &CatalogAnnounceMsg {
            catalog_id: [0; 32],
            generation: 1,
        },
    );
    handle_peer_frame(&mut listener, &b.shared, &mut edge, &legacy, true)
        .await
        .unwrap();
    let get = fed_frame(FrameKind::Request, MT_CATALOG_GET, &CatalogGetMsg {});
    handle_peer_frame(&mut listener, &b.shared, &mut edge, &get, true)
        .await
        .unwrap();
    assert_eq!(
        listener
            .sent
            .iter()
            .map(|f| f.message_type)
            .collect::<Vec<_>>(),
        [MT_CATALOG_ANNOUNCE, MT_CATALOG]
    );

    let mut dialer = CatalogSync::new();
    dialer.probe(&mut wire, now).await.unwrap();
    let probe = wire.sent.last().unwrap().clone();
    let mut upgraded = CatalogSync::new();
    upgraded
        .handle(
            &mut listener,
            &b.shared,
            &key.public().0,
            "remote",
            &probe,
            now,
        )
        .await
        .unwrap();
    let ack = listener.sent.last().unwrap();
    assert_eq!(ack.id, probe.id);
    assert_eq!(ack.kind, FrameKind::Reply);
    dialer
        .handle(&mut wire, &b.shared, &key.public().0, "remote", ack, now)
        .await
        .unwrap();
    assert!(dialer.enabled && upgraded.enabled);
    b.shutdown().await;
}

#[tokio::test]
async fn one_pending_request_survives_wrong_ids_and_serves_reverse_requests() {
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[42; 32]);
    let b = server(work.path(), &key).await;
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    state.enabled = true;
    let now = Instant::now();
    let first = signed(&key, 1);
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &announcement(&first),
            now,
        )
        .await
        .unwrap();
    let id = state.pending.as_ref().unwrap().id;
    for n in 2..100 {
        state
            .handle(
                &mut wire,
                &b.shared,
                &key.public().0,
                "remote",
                &announcement(&signed(&key, n)),
                now,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        wire.sent.len(),
        1,
        "announcements cannot queue unbounded GETs"
    );
    let wrong = catalog_reply(RequestId(id.0 + 1), first.to_bytes()).unwrap();
    state
        .handle(&mut wire, &b.shared, &key.public().0, "remote", &wrong, now)
        .await
        .unwrap();
    let mut wrong_kind = catalog_reply(id, first.to_bytes()).unwrap();
    wrong_kind.kind = FrameKind::Push;
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &wrong_kind,
            now,
        )
        .await
        .unwrap();
    assert_eq!(state.pending.as_ref().unwrap().id, id);
    assert!(b.shared.catalogs.peer_catalog(&key.public().0).is_none());
    let mut forged = first.clone();
    forged.sig = IdentityKey::from_seed(&[250; 32]).sign(b"forged");
    let forged = catalog_reply(id, forged.to_bytes()).unwrap();
    assert!(state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &forged,
            now
        )
        .await
        .is_err());
    assert!(b.shared.catalogs.peer_catalog(&key.public().0).is_none());

    // An independent peer GET can share the numeric ID; kinds/types separate
    // the two directions and its reply must echo that ID.
    let reverse = frame(FrameKind::Request, MT_GET, id, &CatalogGetMsg {});
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &reverse,
            now,
        )
        .await
        .unwrap();
    assert_eq!(wire.sent.last().unwrap().id, id);
    assert_eq!(wire.sent.last().unwrap().message_type, MT_REPLY);
    assert_eq!(state.pending.as_ref().unwrap().id, id);
    let reply = catalog_reply(id, first.to_bytes()).unwrap();
    state
        .handle(&mut wire, &b.shared, &key.public().0, "remote", &reply, now)
        .await
        .unwrap();
    assert_eq!(
        b.shared
            .catalogs
            .peer_catalog(&key.public().0)
            .unwrap()
            .catalog
            .generation,
        1
    );
    assert!(state.pending.is_none());
    state
        .tick(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            now + CHECK_INTERVAL,
        )
        .await
        .unwrap();
    assert_eq!(state.pending.as_ref().unwrap().announced.generation, 99);
    b.shutdown().await;
}

#[tokio::test]
async fn timeout_retries_with_fresh_id_and_reapproval_rejects_inflight_reply() {
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[43; 32]);
    let b = server(work.path(), &key).await;
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    state.enabled = true;
    let now = Instant::now();
    let cat = signed(&key, 1);
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &announcement(&cat),
            now,
        )
        .await
        .unwrap();
    let old_id = state.pending.as_ref().unwrap().id;
    let late = catalog_reply(old_id, cat.to_bytes()).unwrap();
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &late,
            now + REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
    assert!(state.pending.is_none());
    state
        .tick(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            now + CHECK_INTERVAL,
        )
        .await
        .unwrap();
    let id = state.pending.as_ref().unwrap().id;
    assert_ne!(id, old_id);
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &late,
            now + CHECK_INTERVAL,
        )
        .await
        .unwrap();
    assert_eq!(state.pending.as_ref().unwrap().id, id);
    b.shared
        .catalogs
        .revoke_peer(&b.shared.peers, &key.public().0)
        .unwrap();
    assert!(state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &late,
            now + CHECK_INTERVAL
        )
        .await
        .is_err());
    b.shared
        .peers
        .seed_approved(key.public().0, "Remote", Some("remote".into()));
    let reply = catalog_reply(id, cat.to_bytes()).unwrap();
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &reply,
            now + CHECK_INTERVAL,
        )
        .await
        .unwrap();
    assert!(b.shared.catalogs.peer_catalog(&key.public().0).is_none());
    b.shutdown().await;
}

#[test]
fn payload_bounds_and_announced_generation_are_enforced() {
    assert!(catalog_reply(RequestId(1), vec![0; MAX_CATALOG + 1]).is_err());
    assert!(
        catalog_reply(RequestId(1), vec![0; MAX_CATALOG]).is_err(),
        "wire envelope also counts"
    );
    let key = IdentityKey::from_seed(&[44; 32]);
    let first = signed(&key, 1);
    let announced = CatalogAnnounceMsg {
        catalog_id: first.catalog_id().unwrap(),
        generation: 1,
    };
    assert!(verify_announced(&first.to_bytes(), &announced).is_ok());
    assert!(verify_announced(&signed(&key, 2).to_bytes(), &announced).is_ok());
    assert!(verify_announced(&signed(&key, 0).to_bytes(), &announced).is_err());
    assert!(verify_announced(
        &first.to_bytes(),
        &CatalogAnnounceMsg {
            catalog_id: [9; 32],
            ..announced
        }
    )
    .is_err());
    let mut oversize = frame(FrameKind::Push, MT_ANNOUNCE, RequestId::PUSH, &announced);
    oversize.payload.0.resize(MAX_CONTROL + 1, 0);
    assert!(decode_fed_bounded::<CatalogAnnounceMsg>(&oversize, MT_ANNOUNCE, MAX_CONTROL).is_err());
}

#[tokio::test]
async fn local_cache_failure_backs_off_without_stopping_board_dispatch() {
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[45; 32]);
    let b = server(work.path(), &key).await;
    let blocked = work.path().join("federation/peer_catalogs.tmp");
    std::fs::create_dir_all(&blocked).unwrap();
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    state.enabled = true;
    let now = Instant::now();
    let cat = signed(&key, 1);
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &announcement(&cat),
            now,
        )
        .await
        .unwrap();
    let id = state.pending.as_ref().unwrap().id;
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &catalog_reply(id, cat.to_bytes()).unwrap(),
            now,
        )
        .await
        .unwrap();
    assert!(state.pending.is_none());
    assert!(b.shared.catalogs.peer_catalog(&key.public().0).is_none());
    state
        .request_if_needed(
            &mut wire,
            &b.shared,
            &key.public().0,
            now + Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(
        wire.sent.len(),
        1,
        "failed writes cannot cause a tight fetch loop"
    );
    let mut edge = FloodEdge::new(key.public().0, "remote".into());
    let offered = fed_frame(
        FrameKind::Push,
        MT_IHAVE,
        &IHave {
            board: "shared".into(),
            event_ids: vec![[99; 32]],
        },
    );
    handle_peer_frame(&mut wire, &b.shared, &mut edge, &offered, true)
        .await
        .unwrap();
    assert_eq!(
        wire.sent.last().unwrap().message_type,
        MT_PULL,
        "board dispatcher still handles the next frame"
    );
    std::fs::remove_dir(blocked).unwrap();
    state
        .request_if_needed(&mut wire, &b.shared, &key.public().0, now + CHECK_INTERVAL)
        .await
        .unwrap();
    let id = state.pending.as_ref().unwrap().id;
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &catalog_reply(id, cat.to_bytes()).unwrap(),
            now + CHECK_INTERVAL,
        )
        .await
        .unwrap();
    assert!(b.shared.catalogs.peer_catalog(&key.public().0).is_some());
    b.shutdown().await;
}

#[tokio::test]
async fn local_build_failure_returns_correlated_unavailable_and_keeps_tick_alive() {
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[46; 32]);
    let b = server(work.path(), &key).await;
    b.shared.pool.close().await;
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    state.enabled = true;
    let now = Instant::now();
    state
        .tick(&mut wire, &b.shared, &key.public().0, "remote", now)
        .await
        .unwrap();
    assert!(wire.sent.is_empty());
    let get = frame(FrameKind::Request, MT_GET, RequestId(87), &CatalogGetMsg {});
    state
        .handle(&mut wire, &b.shared, &key.public().0, "remote", &get, now)
        .await
        .unwrap();
    let response = wire.sent.last().unwrap();
    assert_eq!(response.id, get.id);
    assert_eq!(response.kind, FrameKind::Reply);
    assert_eq!(
        response.error,
        Some(rabbithole_proto::ErrorCode::Unavailable)
    );
    assert!(response.payload.0.is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn cache_admission_limit_does_not_disconnect_or_queue_fetches() {
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[47; 32]);
    let b = server(work.path(), &key).await;
    // Fill the documented bounded watermark table with other approved peers.
    for n in 0u64..4096 {
        let mut other = [200; 32];
        other[..8].copy_from_slice(&n.to_le_bytes());
        b.shared
            .peers
            .seed_approved(other, "Other", Some(format!("peer-{n}")));
        b.shared
            .catalogs
            .begin_fetch(&b.shared.peers, other)
            .unwrap();
    }
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    state.enabled = true;
    let now = Instant::now();
    let announced = announcement(&signed(&key, 1));
    state
        .handle(
            &mut wire,
            &b.shared,
            &key.public().0,
            "remote",
            &announced,
            now,
        )
        .await
        .unwrap();
    assert!(wire.sent.is_empty());
    assert!(state.pending.is_none());
    assert_eq!(state.next_fetch, Some(now + CHECK_INTERVAL));
    let mut edge = FloodEdge::new(key.public().0, "remote".into());
    let offered = fed_frame(
        FrameKind::Push,
        MT_IHAVE,
        &IHave {
            board: "shared".into(),
            event_ids: vec![[99; 32]],
        },
    );
    handle_peer_frame(&mut wire, &b.shared, &mut edge, &offered, true)
        .await
        .unwrap();
    assert_eq!(wire.sent.last().unwrap().message_type, MT_PULL);
    b.shutdown().await;
}

#[tokio::test]
async fn revocation_during_local_build_prevents_the_announcement() {
    use std::future::Future;
    use std::task::Poll;
    let work = tempfile::tempdir().unwrap();
    let key = IdentityKey::from_seed(&[48; 32]);
    let b = server(work.path(), &key).await;
    // Holding all database connections stops the catalog's first query. Poll
    // once to prove the tick is inside its build await before revoking.
    let mut held = Vec::new();
    for _ in 0..b.shared.pool.options().get_max_connections() {
        held.push(b.shared.pool.acquire().await.unwrap());
    }
    let mut wire = Wire::new();
    let mut state = CatalogSync::new();
    state.enabled = true;
    let public = key.public().0;
    {
        let build = state.tick(&mut wire, &b.shared, &public, "remote", Instant::now());
        tokio::pin!(build);
        std::future::poll_fn(|cx| {
            assert!(build.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        b.shared
            .catalogs
            .revoke_peer(&b.shared.peers, &public)
            .unwrap();
        drop(held);
        assert!(build.await.is_err());
    }
    assert!(
        wire.sent.is_empty(),
        "no catalog identity is sent after withdrawal"
    );
    b.shutdown().await;
}
