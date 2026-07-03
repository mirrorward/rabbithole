//! Wave 6 end-to-end tests: ZMODEM transfers over the telnet BBS. The
//! client side is driven with the same `rabbithole-legacy-zmodem` codec
//! over a real TCP telnet connection — a real handshake, real ZDLE
//! escaping, and the telnet IAC seam in between (including deliberately
//! *unescaped* 0xFF payload bytes, which only survive because of IAC
//! doubling). All sessions are marker-driven — no blind sleeps.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_telnet::proto::{escape_iac, Event, Parser};
use rabbithole_legacy_zmodem::zdle::Escaper;
use rabbithole_legacy_zmodem::{
    crc16_xmodem, crc32, decode_header, decode_subpacket, encode_subpacket, DecodedHeader,
    DecodedSubpacket, FileInfo, FrameEnd, FrameType, HeaderError, HeaderFormat, Receiver,
    RecvAction, RecvEvent, RecvState, SendAction, SendEvent, Sender, SubpacketError, ZPAD,
};
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &Path) -> ServerConfig {
    ServerConfig {
        name: "Zmodem Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// Deterministic 0xFF-heavy bytes that also hit every ZDLE-sensitive class
/// (CAN, XON, parity CR, the @-then-CR rule).
fn noisy(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| match i % 7 {
            0 => 0xFF,
            1 => 0x18,
            2 => 0x11,
            3 => 0x8D,
            4 => b'@',
            5 => 0x0D,
            _ => (i % 251) as u8,
        })
        .collect()
}

/// Store bytes as a blob and register them as a library file.
async fn seed_file(b: &Burrow, area: &str, name: &str, bytes: &[u8]) -> i64 {
    let blob_id = b.shared.blobs.put(bytes).unwrap().0;
    b.shared
        .files
        .add_file(
            area,
            None,
            name,
            &blob_id,
            bytes.len() as i64,
            "application/octet-stream",
            "",
            "",
            "op@warren",
            1,
        )
        .await
        .unwrap()
        .id
}

// ---------------------------------------------------------------------------
// A telnet-aware zmodem test client: the Parser undoubles IAC and strips
// negotiation; outbound wire bytes are IAC-doubled. Zmodem frames are cut
// from the resulting clean payload stream with the codec's own decoders.

struct ZClient {
    sock: TcpStream,
    parser: Parser,
    /// Clean payload bytes (negotiation absorbed, IAC undoubled).
    data: Vec<u8>,
    /// Bytes before this were consumed by `expect`/frame decoding.
    pos: usize,
}

impl ZClient {
    async fn connect(addr: std::net::SocketAddr) -> ZClient {
        ZClient {
            sock: TcpStream::connect(addr).await.unwrap(),
            parser: Parser::new(),
            data: Vec::new(),
            pos: 0,
        }
    }

    /// Read one chunk from the socket into the payload stream.
    async fn recv_more(&mut self) {
        let mut chunk = [0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(30), self.sock.read(&mut chunk))
            .await
            .expect("timed out reading from the server")
            .expect("telnet read");
        assert!(
            n > 0,
            "EOF from server; unconsumed: {:?}",
            String::from_utf8_lossy(&self.data[self.pos..])
        );
        let mut events = Vec::new();
        self.parser.feed(&chunk[..n], &mut events);
        for ev in events {
            if let Event::Data(d) = ev {
                self.data.extend_from_slice(&d);
            }
        }
    }

    /// Send raw wire bytes, IAC-doubled for the telnet hop.
    async fn send_raw(&mut self, bytes: &[u8]) {
        self.sock.write_all(&escape_iac(bytes)).await.unwrap();
        self.sock.flush().await.unwrap();
    }

    async fn send_line(&mut self, line: &str) {
        self.sock
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    /// Wait for `needle` past the consumption point and consume through it.
    async fn expect(&mut self, needle: &[u8]) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(at) = find(&self.data[self.pos..], needle) {
                    self.pos += at + needle.len();
                    return;
                }
                self.recv_more().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {:?}; unconsumed: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.data[self.pos..])
            )
        })
    }

    async fn login(&mut self, user: &str, pass: &str) {
        self.expect(b"login: ").await;
        self.send_line(user).await;
        self.expect(b"password: ").await;
        self.send_line(pass).await;
        self.expect(b"Command: ").await;
    }

    /// Enter the files sub-shell inside `area`.
    async fn enter_area(&mut self, area: &str) {
        self.send_line("f").await;
        self.expect(b"files /> ").await;
        self.send_line(&format!("cd {area}")).await;
        self.expect(format!("files /{area}> ").as_bytes()).await;
    }

    /// The next zmodem header in the stream, skipping shell text/noise.
    async fn next_header(&mut self) -> DecodedHeader {
        loop {
            while self.pos < self.data.len() && self.data[self.pos] != ZPAD {
                self.pos += 1;
            }
            if self.pos >= self.data.len() {
                self.recv_more().await;
                continue;
            }
            match decode_header(&self.data[self.pos..]) {
                Ok(decoded) => {
                    self.pos += decoded.consumed;
                    return decoded;
                }
                Err(HeaderError::Incomplete) => self.recv_more().await,
                Err(_) => self.pos += 1, // stray '*' in shell text: resync
            }
        }
    }

    /// The next data subpacket of the current frame.
    async fn next_subpacket(&mut self, wide: bool) -> DecodedSubpacket {
        loop {
            if self.pos >= self.data.len() {
                self.recv_more().await;
            }
            match decode_subpacket(&self.data[self.pos..], wide) {
                Ok(sub) => {
                    self.pos += sub.consumed;
                    return sub;
                }
                Err(SubpacketError::Incomplete) => self.recv_more().await,
                Err(e) => panic!("bad subpacket from server: {e}"),
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Drive the codec `Receiver` as the client side of a `zget`. Returns the
/// offered file info, the received bytes, and the offset of the first
/// `WriteData` (to prove resume really skipped the head).
async fn client_receive(c: &mut ZClient, resume_at: u32) -> (FileInfo, Vec<u8>, u32) {
    let mut rx = Receiver::new();
    let mut wide = false;
    let mut got: Vec<u8> = Vec::new();
    let mut first_write: Option<u32> = None;
    let mut info_out: Option<FileInfo> = None;
    loop {
        let event = match rx.state() {
            RecvState::AwaitingFileInfo | RecvState::ReceivingData { .. } => {
                let sub = c.next_subpacket(wide).await;
                RecvEvent::Data {
                    payload: sub.payload,
                    end: sub.end,
                }
            }
            _ => {
                let decoded = c.next_header().await;
                if matches!(
                    decoded.header.frame_type,
                    FrameType::Zfile | FrameType::Zdata
                ) {
                    wide = decoded.format == HeaderFormat::Bin32;
                }
                RecvEvent::Header(decoded.header)
            }
        };
        // Arm crash-recovery just before answering the file offer.
        if rx.state() == RecvState::AwaitingFileInfo
            && matches!(event, RecvEvent::Data { .. })
            && resume_at > 0
        {
            rx.set_resume_offset(resume_at);
        }
        for action in rx.advance(event).unwrap() {
            match action {
                RecvAction::SendHeader { header, format } => {
                    c.send_raw(&header.encode(format)).await;
                }
                RecvAction::OpenFile(info) => info_out = Some(info),
                RecvAction::WriteData { offset, data } => {
                    let first = *first_write.get_or_insert(offset);
                    assert_eq!(
                        offset as usize,
                        first as usize + got.len(),
                        "server data must be contiguous"
                    );
                    got.extend_from_slice(&data);
                }
                RecvAction::CloseFile => {}
                RecvAction::SendOverAndOut => c.send_raw(b"OO").await,
                RecvAction::Finished => {
                    return (info_out.unwrap(), got, first_write.unwrap_or(resume_at));
                }
            }
        }
    }
}

/// A data subpacket that deliberately leaves 0xFF bytes **unescaped** —
/// legal ZMODEM (ZRUB1 escaping is the codec's extra caution, not a spec
/// requirement) — so the raw 0xFF only survives the telnet hop thanks to
/// IAC doubling. The CRC still covers the unescaped payload + end byte.
fn raw_ff_subpacket(payload: &[u8], end: FrameEnd, wide: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut esc = Escaper::new();
    for &b in payload {
        if b == 0xFF {
            out.push(0xFF);
        } else {
            esc.push_byte(b, &mut out);
        }
    }
    esc.push_frame_end(end, &mut out);
    let mut crc_input = payload.to_vec();
    crc_input.push(end.to_byte());
    if wide {
        esc.push_slice(&crc32(&crc_input).to_le_bytes(), &mut out);
    } else {
        let crc = crc16_xmodem(&crc_input);
        esc.push_slice(&[(crc >> 8) as u8, crc as u8], &mut out);
    }
    out
}

/// Drive the codec `Sender` as the client side of a `zput` to completion.
/// `raw_ff_prefix` sends the first N bytes (only when streaming from 0) as
/// an unescaped-0xFF subpacket for IAC stress. Returns the offset the
/// server's ZRPOS asked streaming to start from.
async fn client_send(c: &mut ZClient, info: FileInfo, bytes: &[u8], raw_ff_prefix: usize) -> u32 {
    let mut tx = Sender::new(info);
    let mut started_from: Option<u32> = None;
    let mut pending: VecDeque<SendAction> = tx.start().unwrap().into();
    loop {
        while let Some(action) = pending.pop_front() {
            match action {
                SendAction::SendHeader { header, format } => {
                    c.send_raw(&header.encode(format)).await;
                }
                SendAction::SendFileInfo(info) => {
                    let payload = info.encode().unwrap();
                    let sub =
                        encode_subpacket(&payload, FrameEnd::Zcrcw, tx.peer_can_fc32()).unwrap();
                    c.send_raw(&sub).await;
                }
                SendAction::StreamData { from } => {
                    started_from.get_or_insert(from);
                    let wide = tx.peer_can_fc32();
                    let mut at = from as usize;
                    if at == 0 && raw_ff_prefix > 0 && raw_ff_prefix < bytes.len() {
                        let sub = raw_ff_subpacket(&bytes[..raw_ff_prefix], FrameEnd::Zcrcg, wide);
                        c.send_raw(&sub).await;
                        at = raw_ff_prefix;
                    }
                    let mut chunks = bytes[at..].chunks(1024).peekable();
                    if chunks.peek().is_none() {
                        c.send_raw(&encode_subpacket(&[], FrameEnd::Zcrce, wide).unwrap())
                            .await;
                    }
                    while let Some(chunk) = chunks.next() {
                        let end = if chunks.peek().is_some() {
                            FrameEnd::Zcrcg
                        } else {
                            FrameEnd::Zcrce
                        };
                        c.send_raw(&encode_subpacket(chunk, end, wide).unwrap())
                            .await;
                    }
                    pending.extend(
                        tx.advance(SendEvent::DataExhausted {
                            offset: bytes.len() as u32,
                        })
                        .unwrap(),
                    );
                }
                SendAction::SendOverAndOut => c.send_raw(b"OO").await,
                SendAction::Finished => return started_from.unwrap(),
            }
        }
        let decoded = c.next_header().await;
        pending.extend(tx.advance(SendEvent::Header(decoded.header)).unwrap());
    }
}

// ---------------------------------------------------------------------------
// The tests.

#[tokio::test]
async fn zget_roundtrip_is_byte_identical_with_iac_heavy_content() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let payload = noisy(4500);
    seed_file(&burrow, "warez", "noise.bin", &payload).await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zget noise.bin").await;
    c.expect(b"Start your receive now").await;

    let (info, got, first) = client_receive(&mut c, 0).await;
    assert_eq!(info.name, "noise.bin");
    assert_eq!(info.length, Some(4500));
    assert_eq!(first, 0);
    assert_eq!(got, payload, "received bytes must be identical");

    // The download was counted, and the shell is back at its prompt.
    c.expect(b"ZMODEM send complete.").await;
    c.expect(b"files /warez> ").await;
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "noise.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.downloads, 1);

    c.send_line("q").await;
    c.expect(b"Command: ").await;
    c.send_line("q").await;
    c.expect(b"Goodbye, alice!").await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn zget_resume_from_zrpos_offset_stitches_correctly() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let payload = noisy(4000);
    seed_file(&burrow, "warez", "big.bin", &payload).await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zget big.bin").await;
    c.expect(b"Start your receive now").await;

    // The client already holds the first 1300 bytes (a crashed earlier
    // attempt): answer ZFILE with ZRPOS(1300) and receive only the tail.
    let offset = 1300u32;
    let (_, tail, first) = client_receive(&mut c, offset).await;
    assert_eq!(first, offset, "server must start at the requested offset");
    assert_eq!(tail, payload[offset as usize..], "tail bytes identical");

    // Stitch: prior head + received tail == the whole file.
    let mut stitched = payload[..offset as usize].to_vec();
    stitched.extend_from_slice(&tail);
    assert_eq!(stitched, payload);

    c.expect(b"ZMODEM send complete.").await;
    c.expect(b"files /warez> ").await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn zput_uploads_land_with_correct_blake3_including_raw_iac_bytes() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    // 512 leading 0xFF bytes travel UNESCAPED at the zmodem layer — only
    // the telnet IAC doubling carries them — then 0xFF-heavy ZDLE-escaped
    // rest.
    let mut payload = vec![0xFF; 512];
    payload.extend_from_slice(&noisy(2488));

    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;

    let info = FileInfo {
        length: Some(payload.len() as u64),
        mtime: Some(1_700_000_000),
        ..FileInfo::new("upload.bin")
    };
    let from = client_send(&mut c, info, &payload, 512).await;
    assert_eq!(from, 0);

    c.expect(b"ZMODEM receive complete.").await;
    c.expect(b"Received upload.bin (3000 bytes).").await;
    c.expect(b"files /warez> ").await;

    let node = burrow
        .shared
        .files
        .node_by_path("warez", "upload.bin")
        .await
        .unwrap()
        .expect("uploaded file registered");
    assert_eq!(node.size, 3000);
    let blob_id = node.blob_id.expect("has a blob");
    assert_eq!(
        blob_id,
        *blake3::hash(&payload).as_bytes(),
        "blob id is the blake3 of the exact uploaded bytes"
    );
    let stored = burrow
        .shared
        .blobs
        .get(&rabbithole_blobs::BlobId(blob_id))
        .unwrap();
    assert_eq!(stored, payload, "stored bytes identical");
    assert!(node.uploader.starts_with("alice@"));
    burrow.shutdown().await;
}

#[tokio::test]
async fn zput_and_zget_are_refused_without_the_capability() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("visitor", "pw-pw-pw", Role::Guest)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    seed_file(&burrow, "warez", "noise.bin", &noisy(100)).await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    // Guests hold FILE_LIST (they can browse) but neither FILE_UPLOAD nor
    // FILE_DOWNLOAD: both transfer verbs refuse before any zmodem starts.
    let mut c = ZClient::connect(addr).await;
    c.login("visitor", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zput").await;
    c.expect(b"Uploads need a member account.").await;
    c.expect(b"files /warez> ").await;
    c.send_line("zget noise.bin").await;
    c.expect(b"You do not have permission to download that file.")
        .await;
    c.expect(b"files /warez> ").await;

    // The shell stayed in line mode throughout.
    c.send_line("ls").await;
    c.expect(b"noise.bin").await;
    c.expect(b"files /warez> ").await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn deny_hashed_zput_is_refused_at_finalize() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let payload = noisy(1000);
    burrow
        .shared
        .moderation
        .deny_add(blake3::hash(&payload).as_bytes(), "known bad", "mo")
        .await
        .unwrap();

    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;

    // The transfer itself completes; the finalize gate refuses the commit.
    let info = FileInfo {
        length: Some(payload.len() as u64),
        ..FileInfo::new("bad.bin")
    };
    client_send(&mut c, info, &payload, 0).await;
    c.expect(b"bad.bin: refused (that content is not allowed here)")
        .await;
    c.expect(b"files /warez> ").await;

    assert!(burrow
        .shared
        .files
        .node_by_path("warez", "bad.bin")
        .await
        .unwrap()
        .is_none());
    burrow.shutdown().await;
}

#[tokio::test]
async fn cancel_mid_transfer_returns_to_a_usable_prompt() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    seed_file(&burrow, "warez", "noise.bin", &noisy(2000)).await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zget noise.bin").await;
    c.expect(b"Start your receive now").await;

    // Instead of ZRINIT, strike the classic cancel (>= five CANs).
    c.send_raw(&[0x18; 8]).await;
    c.expect(b"Transfer cancelled.").await;
    c.expect(b"files /warez> ").await;

    // The shell is fully usable again: list, then leave cleanly.
    c.send_line("ls").await;
    c.expect(b"noise.bin").await;
    c.expect(b"files /warez> ").await;
    c.send_line("q").await;
    c.expect(b"Command: ").await;
    c.send_line("q").await;
    c.expect(b"Goodbye, alice!").await;

    // Nothing was counted for the aborted send.
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "noise.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.downloads, 0);
    burrow.shutdown().await;
}

#[tokio::test]
async fn zput_partial_resumes_at_the_staged_offset_after_reconnect() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let payload = noisy(1500);
    let info = FileInfo {
        length: Some(payload.len() as u64),
        ..FileInfo::new("resume.bin")
    };

    // Session one: offer the file, send the first 700 bytes, then cancel.
    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;

    let mut tx = Sender::new(info.clone());
    for action in tx.start().unwrap() {
        if let SendAction::SendHeader { header, format } = action {
            c.send_raw(&header.encode(format)).await;
        }
    }
    // ZRINIT -> ZFILE + info.
    let rinit = c.next_header().await;
    assert_eq!(rinit.header.frame_type, FrameType::Zrinit);
    for action in tx.advance(SendEvent::Header(rinit.header)).unwrap() {
        match action {
            SendAction::SendHeader { header, format } => c.send_raw(&header.encode(format)).await,
            SendAction::SendFileInfo(i) => {
                let sub =
                    encode_subpacket(&i.encode().unwrap(), FrameEnd::Zcrcw, tx.peer_can_fc32())
                        .unwrap();
                c.send_raw(&sub).await;
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
    // A fresh destination answers ZRPOS(0).
    let rpos = c.next_header().await;
    assert_eq!(rpos.header.frame_type, FrameType::Zrpos);
    assert_eq!(rpos.header.pos(), 0);
    for action in tx.advance(SendEvent::Header(rpos.header)).unwrap() {
        if let SendAction::SendHeader { header, format } = action {
            c.send_raw(&header.encode(format)).await; // ZDATA(0)
        }
    }
    let half = encode_subpacket(&payload[..700], FrameEnd::Zcrcg, tx.peer_can_fc32()).unwrap();
    c.send_raw(&half).await;
    // Abandon the transfer: the server parks the received 700 bytes.
    c.send_raw(&[0x18; 8]).await;
    c.expect(b"Transfer cancelled.").await;
    c.expect(b"700 byte(s) kept for resume").await;
    c.expect(b"files /warez> ").await;
    drop(c);

    // Session two: the same account re-offers the same destination and the
    // server answers the ZFILE with ZRPOS(700) — only the tail travels.
    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;
    let from = client_send(&mut c, info, &payload, 0).await;
    assert_eq!(from, 700, "server must ask for the tail only");

    c.expect(b"Received resume.bin (1500 bytes).").await;
    c.expect(b"files /warez> ").await;

    let node = burrow
        .shared
        .files
        .node_by_path("warez", "resume.bin")
        .await
        .unwrap()
        .expect("resumed upload registered");
    assert_eq!(node.size, 1500);
    let blob_id = node.blob_id.unwrap();
    assert_eq!(blob_id, *blake3::hash(&payload).as_bytes());
    assert_eq!(
        burrow
            .shared
            .blobs
            .get(&rabbithole_blobs::BlobId(blob_id))
            .unwrap(),
        payload,
        "head from staging + tail from the resumed session stitch exactly"
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn zput_refuses_name_collisions_before_any_data() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    seed_file(&burrow, "warez", "taken.bin", b"already here").await;
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = ZClient::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.enter_area("warez").await;
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;

    // Offer a colliding name (with a sneaky path prefix that must be
    // stripped before the collision check).
    let mut tx = Sender::new(FileInfo {
        length: Some(64),
        ..FileInfo::new("../warez/taken.bin")
    });
    for action in tx.start().unwrap() {
        if let SendAction::SendHeader { header, format } = action {
            c.send_raw(&header.encode(format)).await;
        }
    }
    let rinit = c.next_header().await;
    for action in tx.advance(SendEvent::Header(rinit.header)).unwrap() {
        match action {
            SendAction::SendHeader { header, format } => c.send_raw(&header.encode(format)).await,
            SendAction::SendFileInfo(i) => {
                let sub =
                    encode_subpacket(&i.encode().unwrap(), FrameEnd::Zcrcw, tx.peer_can_fc32())
                        .unwrap();
                c.send_raw(&sub).await;
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
    c.expect(b"Upload refused: taken.bin already exists here.")
        .await;
    c.expect(b"files /warez> ").await;

    // The original is untouched and the shell still answers.
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "taken.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.size, "already here".len() as i64);
    c.send_line("ls").await;
    c.expect(b"taken.bin").await;
    burrow.shutdown().await;
}
