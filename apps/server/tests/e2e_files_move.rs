//! Files are renamed and moved over the wire: a folder takes its contents
//! along, the rules hold, and only a file manager (or a file's own uploader,
//! for its name) may do it.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::{
    AreaCreate, AreaReply, FileUpload, FolderCreate, FolderListRequest, NodeList, NodeMove,
    NodeRename, NodeReply,
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

async fn names_in(c: &mut Client, folder: Option<&str>) -> Vec<String> {
    let list: NodeList = c
        .request(&FolderListRequest::new("music", folder.map(str::to_string)))
        .await
        .unwrap();
    list.nodes.into_iter().map(|n| n.name).collect()
}

#[tokio::test]
async fn a_folder_is_renamed_and_moved_with_everything_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;
    let mut alice = login(&burrow, "alice").await;

    let _: AreaReply = ada
        .request(&AreaCreate::new("music", "Music"))
        .await
        .unwrap();
    let mixes: NodeReply = ada
        .request(&FolderCreate::new("music", None, "mixes"))
        .await
        .unwrap();
    let _: NodeReply = ada
        .request(&FolderCreate::new("music", None, "archive"))
        .await
        .unwrap();
    let side_a: NodeReply = ada
        .request(&FileUpload::new(
            "music",
            Some("mixes".into()),
            "side-a.txt",
            b"track one".to_vec(),
        ))
        .await
        .unwrap();
    let hers: NodeReply = alice
        .request(&FileUpload::new(
            "music",
            Some("mixes".into()),
            "notes.txt",
            b"mine".to_vec(),
        ))
        .await
        .unwrap();

    // Rename the folder: what is inside keeps its place under the new name.
    let renamed: NodeReply = ada
        .request(&NodeRename::new(mixes.node.id, "tapes"))
        .await
        .unwrap();
    assert_eq!(renamed.node.path, "tapes");
    assert_eq!(
        names_in(&mut ada, Some("tapes")).await,
        ["notes.txt", "side-a.txt"]
    );
    refused(
        ada.request::<_, NodeReply>(&NodeRename::new(mixes.node.id, "a/b"))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        ada.request::<_, NodeReply>(&NodeRename::new(mixes.node.id, "archive"))
            .await,
        ErrorCode::AlreadyExists,
    );

    // Move it under archive.
    let moved: NodeReply = ada
        .request(&NodeMove::new(mixes.node.id, Some("archive".into())))
        .await
        .unwrap();
    assert_eq!(moved.node.path, "archive/tapes");
    assert_eq!(names_in(&mut ada, Some("archive/tapes")).await.len(), 2);
    assert_eq!(names_in(&mut ada, None).await, ["archive"]);
    refused(
        ada.request::<_, NodeReply>(&NodeMove::new(mixes.node.id, Some("archive/tapes".into())))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        ada.request::<_, NodeReply>(&NodeMove::new(
            side_a.node.id,
            Some("archive/tapes/notes.txt".into()),
        ))
        .await,
        ErrorCode::BadRequest,
    );
    refused(
        ada.request::<_, NodeReply>(&NodeMove::new(side_a.node.id, Some("nowhere".into())))
            .await,
        ErrorCode::NotFound,
    );

    // A file to the root, and its old path is free again.
    let up: NodeReply = ada
        .request(&NodeMove::new(side_a.node.id, None))
        .await
        .unwrap();
    assert_eq!(up.node.path, "side-a.txt");
    assert_eq!(names_in(&mut ada, None).await, ["archive", "side-a.txt"]);

    // Alice may rename her own upload, and nothing else.
    let hers_renamed: NodeReply = alice
        .request(&NodeRename::new(hers.node.id, "my-notes.txt"))
        .await
        .unwrap();
    assert_eq!(hers_renamed.node.path, "archive/tapes/my-notes.txt");
    refused(
        alice
            .request::<_, NodeReply>(&NodeRename::new(side_a.node.id, "not-hers.txt"))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        alice
            .request::<_, NodeReply>(&NodeMove::new(hers.node.id, None))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        ada.request::<_, NodeReply>(&NodeRename::new(999_999, "ghost"))
            .await,
        ErrorCode::NotFound,
    );

    burrow.shutdown().await;
}
