//! Wave 4.1 end-to-end tests: file libraries — areas, folders, upload/
//! download via blobs, metadata, ratings, search, drop boxes, aliases, ACLs.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::{AliasCreate, FileUpload, FolderCreate};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Files Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn guest(burrow: &Burrow) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_guest(Some("Dormouse".into())).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

#[tokio::test]
async fn area_folders_upload_download_metadata_search() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for (n, r) in [("admin", Role::Admin), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", r)
            .await
            .unwrap();
    }

    // Admin builds the library tree.
    let mut admin = login(&burrow, "admin").await;
    admin
        .area_create("warez", "Warez", "the good stuff")
        .await
        .unwrap();
    let utils = admin
        .folder_create(&FolderCreate::new("warez", None, "utils"))
        .await
        .unwrap();
    assert_eq!(utils.path, "utils");

    // Alice (regular user, has FILE_UPLOAD) uploads a file into utils.
    let mut alice = login(&burrow, "alice").await;
    let up = FileUpload::new(
        "warez",
        Some("utils".into()),
        "hello.txt",
        b"hello rabbit".to_vec(),
    )
    .with_meta("text/plain", "doc", "a greeting");
    let node = alice.file_upload(&up).await.unwrap();
    assert_eq!(node.path, "utils/hello.txt");
    assert_eq!(node.size, 12);
    assert!(node.uploader.starts_with("alice@"));
    assert!(node.blob_id.is_some());

    // Listing the folder shows the file.
    let kids = alice
        .folder_list("warez", Some("utils".into()))
        .await
        .unwrap();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].name, "hello.txt");

    // Download returns the bytes and bumps the counter.
    let content = alice.file_download(node.id).await.unwrap();
    assert_eq!(content.bytes, b"hello rabbit");
    assert_eq!(content.node.downloads, 1);

    // Metadata edit by the uploader.
    let edited = alice
        .set_file_metadata(node.id, "star", "the best greeting")
        .await
        .unwrap();
    assert_eq!(edited.comment, "the best greeting");
    assert_eq!(edited.icon, "star");

    // Ratings average across accounts.
    alice.rate_file(node.id, 5).await.unwrap();
    let rated = admin.rate_file(node.id, 3).await.unwrap();
    assert_eq!(rated.rating_count, 2);
    assert!((rated.rating_avg - 4.0).abs() < 1e-9);

    // Search finds it by name and by comment.
    assert_eq!(alice.file_search(None, "hello", 20).await.unwrap().len(), 1);
    assert_eq!(
        alice
            .file_search(Some("warez".into()), "greeting", 20)
            .await
            .unwrap()
            .len(),
        1
    );

    // An alias points at it and downloads resolve through.
    admin
        .alias_create(&AliasCreate::new(
            "warez",
            None,
            "shortcut",
            "utils/hello.txt",
        ))
        .await
        .unwrap();
    let via_alias = admin.folder_list("warez", None).await.unwrap();
    let alias = via_alias.iter().find(|n| n.name == "shortcut").unwrap();
    let content = admin.file_download(alias.id).await.unwrap();
    assert_eq!(content.bytes, b"hello rabbit");

    burrow.shutdown().await;
}

#[tokio::test]
async fn permissions_and_drop_boxes() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    for (n, r) in [("admin", Role::Admin), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(n, "pw-pw-pw", r)
            .await
            .unwrap();
    }

    let mut admin = login(&burrow, "admin").await;
    admin.area_create("pub", "Public", "").await.unwrap();
    // A drop box: alice can upload but not see its contents.
    admin
        .folder_create(&FolderCreate::new("pub", None, "incoming").dropbox())
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    // Regular users can't create areas or folders (needs FILE_MANAGE).
    assert!(matches!(
        alice.area_create("mine", "Mine", "").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice
            .folder_create(&FolderCreate::new("pub", None, "x"))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Alice uploads into the drop box (allowed with FILE_UPLOAD).
    let secret = FileUpload::new(
        "pub",
        Some("incoming".into()),
        "secret.txt",
        b"psst".to_vec(),
    );
    let node = alice.file_upload(&secret).await.unwrap();

    // Listing the drop box hides its contents from alice (no DROPBOX_VIEW)...
    assert!(alice
        .folder_list("pub", Some("incoming".into()))
        .await
        .unwrap()
        .is_empty());
    // ...but the admin (FILE_MANAGE) sees the drop.
    let seen = admin
        .folder_list("pub", Some("incoming".into()))
        .await
        .unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].name, "secret.txt");

    // Alice can't download drop-boxed content she can't see.
    assert!(matches!(
        alice.file_download(node.id).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    // Admin can.
    assert_eq!(admin.file_download(node.id).await.unwrap().bytes, b"psst");

    // Guests may browse but not upload.
    let mut anon = guest(&burrow).await;
    assert!(!anon.file_areas().await.unwrap().is_empty());
    let denied = FileUpload::new("pub", None, "nope.txt", b"x".to_vec());
    assert!(matches!(
        anon.file_upload(&denied).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    burrow.shutdown().await;
}
