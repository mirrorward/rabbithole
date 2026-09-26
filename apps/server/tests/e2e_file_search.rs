//! RH-5: search visibility is evaluated before the requested result limit.

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_proto::admin::{
    subject_kind, ClassList, ClassListRequest, ClassSet, QuarantineClear, QuarantineSet,
};
use rabbithole_proto::filelib::{NodeMove, NodeRename, NodeReply};
use rabbithole_server_core::{Caps, Role, ServerConfig};
use rabbithole_store_server::repo6::FileNodeRow;

async fn start(path: &std::path::Path) -> Burrow {
    let b = Burrow::start(ServerConfig {
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_enabled: false,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for (name, role) in [("alice", Role::User), ("admin", Role::Admin)] {
        b.shared
            .auth
            .create_account(name, "search-password", role)
            .await
            .unwrap();
    }
    b
}

async fn login(b: &Burrow, name: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "search-test",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(name, "search-password").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn add(b: &Burrow, parent: Option<&str>, name: &str, blob: [u8; 32]) -> FileNodeRow {
    b.shared
        .files
        .add_file(
            "pub",
            parent,
            name,
            &blob,
            1,
            "",
            "",
            "needle",
            "Alice@Home",
            1,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn hidden_newer_matches_do_not_consume_the_limit_and_live_rights_apply() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();
    b.shared
        .files
        .mkdir("pub", None, "drop", true)
        .await
        .unwrap();
    b.shared
        .files
        .mkdir("pub", Some("drop"), "nested", false)
        .await
        .unwrap();
    let oldest = add(&b, None, "older.bin", [1; 32]).await;
    let newer = add(&b, None, "newer.bin", [2; 32]).await;
    // More than a candidate page, with both direct and nested drop entries.
    for i in 0..70 {
        add(
            &b,
            Some(if i % 2 == 0 { "drop" } else { "drop/nested" }),
            &format!("drop-{i}.bin"),
            [3; 32],
        )
        .await;
    }
    let held = add(&b, None, "held.bin", [4; 32]).await;
    let mut admin = login(&b, "admin").await;
    let mut alice = login(&b, "alice").await;
    admin
        .request_ack(&QuarantineSet::new(subject_kind::FILE, vec![4; 32], "held"))
        .await
        .unwrap();
    for area in [None, Some("pub".into())] {
        let found = alice.file_search(area, "needle", 2).await.unwrap();
        assert_eq!(
            found.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![newer.id, oldest.id]
        );
    }
    assert_eq!(
        admin.file_search(None, "needle", 1).await.unwrap()[0].id,
        held.id
    );
    // Search still has no download requirement: authenticated guests see the
    // public metadata, but not drop-box/quarantined matches.
    let mut guest = Client::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "search-test",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("visitor".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    assert_eq!(
        guest.file_search(None, "needle", 1).await.unwrap()[0].id,
        newer.id
    );

    let classes: ClassList = admin.request(&ClassListRequest).await.unwrap();
    let member = classes.classes.iter().find(|c| c.name == "member").unwrap();
    admin
        .request_ack(&ClassSet::new(
            "member",
            member.base_mask | Caps::DROPBOX_VIEW.0,
        ))
        .await
        .unwrap();
    let expanded = alice.file_search(None, "needle", 2).await.unwrap();
    assert!(expanded.iter().all(|n| n.path.starts_with("drop/")));
    admin
        .request_ack(&ClassSet::new("member", member.base_mask))
        .await
        .unwrap();
    assert_eq!(
        alice.file_search(None, "needle", 1).await.unwrap()[0].id,
        newer.id
    );
    admin
        .request_ack(&QuarantineClear::new(subject_kind::FILE, vec![4; 32]))
        .await
        .unwrap();
    assert_eq!(
        alice.file_search(None, "needle", 1).await.unwrap()[0].id,
        held.id
    );
    b.shutdown().await;
}

#[tokio::test]
async fn native_metadata_rename_and_move_refresh_search_without_reindexing() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();
    b.shared
        .files
        .mkdir("pub", None, "drop", true)
        .await
        .unwrap();
    let file = add(&b, None, "original.bin", [5; 32]).await;
    let mut admin = login(&b, "admin").await;
    let mut alice = login(&b, "alice").await;
    assert_eq!(
        alice.file_search(None, "original", 1).await.unwrap()[0].id,
        file.id
    );
    admin
        .set_file_metadata(file.id, "", "a different phrase")
        .await
        .unwrap();
    assert!(alice
        .file_search(None, "needle", 1)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        alice.file_search(None, "different", 1).await.unwrap()[0].id,
        file.id
    );
    let _: NodeReply = admin
        .request(&NodeRename::new(file.id, "renamed.zip"))
        .await
        .unwrap();
    assert!(alice
        .file_search(None, "original", 1)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        alice.file_search(None, "named.z", 1).await.unwrap()[0].id,
        file.id
    );
    let _: NodeReply = admin
        .request(&NodeMove::new(file.id, Some("drop".into())))
        .await
        .unwrap();
    assert!(alice
        .file_search(None, "named.z", 1)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        admin.file_search(None, "named.z", 1).await.unwrap()[0].path,
        "drop/renamed.zip"
    );
    let _: NodeReply = admin.request(&NodeMove::new(file.id, None)).await.unwrap();
    assert_eq!(
        alice.file_search(None, "named.z", 1).await.unwrap()[0].path,
        "renamed.zip"
    );
    b.shutdown().await;
}
