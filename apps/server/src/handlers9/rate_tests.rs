//! RH-6: current account classification and fail-closed policy resolution.

use super::*;
use rabbithole_server_core::ServerConfig;
use rabbithole_store_server::repo::ClassesRepo;

#[tokio::test]
async fn current_class_and_live_policy_replace_session_snapshots() {
    let pool = rabbithole_store_server::open_in_memory().await.unwrap();
    let accounts = AccountsRepo(&pool);
    let classes = ClassesRepo(&pool);
    let member = classes.upsert("member", 0).await.unwrap();
    let vip = classes.upsert("VIP", 0).await.unwrap();
    let account = accounts
        .create("alice", None, "Alice", 1, None)
        .await
        .unwrap();
    let config = LiveConfig::new(ServerConfig {
        transfer_rate_bytes_per_sec: 100,
        transfer_rate_by_class: [("member".into(), 200), ("VIP".into(), 0)].into(),
        ..ServerConfig::default()
    });
    assert_eq!(download_rate(&pool, &config, account.id).await, Ok(100));
    assert_eq!(download_rate(&pool, &config, -1).await, Ok(100));
    for (class, expected) in [(Some(member), 200), (Some(vip), 0), (None, 100)] {
        accounts
            .admin_set("alice", None, Some(class), None)
            .await
            .unwrap();
        assert_eq!(
            download_rate(&pool, &config, account.id).await,
            Ok(expected)
        );
    }
    accounts
        .admin_set("alice", None, Some(Some(vip)), None)
        .await
        .unwrap();
    config
        .set_key("transfer_rate_by_class", "{ vip = 12 }")
        .unwrap();
    assert_eq!(
        download_rate(&pool, &config, account.id).await,
        Ok(100),
        "class names are exact"
    );
    config
        .set_key("transfer_rate_by_class", "{ VIP = 300 }")
        .unwrap();
    assert_eq!(download_rate(&pool, &config, account.id).await, Ok(300));
    config.set_key("transfer_rate_by_class", "{}").unwrap();
    config
        .set_key("transfer_rate_bytes_per_sec", "400")
        .unwrap();
    assert_eq!(download_rate(&pool, &config, account.id).await, Ok(400));
}

#[tokio::test]
async fn missing_disabled_and_failed_lookups_never_become_unlimited() {
    let pool = rabbithole_store_server::open_in_memory().await.unwrap();
    let config = LiveConfig::new(ServerConfig::default());
    let accounts = AccountsRepo(&pool);
    let account = accounts
        .create("alice", None, "Alice", 1, None)
        .await
        .unwrap();
    assert_eq!(
        download_rate(&pool, &config, 0).await,
        Err(ErrorCode::Forbidden)
    );
    accounts
        .admin_set("alice", None, None, Some(true))
        .await
        .unwrap();
    assert_eq!(
        download_rate(&pool, &config, account.id).await,
        Err(ErrorCode::Forbidden)
    );
    pool.close().await;
    assert_eq!(
        download_rate(&pool, &config, account.id).await,
        Err(ErrorCode::Unavailable)
    );
    assert_eq!(
        download_rate(&pool, &config, -9).await,
        Ok(0),
        "synthetic guests have no account lookup"
    );
}
