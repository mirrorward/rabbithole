//! Pulls between burrows, end to end: a person on two federated burrows sends
//! a folder and a file from one to the other. The source grants only what
//! they may download, the destination checks everything before a byte moves,
//! fetches over the federation session it already holds, recreates the tree,
//! numbers a clash, files it all under the person, and remembers where it
//! came from. A grant is good once, for one burrow, and only for as long as
//! both sides allow pulls.

use std::time::Duration;

use burrow::federation::{dial_peer, DialOutcome, DialTarget};
use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::{
    pull_reason, pull_state, PullGrantIssued, PullGrantRequest, RemotePull, RemotePullAccepted,
    RemotePullCancel, RemotePullStatus,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use serde_json::json;

const PW: &str = "pw-pw-pw-pw";

fn fed_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Pulling Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        federation_enabled: true,
        federation_origin: dir.file_name().unwrap().to_string_lossy().into_owned(),
        federation_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        s2s_grants_enabled: true,
        s2s_pull_enabled: true,
        ..ServerConfig::default()
    }
}

fn target_for(b: &Burrow) -> DialTarget {
    DialTarget {
        addr: b.federation_addr.expect("federation enabled").to_string(),
        server_name: "localhost".into(),
        fingerprint: b.fingerprint,
        expected_key: Some(b.shared.server_key),
        expected_origin: b.shared.origin_name(),
    }
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

/// Bring up a source and a destination, approved both ways, with a live
/// federation session the destination can pull over.
async fn pair(work: &std::path::Path) -> (Burrow, Burrow) {
    let source = Burrow::start(fed_config(&work.join("source")))
        .await
        .unwrap();
    let dest = Burrow::start(fed_config(&work.join("dest"))).await.unwrap();
    let dest_key = dest.shared.server_key;
    // The destination knocks; the source approves it; it knocks again.
    assert!(matches!(
        dial_peer(dest.shared.clone(), target_for(&source))
            .await
            .unwrap(),
        DialOutcome::Pending(_)
    ));
    let resp = burrow::ctl::handle(
        &source.shared,
        &json!({"cmd": "peer-approve", "key": hex::encode(dest_key)}),
    )
    .await;
    assert_eq!(resp["ok"], json!(true), "{resp}");
    assert!(matches!(
        dial_peer(dest.shared.clone(), target_for(&source))
            .await
            .unwrap(),
        DialOutcome::Connected(_)
    ));
    let source_key = source.shared.server_key;
    tokio::time::timeout(Duration::from_secs(5), async {
        while dest.shared.s2s.link(&source_key).is_none()
            || source.shared.s2s.link(&dest_key).is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both ends offer the session to pulls");
    for (burrow, who) in [
        (&source, [("ada", Role::Admin), ("alice", Role::User)]),
        (&dest, [("root", Role::Admin), ("alice", Role::User)]),
    ] {
        for (login, role) in who {
            burrow
                .shared
                .auth
                .create_account(login, PW, role)
                .await
                .unwrap();
        }
    }
    (source, dest)
}

/// Put a file into `burrow`'s library directly, as if uploaded.
async fn seed(burrow: &Burrow, folder: Option<&str>, name: &str, bytes: &[u8]) -> i64 {
    let blob = burrow.shared.blobs.put(bytes).unwrap();
    burrow
        .shared
        .files
        .add_file(
            "music",
            folder,
            name,
            &blob.0,
            bytes.len() as i64,
            "text/plain",
            "",
            "",
            "ada@source",
            1,
        )
        .await
        .unwrap()
        .id
}

/// A grant for `node`, made out to `fetcher`.
async fn grant(c: &mut Client, fetcher: [u8; 32], node: i64) -> Vec<u8> {
    c.request::<_, PullGrantIssued>(&PullGrantRequest::new(fetcher, vec![node]))
        .await
        .unwrap()
        .grant
}

/// The pushes for `pull_id` until it ends.
async fn until_done(c: &mut Client, pull_id: u64) -> RemotePullStatus {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let frame = c.next_push().await.unwrap().expect("session open");
            if let Some(Ok(status)) = frame.decode::<RemotePullStatus>() {
                if status.pull_id == pull_id && status.state != pull_state::RUNNING {
                    return status;
                }
            }
        }
    })
    .await
    .expect("the pull ends")
}

