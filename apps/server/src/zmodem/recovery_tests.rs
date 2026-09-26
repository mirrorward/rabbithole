use super::*;
use rabbithole_legacy_zmodem::Header;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn malformed_fragmented_headers_preserve_the_next_coalesced_header() {
    let valid = Header::new(FrameType::Zrinit).encode(HeaderFormat::Hex);
    let mut bad = valid.clone();
    bad[8] = if bad[8] == b'0' { b'1' } else { b'0' }; // valid hex, wrong CRC
    for split in 1..bad.len() {
        let (mut peer, socket) = tokio::io::duplex(4096);
        let mut telnet = TelnetStream::new(socket);
        let mut wire = Wire::new(&mut telnet);
        peer.write_all(&bad[..split]).await.unwrap();
        let rest = [bad[split..].to_vec(), valid.clone()].concat();
        let writer = tokio::spawn(async move {
            tokio::task::yield_now().await;
            peer.write_all(&rest).await.unwrap();
        });
        assert!(matches!(wire.next_header().await, Err(Zx::BadHeader)));
        assert_eq!(
            wire.next_header()
                .await
                .unwrap_or_else(|_| panic!("lost valid header at split {split}"))
                .header
                .frame_type,
            FrameType::Zrinit
        );
        writer.await.unwrap();
    }
}

#[tokio::test]
async fn noise_and_incomplete_pad_headers_have_finite_work_and_keep_cancel_semantics() {
    for bytes in [
        vec![b'x'; MAX_HEADER_SCAN + 8192],
        vec![ZPAD; MAX_HEADER_BUFFER + 1],
        vec![ZDLE; CANCEL_CANS as usize],
    ] {
        let (mut peer, socket) = tokio::io::duplex(MAX_HEADER_SCAN * 2);
        peer.write_all(&bytes).await.unwrap();
        let mut telnet = TelnetStream::new(socket);
        let mut wire = Wire::new(&mut telnet);
        let result = wire.next_header().await;
        match bytes[0] {
            b'x' => assert!(matches!(result, Err(Zx::Protocol(_)))),
            ZPAD => assert!(matches!(result, Err(Zx::BadHeader))),
            _ => assert!(matches!(result, Err(Zx::Cancelled))),
        }
    }
}

#[tokio::test]
async fn continuous_noise_cannot_replenish_the_whole_header_wait_budget() {
    let (mut peer, socket) = tokio::io::duplex(4096);
    let writer = tokio::spawn(async move {
        loop {
            if peer.write_all(b"x").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let mut telnet = TelnetStream::new(socket);
    let mut wire = Wire::new(&mut telnet);
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        wire.next_header_with_budget(Duration::from_millis(25)),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(Zx::Timeout)));
    writer.abort();
}
