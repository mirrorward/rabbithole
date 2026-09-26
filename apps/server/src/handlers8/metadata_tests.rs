//! The native metadata helpers use the real ACL evaluator. Shared.perms is
//! immutable after startup, so path-specific rules are exercised here rather
//! than adding a live ACL mutation surface solely for integration fixtures.

use super::*;
use rabbithole_server_core::permissions::{AclRule, Principal};
use rabbithole_server_core::{PermissionEvaluator, Role, Subject};

fn user() -> Subject {
    Subject {
        account_id: 1,
        role: Role::User,
        class_id: None,
        class_mask: 0,
        grant_mask: 0,
        revoke_mask: 0,
    }
}

async fn add(files: &FileService, parent: Option<&str>, name: &str) -> FileNodeRow {
    files
        .add_file(
            "pub",
            parent,
            name,
            &[1; 32],
            1,
            "",
            "",
            "description",
            "uploader",
            1,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn areas_children_and_known_ids_obey_visibility_and_nearest_path_rules() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    for slug in ["pub", "secret", "unlisted"] {
        files.create_area(slug, slug, "").await.unwrap();
    }
    let open = add(&files, None, "open.bin").await;
    let hidden = add(&files, None, "hidden.bin").await;
    let unlisted = add(&files, None, "unlisted.bin").await;
    files.mkdir("pub", None, "locked", false).await.unwrap();
    let inherited = add(&files, Some("locked"), "inherited.bin").await;
    let exception = add(&files, Some("locked"), "exception.bin").await;
    let mut perms = PermissionEvaluator::new();
    for (res, deny) in [
        ("files/secret", Caps::SEE),
        ("files/unlisted", Caps::FILE_LIST),
        ("files/pub/hidden.bin", Caps::SEE),
        ("files/pub/unlisted.bin", Caps::FILE_LIST),
        ("files/pub/locked", Caps::SEE | Caps::FILE_LIST),
        ("files/pub/open.bin", Caps::FILE_DOWNLOAD),
    ] {
        perms.insert_rule(
            res,
            Principal::Everyone,
            AclRule {
                allow: 0,
                deny: deny.0,
            },
        );
    }
    perms.insert_rule(
        "files/pub/locked/exception.bin",
        Principal::Account(1),
        AclRule {
            allow: (Caps::SEE | Caps::FILE_LIST).0,
            deny: 0,
        },
    );
    let allows = |res: &str, caps| perms.allows(&user(), res, caps);
    assert_eq!(
        visible_areas(&files.areas().await.unwrap(), allows)
            .iter()
            .map(|a| a.slug.as_str())
            .collect::<Vec<_>>(),
        ["pub"]
    );
    let listed = folder_metadata(&files, "pub", None, allows, |_| false)
        .await
        .unwrap();
    assert_eq!(listed.iter().map(|n| n.id).collect::<Vec<_>>(), [open.id]);
    assert_eq!(
        node_metadata(&files, open.id, allows, |_| false)
            .await
            .unwrap()
            .id,
        open.id,
        "download denial alone does not hide metadata"
    );
    for id in [hidden.id, unlisted.id, inherited.id, i64::MAX] {
        assert_eq!(
            node_metadata(&files, id, allows, |_| false).await,
            Err(ErrorCode::NotFound)
        );
    }
    assert_eq!(
        node_metadata(&files, exception.id, allows, |_| false)
            .await
            .unwrap()
            .id,
        exception.id,
        "explicit nearer grants retain the existing ACL semantics"
    );
    assert_eq!(
        folder_metadata(&files, "pub", Some("locked"), allows, |_| false).await,
        Err(ErrorCode::NotFound)
    );
    assert_eq!(
        folder_metadata(&files, "secret", None, allows, |_| false).await,
        Err(ErrorCode::NotFound)
    );
    assert_eq!(
        folder_metadata(&files, "SECRET", None, allows, |_| false).await,
        Err(ErrorCode::NotFound),
        "case-insensitive area aliases use the canonical ACL resource"
    );
    assert_eq!(
        folder_metadata(&files, "unlisted", None, allows, |_| false).await,
        Err(ErrorCode::Forbidden)
    );
    assert_eq!(
        folder_metadata(&files, "UNLISTED", None, allows, |_| false).await,
        Err(ErrorCode::Forbidden)
    );
}

#[tokio::test]
async fn metadata_hides_all_dropbox_descendants_but_keeps_upload_destination_visible() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    files.create_area("pub", "Public", "").await.unwrap();
    let drop = files.mkdir("pub", None, "drop", true).await.unwrap();
    let nested = files
        .mkdir("pub", Some("drop"), "nested", false)
        .await
        .unwrap();
    let file = add(&files, Some("drop/nested"), "hidden.bin").await;
    let mut perms = PermissionEvaluator::new();
    let allows = |res: &str, caps| perms.allows(&user(), res, caps);
    assert_eq!(
        folder_metadata(&files, "pub", None, allows, |_| false)
            .await
            .unwrap()[0]
            .id,
        drop.id
    );
    assert_eq!(
        node_metadata(&files, drop.id, allows, |_| false)
            .await
            .unwrap()
            .id,
        drop.id
    );
    assert!(
        folder_metadata(&files, "pub", Some("drop"), allows, |_| false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        folder_metadata(&files, "pub", Some("drop/nested"), allows, |_| false).await,
        Err(ErrorCode::NotFound)
    );
    for id in [nested.id, file.id] {
        assert_eq!(
            node_metadata(&files, id, allows, |_| false).await,
            Err(ErrorCode::NotFound)
        );
    }
    // An area manager can inspect descendants even with an explicit deny of
    // DROPBOX_VIEW. That exception does not waive SEE or FILE_LIST.
    perms.insert_rule(
        "files/pub",
        Principal::Account(1),
        AclRule {
            allow: Caps::FILE_MANAGE.0,
            deny: Caps::DROPBOX_VIEW.0,
        },
    );
    let allows = |res: &str, caps| perms.allows(&user(), res, caps);
    assert_eq!(
        folder_metadata(&files, "pub", Some("drop/nested"), allows, |_| false)
            .await
            .unwrap()[0]
            .id,
        file.id
    );
    assert_eq!(
        node_metadata(&files, file.id, allows, |_| false)
            .await
            .unwrap()
            .id,
        file.id
    );
    perms.insert_rule(
        "files/pub/drop/nested/hidden.bin",
        Principal::Everyone,
        AclRule {
            allow: 0,
            deny: Caps::SEE.0,
        },
    );
    assert_eq!(
        node_metadata(
            &files,
            file.id,
            |res, caps| perms.allows(&user(), res, caps),
            |_| false
        )
        .await,
        Err(ErrorCode::NotFound)
    );
}
