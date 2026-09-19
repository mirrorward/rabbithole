//! The burrow as a swarm source: any range of a file it stores comes with
//! its proof (the Bao stream), under a download ticket, and the client
//! checks it against the file's root before keeping a byte. The proofs are
//! made in the background the first time, and the asker is told to ask
//! again meanwhile. A stored file that no longer matches its id is never
//! sent as proved.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::transfer::PROVED_RANGE_MAX;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

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

/// A proved range, asked for again while the burrow makes the file's proofs.
async fn proved(
    c: &mut Client,
    transfer_id: u64,
    offset: u64,
    len: u32,
) -> Result<rabbithole_proto::transfer::ProvedRange, ClientError> {
    for _ in 0..200 {
        match c.proved_range(transfer_id, offset, len).await {
            Err(ClientError::Refused(ErrorCode::Unavailable)) => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await
            }
            other => return other,
        }
    }
    panic!("the proofs were never made");
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn the_burrow_sends_any_range_of_a_file_with_its_proof() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(ServerConfig {
        name: "Proof Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: work.path().to_path_buf(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for who in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(who, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let body: Vec<u8> = (0..3 * 1024 * 1024 + 777)
        .map(|i| (i % 241) as u8)
        .collect();
    let root = *blake3::hash(&body).as_bytes();
    let files = &burrow.shared.files;
    files.create_area("music", "Music", "").await.unwrap();
    let blob = burrow.shared.blobs.put(&body).unwrap();
    let node = files
        .add_file(
            "music",
            None,
            "tape.bin",
            &blob.0,
            body.len() as i64,
            "application/octet-stream",
            "",
            "",
            "x@y",
            1,
        )
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let ticket = alice.download_ticket(node.id).await.unwrap();
    let check = |r: rabbithole_proto::transfer::ProvedRange| {
        rabbithole_swarm::decode_proved(root, r.size, r.offset, r.len as u64, &r.stream)
    };

    // The first ask starts making the file's proofs, and is told to ask
    // again: nothing waits on them.
    refused(
        alice.proved_range(ticket.transfer_id, 0, 1024).await,
        ErrorCode::Unavailable,
    );

    // A range in the middle, and the file's short last range: each proved.
    let r = proved(&mut alice, ticket.transfer_id, 1 << 20, PROVED_RANGE_MAX)
        .await
        .unwrap();
    assert_eq!(r.size, body.len() as u64);
    let got = check(r).unwrap();
    assert_eq!(
        got.bytes,
        body[1 << 20..(1 << 20) + PROVED_RANGE_MAX as usize]
    );
    assert!(!got.parents.is_empty());
    let tail = 3 << 20;
    let got = check(
        proved(&mut alice, ticket.transfer_id, tail, PROVED_RANGE_MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(got.bytes, body[tail as usize..]);
    // The proofs are kept beside the stored file from then on.
    assert!(burrow.shared.blobs.outboard_path(&blob).exists());

    // A proof checked against another root is refused: nothing bogus passes.
    let r = alice
        .proved_range(ticket.transfer_id, 0, 1024)
        .await
        .unwrap();
    assert!(rabbithole_swarm::decode_proved([9; 32], r.size, 0, 1024, &r.stream).is_err());

    // Over the limit, past the end, or on someone else's ticket: refused.
    refused(
        alice
            .proved_range(ticket.transfer_id, 0, PROVED_RANGE_MAX + 1)
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        alice
            .proved_range(ticket.transfer_id, body.len() as u64, 1)
            .await,
        ErrorCode::BadRequest,
    );
    let mut bob = login(&burrow, "bob").await;
    refused(
        bob.proved_range(ticket.transfer_id, 0, 1024).await,
        ErrorCode::Forbidden,
    );

    // The stored file changes on disk: it no longer proves out, and is not
    // sent as proved at all.
    let path = burrow.shared.blobs.file_path(&blob);
    let mut changed = body.clone();
    changed[5] ^= 0xFF;
    std::fs::write(&path, &changed).unwrap();
    refused(
        proved(&mut alice, ticket.transfer_id, 0, 1024).await,
        ErrorCode::NotFound,
    );
    // And stays so, without the file being read again for every ask.
    refused(
        alice.proved_range(ticket.transfer_id, 0, 1024).await,
        ErrorCode::NotFound,
    );

    alice.close_transfer(ticket.transfer_id).await.unwrap();
    burrow.shutdown().await;
}
