//! RH-168: native metadata never bypasses child visibility or nested boxes.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    subject_kind, ClassList, ClassListRequest, ClassSet, QuarantineClear, QuarantineSet,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Caps, Role, ServerConfig};

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
    for (name, role) in [
        ("alice", Role::User),
        ("admin", Role::Admin),
        ("moderator", Role::Moderator),
    ] {
        b.shared
            .auth
            .create_account(name, "metadata-password", role)
            .await
            .unwrap();
    }
    b.shared
        .files
        .create_area("pub", "Public", "Visible library")
        .await
        .unwrap();
    b
}

async fn login(b: &Burrow, name: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "metadata-test",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(name, "metadata-password").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(result: Result<T, ClientError>, code: ErrorCode) {
    assert!(
        matches!(result, Err(ClientError::Refused(actual)) if actual == code),
        "{result:?}"
    );
}

#[tokio::test]
async fn nested_metadata_uses_current_class_rights_and_quarantine_before_replying() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    let drop = b
        .shared
        .files
        .mkdir("pub", None, "drop", true)
        .await
        .unwrap();
    let nested = b
        .shared
        .files
        .mkdir("pub", Some("drop"), "nested", false)
        .await
        .unwrap();
    let deepest = b
        .shared
        .files
        .mkdir("pub", Some("drop/nested"), "deep", false)
        .await
        .unwrap();
    let file = b
        .shared
        .files
        .add_file(
            "pub",
            Some("drop/nested/deep"),
            "held.bin",
            &[5; 32],
            1,
            "application/octet-stream",
            "",
            "private metadata",
            "uploader",
            1,
        )
        .await
        .unwrap();
    let mut admin = login(&b, "admin").await;
    let mut alice = login(&b, "alice").await;
    let mut moderator = login(&b, "moderator").await;
    assert_eq!(alice.file_areas().await.unwrap()[0].slug, "pub");
    assert_eq!(alice.folder_list("pub", None).await.unwrap()[0].id, drop.id);
    assert_eq!(
        alice.node_get(drop.id).await.unwrap().id,
        drop.id,
        "the upload destination is visible, not its contents"
    );
    assert!(alice
        .folder_list("pub", Some("drop".into()))
        .await
        .unwrap()
        .is_empty());
    for path in ["drop/nested", "drop/nested/deep", "missing"] {
        refused(
            alice.folder_list("pub", Some(path.into())).await,
            ErrorCode::NotFound,
        );
    }
    for id in [nested.id, deepest.id, file.id, i64::MAX] {
        refused(alice.node_get(id).await, ErrorCode::NotFound);
    }
    for viewer in [&mut admin, &mut moderator] {
        assert_eq!(viewer.node_get(file.id).await.unwrap().id, file.id);
        assert_eq!(
            viewer
                .folder_list("pub", Some("drop/nested/deep".into()))
                .await
                .unwrap()[0]
                .id,
            file.id
        );
    }

    let classes: ClassList = admin.request(&ClassListRequest).await.unwrap();
    let member = classes.classes.iter().find(|c| c.name == "member").unwrap();
    admin
        .request_ack(&ClassSet::new(
            "member",
            member.base_mask | Caps::DROPBOX_VIEW.0,
        ))
        .await
        .unwrap();
    assert_eq!(
        alice.folder_list("pub", Some("drop".into())).await.unwrap()[0].id,
        nested.id
    );
    assert_eq!(
        alice.node_get(file.id).await.unwrap().comment,
        "private metadata"
    );
    assert_eq!(
        alice
            .folder_list("pub", Some("drop/nested/deep".into()))
            .await
            .unwrap()[0]
            .id,
        file.id
    );

    admin
        .request_ack(&QuarantineSet::new(
            subject_kind::FILE,
            vec![5; 32],
            "review",
        ))
        .await
        .unwrap();
    refused(alice.node_get(file.id).await, ErrorCode::NotFound);
    assert!(alice
        .folder_list("pub", Some("drop/nested/deep".into()))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(moderator.node_get(file.id).await.unwrap().id, file.id);
    assert_eq!(
        moderator
            .folder_list("pub", Some("drop/nested/deep".into()))
            .await
            .unwrap()[0]
            .id,
        file.id
    );
    admin
        .request_ack(&QuarantineClear::new(subject_kind::FILE, vec![5; 32]))
        .await
        .unwrap();
    assert_eq!(alice.node_get(file.id).await.unwrap().id, file.id);
    admin
        .request_ack(&ClassSet::new("member", member.base_mask))
        .await
        .unwrap();
    refused(alice.node_get(file.id).await, ErrorCode::NotFound);
    refused(
        alice
            .folder_list("pub", Some("drop/nested/deep".into()))
            .await,
        ErrorCode::NotFound,
    );
    assert!(alice
        .folder_list("pub", Some("drop".into()))
        .await
        .unwrap()
        .is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn public_metadata_stays_visible_when_download_is_denied() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    let bytes = b"visible metadata without download rights";
    let blob = b.shared.blobs.put(bytes).unwrap();
    let file = b
        .shared
        .files
        .add_file(
            "pub",
            None,
            "public.txt",
            &blob.0,
            bytes.len() as i64,
            "text/plain",
            "",
            "public description",
            "uploader",
            1,
        )
        .await
        .unwrap();
    let mut guest = Client::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "metadata-test",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("visitor".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    assert_eq!(guest.file_areas().await.unwrap()[0].title, "Public");
    assert_eq!(guest.folder_list("pub", None).await.unwrap()[0].id, file.id);
    assert_eq!(
        guest.node_get(file.id).await.unwrap().comment,
        "public description"
    );
    refused(guest.file_download(file.id).await, ErrorCode::Forbidden);
    let mut alice = login(&b, "alice").await;
    assert_eq!(alice.file_download(file.id).await.unwrap().bytes, bytes);
    b.shutdown().await;
}
