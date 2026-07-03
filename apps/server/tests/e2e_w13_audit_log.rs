//! Wave 13 end-to-end test: the `audit-log` ctl command surfaces the audit
//! trail (`AuditRepo::recent`) for local operators, with optional actor/action
//! filters. Entries are recorded directly here so the assertion doesn't race
//! the fire-and-forget audit writes the ctl handlers spawn.

use burrow::Burrow;
use rabbithole_server_core::ServerConfig;
use rabbithole_store_server::repo::AuditRepo;
use serde_json::json;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Audit Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn audit_log_ctl_reports_recent_entries_with_filters() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    let audit = AuditRepo(&burrow.shared.pool);
    audit
        .record("alice", "config-set", "motd=hi")
        .await
        .unwrap();
    audit
        .record("bob", "account-create", "carol")
        .await
        .unwrap();
    audit.record("alice", "hash-deny", "abcd").await.unwrap();

    // All recent entries, oldest first.
    let r = burrow::ctl::handle(&burrow.shared, &json!({"cmd": "audit-log"})).await;
    assert_eq!(r["ok"], true, "{r}");
    let entries = r["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3, "{r}");
    assert_eq!(entries[0]["actor"], "alice");
    assert_eq!(entries[0]["action"], "config-set");
    assert_eq!(entries[2]["action"], "hash-deny");

    // Filter by actor.
    let by_actor = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "audit-log", "actor": "alice"}),
    )
    .await;
    assert_eq!(by_actor["data"]["count"], 2, "{by_actor}");

    // Filter by action.
    let by_action = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "audit-log", "action": "hash-deny"}),
    )
    .await;
    assert_eq!(by_action["data"]["count"], 1);
    assert_eq!(by_action["data"]["entries"][0]["detail"], "abcd");

    // Limit caps the window.
    let limited =
        burrow::ctl::handle(&burrow.shared, &json!({"cmd": "audit-log", "limit": 1})).await;
    assert_eq!(limited["data"]["count"], 1, "{limited}");
    assert_eq!(
        limited["data"]["entries"][0]["action"], "hash-deny",
        "the single most-recent entry"
    );
}
