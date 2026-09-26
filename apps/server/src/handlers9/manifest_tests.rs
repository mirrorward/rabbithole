//! Real ACL evaluation at the manifest seam; Shared.perms is immutable.

use super::*;
use rabbithole_server_core::permissions::{AclRule, Principal};
use rabbithole_server_core::{PermissionEvaluator, Role, Subject};
use rabbithole_store_server::repo6::FileNodeRow;

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
            &[7; 32],
            19,
            "text/plain",
            "",
            "",
            "a",
            1,
        )
        .await
        .unwrap()
}

fn rule(perms: &mut PermissionEvaluator, path: &str, allow: Caps, deny: Caps) {
    perms.insert_rule(
        path,
        Principal::Everyone,
        AclRule {
            allow: allow.0,
            deny: deny.0,
        },
    );
}

#[tokio::test]
async fn every_manifest_entry_uses_its_own_inherited_visibility() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    files.create_area("pub", "Public", "").await.unwrap();
    files.mkdir("pub", None, "locked", false).await.unwrap();
    let visible = add(&files, None, "visible").await;
    add(&files, None, "hidden").await;
    add(&files, None, "unlisted").await;
    add(&files, Some("locked"), "inherited").await;
    let exception = add(&files, Some("locked"), "exception").await;
    let mut perms = PermissionEvaluator::new();
    for (path, deny) in [
        ("files/pub/hidden", Caps::SEE),
        ("files/pub/unlisted", Caps::FILE_LIST),
        ("files/pub/locked", Caps::SEE | Caps::FILE_LIST),
        ("files/pub/visible", Caps::FILE_DOWNLOAD),
    ] {
        rule(&mut perms, path, Caps(0), deny);
    }
    perms.insert_rule(
        "files/pub/locked/exception",
        Principal::Account(1),
        AclRule {
            allow: (Caps::SEE | Caps::FILE_LIST).0,
            deny: 0,
        },
    );
    let allows = |res: &str, caps| perms.allows(&user(), res, caps);
    let entries = visible_manifest(&files, "PUB", None, allows, |_| false)
        .await
        .unwrap();
    assert_eq!(entries, vec![
        pt::ManifestEntry::new(visible.id, "visible", [7; 32], 19).with_mime("text/plain"),
        pt::ManifestEntry::new(exception.id, "locked/exception", [7; 32], 19).with_mime("text/plain"),
    ], "hidden files expose none of their manifest fields; download rights do not hide visible metadata");
    let mut other = user();
    other.account_id = 2;
    let entries = visible_manifest(
        &files,
        "pub",
        None,
        |res, caps| perms.allows(&other, res, caps),
        |_| false,
    )
    .await
    .unwrap();
    assert_eq!(
        entries.iter().map(|n| n.node_id).collect::<Vec<_>>(),
        [visible.id]
    );
    assert_eq!(
        visible_manifest(&files, "pub", Some("locked"), allows, |_| false).await,
        Err(ErrorCode::NotFound),
        "a known hidden start folder cannot be opened"
    );
}

#[tokio::test]
async fn roots_use_canonical_area_rules_and_current_subject_masks() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    for area in ["pub", "secret", "unlisted"] {
        files.create_area(area, area, "").await.unwrap();
    }
    files.mkdir("pub", None, "denied", false).await.unwrap();
    let visible = add(&files, None, "visible").await;
    let mut perms = PermissionEvaluator::new();
    rule(&mut perms, "files/secret", Caps(0), Caps::SEE);
    rule(&mut perms, "files/unlisted", Caps(0), Caps::FILE_LIST);
    rule(&mut perms, "files/pub/denied", Caps(0), Caps::FILE_LIST);
    let allows = |res: &str, caps| perms.allows(&user(), res, caps);
    for area in ["secret", "SECRET", "missing"] {
        assert_eq!(
            visible_manifest(&files, area, None, allows, |_| false).await,
            Err(ErrorCode::NotFound)
        );
    }
    for area in ["unlisted", "UNLISTED"] {
        assert_eq!(
            visible_manifest(&files, area, None, allows, |_| false).await,
            Err(ErrorCode::Forbidden)
        );
    }
    assert_eq!(
        visible_manifest(&files, "pub", Some("denied"), allows, |_| false).await,
        Err(ErrorCode::Forbidden)
    );
    assert_eq!(
        visible_manifest(&files, "pub", Some("missing"), allows, |_| false).await,
        Err(ErrorCode::NotFound)
    );
    for (revoke, expected) in [
        (Caps::SEE, ErrorCode::NotFound),
        (Caps::FILE_LIST, ErrorCode::Forbidden),
    ] {
        let mut subject = user();
        subject.revoke_mask = revoke.0;
        assert_eq!(
            visible_manifest(
                &files,
                "pub",
                None,
                |res, caps| perms.allows(&subject, res, caps),
                |_| false
            )
            .await,
            Err(expected)
        );
    }
    let entries = visible_manifest(&files, "pub", Some(""), allows, |_| false)
        .await
        .unwrap();
    assert_eq!(
        entries[0].node_id, visible.id,
        "restored rights do not reuse a denied cached result"
    );
}

#[tokio::test]
async fn inspection_rights_do_not_override_child_acls_or_quarantine() {
    let files = FileService::new(rabbithole_store_server::open_in_memory().await.unwrap());
    files.create_area("pub", "Public", "").await.unwrap();
    files.mkdir("pub", None, "drop", true).await.unwrap();
    files
        .mkdir("pub", Some("drop"), "nested", false)
        .await
        .unwrap();
    let visible = add(&files, Some("drop/nested"), "visible").await;
    add(&files, Some("drop/nested"), "hidden").await;
    let mut perms = PermissionEvaluator::new();
    rule(
        &mut perms,
        "files/pub/drop/nested/hidden",
        Caps(0),
        Caps::SEE,
    );
    assert_eq!(
        visible_manifest(
            &files,
            "pub",
            Some("drop/nested"),
            |res, caps| perms.allows(&user(), res, caps),
            |_| false
        )
        .await,
        Err(ErrorCode::Forbidden)
    );
    for grant in [Caps::DROPBOX_VIEW, Caps::FILE_MANAGE] {
        let mut subject = user();
        subject.grant_mask = grant.0;
        let allows = |res: &str, caps| perms.allows(&subject, res, caps);
        let entries = visible_manifest(&files, "pub", Some("drop/nested"), allows, |_| false)
            .await
            .unwrap();
        assert_eq!(
            entries,
            vec![pt::ManifestEntry::new(visible.id, "visible", [7; 32], 19).with_mime("text/plain")]
        );
        assert!(
            visible_manifest(&files, "pub", Some("drop/nested"), allows, |_| true)
                .await
                .unwrap()
                .is_empty()
        );
    }
    let mut moderator = user();
    moderator.role = Role::Moderator;
    let entries = visible_manifest(
        &files,
        "pub",
        Some("drop/nested"),
        |res, caps| perms.allows(&moderator, res, caps),
        |_| true,
    )
    .await
    .unwrap();
    assert_eq!(
        entries.iter().map(|n| n.node_id).collect::<Vec<_>>(),
        [visible.id]
    );
}
