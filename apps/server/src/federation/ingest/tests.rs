use super::*;

#[test]
fn weighted_budgets_isolate_peers_keep_reconnect_debt_and_apply_live_policy() {
    let state = IngestState::default();
    let mut config = ServerConfig {
        federation_ingest_frames_burst: 2,
        federation_ingest_frames_per_sec: 0,
        federation_ingest_bytes_burst: 10,
        federation_ingest_bytes_per_sec: 0,
        federation_ingest_events_burst: 2,
        federation_ingest_events_per_sec: 0,
        ..Default::default()
    };
    let a = [1; 32];
    let b = [2; 32];
    assert!(state.check(&config, &a, [1, 6, 1], 0).is_ok());
    assert!(state.check(&config, &a, [1, 6, 0], 0).is_err());
    assert!(state.check(&config, &b, [1, 6, 1], 0).is_ok());
    // A fresh admission/reconnected link allocates no new bucket or burst.
    assert!(state.check(&config, &a, [0; 3], 0).is_ok());
    assert!(state.check(&config, &a, [1, 0, 0], 0).is_err());
    config.federation_ingest_frames_burst = 20;
    config.federation_ingest_frames_per_sec = 1;
    assert!(
        state.check(&config, &a, [1, 0, 0], 1000).is_err(),
        "old zero rate applies until change observed"
    );
    assert!(state.check(&config, &a, [1, 0, 0], 2000).is_ok());
    config.federation_ingest_frames_burst = 0;
    assert!(state.check(&config, &b, [1, 0, 0], 2000).is_err());
    // Explicit deny neither mutates approval nor resets debt when removed.
    config.federation_denied_keys = vec![hex::encode(a)];
    assert!(state.check(&config, &a, [0; 3], 2000).is_err());
    assert!(state.check(&config, &b, [0; 3], 2000).is_ok());
    config.federation_denied_keys.clear();
    assert!(state.check(&config, &a, [0, 0, 2], 2000).is_err());
    assert!(state.check(&config, &a, [0, 0, 1], 2000).is_ok());
}

#[test]
fn unknown_or_denied_admission_does_not_allocate_buckets() {
    let state = IngestState::default();
    let config = ServerConfig {
        federation_denied_keys: vec![hex::encode([9; 32])],
        ..Default::default()
    };
    for value in 0..100 {
        let _ = state.check(&config, &[value; 32], [0; 3], 0);
    }
    let held = state.0.lock().unwrap();
    assert!(held
        .as_ref()
        .unwrap()
        .buckets
        .iter()
        .all(|bucket| bucket.tracked_peers() == 0));
}

#[test]
fn bounded_weighted_primitive_never_evicts_debt_and_clock_rollback_does_not_refill_twice() {
    let mut limiter = RateLimiter::bounded(10, 1.0, 2);
    assert!(limiter.try_acquire_many(&[1; 32], 10, 1000));
    assert!(!limiter.try_acquire_many(&[1; 32], 1, 0));
    assert!(!limiter.try_acquire_many(&[1; 32], 1, 1000));
    assert!(limiter.try_acquire_many(&[2; 32], 10, 1000));
    assert!(!limiter.try_acquire_many(&[3; 32], 1, 1000));
    assert_eq!(limiter.tracked_peers(), 2);
    assert!(limiter.try_acquire_many(&[3; 32], 1, 11000));
    assert!(limiter.tracked_peers() <= 2);
    assert!(!limiter.try_acquire_many(&[3; 32], 11, 11000));
}

#[tokio::test]
async fn event_budget_rejects_whole_batch_before_decode_or_board_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let b = crate::Burrow::start(ServerConfig {
        data_dir: dir.path().into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_ingest_events_burst: 1,
        federation_ingest_events_per_sec: 0,
        ..Default::default()
    })
    .await
    .unwrap();
    let key = [7; 32];
    b.shared
        .peers
        .seed_approved(key, "peer", Some("peer.example".into()));
    let mut edge = super::super::FloodEdge::new(key, "peer.example".into());
    let msg = super::super::EventsMsg {
        push: rabbithole_federation::PushEvents {
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
        },
        origin_keys: vec![[7; 32]; 2],
    };
    let frame = super::super::fed_frame(
        rabbithole_proto::FrameKind::Reply,
        super::super::MT_EVENTS,
        &msg,
    );
    let error = super::super::handle_events(&b.shared, &mut edge, &frame)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("events budget"));
    assert!(b.shared.fed_flood.resolve("peer.example").is_none());
    b.shutdown().await;
}

#[test]
fn invalid_programmatic_policy_fails_closed_without_resetting_debt() {
    let state = IngestState::default();
    let mut config = ServerConfig {
        federation_ingest_frames_burst: 1,
        federation_ingest_frames_per_sec: 0,
        ..Default::default()
    };
    let key = [7; 32];
    assert!(state.check(&config, &key, [1, 0, 0], 0).is_ok());
    config.federation_denied_keys = vec!["invalid".into()];
    assert!(state.check(&config, &key, [0; 3], 0).is_err());
    config.federation_denied_keys.clear();
    assert!(state.check(&config, &key, [1, 0, 0], 0).is_err());
}
