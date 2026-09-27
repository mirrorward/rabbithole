use super::*;
use rabbithole_legacy_telnet::proto::escape_iac;
use rabbithole_legacy_zmodem::Header;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn whole_rep_deadline_ends_silent_and_continuously_active_senders() {
    for trickle in [false, true] {
        let (mut peer, socket) = tokio::io::duplex(4096);
        let writer = tokio::spawn(async move {
            if !trickle {
                std::future::pending::<()>().await;
            }
            // A valid, repeatedly offered init keeps individual reads active.
            loop {
                if peer
                    .write_all(&Header::new(FrameType::Zrqinit).encode(HeaderFormat::Hex))
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        let mut telnet = TelnetStream::new(socket);
        let result =
            receive_with_budget(&mut Wire::new(&mut telnet), Duration::from_millis(25)).await;
        assert!(matches!(result, Err(Zx::Timeout)));
        writer.abort();
    }
}

#[tokio::test]
async fn actual_rep_byte_cap_accepts_exact_limit_and_refuses_overflow_without_declared_size() {
    for length in [MAX_ARCHIVE_BYTES, MAX_ARCHIVE_BYTES + 1] {
        let (mut peer, socket) = tokio::io::duplex(64 * 1024);
        let writer = tokio::spawn(async move {
            let mut bytes = Header::new(FrameType::Zrqinit).encode(HeaderFormat::Hex);
            bytes.extend(Header::new(FrameType::Zfile).encode(HeaderFormat::Bin32));
            bytes.extend(
                encode_subpacket(
                    &FileInfo::new("limit.rep").encode().unwrap(),
                    FrameEnd::Zcrcw,
                    true,
                )
                .unwrap(),
            );
            bytes.extend(Header::with_pos(FrameType::Zdata, 0).encode(HeaderFormat::Bin32));
            let chunk = [b'x'; 1024];
            let mut sent = 0;
            while sent < length {
                let size = (length - sent).min(chunk.len());
                sent += size;
                let end = if sent == length {
                    FrameEnd::Zcrce
                } else {
                    FrameEnd::Zcrcg
                };
                bytes.extend(encode_subpacket(&chunk[..size], end, true).unwrap());
            }
            bytes.extend(
                Header::with_pos(FrameType::Zeof, length as u32).encode(HeaderFormat::Bin32),
            );
            bytes.extend(Header::new(FrameType::Zfin).encode(HeaderFormat::Hex));
            bytes.extend(b"OO");
            let _ = peer.write_all(&escape_iac(&bytes)).await;
            peer // Keep the peer alive while the receiver writes its final ACKs.
        });
        let mut telnet = TelnetStream::new(socket);
        let result =
            receive_with_budget(&mut Wire::new(&mut telnet), Duration::from_secs(30)).await;
        if length == MAX_ARCHIVE_BYTES {
            assert!(matches!(result, Ok(bytes) if bytes.len() == MAX_ARCHIVE_BYTES));
        } else {
            assert!(matches!(result, Err(Zx::Refused(_))));
        }
        drop(telnet);
        writer.await.unwrap();
    }
}
