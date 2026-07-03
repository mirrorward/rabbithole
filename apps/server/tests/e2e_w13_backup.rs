//! Wave 13 end-to-end tests: backups — consistent ctl snapshots with a
//! hashed manifest, offline restore, and corruption detection.

use burrow::Burrow;
use rabbithole_server_core::{Role, ServerConfig};
use serde_json::json;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Backed-Up Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn start(dir: &std::path::Path) -> Burrow {
    let burrow = Burrow::start(test_config(dir)).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
}

async fn seed_post(burrow: &Burrow, subject: &str, body: &str, now_ms: i64) {
    burrow
        .shared
        .boards
        .post(
            "lounge",
            None,
            "alice",
            &[7u8; 32],
            subject,
            body,
            "text/plain",
            now_ms,
        )
        .await
        .unwrap();
}

async fn thread_subjects(burrow: &Burrow) -> Vec<String> {
    burrow
        .shared
        .boards
        .threads("lounge", 100)
        .await
        .unwrap()
        .into_iter()
        .map(|(post, _, _)| post.subject)
        .collect()
}

/// The whole operator story: seed a burrow, `ctl backup`, verify the
/// manifest, mutate the live server, then restore the snapshot offline into
/// another data dir and boot from it — the pre-snapshot state is back, the
/// post-snapshot mutations are not, and the server keeps its identity.
#[tokio::test]
async fn backup_then_offline_restore_roundtrip() {
    let live = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let restored_root = tempfile::tempdir().unwrap();

    // ---- Seed: an account, a board with one post, and a blob ----------
    let burrow = start(live.path()).await;
    burrow
        .shared
        .boards
        .create_board("lounge", "The Lounge", "", 2, None, 100)
        .await
        .unwrap();
    seed_post(&burrow, "pre-snapshot", "was here before the backup", 1_000).await;
    let blob_bytes = b"blob content that must survive the restore".to_vec();
    let blob_id = burrow.shared.blobs.put(&blob_bytes).unwrap();
    let original_key = burrow.shared.server_key;

    // ---- Snapshot through ctl ------------------------------------------
    let dest = backups.path().to_str().unwrap();
    let resp = burrow::ctl::handle(&burrow.shared, &json!({"cmd": "backup", "dest": dest})).await;
    assert_eq!(resp["ok"], json!(true), "backup succeeds: {resp}");
    let snapshot_dir = std::path::PathBuf::from(resp["data"]["snapshot_dir"].as_str().unwrap());
    assert!(snapshot_dir.starts_with(backups.path()));
    assert!(snapshot_dir.join("MANIFEST.json").is_file());
    assert!(snapshot_dir.join("burrow.db").is_file());
    assert!(resp["data"]["files"].as_u64().unwrap() >= 3); // db + identity + blob

    // The snapshot is audited.
    let audited = {
        use rabbithole_store_server::repo::AuditRepo;
        let mut seen = false;
        for _ in 0..50 {
            let rows = AuditRepo(&burrow.shared.pool).recent(20).await.unwrap();
            if rows.iter().any(|r| r.action == "backup") {
                seen = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        seen
    };
    assert!(audited, "backup lands in the audit log");

    // ---- backup-verify passes on a pristine snapshot --------------------
    let resp = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "backup-verify", "path": snapshot_dir.to_str().unwrap()}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "verify passes: {resp}");
    assert_eq!(resp["data"]["integrity_check"], json!("ok"));

    // ---- ctl restore always refuses while the server runs ---------------
    let resp = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "restore", "path": snapshot_dir.to_str().unwrap()}),
    )
    .await;
    assert_eq!(resp["ok"], json!(false));
    assert!(
        resp["error"].as_str().unwrap().contains("offline"),
        "refusal points at the offline flow: {resp}"
    );

    // The offline path also refuses while this burrow is up (live socket).
    let err = burrow::backup::restore_offline(&snapshot_dir, live.path()).unwrap_err();
    assert!(err.to_string().contains("running"), "refused: {err}");

    // ---- Mutate the live server *after* the snapshot ---------------------
    seed_post(
        &burrow,
        "post-snapshot",
        "must not survive the restore",
        2_000,
    )
    .await;
    burrow
        .shared
        .auth
        .create_account("mallory", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let late_blob = burrow
        .shared
        .blobs
        .put(b"late blob, not in snapshot")
        .unwrap();
    burrow.shutdown().await;

    // ---- Offline restore into a data dir that already has content -------
    let restored_dir = restored_root.path().join("warren");
    std::fs::create_dir_all(&restored_dir).unwrap();
    std::fs::write(restored_dir.join("junk.txt"), b"old world").unwrap();

    let outcome = burrow::backup::restore_offline(&snapshot_dir, &restored_dir).unwrap();
    let aside = outcome.moved_aside.expect("existing dir moved aside");
    assert!(aside.join("junk.txt").is_file(), "old data preserved aside");
    assert!(
        !restored_dir.join("junk.txt").exists(),
        "fresh dir is clean"
    );

    // ---- Boot from the restored dir: pre-snapshot world, same identity --
    let revived = Burrow::start(test_config(&restored_dir)).await.unwrap();
    assert_eq!(
        revived.shared.server_key, original_key,
        "identity key restored: the server is still itself"
    );

    let subjects = thread_subjects(&revived).await;
    assert_eq!(subjects, vec!["pre-snapshot".to_string()]);

    assert_eq!(revived.shared.blobs.get(&blob_id).unwrap(), blob_bytes);
    assert!(
        !revived.shared.blobs.contains(&late_blob),
        "post-snapshot blob absent"
    );

    {
        use rabbithole_store_server::repo::AccountsRepo;
        let repo = AccountsRepo(&revived.shared.pool);
        assert!(repo.by_login("alice").await.unwrap().is_some());
        assert!(
            repo.by_login("mallory").await.unwrap().is_none(),
            "post-snapshot account absent"
        );
    }
    revived.shutdown().await;
}

