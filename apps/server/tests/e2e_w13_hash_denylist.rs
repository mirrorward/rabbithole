//! Wave 13 end-to-end test: the content hash deny-list reaches the blob path.
//!
//! The deny-list store + `ModerationService` are covered elsewhere; this closes
//! the documented gap that RHP `BlobPut`/`BlobGet` did not consult it. Via the
//! new local-operator `hash-deny`/`hash-allow`/`hash-deny-list` ctl surface, we
//! prove a deny-listed blob is refused on both serve and ingest, listed for the
//! operator, and fully restored by `hash-allow`.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::blob::BlobPurpose;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use serde_json::json;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Denylist Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn connect(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .expect("connect")
}

#[tokio::test]
async fn deny_listed_blob_is_refused_on_serve_and_ingest() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "wonderland-pw", Role::User)
        .await
        .unwrap();

    let mut alice = connect(&burrow).await;
    alice.auth_password("alice", "wonderland-pw").await.unwrap();
    alice.expect_welcome().await.unwrap();

    // Upload a blob; it round-trips normally.
    let bytes = b"content-to-ban".to_vec();
    let id = alice
        .blob_put(BlobPurpose::Avatar, bytes.clone())
        .await
        .unwrap();
    assert_eq!(alice.blob_get(id).await.unwrap(), bytes);

    // The operator deny-lists it by its blake3 id.
    let hex_id = hex::encode(id);
    let r = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "hash-deny", "hash": hex_id, "reason": "known-bad"}),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");

    // Serving it is now refused...
    assert!(
        matches!(
            alice.blob_get(id).await,
            Err(ClientError::Refused(ErrorCode::Forbidden))
        ),
        "a deny-listed blob is not served"
    );
    // ...and re-uploading the same content is refused too.
    assert!(
        matches!(
            alice.blob_put(BlobPurpose::Avatar, bytes.clone()).await,
            Err(ClientError::Refused(ErrorCode::Forbidden))
        ),
        "deny-listed content cannot be re-ingested"
    );

    // It appears on the operator's deny-list with its reason.
    let list = burrow::ctl::handle(&burrow.shared, &json!({"cmd": "hash-deny-list"})).await;
    assert_eq!(list["data"]["count"], 1, "{list}");
    assert_eq!(list["data"]["denied"][0]["hash"], hex_id);
    assert_eq!(list["data"]["denied"][0]["reason"], "known-bad");

    // Allowing it restores both serve and ingest.
    let allow = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "hash-allow", "hash": hex_id}),
    )
    .await;
    assert_eq!(allow["data"]["removed"], true, "{allow}");
    assert_eq!(
        alice.blob_get(id).await.unwrap(),
        bytes,
        "serving is restored"
    );
    let id2 = alice
        .blob_put(BlobPurpose::Avatar, bytes.clone())
        .await
        .unwrap();
    assert_eq!(id2, id, "re-ingest restored; same content-addressed id");
}
