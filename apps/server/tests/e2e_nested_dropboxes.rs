//! RH-169: known IDs, aliases and manifests honor every drop-box ancestor.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_federation::pull::SignedPullGrant;
use rabbithole_proto::admin::ClassSet;
use rabbithole_proto::filelib::{
    FileContent, FileDownloadRequest, NodeMove, NodeReply, PullGrantIssued, PullGrantRequest,
};
use rabbithole_proto::transfer::{
    FileByContent, FileByContentRequest, FolderManifest, FolderManifestRequest, TransferAbort,
    TransferOpen, TransferTicket,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Caps, Role, ServerConfig};
use rabbithole_store_server::repo6::FileNodeRow;

const PW: &str = "nested-dropbox-password";

struct Tree {
    public: FileNodeRow,
    hidden: FileNodeRow,
    nested: FileNodeRow,
    into: FileNodeRow,
    out: FileNodeRow,
    public_alias: FileNodeRow,
}

async fn start(path: &std::path::Path) -> (Burrow, Tree) {
    let b = Burrow::start(ServerConfig {
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        s2s_grants_enabled: true,
        s2s_grants_to_any: true,
        ratelimit_enabled: false,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for (name, role) in [("admin", Role::Admin), ("alice", Role::User)] {
        b.shared.auth.create_account(name, PW, role).await.unwrap();
    }
    let files = &b.shared.files;
    files.create_area("pub", "Public", "").await.unwrap();
    files.mkdir("pub", None, "drop", true).await.unwrap();
    let nested = files
        .mkdir("pub", Some("drop"), "nested", false)
        .await
        .unwrap();
    files.mkdir("pub", None, "ordinary", false).await.unwrap();
    let public = seed(&b, Some("ordinary"), "public.txt", b"public bytes").await;
    let hidden = seed(&b, Some("drop/nested"), "secret.txt", b"hidden bytes").await;
    let into = files
        .add_alias("pub", None, "into.txt", &hidden.path)
        .await
        .unwrap();
    let out = files
        .add_alias("pub", Some("drop/nested"), "out.txt", &public.path)
        .await
        .unwrap();
    let public_alias = files
        .add_alias("pub", None, "public-alias.txt", &public.path)
        .await
        .unwrap();
    (
        b,
        Tree {
            public,
            hidden,
            nested,
            into,
            out,
            public_alias,
        },
    )
}

async fn seed(b: &Burrow, parent: Option<&str>, name: &str, body: &[u8]) -> FileNodeRow {
    let blob = b.shared.blobs.put(body).unwrap();
    b.shared
        .files
        .add_file(
            "pub",
            parent,
            name,
            &blob.0,
            body.len() as i64,
            "text/plain",
            "",
            "",
            "admin",
            1,
        )
        .await
        .unwrap()
}

async fn login(b: &Burrow, name: &str) -> Client {
    let mut c = Client::connect(&format!("ws://{}", b.ws_addr), None, None, "dropboxes", "0")
        .await
        .unwrap();
    c.auth_password(name, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(result: Result<T, ClientError>, code: ErrorCode) {
    assert!(
        matches!(&result, Err(ClientError::Refused(got)) if *got == code),
        "expected {code:?}, got {result:?}"
    );
}

async fn blocked(c: &mut Client, id: i64) {
    refused(
        c.request::<_, FileContent>(&FileDownloadRequest::new(id))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        c.request::<_, TransferTicket>(&TransferOpen::download(id))
            .await,
        ErrorCode::Forbidden,
    );
}

async fn readable(c: &mut Client, id: i64, body: &[u8]) {
    let inline: FileContent = c.request(&FileDownloadRequest::new(id)).await.unwrap();
    assert_eq!(inline.bytes, body);
    let ticket: TransferTicket = c.request(&TransferOpen::download(id)).await.unwrap();
    assert_eq!(ticket.size, body.len() as u64);
    c.request_ack(&TransferAbort::new(ticket.transfer_id))
        .await
        .unwrap();
}

#[tokio::test]
async fn native_downloads_check_alias_location_and_target_and_preserve_both_exceptions() {
    let dir = tempfile::tempdir().unwrap();
    let (b, t) = start(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut admin = login(&b, "admin").await;
    for id in [t.hidden.id, t.into.id, t.out.id] {
        blocked(&mut alice, id).await;
    }
    readable(&mut alice, t.public.id, b"public bytes").await;
    readable(&mut alice, t.public_alias.id, b"public bytes").await;
    // A viewer and a manager without DROPBOX_VIEW each retain their own
    // exception. The same live account observes class edits immediately.
    for extra in [Caps::DROPBOX_VIEW, Caps::FILE_MANAGE] {
        admin
            .request_ack(&ClassSet::new("member", extra.0))
            .await
            .unwrap();
        readable(&mut alice, t.hidden.id, b"hidden bytes").await;
        readable(&mut alice, t.into.id, b"hidden bytes").await;
        readable(&mut alice, t.out.id, b"public bytes").await;
    }
    admin
        .request_ack(&ClassSet::new("member", 0))
        .await
        .unwrap();
    blocked(&mut alice, t.hidden.id).await;
    // Move the ancestor out, then back: unchanged IDs and aliases must
    // reflect current placement at each new authorization.
    let _: NodeReply = admin
        .request(&NodeMove::new(t.nested.id, None))
        .await
        .unwrap();
    readable(&mut alice, t.hidden.id, b"hidden bytes").await;
    readable(&mut alice, t.into.id, b"hidden bytes").await;
    readable(&mut alice, t.out.id, b"public bytes").await;
    let _: NodeReply = admin
        .request(&NodeMove::new(t.nested.id, Some("drop".into())))
        .await
        .unwrap();
    for id in [t.hidden.id, t.into.id, t.out.id] {
        blocked(&mut alice, id).await;
    }
    b.shutdown().await;
}

#[tokio::test]
async fn native_manifests_and_content_lookup_do_not_advertise_nested_hidden_content() {
    let dir = tempfile::tempdir().unwrap();
    let (b, t) = start(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut admin = login(&b, "admin").await;
    let root: FolderManifest = alice
        .request(&FolderManifestRequest::new("pub", None))
        .await
        .unwrap();
    assert_eq!(
        root.entries.iter().map(|n| n.node_id).collect::<Vec<_>>(),
        vec![t.public.id]
    );
    for path in ["drop", "drop/nested"] {
        refused(
            alice
                .request::<_, FolderManifest>(&FolderManifestRequest::new("pub", Some(path.into())))
                .await,
            ErrorCode::Forbidden,
        );
    }
    let hidden_root = t.hidden.blob_id.unwrap();
    refused(
        alice
            .request::<_, FileByContent>(&FileByContentRequest::new(hidden_root))
            .await,
        ErrorCode::NotFound,
    );
    let public: FileByContent = alice
        .request(&FileByContentRequest::new(t.public.blob_id.unwrap()))
        .await
        .unwrap();
    assert_eq!(public.node_id, t.public.id);
    for extra in [Caps::DROPBOX_VIEW, Caps::FILE_MANAGE] {
        admin
            .request_ack(&ClassSet::new("member", extra.0))
            .await
            .unwrap();
        let manifest: FolderManifest = alice
            .request(&FolderManifestRequest::new(
                "pub",
                Some("drop/nested".into()),
            ))
            .await
            .unwrap();
        assert_eq!(
            manifest
                .entries
                .iter()
                .map(|n| n.node_id)
                .collect::<Vec<_>>(),
            vec![t.hidden.id]
        );
        let found: FileByContent = alice
            .request(&FileByContentRequest::new(hidden_root))
            .await
            .unwrap();
        assert_eq!(found.node_id, t.hidden.id);
    }
    // A public duplicate of the same blob remains discoverable.
    admin
        .request_ack(&ClassSet::new("member", 0))
        .await
        .unwrap();
    let copy = seed(&b, None, "copy.txt", b"hidden bytes").await;
    let found: FileByContent = alice
        .request(&FileByContentRequest::new(hidden_root))
        .await
        .unwrap();
    assert_eq!(found.node_id, copy.id);
    b.shutdown().await;
}

#[tokio::test]
async fn s2s_grants_check_nested_folder_and_both_alias_placements() {
    let dir = tempfile::tempdir().unwrap();
    let (b, t) = start(dir.path()).await;
    let mut alice = login(&b, "alice").await;
    let mut admin = login(&b, "admin").await;
    let ask = |id| PullGrantRequest::new([42; 32], vec![id]);
    for id in [t.hidden.id, t.nested.id, t.into.id, t.out.id] {
        refused(
            alice.request::<_, PullGrantIssued>(&ask(id)).await,
            ErrorCode::Forbidden,
        );
    }
    let public: PullGrantIssued = alice.request(&ask(t.public_alias.id)).await.unwrap();
    let grant = SignedPullGrant::from_bytes(&public.grant).unwrap();
    assert_eq!(grant.grant.items[0].node_id, t.public.id);
    for extra in [Caps::DROPBOX_VIEW, Caps::FILE_MANAGE] {
        admin
            .request_ack(&ClassSet::new("member", extra.0))
            .await
            .unwrap();
        for id in [t.hidden.id, t.nested.id, t.into.id, t.out.id] {
            let reply: PullGrantIssued = alice.request(&ask(id)).await.unwrap();
            assert_eq!(
                SignedPullGrant::from_bytes(&reply.grant)
                    .unwrap()
                    .grant
                    .items
                    .len(),
                1
            );
        }
    }
    admin
        .request_ack(&ClassSet::new("member", 0))
        .await
        .unwrap();
    let _: NodeReply = admin
        .request(&NodeMove::new(t.nested.id, None))
        .await
        .unwrap();
    let moved: PullGrantIssued = alice.request(&ask(t.nested.id)).await.unwrap();
    assert_eq!(
        SignedPullGrant::from_bytes(&moved.grant)
            .unwrap()
            .grant
            .items[0]
            .node_id,
        t.hidden.id
    );
    b.shutdown().await;
}

#[tokio::test]
async fn federation_catalog_tracks_nested_folder_moves_without_advertising_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let (b, t) = start(dir.path()).await;
    let names = |catalog: rabbithole_federation::SignedCatalog| {
        catalog
            .catalog
            .entries
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(burrow::fed_catalog::local_catalog(&b.shared).await.unwrap()),
        vec!["public.txt"]
    );
    b.shared.files.move_to(t.nested.id, None).await.unwrap();
    let mut visible = names(burrow::fed_catalog::local_catalog(&b.shared).await.unwrap());
    visible.sort();
    assert_eq!(visible, vec!["public.txt", "secret.txt"]);
    b.shared
        .files
        .move_to(t.nested.id, Some("drop"))
        .await
        .unwrap();
    assert_eq!(
        names(burrow::fed_catalog::local_catalog(&b.shared).await.unwrap()),
        vec!["public.txt"]
    );
    b.shutdown().await;
}
