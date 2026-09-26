//! Safe, structured configured-feed monitoring through real authenticated
//! native requests. Polling may be off: configuration still has rows.
use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    FeedMappingsReply, FeedMappingsRequest, GatewayStatsReply, GatewayStatsRequest,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};

const FIRST: &str = "https://first:secret-one@example.test/feed?token=token-one#fragment-one";
const SECOND: &str = "https://second:secret-two@example.test/feed?token=token-two#fragment-two";
const NEVER: &str = "https://example.test/pending?token=pending-secret";
const INVALID: &str = "invalid:malformed-secret";

async fn login(server: &Burrow, name: &str) -> Client {
    let mut client = Client::connect(
        &format!("ws://127.0.0.1:{}", server.ws_addr.port()),
        None,
        None,
        "test",
        "0",
    )
    .await
    .unwrap();
    client
        .auth_password(name, "fixture-password")
        .await
        .unwrap();
    client.expect_welcome().await.unwrap();
    client
}

#[tokio::test]
async fn mappings_are_gated_joined_before_redaction_and_include_unpolled_feeds() {
    let tmp = tempfile::tempdir().unwrap();
    let server = Burrow::start(ServerConfig {
        data_dir: tmp.path().into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        syndication_enabled: false,
        syndication_feeds: [
            (FIRST.into(), "alpha".into()),
            (SECOND.into(), "beta".into()),
            (NEVER.into(), "pending".into()),
            (INVALID.into(), "invalid".into()),
        ]
        .into(),
        ..Default::default()
    })
    .await
    .unwrap();
    for (name, role) in [("operator", Role::Admin), ("member", Role::User)] {
        server
            .shared
            .auth
            .create_account(name, "fixture-password", role)
            .await
            .unwrap();
    }
    // Controlled counters isolate identity/join correctness from HTTP timing.
    server.shared.stats.feed_poll(FIRST, 100, "ok");
    server.shared.stats.feed_ingest(FIRST, 5, 3, 2);
    server.shared.stats.feed_poll(SECOND, 200, "error");
    server.shared.stats.feed_ingest(SECOND, 9, 7, 2);
    let mut operator = login(&server, "operator").await;
    let reply: FeedMappingsReply = operator.request(&FeedMappingsRequest).await.unwrap();
    assert_eq!(reply.feeds.len(), 4);
    let alpha = reply.feeds.iter().find(|r| r.board == "alpha").unwrap();
    let beta = reply.feeds.iter().find(|r| r.board == "beta").unwrap();
    assert_eq!(alpha.url, "https://example.test/feed");
    assert_eq!(alpha.url, beta.url);
    assert_ne!(alpha.id, beta.id);
    assert_eq!((alpha.stats.items_posted, beta.stats.items_posted), (3, 7));
    assert_eq!(
        (alpha.stats.last_poll_ms, beta.stats.last_poll_ms),
        (100, 200)
    );
    for board in ["pending", "invalid"] {
        let row = reply.feeds.iter().find(|r| r.board == board).unwrap();
        assert_eq!(row.stats, Default::default());
    }
    assert_eq!(
        reply
            .feeds
            .iter()
            .find(|r| r.board == "invalid")
            .unwrap()
            .url,
        "(invalid feed URL)"
    );
    let again: FeedMappingsReply = operator.request(&FeedMappingsRequest).await.unwrap();
    assert_eq!(reply.feeds, again.feeds);
    let legacy: GatewayStatsReply = operator.request(&GatewayStatsRequest).await.unwrap();
    let ctl = burrow::ctl::handle(&server.shared, &json!({"cmd": "gateway-stats"})).await;
    assert_eq!(ctl["ok"], true);
    let exposed = format!("{reply:?} {legacy:?} {ctl}");
    for secret in [
        "first:",
        "second:",
        "secret-one",
        "secret-two",
        "token-one",
        "token-two",
        "fragment-one",
        "pending-secret",
        "malformed-secret",
    ] {
        assert!(!exposed.contains(secret), "exposed a URL secret");
    }
    let mut member = login(&server, "member").await;
    assert!(matches!(
        member
            .request::<_, FeedMappingsReply>(&FeedMappingsRequest)
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(
        server
            .shared
            .config
            .set_key("syndication_feeds", "{}")
            .is_err(),
        "mapping mutations remain TOML-only"
    );
    server.shutdown().await;
}

#[derive(Clone)]
struct Captured(Arc<Mutex<Vec<u8>>>);
impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_credential_urls_do_not_reach_routine_errors_or_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let server = Burrow::start(ServerConfig {
        data_dir: tmp.path().into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        syndication_feeds: [
            (FIRST.into(), "alpha".into()),
            (INVALID.into(), "invalid".into()),
        ]
        .into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let captured = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || Captured(captured.clone()))
        .finish();
    let mut service = burrow::syndication::SyndicationService::new(
        server.shared.clone(),
        tmp.path().join("seen"),
    )
    .await
    .unwrap();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        assert_eq!(
            service.poll_due(chrono::Utc::now().timestamp() + 1).await,
            0
        );
    }
    let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(log.contains("syndication fetch failed"));
    assert!(log.contains("https://example.test/feed"));
    for raw in [
        FIRST,
        INVALID,
        "secret-one",
        "token-one",
        "fragment-one",
        "malformed-secret",
    ] {
        assert!(!log.contains(raw), "routine log exposed a URL secret");
    }
    for raw in [FIRST, INVALID, "http://user:password@[broken:port?q=secret"] {
        let error = burrow::syndication::FeedUrl::parse(raw)
            .unwrap_err()
            .to_string();
        assert!(!error.contains(raw));
        assert!(!error.contains("password"));
        assert!(!error.contains("secret"));
    }
    server.shutdown().await;
}
