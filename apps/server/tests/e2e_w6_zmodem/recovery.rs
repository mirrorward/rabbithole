//! RH-26: deliberately scripted peers exercise recovery independently of the
//! codec's own state transitions. These are fixtures, not RH-25 terminal proof.
use super::*;
use rabbithole_legacy_zmodem::{session::MAX_RECOVERY_ATTEMPTS, Header, CANFC32};

async fn fixture() -> (tempfile::TempDir, Burrow, ZClient) {
    let work = tempfile::tempdir().unwrap();
    let b = Burrow::start(test_config(work.path())).await.unwrap();
    b.shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    b.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    seed_file(&b, "warez", "taken.bin", b"original").await;
    let mut c = ZClient::connect(b.telnet_addr.unwrap()).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    (work, b, c)
}

async fn send(c: &mut ZClient, kind: FrameType) {
    c.send_raw(&Header::new(kind).encode(HeaderFormat::Hex))
        .await;
}

async fn expect_header(c: &mut ZClient, kind: FrameType) -> Header {
    let header = c.next_header().await.header;
    assert_eq!(header.frame_type, kind);
    header
}

fn corrupted(kind: FrameType) -> Vec<u8> {
    let mut bytes = Header::new(kind).encode(HeaderFormat::Hex);
    bytes[8] = if bytes[8] == b'0' { b'1' } else { b'0' };
    bytes
}

async fn begin_get(c: &mut ZClient) -> FileInfo {
    c.send_line("zget taken.bin").await;
    c.expect(b"Start your receive now").await;
    expect_header(c, FrameType::Zrqinit).await;
    c.send_raw(&Header::with_flags(FrameType::Zrinit, 0, 0, 0, CANFC32).encode(HeaderFormat::Hex))
        .await;
    expect_header(c, FrameType::Zfile).await;
    FileInfo::decode(&c.next_subpacket(true).await.payload).unwrap()
}

async fn offer(c: &mut ZClient, name: &str, size: usize) {
    c.send_raw(&Header::new(FrameType::Zfile).encode(HeaderFormat::Bin32))
        .await;
    let info = FileInfo {
        length: Some(size as u64),
        ..FileInfo::new(name)
    };
    c.send_raw(&encode_subpacket(&info.encode().unwrap(), FrameEnd::Zcrcw, true).unwrap())
        .await;
}

async fn body(c: &mut ZClient, bytes: &[u8]) {
    c.send_raw(&Header::with_pos(FrameType::Zdata, 0).encode(HeaderFormat::Bin32))
        .await;
    c.send_raw(&encode_subpacket(bytes, FrameEnd::Zcrce, true).unwrap())
        .await;
    c.send_raw(&Header::with_pos(FrameType::Zeof, bytes.len() as u32).encode(HeaderFormat::Hex))
        .await;
    expect_header(c, FrameType::Zrinit).await;
}

async fn begin_put(c: &mut ZClient) {
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;
    send(c, FrameType::Zrqinit).await;
    expect_header(c, FrameType::Zrinit).await;
}

async fn finish_put(c: &mut ZClient, retry: bool) {
    send(c, FrameType::Zfin).await;
    expect_header(c, FrameType::Zfin).await;
    if retry {
        for request in [FrameType::Znak, FrameType::Zfin] {
            send(c, request).await;
            expect_header(c, FrameType::Zfin).await;
        }
    }
    c.send_raw(b"OO").await;
    c.expect(b"ZMODEM receive complete.").await;
    c.expect(b"files /warez> ").await;
}

#[tokio::test]
async fn download_decline_finishes_without_counting_and_next_transfer_works() {
    let (_work, b, mut c) = fixture().await;
    assert_eq!(begin_get(&mut c).await.name, "taken.bin");
    send(&mut c, FrameType::Zskip).await;
    expect_header(&mut c, FrameType::Zfin).await;
    send(&mut c, FrameType::Zskip).await; // duplicate decline retries close
    expect_header(&mut c, FrameType::Zfin).await;
    send(&mut c, FrameType::Zfin).await;
    c.expect(b"OO").await;
    c.send_raw(b"OO").await;
    c.expect(b"ZMODEM file skipped by receiver.").await;
    c.expect(b"files /warez> ").await;
    assert_eq!(
        b.shared
            .files
            .node_by_path("warez", "taken.bin")
            .await
            .unwrap()
            .unwrap()
            .downloads,
        0
    );
    c.send_line("zget taken.bin").await;
    c.expect(b"Start your receive now").await;
    assert_eq!(client_receive(&mut c, 0).await.1, b"original");
    c.expect(b"ZMODEM send complete.").await;
    c.expect(b"files /warez> ").await;
    assert_eq!(
        b.shared
            .files
            .node_by_path("warez", "taken.bin")
            .await
            .unwrap()
            .unwrap()
            .downloads,
        1
    );
    b.shutdown().await;
}

