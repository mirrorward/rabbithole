//! Wave 10 end-to-end tests: live gateway/feed statistics over the admin
//! wire + `burrow ctl gateway-stats`. Real activity (a QWK packet build)
//! bumps a real counter; the admin snapshot reflects it, a non-admin is
//! refused, and the ctl surface returns the same numbers as JSON.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{GatewayStatsReply, GatewayStatsRequest};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use serde_json::json;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Stats Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        qwk_enabled: true,
        ..ServerConfig::default()
    }
}

async fn start(dir: &std::path::Path) -> Burrow {
    let burrow = Burrow::start(test_config(dir)).await.unwrap();
    for (login, role) in [("root", Role::Admin), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    // A postable board + one post so a QWK build produces a real packet.
    let b = &burrow.shared.boards;
    b.create_board("alpha", "Alpha", "", 2, None, 0)
        .await
        .unwrap();
    b.post(
        "alpha",
        None,
        "seeder@stats",
        &[7u8; 32],
        "hello",
        "body",
        "text/plain",
        1,
    )
    .await
    .unwrap();
    burrow
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn counter(reply: &GatewayStatsReply, gateway: &str, name: &str) -> Option<u64> {
    reply
        .gateways
        .iter()
        .find(|g| g.name == gateway)?
        .counters
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| *v)
}

#[tokio::test]
async fn gateway_stats_reflect_real_activity_and_gate_non_admins() {
    let work = tempfile::tempdir().unwrap();
    let burrow = start(&work.path().join("srv")).await;

    // A real QWK packet build bumps `qwk.packets_built`.
    let built = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "root"}),
    )
    .await;
    assert_eq!(built.get("error"), None, "qwk-build failed: {built}");

    // Admin native request sees the counter and the enabled flags.
    let mut root = login(&burrow, "root").await;
    let reply: GatewayStatsReply = root.request(&GatewayStatsRequest).await.unwrap();
    assert!(reply.generated_at_ms > 0);
    assert_eq!(
        counter(&reply, "qwk", "packets_built"),
        Some(1),
        "qwk.packets_built should be 1 after one build: {reply:?}"
    );
    // qwk is enabled in this config; syndication is present but off.
    assert!(reply.gateways.iter().any(|g| g.name == "qwk" && g.enabled));
    assert!(reply
        .gateways
        .iter()
        .any(|g| g.name == "syndication" && !g.enabled));

    // A second build advances the pointer so no packet is produced, but the
    // build path still ran — the counter climbs to 2 (activity meter, not a
    // per-message count).
    let _ = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "root"}),
    )
    .await;
    let reply2: GatewayStatsReply = root.request(&GatewayStatsRequest).await.unwrap();
    assert_eq!(counter(&reply2, "qwk", "packets_built"), Some(2));

    // Non-admin is refused.
    let mut alice = login(&burrow, "alice").await;
    assert!(matches!(
        alice
            .request::<_, GatewayStatsReply>(&GatewayStatsRequest)
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn ctl_gateway_stats_returns_json() {
    let work = tempfile::tempdir().unwrap();
    let burrow = start(&work.path().join("srv")).await;

    let _ = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "qwk-build", "login": "root"}),
    )
    .await;

    let out = burrow::ctl::handle(&burrow.shared, &json!({"cmd": "gateway-stats"})).await;
    assert_eq!(out["ok"], json!(true), "{out}");
    let data = &out["data"];
    assert!(data.get("generated_at_ms").is_some(), "{out}");
    let gateways = data.get("gateways").and_then(|g| g.as_array()).unwrap();
    let qwk = gateways
        .iter()
        .find(|g| g.get("name").and_then(|n| n.as_str()) == Some("qwk"))
        .expect("qwk gateway present");
    assert_eq!(qwk["enabled"], json!(true));
    assert_eq!(qwk["counters"]["packets_built"], json!(1));

    burrow.shutdown().await;
}
