//! CI smoke: boot a real burrow in-process (same setup as
//! `apps/server/tests/e2e.rs`), then run the library-form scenario driver
//! with 50 guest chat sessions for ~5 seconds over WebSocket.
//!
//! The full 10k-session run is a documented real-hardware target (see the
//! crate README / module docs), not a CI job — this smoke keeps the whole
//! test well under 30 seconds.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::ServerConfig;
use tokio::sync::watch;
use warren_stampede::{run, AuthMode, RunConfig, Scenario};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_guest_chat_sessions_zero_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = ServerConfig {
        name: "Stampede Smoke Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.path().to_path_buf(),
        ..ServerConfig::default()
    };
    // 50 loopback sessions connecting/authing in ~1s would trip the per-IP
    // token buckets meant for real deployments; this is a load test.
    cfg.ratelimit_enabled = false;

    let burrow = Burrow::start(cfg).await.expect("burrow starts");
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());

    let mut rc = RunConfig::new(&url);
    rc.sessions = 50;
    rc.ramp_per_sec = 100.0;
    rc.duration = Duration::from_secs(5);
    rc.scenario = Scenario::Chat;
    rc.auth = AuthMode::Guest;
    // Tighter cadence than the 5-15s default so the short run still chats.
    rc.chat_interval = (Duration::from_secs(2), Duration::from_secs(4));
    rc.echo_timeout = Duration::from_secs(5);
    rc.max_reconnects = 0; // a drop in a 5s loopback run is a real failure

    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let outcome = run(rc, shutdown_rx).await;
    let r = &outcome.report;
    eprintln!("{}", r.render_text());

    assert!(r.aborted.is_none(), "aborted: {:?}", r.aborted);
    assert_eq!(r.sessions_started, 50);
    assert_eq!(r.sessions_logged_in, 50, "all sessions logged in");
    assert_eq!(r.sessions_completed, 50, "all sessions drained");
    assert_eq!(r.errors, 0, "zero errors");
    assert_eq!(r.disconnects, 0);
    assert_eq!(r.reconnects, 0);
    assert_eq!(r.echo_timeouts, 0);

    // Every session saw its own echo, and every send echoed back.
    assert_eq!(r.sessions_echoed, 50, "every session saw its own echo");
    assert!(r.msgs_sent >= 50, "each session sent at least one line");
    assert_eq!(r.msgs_sent, r.echoes_seen, "every send echoed");

    // Latency sanity on loopback.
    assert_eq!(r.connect.count, 50);
    assert!(
        r.connect.p95_ms < 2000.0,
        "p95 connect latency sane: {:?}",
        r.connect
    );

    burrow.shutdown().await;
}
