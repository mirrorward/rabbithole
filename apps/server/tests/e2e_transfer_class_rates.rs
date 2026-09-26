//! RH-6: current class policy on both download transports and proved ranges.

use std::time::{Duration, Instant};

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{AccountSet, ClassSet};
use rabbithole_proto::transfer::{FileChunk, FileChunkRequest, TransferResume, TransferTicket};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo::AccountsRepo;
use tokio::time::timeout;

const CHUNK: usize = 256 * 1024;
const RATE: u64 = (CHUNK * 2) as u64;
const PW: &str = "class-rate-password";

async fn start(path: &std::path::Path) -> (Burrow, i64, Vec<u8>) {
    let b = Burrow::start(ServerConfig {
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        transfer_rate_bytes_per_sec: RATE,
        max_concurrent_transfers: 1,
        ratelimit_enabled: false,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for (name, role) in [
        ("alice", Role::User),
        ("bob", Role::User),
        ("admin", Role::Admin),
    ] {
        b.shared.auth.create_account(name, PW, role).await.unwrap();
    }
    b.shared
        .files
        .create_area("pub", "Public", "")
        .await
        .unwrap();
    let bytes: Vec<_> = (0..CHUNK * 2).map(|i| (i % 251) as u8).collect();
    let blob = b.shared.blobs.put(&bytes).unwrap();
    let node = b
        .shared
        .files
        .add_file(
            "pub",
            None,
            "file.bin",
            &blob.0,
            bytes.len() as i64,
            "application/octet-stream",
            "",
            "",
            "admin",
            1,
        )
        .await
        .unwrap();
    (b, node.id, bytes)
}

async fn login(b: &Burrow, name: &str, quic: bool) -> Client {
    let fp = b.fingerprint.to_hex();
    let mut c = if quic {
        Client::connect(
            &b.quic_addr.to_string(),
            Some("localhost"),
            Some(&fp),
            "class-rates",
            "0",
        )
        .await
    } else {
        Client::connect(
            &format!("ws://{}", b.ws_addr),
            None,
            None,
            "class-rates",
            "0",
        )
        .await
    }
    .unwrap();
    c.auth_password(name, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(result: Result<T, ClientError>, code: ErrorCode) {
    assert!(
        matches!(&result, Err(ClientError::Refused(actual)) if *actual == code),
        "{result:?}"
    );
}

/// The reply precedes the existing per-chunk sleep. A subsequent protocol
/// roundtrip proves that the entire shaping interval has finished.
async fn chunk(c: &mut Client, ticket: &TransferTicket, expected: &[u8]) -> Duration {
    let start = Instant::now();
    let got: FileChunk = c
        .request(&FileChunkRequest::new(ticket.transfer_id, 0, CHUNK as u32))
        .await
        .unwrap();
    assert_eq!(got.bytes, expected[..CHUNK]);
    let _: TransferTicket = c
        .request(&TransferResume::new(ticket.transfer_id, ticket.token, 0))
        .await
        .unwrap();
    start.elapsed()
}

async fn class(admin: &mut Client, name: &str) {
    admin.request_ack(&ClassSet::new(name, 0)).await.unwrap();
    let mut req = AccountSet::new("alice");
    req.class = Some(name.into());
    admin.request_ack(&req).await.unwrap();
}

#[tokio::test]
async fn existing_ws_sessions_use_current_class_and_live_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let (b, node, bytes) = start(dir.path()).await;
    let mut admin = login(&b, "admin", false).await;
    let mut first = login(&b, "alice", false).await;
    let mut second = login(&b, "alice", false).await;
    let mut bob = login(&b, "bob", false).await;
    let ticket = first.download_ticket(node).await.unwrap();
    assert!(
        chunk(&mut first, &ticket, &bytes).await >= Duration::from_millis(450),
        "unmapped class inherits default"
    );
    refused(second.download_ticket(node).await, ErrorCode::RateLimited);

    b.shared
        .config
        .set_key("transfer_rate_bytes_per_sec", "1")
        .unwrap();
    b.shared
        .config
        .set_key("transfer_rate_by_class", "{ member = 0 }")
        .unwrap();
    timeout(Duration::from_secs(10), chunk(&mut first, &ticket, &bytes))
        .await
        .expect("explicit zero replaces a nonzero default");
    // AccountSet normally takes effect at next login for permission identity;
    // rate policy deliberately uses the current account at each request.
    class(&mut admin, "limited").await;
    b.shared
        .config
        .set_key(
            "transfer_rate_by_class",
            &format!("{{ member = 0, limited = {RATE} }}"),
        )
        .unwrap();
    for client in [&mut first, &mut second] {
        assert!(chunk(client, &ticket, &bytes).await >= Duration::from_millis(450));
    }
    let other = bob.download_ticket(node).await.unwrap();
    timeout(Duration::from_secs(10), chunk(&mut bob, &other, &bytes))
        .await
        .expect("unrelated account keeps its own class policy");
    bob.close_transfer(other.transfer_id).await.unwrap();
    b.shared
        .config
        .set_key("transfer_rate_by_class", "{ limited = 0 }")
        .unwrap();
    timeout(Duration::from_secs(10), chunk(&mut second, &ticket, &bytes))
        .await
        .expect("live map edits reach existing sessions");
    first.close_transfer(ticket.transfer_id).await.unwrap();
    b.shutdown().await;
}

#[tokio::test]
async fn native_bulk_uses_class_at_stream_start_and_preserves_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (b, node, bytes) = start(&dir.path().join("server")).await;
    let mut admin = login(&b, "admin", false).await;
    let mut native = login(&b, "alice", true).await;
    let ticket = native.download_ticket(node).await.unwrap();
    // The ticket and session predate both changes. Bulk resolves on opening
    // its data stream, not at login or ticket creation.
    class(&mut admin, "vip").await;
    b.shared
        .config
        .set_key("transfer_rate_bytes_per_sec", "1")
        .unwrap();
    b.shared
        .config
        .set_key("transfer_rate_by_class", "{ vip = 0 }")
        .unwrap();
    let dest = dir.path().join("unlimited.bin");
    timeout(
        Duration::from_secs(10),
        native.transfer_download_with(&ticket, &dest),
    )
    .await
    .expect("native explicit zero overrides the default")
    .unwrap();
    assert_eq!(std::fs::read(dest).unwrap(), bytes);

    b.shared
        .config
        .set_key("transfer_rate_by_class", &format!("{{ vip = {RATE} }}"))
        .unwrap();
    let dest = dir.path().join("limited.bin");
    let start = Instant::now();
    native.transfer_download(node, &dest).await.unwrap();
    // Bulk reads through EOF, after both chunk sleeps.
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert_eq!(std::fs::read(dest).unwrap(), bytes);
    b.shutdown().await;
}

#[tokio::test]
async fn an_active_bulk_stream_keeps_its_selected_rate_until_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let (b, node, bytes) = start(dir.path()).await;
    let mut client = login(&b, "alice", false).await;
    let ticket = client.download_ticket(node).await.unwrap();
    let account = AccountsRepo(&b.shared.pool)
        .by_login("alice")
        .await
        .unwrap()
        .unwrap();
    // A bounded duplex supplies an exact barrier inside the production bulk
    // handler: its first byte proves policy resolution has completed, while
    // the first chunk remains blocked until this test resumes reading.
    let (mut preamble, receiver) = tokio::io::duplex(1024);
    let (sender, mut output) = tokio::io::duplex(1024);
    let job = tokio::spawn(burrow::handlers9::serve_bulk_stream(
        b.shared.clone(),
        account.id,
        Box::new(sender),
        Box::new(receiver),
    ));
    let pre = rabbithole_proto::transfer::BulkPreamble::new(
        ticket.transfer_id,
        ticket.token,
        0,
        rabbithole_proto::transfer::DIR_DOWNLOAD,
    );
    rabbithole_net::write_framed(&mut preamble, &postcard::to_allocvec(&pre).unwrap())
        .await
        .unwrap();
    preamble.shutdown().await.unwrap();
    let mut first = [0];
    output.read_exact(&mut first).await.unwrap();
    b.shared
        .config
        .set_key("transfer_rate_bytes_per_sec", "1")
        .unwrap();
    let mut received = vec![first[0]];
    timeout(Duration::from_secs(10), output.read_to_end(&mut received))
        .await
        .expect("an active stream retains its start-time rate")
        .unwrap();
    job.await.unwrap();
    assert_eq!(received, bytes);
    assert_eq!(b.shared.transfers.count_for_account(account.id), 0);
    b.shutdown().await;
}

#[tokio::test]
async fn proved_ranges_use_class_rate_and_account_failures_send_no_payload() {
    let dir = tempfile::tempdir().unwrap();
    let (b, node, bytes) = start(&dir.path().join("server")).await;
    let mut ws = login(&b, "alice", false).await;
    let mut native = login(&b, "alice", true).await;
    let ticket = ws.download_ticket(node).await.unwrap();
    // Build the proof cache while unlimited so cache work cannot be mistaken
    // for rate shaping in the subsequent measured request.
    b.shared
        .config
        .set_key("transfer_rate_by_class", "{ member = 0 }")
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            match ws.proved_range(ticket.transfer_id, 0, CHUNK as u32).await {
                Ok(_) => break,
                Err(ClientError::Refused(ErrorCode::Unavailable)) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                other => panic!("{other:?}"),
            }
        }
    })
    .await
    .unwrap();
    b.shared
        .config
        .set_key("transfer_rate_bytes_per_sec", "0")
        .unwrap();
    b.shared
        .config
        .set_key("transfer_rate_by_class", &format!("{{ member = {RATE} }}"))
        .unwrap();
    let start = Instant::now();
    let proof = ws
        .proved_range(ticket.transfer_id, 0, CHUNK as u32)
        .await
        .unwrap();
    let proved = rabbithole_swarm::decode_proved(
        ticket.root,
        proof.size,
        proof.offset,
        proof.len as u64,
        &proof.stream,
    )
    .unwrap();
    assert_eq!(proved.bytes, bytes[..CHUNK]);
    let _: TransferTicket = ws
        .request(&TransferResume::new(ticket.transfer_id, ticket.token, 0))
        .await
        .unwrap();
    assert!(
        start.elapsed() >= Duration::from_millis(450),
        "proved bytes obey the class override"
    );

    // Persisted disablement may race a session's kick notification. The data
    // paths independently refuse before reading or transmitting the payload.
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", None, None, Some(true))
        .await
        .unwrap();
    refused(
        ws.request::<_, FileChunk>(&FileChunkRequest::new(ticket.transfer_id, 0, 1024))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        ws.proved_range(ticket.transfer_id, 0, 1024).await,
        ErrorCode::Forbidden,
    );
    let refused_path = dir.path().join("disabled.bin");
    assert!(native
        .transfer_download_with(&ticket, &refused_path)
        .await
        .is_err());
    assert_eq!(std::fs::metadata(refused_path).unwrap().len(), 0);
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", None, None, Some(false))
        .await
        .unwrap();
    b.shared.pool.close().await;
    refused(
        ws.request::<_, FileChunk>(&FileChunkRequest::new(ticket.transfer_id, 0, 1024))
            .await,
        ErrorCode::Unavailable,
    );
    refused(
        ws.proved_range(ticket.transfer_id, 0, 1024).await,
        ErrorCode::Unavailable,
    );
    let failed_path = dir.path().join("failed-lookup.bin");
    assert!(native
        .transfer_download_with(&ticket, &failed_path)
        .await
        .is_err());
    assert_eq!(std::fs::metadata(failed_path).unwrap().len(), 0);
    b.shutdown().await;
}
