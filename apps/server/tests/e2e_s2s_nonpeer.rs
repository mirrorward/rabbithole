//! Sends between burrows that are not federation peers, and do not run
//! federation at all: the destination connects to the source's QUIC client
//! port at an address the grant carries, pinned to the certificate the grant
//! names, proves its own server key, and fetches over that connection. Each
//! end's operator allows it separately, private addresses stay refused unless
//! allowed, and a grant is good only for the burrow it names.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_identity::IdentityKey;
use rabbithole_proto::filelib::{
    pull_state, PullGrantAsk, PullGrantIssued, PullGrantRequest, RemotePull, RemotePullAccepted,
    RemotePullStatus,
};
use rabbithole_proto::hello::PullSessionOpen;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

const PW: &str = "pw-pw-pw-pw";

fn config(dir: &std::path::Path, name: &str) -> ServerConfig {
    ServerConfig {
        name: name.into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        s2s_grants_enabled: true,
        s2s_pull_enabled: true,
        s2s_grants_to_any: true,
        s2s_pull_from_any: true,
        // Both burrows are on this machine.
        s2s_private_addresses: true,
        ..ServerConfig::default()
    }
}

async fn start(dir: &std::path::Path, name: &str) -> Burrow {
    let burrow = Burrow::start(config(dir, name)).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", PW, Role::User)
        .await
        .unwrap();
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

async fn seed(burrow: &Burrow, name: &str, bytes: &[u8]) -> i64 {
    let files = &burrow.shared.files;
    if files.areas().await.unwrap().is_empty() {
        files.create_area("music", "Music", "").await.unwrap();
    }
    let blob = burrow.shared.blobs.put(bytes).unwrap();
    files
        .add_file(
            "music",
            None,
            name,
            &blob.0,
            bytes.len() as i64,
            "text/plain",
            "",
            "",
            "x@y",
            1,
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn a_file_crosses_between_burrows_that_are_not_peers() {
    let work = tempfile::tempdir().unwrap();
    let source = start(&work.path().join("source"), "Lonely Source").await;
    let dest = start(&work.path().join("dest"), "Lonely Dest").await;
    let dest_key = dest.shared.server_key;
    assert!(!source.shared.peers.is_approved(&dest_key));
    assert!(
        source.federation_addr.is_none() && dest.federation_addr.is_none(),
        "no federation"
    );
    let tape = seed(&source, "tape.txt", b"sent without a federation").await;
    dest.shared
        .files
        .create_area("inbox", "Inbox", "")
        .await
        .unwrap();

    let mut alice_s = login(&source, "alice").await;
    let mut alice_d = login(&dest, "alice").await;
    // The app says where it reaches the source; the grant carries that,
    // signed, with the source's QUIC port and certificate.
    let ask = |node| PullGrantAsk::new(dest_key, vec![node], "127.0.0.1");
    let issued: PullGrantIssued = alice_s.request(&ask(tape)).await.unwrap();
    let grant = rabbithole_federation::pull::SignedPullGrant::from_bytes(&issued.grant).unwrap();
    assert_eq!(
        grant.grant.endpoints,
        vec![format!("127.0.0.1:{}", source.quic_addr.port())]
    );
    assert_eq!(grant.grant.tls_fingerprint, source.fingerprint.0);
    assert_eq!(
        grant.grant.version,
        rabbithole_federation::pull::PULL_GRANT_VERSION
    );

    let accepted: RemotePullAccepted = alice_d
        .request(&RemotePull::new(issued.grant.clone(), "inbox", None))
        .await
        .unwrap();
    assert_eq!(accepted.source, "Lonely Source");
    let done = until_done(&mut alice_d, accepted.pull_id).await;
    assert_eq!(
        (done.state, done.files_done),
        (pull_state::DONE, 1),
        "{done:?}"
    );
    let node = dest
        .shared
        .files
        .node_by_path("inbox", "tape.txt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        node.uploader,
        format!("alice@{}", dest.shared.origin_name())
    );
    assert_eq!(
        dest.shared.files.provenance(node.id).await.unwrap(),
        Some(("Lonely Source".to_string(), source.shared.server_key))
    );
    // The pull session closed with the pull.
    assert_eq!(dest.shared.s2s.running(), 0);

    // Each operator decides for their own end.
    source
        .shared
        .config
        .set_key("s2s_grants_to_any", "false")
        .unwrap();
    refused(
        alice_s.request::<_, PullGrantIssued>(&ask(tape)).await,
        ErrorCode::Unavailable,
    );
    source
        .shared
        .config
        .set_key("s2s_grants_to_any", "true")
        .unwrap();
    let issued: PullGrantIssued = alice_s.request(&ask(tape)).await.unwrap();
    dest.shared
        .config
        .set_key("s2s_pull_from_any", "false")
        .unwrap();
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(issued.grant.clone(), "inbox", None))
            .await,
        ErrorCode::Unavailable,
    );
    dest.shared
        .config
        .set_key("s2s_pull_from_any", "true")
        .unwrap();

    // Private addresses are refused unless the operator allows them.
    dest.shared
        .config
        .set_key("s2s_private_addresses", "false")
        .unwrap();
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(issued.grant.clone(), "inbox", None))
            .await,
        ErrorCode::Unavailable,
    );
    dest.shared
        .config
        .set_key("s2s_private_addresses", "true")
        .unwrap();

    // A grant with nowhere to connect is no use to a burrow that is not a
    // peer, and the refusals above spent nothing: the grant still works.
    let bare: PullGrantIssued = alice_s
        .request(&PullGrantRequest::new(dest_key, vec![tape]))
        .await
        .unwrap();
    refused(
        alice_d
            .request::<_, RemotePullAccepted>(&RemotePull::new(bare.grant, "inbox", None))
            .await,
        ErrorCode::Unavailable,
    );
    let accepted: RemotePullAccepted = alice_d
        .request(&RemotePull::new(issued.grant, "inbox", None))
        .await
        .unwrap();
    assert_eq!(
        until_done(&mut alice_d, accepted.pull_id).await.state,
        pull_state::DONE
    );
    assert!(dest
        .shared
        .files
        .node_by_path("inbox", "tape (2).txt")
        .await
        .unwrap()
        .is_some());

    source.shutdown().await;
    dest.shutdown().await;
}