#[tokio::test]
async fn a_folder_and_a_file_cross_from_one_burrow_to_another() {
    let work = tempfile::tempdir().unwrap();
    let (source, dest) = pair(work.path()).await;
    let dest_key = dest.shared.server_key;

    // At the source: a folder with a nested folder, a loose file, and a drop
    // box alice may put things in but not look into.
    let files = &source.shared.files;
    files.create_area("music", "Music", "").await.unwrap();
    let tapes = files.mkdir("music", None, "tapes", false).await.unwrap();
    files
        .mkdir("music", Some("tapes"), "b-sides", false)
        .await
        .unwrap();
    let drop = files.mkdir("music", None, "drop", true).await.unwrap();
    seed(&source, Some("tapes"), "side-a.txt", b"side a, all of it").await;
    seed(&source, Some("tapes/b-sides"), "side-b.txt", b"the b side").await;
    let notes = seed(&source, None, "notes.txt", b"liner notes").await;
    seed(&source, Some("drop"), "secret.txt", b"not yours to see").await;
    dest.shared
        .files
        .create_area("inbox", "Inbox", "")
        .await
        .unwrap();

    // Alice asks the source to let the destination fetch the folder and the
    // file. The drop box is hers to fill, not to read, so it is not sendable.
    let mut alice_s = login(&source, "alice").await;
    refused(
        alice_s
            .request::<_, PullGrantIssued>(&PullGrantRequest::new(dest_key, vec![drop.id]))
            .await,
        ErrorCode::Forbidden,
    );
    // A burrow the source does not federate with gets nothing.
    refused(
        alice_s
            .request::<_, PullGrantIssued>(&PullGrantRequest::new([7u8; 32], vec![notes]))
            .await,
        ErrorCode::Unavailable,
    );
    let issued: PullGrantIssued = alice_s
        .request(&PullGrantRequest::new(dest_key, vec![tapes.id, notes]))
        .await
        .unwrap();
    assert_eq!((issued.files, issued.skipped), (3, 0));
    let total = (b"side a, all of it".len() + b"the b side".len() + b"liner notes".len()) as u64;
    assert_eq!(issued.bytes, total);

    // At the destination a member may not recreate a folder (making folders
    // is a file manager's right), and the refusal spends nothing.
    let mut alice_d = login(&dest, "alice").await;
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(issued.grant.clone(), "inbox", None))
            .await,
        ErrorCode::Forbidden,
    );
    // The destination's admin hands it over, and watches it arrive.
    let mut root_d = login(&dest, "root").await;
    let accepted: RemotePullAccepted = root_d
        .request(&RemotePull::new(issued.grant.clone(), "inbox", None))
        .await
        .unwrap();
    assert_eq!((accepted.files, accepted.bytes), (3, total));
    assert_eq!(accepted.source, "source");
    let done = until_done(&mut root_d, accepted.pull_id).await;
    assert_eq!(done.state, pull_state::DONE, "{done:?}");
    assert_eq!(done.landed, "tapes");
    assert_eq!(
        (done.files_done, done.bytes_done, done.missing),
        (3, total, 0)
    );

    // The tree is recreated, the bytes are the source's, the files are hers,
    // and each remembers where it came from.
    let landed = &dest.shared.files;
    for (path, bytes) in [
        ("tapes/side-a.txt", &b"side a, all of it"[..]),
        ("tapes/b-sides/side-b.txt", &b"the b side"[..]),
        ("notes.txt", &b"liner notes"[..]),
    ] {
        let node = landed
            .node_by_path("inbox", path)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{path} landed"));
        assert_eq!(node.uploader, "root@dest", "{path}");
        let blob = dest
            .shared
            .blobs
            .get(&rabbithole_blobs::BlobId(node.blob_id.unwrap()))
            .unwrap();
        assert_eq!(blob, bytes, "{path}");
        assert_eq!(
            landed.provenance(node.id).await.unwrap(),
            Some(("source".to_string(), source.shared.server_key)),
            "{path}"
        );
    }
    let root_id = rabbithole_store_server::repo::AccountsRepo(&dest.shared.pool)
        .by_login("root")
        .await
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(landed.uploaded_bytes(root_id).await.unwrap(), total as i64);

    // The same grant twice is refused: it is spent, and stays spent in the
    // store, restart or no restart.
    refused(
        root_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(issued.grant.clone(), "inbox", None))
            .await,
        ErrorCode::AlreadyExists,
    );
    let spent = rabbithole_federation::pull::SignedPullGrant::from_bytes(&issued.grant).unwrap();
    assert!(!landed
        .spend_pull_grant(&spent.grant.nonce, spent.grant.expires_unix, 0)
        .await
        .unwrap());

    // A member can send files: into a drop box, even.
    let dropbox = landed.mkdir("inbox", None, "drop", true).await.unwrap();
    let file_only: PullGrantIssued = alice_s
        .request(&PullGrantRequest::new(dest_key, vec![notes]))
        .await
        .unwrap();
    let accepted: RemotePullAccepted = alice_d
        .request(&RemotePull::new(
            file_only.grant,
            "inbox",
            Some(dropbox.path.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(
        until_done(&mut alice_d, accepted.pull_id).await.state,
        pull_state::DONE
    );
    assert!(landed
        .node_by_path("inbox", "drop/notes.txt")
        .await
        .unwrap()
        .is_some());
    // But no folder goes into a drop box, whoever sends it: folders made
    // there would not hide what they hold.
    let folder_grant: PullGrantIssued = alice_s
        .request(&PullGrantRequest::new(dest_key, vec![tapes.id]))
        .await
        .unwrap();
    refused(
        root_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(
                folder_grant.grant.clone(),
                "inbox",
                Some(dropbox.path.clone()),
            ))
            .await,
        ErrorCode::Forbidden,
    );

    // Sending the folder again lands beside the first as "tapes (2)": a new
    // folder, never a merge into the one already there.
    let accepted: RemotePullAccepted = root_d
        .request(&RemotePull::new(folder_grant.grant, "inbox", None))
        .await
        .unwrap();
    let done = until_done(&mut root_d, accepted.pull_id).await;
    assert_eq!(done.state, pull_state::DONE);
    assert_eq!(done.landed, "tapes (2)");
    assert!(landed
        .node_by_path("inbox", "tapes (2)/b-sides/side-b.txt")
        .await
        .unwrap()
        .is_some());

    // A file that went at the source before the fetch is left out, and said.
    let gone = seed(&source, None, "gone.txt", b"here now").await;
    let grant: PullGrantIssued = alice_s
        .request(&PullGrantRequest::new(dest_key, vec![gone, notes]))
        .await
        .unwrap();
    files.delete(gone).await.unwrap();
    let accepted: RemotePullAccepted = alice_d
        .request(&RemotePull::new(grant.grant, "inbox", None))
        .await
        .unwrap();
    let done = until_done(&mut alice_d, accepted.pull_id).await;
    assert_eq!(done.state, pull_state::DONE);
    assert_eq!((done.files_done, done.missing), (1, 1));
    assert_eq!(done.landed, "notes (2).txt");

    // Two things of one name sent together land as two, told apart at the
    // source; and a quarantined file reads as absent, not as forbidden.
    let archive = files.mkdir("music", None, "archive", false).await.unwrap();
    files
        .mkdir("music", Some("archive"), "tapes", false)
        .await
        .unwrap();
    seed(&source, Some("archive/tapes"), "old.txt", b"older tapes").await;
    let old_tapes = files
        .node_by_path("music", "archive/tapes")
        .await
        .unwrap()
        .unwrap();
    let both: PullGrantIssued = alice_s
        .request(&PullGrantRequest::new(
            dest_key,
            vec![tapes.id, old_tapes.id],
        ))
        .await
        .unwrap();
    let twin = rabbithole_federation::pull::SignedPullGrant::from_bytes(&both.grant).unwrap();
    assert!(twin
        .grant
        .items
        .iter()
        .any(|i| i.rel_path == "tapes (2)/old.txt"));
    let _ = archive;
    let hidden = seed(&source, None, "hidden.txt", b"quarantined").await;
    source
        .shared
        .moderation
        .quarantine_set(
            rabbithole_proto::admin::subject_kind::FILE,
            blake3::hash(b"quarantined").as_bytes(),
            "spam",
            "ada",
        )
        .await
        .unwrap();
    refused(
        alice_s
            .request::<_, PullGrantIssued>(&PullGrantRequest::new(dest_key, vec![hidden]))
            .await,
        ErrorCode::NotFound,
    );
    assert!(landed
        .node_by_path("inbox", "notes (2).txt")
        .await
        .unwrap()
        .is_some());

    // Someone else's pull is not hers to cancel, and an unknown one is unknown.
    refused(
        alice_d.request_ack(&RemotePullCancel::new(9_999)).await,
        ErrorCode::NotFound,
    );

    source.shutdown().await;
    dest.shutdown().await;
}

#[tokio::test]
async fn the_destination_says_no_before_a_byte_moves() {
    let work = tempfile::tempdir().unwrap();
    let (source, dest) = pair(work.path()).await;
    let dest_key = dest.shared.server_key;
    source
        .shared
        .files
        .create_area("music", "Music", "")
        .await
        .unwrap();
    let big = seed(&source, None, "big.bin", &[5u8; 4096]).await;
    dest.shared
        .files
        .create_area("inbox", "Inbox", "")
        .await
        .unwrap();
    let mut alice_s = login(&source, "alice").await;
    let mut alice_d = login(&dest, "alice").await;

    // Pulls off at the destination.
    dest.shared
        .config
        .set_key("s2s_pull_enabled", "false")
        .unwrap();
    let g = grant(&mut alice_s, dest_key, big).await;
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(g.clone(), "inbox", None))
            .await,
        ErrorCode::Unsupported,
    );
    dest.shared
        .config
        .set_key("s2s_pull_enabled", "true")
        .unwrap();

    // A folder that is not there, and a file over the largest it takes.
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(
                g.clone(),
                "inbox",
                Some("nowhere".into()),
            ))
            .await,
        ErrorCode::NotFound,
    );
    dest.shared
        .config
        .set_key("upload_max_file_bytes", "1000")
        .unwrap();
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(g.clone(), "inbox", None))
            .await,
        ErrorCode::TooLarge,
    );
    dest.shared
        .config
        .set_key("upload_max_file_bytes", "0")
        .unwrap();

    // Over her space here.
    dest.shared
        .config
        .set_key("upload_quota_bytes", "100")
        .unwrap();
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(g.clone(), "inbox", None))
            .await,
        ErrorCode::TooLarge,
    );
    dest.shared
        .config
        .set_key("upload_quota_bytes", "0")
        .unwrap();

    // Content this burrow refuses.
    let root = *blake3::hash(&[5u8; 4096]).as_bytes();
    dest.shared
        .moderation
        .deny_add(&root, "not here", "root")
        .await
        .unwrap();
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(g.clone(), "inbox", None))
            .await,
        ErrorCode::Forbidden,
    );
    dest.shared
        .moderation
        .deny_remove(&root, "root")
        .await
        .unwrap();

    // A grant made out to another burrow, or altered, is no grant here.
    let mut bent = rabbithole_federation::pull::SignedPullGrant::from_bytes(&g).unwrap();
    bent.grant.items[0].size = 1;
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(bent.to_bytes(), "inbox", None))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(vec![1, 2, 3], "inbox", None))
            .await,
        ErrorCode::BadRequest,
    );

    // The refusals spent nothing: the grant still works.
    let accepted: RemotePullAccepted = alice_d
        .request(&RemotePull::new(g, "inbox", None))
        .await
        .unwrap();
    assert_eq!(
        until_done(&mut alice_d, accepted.pull_id).await.state,
        pull_state::DONE
    );

    // The source stops issuing, then stops serving: a grant issued before is
    // refused when the destination comes to fetch.
    let late = grant(&mut alice_s, dest_key, big).await;
    source
        .shared
        .config
        .set_key("s2s_grants_enabled", "false")
        .unwrap();
    refused(
        alice_s
            .request::<_, PullGrantIssued>(&PullGrantRequest::new(dest_key, vec![big]))
            .await,
        ErrorCode::Unsupported,
    );
    let accepted: RemotePullAccepted = alice_d
        .request(&RemotePull::new(late, "inbox", None))
        .await
        .unwrap();
    let ended = until_done(&mut alice_d, accepted.pull_id).await;
    assert_eq!(
        (ended.state, ended.reason),
        (pull_state::FAILED, pull_reason::SOURCE_REFUSED)
    );
    assert_eq!(dest.shared.s2s.running(), 0);

    source.shutdown().await;
    dest.shutdown().await;
}
