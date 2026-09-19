//! File areas, as an operator manages them over the wire: make one, change
//! what it says, and remove it only once it is empty.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::{
    AreaCreate, AreaDelete, AreaList, AreaListRequest, AreaReply, AreaUpdate, FileUpload,
    FolderCreate, NodeDelete, NodeReply,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

const PW: &str = "pw-pw-pw-pw";

async fn start(dir: &std::path::Path) -> Burrow {
    let config = ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(config).await.unwrap();
    for (login, role) in [("ada", Role::Admin), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(login, PW, role)
            .await
            .unwrap();
    }
    burrow
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    c.auth_password(user, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn an_area_is_made_edited_and_removed_only_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;
    let mut alice = login(&burrow, "alice").await;

    let made: AreaReply = ada
        .request(&AreaCreate::new("music", "Music").with_description("Tapes."))
        .await
        .unwrap();
    assert_eq!(made.area.slug, "music");
    refused(
        ada.request::<_, AreaReply>(&AreaCreate::new("untitled", "  "))
            .await,
        ErrorCode::BadRequest,
    );

    // Edited: everything but the slug.
    ada.request_ack(&AreaUpdate::new(
        "music",
        "The Music Room",
        "Tapes and mixes.",
    ))
    .await
    .unwrap();
    let list: AreaList = alice.request(&AreaListRequest).await.unwrap();
    let area = list.areas.iter().find(|a| a.slug == "music").unwrap();
    assert_eq!(area.title, "The Music Room");
    assert_eq!(area.description, "Tapes and mixes.");
    refused(
        ada.request_ack(&AreaUpdate::new("music", "", "x")).await,
        ErrorCode::BadRequest,
    );
    refused(
        ada.request_ack(&AreaUpdate::new("nowhere", "Title", ""))
            .await,
        ErrorCode::NotFound,
    );

    // A member manages nothing.
    refused(
        alice
            .request_ack(&AreaUpdate::new("music", "Mine now", ""))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        alice.request_ack(&AreaDelete::new("music")).await,
        ErrorCode::Forbidden,
    );

    // With a folder and a file in it, it stays.
    let folder: NodeReply = ada
        .request(&FolderCreate::new("music", None, "mixes"))
        .await
        .unwrap();
    let file: NodeReply = ada
        .request(&FileUpload::new(
            "music",
            Some("mixes".into()),
            "side-a.txt",
            b"track one".to_vec(),
        ))
        .await
        .unwrap();
    refused(
        ada.request_ack(&AreaDelete::new("music")).await,
        ErrorCode::BadRequest,
    );

    // Emptied, it goes.
    ada.request_ack(&NodeDelete::new(file.node.id))
        .await
        .unwrap();
    ada.request_ack(&NodeDelete::new(folder.node.id))
        .await
        .unwrap();
    ada.request_ack(&AreaDelete::new("music")).await.unwrap();
    refused(
        ada.request_ack(&AreaDelete::new("music")).await,
        ErrorCode::NotFound,
    );
    let list: AreaList = ada.request(&AreaListRequest).await.unwrap();
    assert!(list.areas.iter().all(|a| a.slug != "music"));
    burrow.shutdown().await;
}
