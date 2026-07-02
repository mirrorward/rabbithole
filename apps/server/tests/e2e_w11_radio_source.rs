//! Wave 11 end-to-end tests: the DJ **source ingest** surface, the
//! library-from-file-areas playlist source, and now-playing in presence, all
//! wired into `burrow`.
//!
//! The delivery (listener-pull) surface is covered by `e2e_w114`; here we prove
//! burrow can (1) accept an inbound DJ source on its own port, authenticated
//! against the admin-configured source credentials, take the station live and
//! surface it in presence while its bytes reach the station, (2) refuse bad
//! credentials, (3) leave the surface off by default, and (4) pull a file
//! area's audio into a station's playlist rotation.

use std::path::Path;
use std::time::{Duration, Instant};

use burrow::Burrow;
use data_encoding::BASE64;
use rabbithole_server_core::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn base_config(dir: &Path) -> ServerConfig {
    ServerConfig {
        name: "Radio Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

fn source_config(dir: &Path) -> ServerConfig {
    ServerConfig {
        radio_source_enabled: true,
        radio_source_addr: "127.0.0.1:0".parse().unwrap(),
        radio_source_user: "source".into(),
        radio_source_password: "hackme".into(),
        ..base_config(dir)
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

/// Poll a real condition until it holds, or fail after a deadline. No blind
/// sleeps: this waits on observable state, not a fixed guess.
async fn poll_until(label: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition never held: {label}");
}

/// Install a one-track library program for the mount so the DJ has something to
/// pre-empt (and resume). Uses the real file-listing → track-list mapping.
async fn install_live_program(burrow: &Burrow) {
    let files = &burrow.shared.files;
    files.create_area("music", "Music", "").await.unwrap();
    files
        .add_file(
            "music",
            None,
            "auto.mp3",
            &[7u8; 32],
            10,
            "audio/mpeg",
            "",
            "",
            "dj@h",
            1,
        )
        .await
        .unwrap();
    let nodes: Vec<_> = files
        .manifest("music", None)
        .await
        .unwrap()
        .into_iter()
        .map(|(node, _rel)| node)
        .collect();
    let tracks = burrow::radio::tracks_from_nodes(&nodes);
    burrow
        .shared
        .radio
        .install_program("live", "Live FM", "music", tracks);
}

#[tokio::test]
async fn dj_source_goes_live_bytes_reach_station_and_presence_updates() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(source_config(&work.path().join("srv")))
        .await
        .unwrap();
    install_live_program(&burrow).await;
    let addr = burrow.radio_source_addr.expect("source ingest enabled");

    // Automation is playing before the DJ arrives.
    assert!(!burrow.shared.radio.is_live("live"));
    assert_eq!(
        burrow.shared.radio.now_playing("live").unwrap().title,
        "auto.mp3"
    );

    // 1. The DJ connects with valid credentials and takes the mount.
    let mut source = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "PUT /live HTTP/1.1\r\n\
         Authorization: Basic {}\r\n\
         ice-name: Live Set\r\n\
         ice-genre: Techno\r\n\
         content-type: audio/mpeg\r\n\r\n",
        basic_auth("source", "hackme")
    );
    source.write_all(head.as_bytes()).await.unwrap();
    source.flush().await.unwrap();
    let ack = read_at_least(&mut source, 12).await;
    let ack = String::from_utf8_lossy(&ack);
    assert!(ack.contains("200 OK"), "source accepted: {ack:?}");

    // 2. The DJ pushes audio; the bytes reach the station and it goes live.
    let audio: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    source.write_all(&audio).await.unwrap();
    source.flush().await.unwrap();

    let radio = &burrow.shared.radio;
    poll_until("station goes live", || radio.is_live("live")).await;
    poll_until("source bytes reach the station", || {
        radio.source_bytes("live") as usize >= audio.len()
    })
    .await;

    // Now-playing switched from automation to the live DJ metadata.
    let np = radio.now_playing("live").unwrap();
    assert_eq!(np.title, "Live Set");
    assert_eq!(np.dj, "source");

    // 3. It is surfaced in presence as a live radio status.
    let status = burrow
        .shared
        .presence
        .radio_status("live")
        .expect("now-playing in presence");
    assert!(status.live, "presence shows the mount as live");
    assert_eq!(status.title, "Live Set");
    assert_eq!(status.artist, "Techno");

    // 4. The DJ disconnects (graceful shutdown so the last bytes are not RST'd);
    //    rotation resumes and automation now-playing returns.
    source.shutdown().await.unwrap();
    drop(source);
    poll_until("rotation resumes when the DJ leaves", || {
        !radio.is_live("live")
    })
    .await;
    assert_eq!(radio.now_playing("live").unwrap().title, "auto.mp3");

    burrow.shutdown().await;
}

#[tokio::test]
async fn updinfo_changes_now_playing_and_presence() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(source_config(&work.path().join("srv")))
        .await
        .unwrap();
    install_live_program(&burrow).await;
    let addr = burrow.radio_source_addr.expect("source ingest enabled");

    // A DJ goes live on /live first (updinfo targets a live mount).
    let mut source = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "PUT /live HTTP/1.1\r\n\
         Authorization: Basic {}\r\n\
         ice-name: Live Set\r\n\
         content-type: audio/mpeg\r\n\r\n",
        basic_auth("source", "hackme")
    );
    source.write_all(head.as_bytes()).await.unwrap();
    source.flush().await.unwrap();
    let ack = read_at_least(&mut source, 12).await;
    assert!(String::from_utf8_lossy(&ack).contains("200 OK"));
    let radio = &burrow.shared.radio;
    poll_until("station goes live", || radio.is_live("live")).await;

    // The encoder announces a track change over a second, short-lived request.
    let mut admin = TcpStream::connect(addr).await.unwrap();
    admin
        .write_all(
            b"GET /admin/metadata?mode=updinfo&mount=/live&pass=hackme&song=Daft+Punk+-+Da+Funk HTTP/1.0\r\n\r\n",
        )
        .await
        .unwrap();
    admin.flush().await.unwrap();
    let reply = read_at_least(&mut admin, 12).await;
    let reply = String::from_utf8_lossy(&reply);
    assert!(reply.starts_with("HTTP/1.0 200 OK"), "reply: {reply:?}");
    assert!(
        reply.contains("<return>1</return>"),
        "Icecast XML success body: {reply:?}"
    );

    // Now-playing switched, with the song split into artist/title, keeping
    // the DJ; presence carries the same update.
    let np = radio.now_playing("live").expect("live now-playing");
    assert_eq!(np.title, "Da Funk");
    assert_eq!(np.artist, "Daft Punk");
    assert_eq!(np.dj, "source");
    let status = burrow
        .shared
        .presence
        .radio_status("live")
        .expect("now-playing in presence");
    assert!(status.live);
    assert_eq!(status.title, "Da Funk");
    assert_eq!(status.artist, "Daft Punk");

    // A song without " - " is all title (empty artist).
    let mut admin = TcpStream::connect(addr).await.unwrap();
    admin
        .write_all(
            format!(
                "GET /admin/metadata?mode=updinfo&mount=/live&song=Untitled+Jam HTTP/1.0\r\n\
                 Authorization: Basic {}\r\n\r\n",
                basic_auth("source", "hackme")
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    admin.flush().await.unwrap();
    let reply = read_at_least(&mut admin, 12).await;
    assert!(String::from_utf8_lossy(&reply).contains("<return>1</return>"));
    let np = radio.now_playing("live").unwrap();
    assert_eq!(np.title, "Untitled Jam");
    assert_eq!(np.artist, "");

    source.shutdown().await.unwrap();
    burrow.shutdown().await;
}

#[tokio::test]
async fn updinfo_with_bad_credentials_is_401() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(source_config(&work.path().join("srv")))
        .await
        .unwrap();
    install_live_program(&burrow).await;
    let addr = burrow.radio_source_addr.expect("source ingest enabled");

    let mut admin = TcpStream::connect(addr).await.unwrap();
    admin
        .write_all(
            b"GET /admin/metadata?mode=updinfo&mount=/live&pass=wrong&song=Nope HTTP/1.0\r\n\r\n",
        )
        .await
        .unwrap();
    admin.flush().await.unwrap();
    let reply = read_at_least(&mut admin, 12).await;
    let reply = String::from_utf8_lossy(&reply);
    assert!(
        reply.starts_with("HTTP/1.0 401"),
        "bad creds refused: {reply:?}"
    );
    // Nothing changed: automation is still what is playing.
    assert_eq!(
        burrow.shared.radio.now_playing("live").unwrap().title,
        "auto.mp3"
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn dj_source_with_bad_credentials_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(source_config(&work.path().join("srv")))
        .await
        .unwrap();
    let addr = burrow.radio_source_addr.expect("source ingest enabled");

    // Wrong password: rejected with 401 before it can publish.
    let mut source = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "PUT /live HTTP/1.1\r\nAuthorization: Basic {}\r\n\r\n",
        basic_auth("source", "wrong-password")
    );
    source.write_all(head.as_bytes()).await.unwrap();
    source.flush().await.unwrap();
    let resp = read_at_least(&mut source, 12).await;
    let resp = String::from_utf8_lossy(&resp);
    assert!(resp.contains("401"), "bad creds refused: {resp:?}");
    assert!(!burrow.shared.radio.is_live("live"));

    burrow.shutdown().await;
}

#[tokio::test]
async fn source_ingest_is_off_by_default() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(base_config(&work.path().join("srv")))
        .await
        .unwrap();
    assert!(
        burrow.radio_source_addr.is_none(),
        "source ingest must be opt-in"
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn library_program_pulls_audio_from_file_area() {
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("srv");

    // First boot: populate a file area with audio and non-audio files.
    {
        let burrow = Burrow::start(base_config(&data)).await.unwrap();
        let files = &burrow.shared.files;
        files.create_area("music", "Music", "").await.unwrap();
        for (name, mime) in [
            ("track-a.mp3", "audio/mpeg"),
            ("readme.txt", "text/plain"),
            ("track-b.ogg", "application/octet-stream"),
        ] {
            files
                .add_file("music", None, name, &[1u8; 32], 10, mime, "", "", "dj@h", 1)
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    // Second boot: map the area into a station via config; startup installs it.
    let mut cfg = base_config(&data);
    cfg.radio_library_areas
        .insert("jukebox".into(), "music".into());
    let burrow = Burrow::start(cfg).await.unwrap();

    assert_eq!(
        burrow.shared.radio.program_slugs(),
        vec!["jukebox".to_string()]
    );
    // Children sort by name; the first audio file leads (non-audio dropped).
    let np = burrow
        .shared
        .radio
        .now_playing("jukebox")
        .expect("automation now-playing");
    assert_eq!(np.title, "track-a.mp3");
    assert_ne!(np.title, "readme.txt");
    assert!(!burrow.shared.radio.is_live("jukebox"));

    burrow.shutdown().await;
}
