//! The largest file and each person's space, as a client meets them: the
//! burrow says what they are before anything is sent, refuses a file that
//! breaks either when the transfer opens, and checks again on the bytes that
//! arrive, because a declared size is only a claim and the operator may
//! change the limit in between.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::filelib::{
    AreaCreate, AreaReply, FileUpload, NodeReply, UploadLimits, UploadLimitsRequest,
};
use rabbithole_proto::transfer::{FileChunkPut, TransferOpen, TransferTicket, UploadFinish};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

const PW: &str = "pw-pw-pw-pw";
const CHUNK: usize = 256 * 1024;

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

fn open(name: &str, bytes: &[u8]) -> TransferOpen {
    TransferOpen::upload(
        "music",
        None,
        name,
        bytes.len() as u64,
        *blake3::hash(bytes).as_bytes(),
    )
}

/// Send `bytes` in 256 KiB chunks on an open ticket, then finish it.
async fn send_and_finish(
    c: &mut Client,
    ticket: &TransferTicket,
    bytes: &[u8],
) -> Result<NodeReply, ClientError> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let end = (offset + CHUNK).min(bytes.len());
        c.request_ack(&FileChunkPut::new(
            ticket.transfer_id,
            offset as u64,
            end == bytes.len(),
            bytes[offset..end].to_vec(),
        ))
        .await?;
        offset = end;
    }
    c.request(&UploadFinish::new(ticket.transfer_id)).await
}

#[tokio::test]
async fn the_largest_file_and_each_persons_space_are_announced_and_held() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;
    let _: AreaReply = ada
        .request(&AreaCreate::new("music", "Music"))
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;

    // Out of the box: fifty mebibytes a file, no quota, nothing used.
    let limits: UploadLimits = alice.request(&UploadLimitsRequest).await.unwrap();
    assert_eq!(limits, UploadLimits::new(50 * 1024 * 1024, 0, 0));

    // The operator lowers the largest file to a megabyte.
    burrow
        .shared
        .config
        .set_key("upload_max_file_bytes", "1000000")
        .unwrap();
    let big = vec![7u8; 1_000_001];
    refused(
        alice
            .request::<_, TransferTicket>(&open("big.bin", &big))
            .await,
        ErrorCode::TooLarge,
    );

    // Just under it goes through, chunk by chunk, and counts as used.
    let fits = vec![3u8; 900_000];
    let ticket: TransferTicket = alice.request(&open("fits.bin", &fits)).await.unwrap();
    assert_eq!(ticket.server_have, 0);
    let landed = send_and_finish(&mut alice, &ticket, &fits).await.unwrap();
    assert_eq!(landed.node.size, 900_000);
    let limits: UploadLimits = alice.request(&UploadLimitsRequest).await.unwrap();
    assert_eq!(limits.max_file_bytes, 1_000_000);
    assert_eq!(limits.used_bytes, 900_000);

    // A limit lowered while a file is on its way is held at the finish.
    let second = vec![5u8; 600_000];
    let ticket: TransferTicket = alice.request(&open("second.bin", &second)).await.unwrap();
    burrow
        .shared
        .config
        .set_key("upload_max_file_bytes", "500000")
        .unwrap();
    refused(
        send_and_finish(&mut alice, &ticket, &second).await,
        ErrorCode::TooLarge,
    );

    // The inline path is held to the same limit.
    refused(
        alice
            .request::<_, NodeReply>(&FileUpload::new(
                "music",
                None,
                "inline.bin",
                vec![1u8; 500_001],
            ))
            .await,
        ErrorCode::TooLarge,
    );

    // Each person's space: 900 000 are kept, so 700 000 more would not fit
    // in 1 500 000, and 500 000 would.
    burrow
        .shared
        .config
        .set_key("upload_max_file_bytes", "0")
        .unwrap();
    burrow
        .shared
        .config
        .set_key("upload_quota_bytes", "1500000")
        .unwrap();
    refused(
        alice
            .request::<_, TransferTicket>(&open("over.bin", &vec![9u8; 700_000]))
            .await,
        ErrorCode::TooLarge,
    );
    let last = vec![4u8; 500_000];
    let ticket: TransferTicket = alice.request(&open("last.bin", &last)).await.unwrap();
    send_and_finish(&mut alice, &ticket, &last).await.unwrap();
    let limits: UploadLimits = alice.request(&UploadLimitsRequest).await.unwrap();
    assert_eq!(limits, UploadLimits::new(0, 1_500_000, 1_400_000));

    // Someone else's space is their own.
    let limits: UploadLimits = ada.request(&UploadLimitsRequest).await.unwrap();
    assert_eq!(limits.used_bytes, 0);

    burrow.shutdown().await;
}