#[tokio::test]
async fn a_pull_session_opens_only_for_the_key_the_grant_names() {
    let work = tempfile::tempdir().unwrap();
    let source = start(&work.path().join("source"), "Source").await;
    let dest = start(&work.path().join("dest"), "Dest").await;
    let dest_key = dest.shared.server_key;
    let tape = seed(&source, "tape.txt", b"for dest only").await;
    let mut alice_s = login(&source, "alice").await;
    let issued: PullGrantIssued = alice_s
        .request(&PullGrantAsk::new(dest_key, vec![tape], "127.0.0.1"))
        .await
        .unwrap();
    let quic = format!("127.0.0.1:{}", source.quic_addr.port());
    let pin = source.fingerprint.to_hex();

    // Another key, even one that proves itself, cannot use it.
    let stranger = IdentityKey::from_seed(&[7u8; 32]);
    let mut other = Client::connect_with_identity(
        &quic,
        Some("localhost"),
        Some(&pin),
        "e2e",
        "0",
        Some(&stranger),
    )
    .await
    .unwrap();
    refused(
        other
            .request_ack(&PullSessionOpen::new(issued.grant.clone()))
            .await,
        ErrorCode::Forbidden,
    );

    // Without a proven key, or over the WebSocket (no certificate to bind
    // the proof to), there is no pull session at all.
    let mut anonymous = Client::connect(&quic, Some("localhost"), Some(&pin), "e2e", "0")
        .await
        .unwrap();
    refused(
        anonymous
            .request_ack(&PullSessionOpen::new(issued.grant.clone()))
            .await,
        ErrorCode::Unauthenticated,
    );
    let dest_identity = IdentityKey::from_seed(&dest.shared.server_signing_seed);
    let ws = format!("ws://127.0.0.1:{}", source.ws_addr.port());
    let mut over_ws =
        Client::connect_with_identity(&ws, None, None, "e2e", "0", Some(&dest_identity))
            .await
            .unwrap();
    refused(
        over_ws
            .request_ack(&PullSessionOpen::new(issued.grant.clone()))
            .await,
        ErrorCode::Unauthenticated,
    );

    // The burrow the grant names, proving its key over QUIC, gets one.
    let mut named = Client::connect_with_identity(
        &quic,
        Some("localhost"),
        Some(&pin),
        "e2e",
        "0",
        Some(&dest_identity),
    )
    .await
    .unwrap();
    named
        .request_ack(&PullSessionOpen::new(issued.grant.clone()))
        .await
        .unwrap();

    // One session per grant at a time: a second is refused while the first
    // is open, and opens once it has closed (a destination that lost its
    // connection retries).
    let open_again = || async {
        let mut again = Client::connect_with_identity(
            &quic,
            Some("localhost"),
            Some(&pin),
            "e2e",
            "0",
            Some(&dest_identity),
        )
        .await
        .unwrap();
        let result = again
            .request_ack(&PullSessionOpen::new(issued.grant.clone()))
            .await;
        (again, result)
    };
    let (_, second) = open_again().await;
    refused(second, ErrorCode::AlreadyExists);
    named.close().await;
    let reopened = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (conn, result) = open_again().await;
            if result.is_ok() {
                return conn;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the grant opens a session again once the first has closed");
    drop(reopened);

    // With sends to non-peers switched off at the source, it gets none.
    source
        .shared
        .config
        .set_key("s2s_grants_to_any", "false")
        .unwrap();
    let mut refused_now = Client::connect_with_identity(
        &quic,
        Some("localhost"),
        Some(&pin),
        "e2e",
        "0",
        Some(&dest_identity),
    )
    .await
    .unwrap();
    refused(
        refused_now
            .request_ack(&PullSessionOpen::new(issued.grant))
            .await,
        ErrorCode::Unsupported,
    );

    source.shutdown().await;
    dest.shutdown().await;
}
