use super::*;

async fn service() -> FileService {
    FileService::new(rabbithole_store_server::open_in_memory().await.unwrap())
}

async fn publish(
    svc: &FileService,
    area: &str,
    parent: Option<&str>,
    area_id: i64,
    parent_id: Option<i64>,
    name: &str,
    blob: [u8; 32],
) -> Result<Option<i64>, FileError> {
    svc.add_file_if_target(
        area,
        parent,
        area_id,
        parent_id,
        name,
        &blob,
        42,
        "application/octet-stream",
        "disk",
        "checked",
        "alice@burrow",
        7,
    )
    .await
}

#[tokio::test]
async fn matching_root_and_folder_publish_with_existing_name_and_area_rules() {
    let svc = service().await;
    let area = svc.create_area("warez", "Warez", "").await.unwrap();
    let root_id = publish(
        &svc,
        "WAREZ",
        Some(""),
        area.id,
        None,
        " root.bin ",
        [1; 32],
    )
    .await
    .unwrap()
    .unwrap();
    let root = svc.node(root_id).await.unwrap().unwrap();
    assert_eq!(root.path, "root.bin");
    assert_eq!(root.area, "warez");
    assert_eq!(root.parent_id, None);

    let folder = svc.mkdir("warez", None, "drop", false).await.unwrap();
    let id = publish(
        &svc,
        "warez",
        Some("drop"),
        area.id,
        Some(folder.id),
        "a.bin",
        [2; 32],
    )
    .await
    .unwrap()
    .unwrap();
    let node = svc.node(id).await.unwrap().unwrap();
    assert_eq!((node.area_id, node.parent_id), (area.id, Some(folder.id)));
    assert_eq!((node.kind, node.path.as_str()), (KIND_FILE, "drop/a.bin"));
    assert_eq!(node.blob_id, Some([2; 32]));
    assert_eq!(node.size, 42);
    assert_eq!(node.mime, "application/octet-stream");
    assert_eq!(node.icon, "disk");
    assert_eq!(node.comment, "checked");
    assert_eq!(node.uploader, "alice@burrow");
    assert_eq!(node.uploader_id, Some(7));
    assert!(matches!(
        publish(&svc, "warez", None, area.id, None, "../bad", [0; 32]).await,
        Err(FileError::BadName)
    ));
}

#[tokio::test]
async fn moved_or_replaced_folder_cannot_receive_an_old_targets_upload() {
    let svc = service().await;
    let area = svc.create_area("warez", "Warez", "").await.unwrap();
    let old = svc.mkdir("warez", None, "drop", false).await.unwrap();
    svc.mkdir("warez", None, "archive", false).await.unwrap();
    svc.move_to(old.id, Some("archive")).await.unwrap();
    let replacement = svc.mkdir("warez", None, "drop", false).await.unwrap();
    for (path, expected) in [("drop", old.id), ("archive/drop", replacement.id)] {
        assert_eq!(
            publish(
                &svc,
                "warez",
                Some(path),
                area.id,
                Some(expected),
                "old.bin",
                [3; 32]
            )
            .await
            .unwrap(),
            None,
            "both the ID and the original placement must match"
        );
    }
    assert!(svc
        .node_by_path("warez", "drop/old.bin")
        .await
        .unwrap()
        .is_none());
    assert!(svc
        .node_by_path("warez", "archive/drop/old.bin")
        .await
        .unwrap()
        .is_none());

    // A fresh offer for the replacement is a distinct valid target.
    assert!(publish(
        &svc,
        "warez",
        Some("drop"),
        area.id,
        Some(replacement.id),
        "fresh.bin",
        [4; 32]
    )
    .await
    .unwrap()
    .is_some());
    svc.delete(old.id).await.unwrap();
    let recreated = svc
        .mkdir("warez", Some("archive"), "drop", false)
        .await
        .unwrap();
    assert_ne!(old.id, recreated.id);
    assert_eq!(
        publish(
            &svc,
            "warez",
            Some("archive/drop"),
            area.id,
            Some(old.id),
            "deleted.bin",
            [5; 32]
        )
        .await
        .unwrap(),
        None
    );
}

#[tokio::test]
async fn stale_area_wrong_area_nonfolder_and_inconsistent_root_are_refused() {
    let svc = service().await;
    let old = svc.create_area("old", "Old", "").await.unwrap();
    svc.delete_area("old").await.unwrap();
    let area = svc.create_area("old", "Replacement", "").await.unwrap();
    assert_ne!(old.id, area.id);
    assert_eq!(
        publish(&svc, "old", None, old.id, None, "a.bin", [1; 32])
            .await
            .unwrap(),
        None
    );
    let other = svc.create_area("other", "Other", "").await.unwrap();
    assert_eq!(
        publish(&svc, "other", None, area.id, None, "a.bin", [1; 32])
            .await
            .unwrap(),
        None
    );
    let folder = svc.mkdir("other", None, "folder", false).await.unwrap();
    assert_eq!(
        publish(
            &svc,
            "old",
            Some("folder"),
            area.id,
            Some(folder.id),
            "a.bin",
            [1; 32]
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        publish(
            &svc,
            "other",
            Some("folder"),
            other.id,
            None,
            "a.bin",
            [1; 32]
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        publish(
            &svc,
            "other",
            None,
            other.id,
            Some(folder.id),
            "a.bin",
            [1; 32]
        )
        .await
        .unwrap(),
        None
    );
    let file = publish(&svc, "other", None, other.id, None, "file", [1; 32])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        publish(
            &svc,
            "other",
            Some("file"),
            other.id,
            Some(file),
            "a.bin",
            [1; 32]
        )
        .await
        .unwrap(),
        None
    );
}

#[tokio::test]
async fn concurrent_publications_and_existing_folders_never_clobber() {
    let svc = service().await;
    let area = svc.create_area("warez", "Warez", "").await.unwrap();
    let (a, b) = tokio::join!(
        publish(&svc, "warez", None, area.id, None, "same.bin", [1; 32]),
        publish(&svc, "warez", None, area.id, None, "same.bin", [2; 32]),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.is_some(), b.is_some());
    let node = svc.node(a.or(b).unwrap()).await.unwrap().unwrap();
    assert_eq!(
        node.blob_id,
        Some(if a.is_some() { [1; 32] } else { [2; 32] })
    );
    assert_eq!(svc.list("warez", None).await.unwrap().len(), 1);
    let folder = svc.mkdir("warez", None, "taken", false).await.unwrap();
    assert_eq!(
        publish(&svc, "warez", None, area.id, None, "taken", [3; 32])
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        svc.node(folder.id).await.unwrap().unwrap().kind,
        KIND_FOLDER
    );
}
