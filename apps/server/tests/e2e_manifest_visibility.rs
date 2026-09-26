//! RH-170: native manifests use session rights without granting downloads.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{subject_kind, AccountSet, ClassSet, QuarantineClear, QuarantineSet};
use rabbithole_proto::transfer::{FolderManifest, FolderManifestRequest, ManifestEntry};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Caps, Role, ServerConfig};
use rabbithole_store_server::repo6::FileNodeRow;

const PW: &str = "manifest-test-password";

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
        ("mod", Role::Moderator),
    ] {
        b.shared.auth.create_account(name, PW, role).await.unwrap();
    }
    b.shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();
    b
}

async fn connect(b: &Burrow) -> Client {
    Client::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "manifest-test",
        "0",
    )
    .await
    .unwrap()
}

async fn login(b: &Burrow, name: &str) -> Client {
    let mut c = connect(b).await;
    c.auth_password(name, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn add(b: &Burrow, parent: Option<&str>, name: &str, bytes: &[u8]) -> FileNodeRow {
    let blob = b.shared.blobs.put(bytes).unwrap();
    b.shared
        .files
        .add_file(
            "pub",
            parent,
            name,
            &blob.0,
            bytes.len() as i64,
            "text/plain",
            "",
            "",
            "admin",
            1,
        )
        .await
        .unwrap()
}

async fn manifest(c: &mut Client, path: Option<&str>) -> Result<FolderManifest, ClientError> {
    c.request(&FolderManifestRequest::new("PUB", path.map(str::to_owned)))
        .await
}

fn refused<T: std::fmt::Debug>(result: Result<T, ClientError>, code: ErrorCode) {
    assert!(
        matches!(&result, Err(ClientError::Refused(actual)) if *actual == code),
        "{result:?}"
    );
}

fn entry(node: &FileNodeRow, relative: &str) -> ManifestEntry {
    ManifestEntry::new(node.id, relative, node.blob_id.unwrap(), node.size as u64)
        .with_mime(&node.mime)
}

#[tokio::test]
async fn live_class_changes_and_account_reassignment_preserve_inspection_and_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
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
    let file = add(&b, Some("drop/nested"), "secret.txt", b"manifest secret").await;
    let mut alice = login(&b, "alice").await;
    let mut admin = login(&b, "admin").await;
    let mut moderator = login(&b, "mod").await;
    assert!(manifest(&mut alice, None).await.unwrap().entries.is_empty());
    refused(
        manifest(&mut alice, Some("drop/nested")).await,
        ErrorCode::Forbidden,
    );

    // Class masks are re-evaluated on each request in an existing session.
    for extra in [Caps::DROPBOX_VIEW, Caps::FILE_MANAGE] {
        admin
            .request_ack(&ClassSet::new("member", extra.0))
            .await
            .unwrap();
        assert_eq!(
            manifest(&mut alice, Some("drop/nested"))
                .await
                .unwrap()
                .entries,
            vec![entry(&file, "secret.txt")]
        );
    }
    admin
        .request_ack(&ClassSet::new("member", 0))
        .await
        .unwrap();
    refused(
        manifest(&mut alice, Some("drop/nested")).await,
        ErrorCode::Forbidden,
    );

    // AccountSet deliberately changes role/class at the next login. Verify
    // both that established session contract and the newly assigned class.
    admin
        .request_ack(&ClassSet::new("inspector", Caps::DROPBOX_VIEW.0))
        .await
        .unwrap();
    let mut change = AccountSet::new("alice");
    change.class = Some("inspector".into());
    admin.request_ack(&change).await.unwrap();
    refused(
        manifest(&mut alice, Some("drop/nested")).await,
        ErrorCode::Forbidden,
    );
    let mut reassigned = login(&b, "alice").await;
    assert_eq!(
        manifest(&mut reassigned, Some("drop/nested"))
            .await
            .unwrap()
            .entries,
        vec![entry(&file, "secret.txt")]
    );
    admin
        .request_ack(&ClassSet::new("inspector", 0))
        .await
        .unwrap();
    refused(
        manifest(&mut reassigned, Some("drop/nested")).await,
        ErrorCode::Forbidden,
    );
    admin
        .request_ack(&ClassSet::new("inspector", Caps::DROPBOX_VIEW.0))
        .await
        .unwrap();

    admin
        .request_ack(&QuarantineSet::new(
            subject_kind::FILE,
            file.blob_id.unwrap().to_vec(),
            "review",
        ))
        .await
        .unwrap();
    assert!(manifest(&mut reassigned, Some("drop/nested"))
        .await
        .unwrap()
        .entries
        .is_empty());
    assert_eq!(
        manifest(&mut moderator, Some("drop/nested"))
            .await
            .unwrap()
            .entries,
        vec![entry(&file, "secret.txt")]
    );
    admin
        .request_ack(&QuarantineClear::new(
            subject_kind::FILE,
            file.blob_id.unwrap().to_vec(),
        ))
        .await
        .unwrap();
    assert_eq!(
        manifest(&mut reassigned, Some("drop/nested"))
            .await
            .unwrap()
            .entries,
        vec![entry(&file, "secret.txt")]
    );
    b.shutdown().await;
}

#[tokio::test]
async fn guest_manifest_keeps_visible_metadata_when_download_is_denied() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .files
        .mkdir("pub", None, "ordinary", false)
        .await
        .unwrap();
    let bytes = b"public manifest data";
    let file = add(&b, Some("ordinary"), "readme.txt", bytes).await;
    let mut guest = connect(&b).await;
    guest
        .auth_guest(Some("manifest-guest".into()))
        .await
        .unwrap();
    guest.expect_welcome().await.unwrap();
    assert_eq!(
        manifest(&mut guest, None).await.unwrap().entries,
        vec![entry(&file, "ordinary/readme.txt")]
    );
    assert_eq!(
        manifest(&mut guest, Some("ordinary"))
            .await
            .unwrap()
            .entries,
        vec![entry(&file, "readme.txt")]
    );
    refused(guest.file_download(file.id).await, ErrorCode::Forbidden);
    refused(
        manifest(&mut guest, Some("missing")).await,
        ErrorCode::NotFound,
    );
    let mut alice = login(&b, "alice").await;
    assert_eq!(alice.file_download(file.id).await.unwrap().bytes, bytes);
    b.shutdown().await;
}
