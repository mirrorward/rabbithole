//! Wave 11.4 end-to-end tests: the Icecast-compatible radio delivery listener
//! wired into `burrow`. The ICY wire codec, station registry, and metadata
//! interleaving are unit-tested in their own crates; here we prove burrow binds
//! the surface, authenticates a DJ source against real accounts + the broadcast
//! capability, fans raw bytes out to a listener, and splices an in-band
//! metadata block at the negotiated `icy-metaint` boundary.

use std::time::Duration;

use burrow::Burrow;
use data_encoding::BASE64;
use rabbithole_legacy_icecast::DEFAULT_METAINT;
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Radio Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        radio_enabled: true,
        radio_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

fn basic_auth(user: &str, pass: &str) -> String {
    BASE64.encode(format!("{user}:{pass}").as_bytes())
}

/// Read from `sock` until `buf` holds at least `want` bytes (or timeout).
async fn read_at_least(sock: &mut TcpStream, want: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < want {
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut chunk))
            .await
            .expect("read did not time out")
            .expect("socket readable");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    buf
}

/// Read up to the end of an HTTP/ICY response head (`\r\n\r\n`), returning the
/// head as a string plus any body bytes already buffered past it.
async fn read_head(sock: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let body = buf[i + 4..].to_vec();
            let head = String::from_utf8_lossy(&buf[..i]).to_string();
            return (head, body);
        }
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut chunk))
            .await
            .expect("read did not time out")
            .expect("socket readable");
        if n == 0 {
            return (String::from_utf8_lossy(&buf).to_string(), Vec::new());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[tokio::test]
async fn source_pushes_and_listener_receives_with_metadata_at_boundary() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    // A DJ needs the broadcast capability — admins hold it by role default.
    burrow
        .shared
        .auth
        .create_account("dj", "spin-spin-spin", Role::Admin)
        .await
        .unwrap();
    let addr = burrow.radio_addr.expect("radio enabled");

    // 1. The source connects, authenticates, and claims the mount.
    let mut source = TcpStream::connect(addr).await.unwrap();
    let src_head = format!(
        "PUT /live HTTP/1.1\r\n\
         Authorization: Basic {}\r\n\
         ice-name: Warren FM\r\n\
         content-type: audio/mpeg\r\n\r\n",
        basic_auth("dj", "spin-spin-spin")
    );
    source.write_all(src_head.as_bytes()).await.unwrap();
    source.flush().await.unwrap();
    let ack = read_at_least(&mut source, 12).await;
    let ack = String::from_utf8_lossy(&ack);
    assert!(ack.contains("200 OK"), "source accepted: {ack:?}");

    // 2. A listener connects opting into in-band metadata.
    let mut listener = TcpStream::connect(addr).await.unwrap();
    listener
        .write_all(b"GET /live HTTP/1.0\r\nIcy-MetaData: 1\r\n\r\n")
        .await
        .unwrap();
    listener.flush().await.unwrap();
    let (head, mut received) = read_head(&mut listener).await;
    assert!(head.starts_with("ICY 200 OK"), "icy status: {head:?}");
    assert!(
        head.contains("icy-name:Warren FM"),
        "station name: {head:?}"
    );
    assert!(
        head.contains(&format!("icy-metaint:{DEFAULT_METAINT}")),
        "negotiated metaint: {head:?}"
    );

    // 3. The source pushes enough audio to cross one metaint boundary. Use a
    //    known ramp so we can verify the audio survives verbatim.
    let audio: Vec<u8> = (0..(DEFAULT_METAINT + 4096))
        .map(|i| (i % 251) as u8)
        .collect();
    source.write_all(&audio).await.unwrap();
    source.flush().await.unwrap();

    // 4. The listener must receive metaint audio bytes, then a metadata block.
    //    Pull until we have the boundary block plus a little of the next run.
    let want = DEFAULT_METAINT + 1 + 16 + 16;
    while received.len() < want {
        let more = read_at_least(&mut listener, 1).await;
        if more.is_empty() {
            break;
        }
        received.extend_from_slice(&more);
    }
    assert!(
        received.len() > DEFAULT_METAINT,
        "got {} bytes, need past the boundary",
        received.len()
    );

    // The first metaint bytes are the audio, verbatim.
    assert_eq!(
        &received[..DEFAULT_METAINT],
        &audio[..DEFAULT_METAINT],
        "audio delivered verbatim up to the boundary"
    );

    // At the boundary: a non-zero length byte introducing the StreamTitle block.
    let len_byte = received[DEFAULT_METAINT] as usize;
    assert!(len_byte > 0, "a real metadata block, not the 0x00 filler");
    let meta_start = DEFAULT_METAINT + 1;
    let meta_end = meta_start + len_byte * 16;
    assert!(received.len() >= meta_end, "full metadata block received");
    let meta = &received[meta_start..meta_end];
    let meta_text = String::from_utf8_lossy(meta);
    assert!(
        meta_text.contains("StreamTitle='Warren FM'"),
        "metadata carries the stream title: {meta_text:?}"
    );

    // Audio resumes right after the block.
    assert_eq!(
        received[meta_end], audio[DEFAULT_METAINT],
        "audio resumes after the metadata block"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn unauthenticated_source_is_rejected_401() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    let addr = burrow.radio_addr.expect("radio enabled");

    // A source with no Authorization header is rejected before it can publish.
    let mut source = TcpStream::connect(addr).await.unwrap();
    source
        .write_all(b"PUT /live HTTP/1.1\r\nice-name: Pirate\r\n\r\n")
        .await
        .unwrap();
    source.flush().await.unwrap();
    let resp = read_at_least(&mut source, 12).await;
    let resp = String::from_utf8_lossy(&resp);
    assert!(
        resp.contains("401"),
        "unauthenticated source rejected: {resp:?}"
    );

    burrow.shutdown().await;
}
