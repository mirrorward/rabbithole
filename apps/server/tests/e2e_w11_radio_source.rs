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
    let sound = burrow::radio::sound_of_tracks(&tracks);
    burrow
        .shared
        .radio
        .install_program("live", "Live FM", "music", tracks, sound);
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

    // The area holds both kinds, so it is a mount of each: the MP3 file
    // where it has always been, the Ogg file beside it.
    assert_eq!(
        burrow.shared.radio.program_slugs(),
        vec!["jukebox".to_string(), "jukebox.ogg".to_string()]
    );
    assert_eq!(
        burrow
            .shared
            .radio
            .now_playing("jukebox.ogg")
            .expect("the Ogg mount plays too")
            .title,
        "track-b.ogg"
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

/// What the reference FLAC fixture is: 8 kHz, one channel, 16 bits. A
/// library reads this from the front of each file when it is installed, so
/// a mount says what its stream is before a note of it has played.
const FLAC_FORM: burrow::radio::Form = burrow::radio::Form {
    rate: 8_000,
    channels: 1,
    bits: 16,
};

/// Which kind of sound keeps the bare mount does not depend on how many of
/// each a library happens to hold: adding files must not move a station's
/// listeners onto a different kind. And a station the operator named
/// themselves is not replaced by one derived from another library.
#[tokio::test]
async fn the_bare_mount_keeps_its_kind_and_a_named_station_is_left_alone() {
    const REFERENCE_FLAC: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");

    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("srv");

    // A library of one MP3 and three FLACs: the MP3 keeps the bare mount,
    // outnumbered three to one.
    {
        let burrow = Burrow::start(base_config(&data)).await.unwrap();
        let files = &burrow.shared.files;
        files.create_area("music", "Music", "").await.unwrap();
        let mp3 = burrow.shared.blobs.put(&[0xFFu8; 64]).unwrap();
        let flac = burrow.shared.blobs.put(REFERENCE_FLAC).unwrap();
        files
            .add_file(
                "music",
                None,
                "one.mp3",
                &mp3.0,
                64,
                "audio/mpeg",
                "",
                "",
                "dj@h",
                1,
            )
            .await
            .unwrap();
        for n in 0..3 {
            files
                .add_file(
                    "music",
                    None,
                    &format!("track-{n}.flac"),
                    &flac.0,
                    REFERENCE_FLAC.len() as i64,
                    "audio/flac",
                    "",
                    "",
                    "dj@h",
                    1,
                )
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    let mut cfg = base_config(&data);
    cfg.radio_library_areas
        .insert("jukebox".into(), "music".into());
    let burrow = Burrow::start(cfg).await.unwrap();
    assert_eq!(
        burrow.shared.radio.program_slugs(),
        vec!["jukebox".to_string(), "jukebox.flac".to_string()],
        "the MP3 keeps the bare mount, outnumbered or not"
    );
    assert_eq!(burrow.shared.radio.track_count("jukebox"), 1);
    assert_eq!(burrow.shared.radio.track_count("jukebox.flac"), 3);
    // Each mount goes up as what it is about to send, so nobody who tuned
    // in early is cut off when the first track is read.
    assert_eq!(
        burrow.shared.radio.expected_sound("jukebox"),
        Some(burrow::radio::Sound::Mpeg)
    );
    assert_eq!(
        burrow.shared.radio.expected_sound("jukebox.flac"),
        Some(burrow::radio::Sound::Flac(FLAC_FORM))
    );
    burrow.shutdown().await;

    // An operator who names a mount for a kind gets that kind under that
    // name: /jukebox.flac is the FLAC, and the MP3 moves to /jukebox.mp3
    // rather than taking the name and leaving FLAC at jukebox.flac.flac.
    let mut cfg = base_config(&data);
    cfg.radio_library_areas
        .insert("jukebox.flac".into(), "music".into());
    let burrow = Burrow::start(cfg).await.unwrap();
    let slugs = burrow.shared.radio.program_slugs();
    assert_eq!(
        slugs,
        vec!["jukebox.flac".to_string(), "jukebox.mp3".to_string()],
        "a mount named for a kind is that kind"
    );
    assert_eq!(burrow.shared.radio.track_count("jukebox.flac"), 3);
    assert_eq!(
        burrow.shared.radio.expected_sound("jukebox.flac"),
        Some(burrow::radio::Sound::Flac(FLAC_FORM)),
        "the name says FLAC, so the mount had better send FLAC"
    );
    burrow.shutdown().await;

    // And a station the operator configured themselves is not replaced by
    // one another library derived: theirs is the one on the air.
    let mut cfg = base_config(&data);
    cfg.radio_library_areas
        .insert("jukebox".into(), "music".into());
    cfg.radio_library_areas
        .insert("jukebox.flac".into(), "music".into());
    let burrow = Burrow::start(cfg).await.unwrap();
    let slugs = burrow.shared.radio.program_slugs();
    assert!(slugs.contains(&"jukebox.flac".to_string()));
    assert_eq!(
        burrow.shared.radio.track_count("jukebox"),
        1,
        "the bare mount is still the MP3 one: {slugs:?}"
    );
    assert_eq!(
        burrow.shared.radio.track_count("jukebox.flac"),
        3,
        "the operator's own station, from its own area: {slugs:?}"
    );

    burrow.shutdown().await;
}

/// A mount goes up as the kind of sound its rotation will actually send,
/// even when the area it came from holds just as many files this burrow
/// cannot play at all. Getting that wrong puts the mount up as MP3 and cuts
/// every listener a second later, when the first real track is read.
#[tokio::test]
async fn a_mount_goes_up_as_what_it_will_send_not_what_it_cannot_play() {
    const REFERENCE_FLAC: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");

    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("srv");
    {
        let burrow = Burrow::start(base_config(&data)).await.unwrap();
        let files = &burrow.shared.files;
        files.create_area("music", "Music", "").await.unwrap();
        let flac = burrow.shared.blobs.put(REFERENCE_FLAC).unwrap();
        let wav = burrow.shared.blobs.put(&[0x00u8; 64]).unwrap();
        for n in 0..3 {
            files
                .add_file(
                    "music",
                    None,
                    &format!("track-{n}.flac"),
                    &flac.0,
                    REFERENCE_FLAC.len() as i64,
                    "audio/flac",
                    "",
                    "",
                    "dj@h",
                    1,
                )
                .await
                .unwrap();
            files
                .add_file(
                    "music",
                    None,
                    &format!("rip-{n}.wav"),
                    &wav.0,
                    64,
                    "audio/wav",
                    "",
                    "",
                    "dj@h",
                    1,
                )
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    let mut cfg = base_config(&data);
    cfg.radio_library_areas
        .insert("jukebox".into(), "music".into());
    let burrow = Burrow::start(cfg).await.unwrap();
    assert_eq!(
        burrow.shared.radio.program_slugs(),
        vec!["jukebox".to_string()],
        "one kind it can send, so one mount"
    );
    assert_eq!(
        burrow.shared.radio.track_count("jukebox"),
        6,
        "the WAVs ride along so the console can say why they are silent"
    );
    assert_eq!(
        burrow.shared.radio.expected_sound("jukebox"),
        Some(burrow::radio::Sound::Flac(FLAC_FORM)),
        "three FLACs and three files it cannot play is a FLAC station"
    );
    burrow.shutdown().await;
}

/// A station sends one form of FLAC, and which one is what most of its
/// library is — not whichever file the rotation happens to read first. A
/// voice memo at the top of a folder of albums would otherwise pin the
/// mount to 8 kHz mono and leave every album out of it, for good.
#[tokio::test]
async fn a_stations_form_is_what_most_of_its_library_is() {
    const MEMO: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");
    const ALBUM: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-44k-stereo.flac");

    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("srv");
    {
        let burrow = Burrow::start(base_config(&data)).await.unwrap();
        let files = &burrow.shared.files;
        files.create_area("music", "Music", "").await.unwrap();
        // The memo is first in the area, and first in the rotation.
        let memo = burrow.shared.blobs.put(MEMO).unwrap();
        files
            .add_file(
                "music",
                None,
                "a-memo.flac",
                &memo.0,
                MEMO.len() as i64,
                "audio/flac",
                "",
                "",
                "dj@h",
                1,
            )
            .await
            .unwrap();
        // And the albums carry a tag longer than the look a library takes
        // at a file, the way one with a picture in it does.
        let tagged = {
            const PAYLOAD: u32 = 2048;
            let mut out = b"ID3\x04\x00\x00".to_vec();
            out.extend_from_slice(&[
                ((PAYLOAD >> 21) & 0x7F) as u8,
                ((PAYLOAD >> 14) & 0x7F) as u8,
                ((PAYLOAD >> 7) & 0x7F) as u8,
                (PAYLOAD & 0x7F) as u8,
            ]);
            out.resize(10 + PAYLOAD as usize, 0);
            out.extend_from_slice(ALBUM);
            out
        };
        let album = burrow.shared.blobs.put(&tagged).unwrap();
        for n in 0..3 {
            files
                .add_file(
                    "music",
                    None,
                    &format!("b-album-{n}.flac"),
                    &album.0,
                    tagged.len() as i64,
                    "audio/flac",
                    "",
                    "",
                    "dj@h",
                    1,
                )
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    let mut cfg = base_config(&data);
    cfg.radio_library_areas
        .insert("jukebox".into(), "music".into());
    let burrow = Burrow::start(cfg).await.unwrap();
    assert_eq!(
        burrow.shared.radio.expected_sound("jukebox"),
        Some(burrow::radio::Sound::Flac(burrow::radio::Form {
            rate: 44_100,
            channels: 2,
            bits: 16,
        })),
        "three albums against one memo: the albums are the station"
    );
    burrow.shutdown().await;
}
