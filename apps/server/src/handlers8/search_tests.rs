//! Exercise the handler's real permission evaluator without introducing a
//! mutable live ACL API solely for fixtures (Shared.perms is immutable).

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

async fn add(files: &FileService, area: &str, parent: Option<&str>, name: &str) -> FileNodeRow {
    files
        .add_file(
            area, parent, name, &[1; 32], 1, "", "", "needle", "uploader", 1,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn search_checks_each_path_and_area_before_counting_results() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    files.create_area("pub", "Public", "").await.unwrap();
    files.create_area("secret", "Secret", "").await.unwrap();
    files.mkdir("pub", None, "hidden", false).await.unwrap();
    let older = add(&files, "pub", None, "older.bin").await;
    let newer = add(&files, "pub", None, "newer.bin").await;
    for n in 0..96 {
        if n % 2 == 0 {
            add(&files, "pub", Some("hidden"), &format!("hidden-{n}.bin")).await;
        } else {
            add(&files, "secret", None, &format!("secret-{n}.bin")).await;
        }
    }
    let mut perms = PermissionEvaluator::new();
    for (res, denied) in [
        ("files/pub/hidden", Caps::FILE_LIST),
        ("files/secret", Caps::SEE),
        ("files/pub/newer.bin", Caps::FILE_DOWNLOAD),
    ] {
        perms.insert_rule(
            res,
            Principal::Everyone,
            AclRule {
                allow: 0,
                deny: denied.0,
            },
        );
    }
    let found = visible_search(
        &files,
        None,
        "needle",
        2,
        |res, caps| perms.allows(&user(), res, caps),
        |_| false,
    )
    .await
    .unwrap();
    assert_eq!(
        found.iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![newer.id, older.id],
        "hidden candidates span two pages; download denial alone does not hide metadata"
    );
    let scoped = visible_search(
        &files,
        Some("secret"),
        "needle",
        2,
        |res, caps| perms.allows(&user(), res, caps),
        |_| false,
    )
    .await
    .unwrap();
    assert!(scoped.is_empty());
}

#[tokio::test]
async fn search_observes_nested_dropbox_inheritance_and_area_manager_bypass() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    files.create_area("pub", "Public", "").await.unwrap();
    files.mkdir("pub", None, "drop", true).await.unwrap();
    files
        .mkdir("pub", Some("drop"), "nested", false)
        .await
        .unwrap();
    let file = add(&files, "pub", Some("drop/nested"), "needle.bin").await;
    let mut perms = PermissionEvaluator::new();
    async fn found(files: &FileService, perms: &PermissionEvaluator) -> Vec<pf::FileNodeView> {
        visible_search(
            files,
            None,
            "needle",
            10,
            |res, caps| perms.allows(&user(), res, caps),
            |_| false,
        )
        .await
        .unwrap()
    }
    assert!(found(&files, &perms).await.is_empty());
    perms.insert_rule(
        "files",
        Principal::Everyone,
        AclRule {
            allow: Caps::DROPBOX_VIEW.0,
            deny: 0,
        },
    );
    assert_eq!(found(&files, &perms).await[0].id, file.id);
    perms.insert_rule(
        "files/pub/drop",
        Principal::Everyone,
        AclRule {
            allow: 0,
            deny: Caps::DROPBOX_VIEW.0,
        },
    );
    assert!(
        found(&files, &perms).await.is_empty(),
        "nearest inherited deny wins"
    );
    perms.insert_rule(
        "files/pub/drop/nested/needle.bin",
        Principal::Everyone,
        AclRule {
            allow: Caps::DROPBOX_VIEW.0,
            deny: 0,
        },
    );
    assert_eq!(
        found(&files, &perms).await[0].id,
        file.id,
        "explicit leaf grant uses normal nearest-rule semantics"
    );
    perms.insert_rule(
        "files/pub/drop/nested/needle.bin",
        Principal::Everyone,
        AclRule {
            allow: 0,
            deny: Caps::DROPBOX_VIEW.0,
        },
    );
    perms.insert_rule(
        "files/pub",
        Principal::Everyone,
        AclRule {
            allow: Caps::FILE_MANAGE.0,
            deny: 0,
        },
    );
    assert_eq!(
        found(&files, &perms).await[0].id,
        file.id,
        "area managers may inspect drop boxes"
    );
}