/// A flipped byte anywhere in the snapshot is caught by verification, and a
/// corrupted snapshot refuses to restore.
#[tokio::test]
async fn backup_verify_catches_corruption() {
    let live = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();

    let burrow = start(live.path()).await;
    burrow.shared.blobs.put(b"soon to be corrupted").unwrap();

    let dest = backups.path().to_str().unwrap();
    let resp = burrow::ctl::handle(&burrow.shared, &json!({"cmd": "backup", "dest": dest})).await;
    assert_eq!(resp["ok"], json!(true), "backup succeeds: {resp}");
    let snapshot_dir = std::path::PathBuf::from(resp["data"]["snapshot_dir"].as_str().unwrap());

    // Flip one byte in a manifest-listed blob file (size unchanged).
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(snapshot_dir.join("MANIFEST.json")).unwrap())
            .unwrap();
    let blob_rel = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .find(|p| p.starts_with("blobs/"))
        .expect("snapshot contains the blob");
    let victim = snapshot_dir.join(blob_rel);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&victim, &bytes).unwrap();

    // ctl backup-verify reports the mismatch.
    let resp = burrow::ctl::handle(
        &burrow.shared,
        &json!({"cmd": "backup-verify", "path": snapshot_dir.to_str().unwrap()}),
    )
    .await;
    assert_eq!(resp["ok"], json!(false), "corruption detected: {resp}");
    assert!(resp["error"].as_str().unwrap().contains("hash mismatch"));

    burrow.shutdown().await;

    // Offline restore refuses the corrupt snapshot and leaves the target
    // untouched.
    let target = backups.path().join("never-created");
    let err = burrow::backup::restore_offline(&snapshot_dir, &target).unwrap_err();
    assert!(
        format!("{err:#}").contains("hash mismatch"),
        "refused: {err:#}"
    );
    assert!(!target.exists(), "nothing written on refusal");
}

/// Restoring into a data dir that does not exist yet needs no move-aside
/// and produces a bootable burrow (the fresh-machine recovery path).
#[tokio::test]
async fn restore_into_fresh_dir_has_no_move_aside() {
    let live = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();

    let burrow = start(live.path()).await;
    let original_key = burrow.shared.server_key;
    let dest = backups.path().to_str().unwrap();
    let resp = burrow::ctl::handle(&burrow.shared, &json!({"cmd": "backup", "dest": dest})).await;
    assert_eq!(resp["ok"], json!(true));
    let snapshot_dir = std::path::PathBuf::from(resp["data"]["snapshot_dir"].as_str().unwrap());
    burrow.shutdown().await;

    let fresh = backups.path().join("fresh-warren");
    let outcome = burrow::backup::restore_offline(&snapshot_dir, &fresh).unwrap();
    assert!(outcome.moved_aside.is_none());
    assert!(outcome.files >= 2);

    let revived = Burrow::start(test_config(&fresh)).await.unwrap();
    assert_eq!(revived.shared.server_key, original_key);
    revived.shutdown().await;
}