#[tokio::test]
async fn download_recovers_file_offer_corrupt_position_eof_and_final_header() {
    let (_work, b, mut c) = fixture().await;
    let first = begin_get(&mut c).await;
    send(&mut c, FrameType::Znak).await;
    expect_header(&mut c, FrameType::Zfile).await;
    assert_eq!(
        FileInfo::decode(&c.next_subpacket(true).await.payload).unwrap(),
        first
    );
    let bad = corrupted(FrameType::Zrpos);
    c.send_raw(&bad[..5]).await;
    c.send_raw(&bad[5..]).await;
    expect_header(&mut c, FrameType::Znak).await;
    send(&mut c, FrameType::Zrpos).await;
    assert_eq!(expect_header(&mut c, FrameType::Zdata).await.pos(), 0);
    assert_eq!(c.next_subpacket(true).await.payload, b"original");
    let eof = expect_header(&mut c, FrameType::Zeof).await;
    send(&mut c, FrameType::Znak).await;
    assert_eq!(expect_header(&mut c, FrameType::Zeof).await, eof);
    send(&mut c, FrameType::Zrinit).await;
    expect_header(&mut c, FrameType::Zfin).await;
    send(&mut c, FrameType::Znak).await;
    expect_header(&mut c, FrameType::Zfin).await;
    send(&mut c, FrameType::Zfin).await;
    c.expect(b"OO").await;
    c.send_raw(b"OO").await;
    c.expect(b"ZMODEM send complete.").await;
    c.expect(b"files /warez> ").await;
    assert_eq!(
        b.shared
            .files
            .node_by_path("warez", "taken.bin")
            .await
            .unwrap()
            .unwrap()
            .downloads,
        1
    );
    b.shutdown().await;
}

#[tokio::test]
async fn upload_skips_existing_names_then_recovers_and_accepts_the_next_file() {
    let (_work, b, mut c) = fixture().await;
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;
    c.send_raw(&corrupted(FrameType::Zrqinit)).await;
    expect_header(&mut c, FrameType::Znak).await;
    send(&mut c, FrameType::Zrqinit).await;
    expect_header(&mut c, FrameType::Zrinit).await;
    send(&mut c, FrameType::Znak).await;
    expect_header(&mut c, FrameType::Zrinit).await;
    for name in ["../warez/taken.bin", "taken.bin"] {
        offer(&mut c, name, 500).await;
        expect_header(&mut c, FrameType::Zskip).await;
    }
    let bytes = noisy(128);
    offer(&mut c, "accepted.bin", bytes.len()).await;
    assert_eq!(expect_header(&mut c, FrameType::Zrpos).await.pos(), 0);
    send(&mut c, FrameType::Znak).await;
    assert_eq!(expect_header(&mut c, FrameType::Zrpos).await.pos(), 0);
    body(&mut c, &bytes).await;
    finish_put(&mut c, true).await;
    let original = b
        .shared
        .files
        .node_by_path("warez", "taken.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        b.shared
            .blobs
            .get(&rabbithole_blobs::BlobId(original.blob_id.unwrap()))
            .unwrap(),
        b"original"
    );
    let accepted = b
        .shared
        .files
        .node_by_path("warez", "accepted.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.blob_id.unwrap(), *blake3::hash(&bytes).as_bytes());
    assert_eq!(
        b.shared
            .blobs
            .get(&rabbithole_blobs::BlobId(accepted.blob_id.unwrap()))
            .unwrap(),
        bytes
    );
    c.send_line("ls").await;
    c.expect(b"accepted.bin").await;
    b.shutdown().await;
}

#[tokio::test]
async fn repeated_upload_declines_are_bounded_even_with_an_accepted_file_between() {
    let (_work, b, mut c) = fixture().await;
    begin_put(&mut c).await;
    for attempt in 0..8 {
        offer(&mut c, "taken.bin", 1).await;
        expect_header(&mut c, FrameType::Zskip).await;
        if attempt == 3 {
            offer(&mut c, "accepted.bin", 1).await;
            expect_header(&mut c, FrameType::Zrpos).await;
            body(&mut c, b"x").await;
        }
    }
    offer(&mut c, "taken.bin", 1).await;
    c.expect(b"too many declined file offers").await;
    c.expect(b"files /warez> ").await;
    c.send_line("ls").await;
    c.expect(b"accepted.bin").await;
    assert_eq!(
        b.shared
            .files
            .node_by_path("warez", "taken.bin")
            .await
            .unwrap()
            .unwrap()
            .size,
        8
    );
    b.shutdown().await;
}

#[tokio::test]
async fn negative_ack_and_bad_header_exhaustion_restore_a_usable_prompt() {
    let (_work, b, mut c) = fixture().await;
    c.send_line("zget taken.bin").await;
    c.expect(b"Start your receive now").await;
    expect_header(&mut c, FrameType::Zrqinit).await;
    for _ in 0..MAX_RECOVERY_ATTEMPTS {
        send(&mut c, FrameType::Znak).await;
        expect_header(&mut c, FrameType::Zrqinit).await;
    }
    send(&mut c, FrameType::Znak).await;
    c.expect(b"header recovery retry limit reached").await;
    c.expect(b"files /warez> ").await;
    assert_eq!(
        b.shared
            .files
            .node_by_path("warez", "taken.bin")
            .await
            .unwrap()
            .unwrap()
            .downloads,
        0
    );
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;
    for _ in 0..MAX_RECOVERY_ATTEMPTS {
        c.send_raw(&corrupted(FrameType::Zrqinit)).await;
        expect_header(&mut c, FrameType::Znak).await;
    }
    c.send_raw(&corrupted(FrameType::Zrqinit)).await;
    c.expect(b"header recovery retry limit reached").await;
    c.expect(b"files /warez> ").await;
    c.send_line("ls").await;
    c.expect(b"taken.bin").await;
    b.shutdown().await;
}
