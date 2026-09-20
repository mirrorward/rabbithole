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
    //    How much that is depends on the block's own length byte, so read up
    //    to the length byte first and then to one byte past the block. (This
    //    used to read a fixed `metaint + 33` and then index byte `metaint +
    //    33`: it passed only when a read happened to overshoot, and failed
    //    whenever a chunk boundary landed exactly there.)
    let mut want = DEFAULT_METAINT + 1;
    loop {
        while received.len() < want {
            let more = read_at_least(&mut listener, 1).await;
            if more.is_empty() {
                break;
            }
            received.extend_from_slice(&more);
        }
        if received.len() < want {
            break; // the stream ended early; the asserts below say so
        }
        let after_block = DEFAULT_METAINT + 1 + received[DEFAULT_METAINT] as usize * 16 + 1;
        if want >= after_block {
            break;
        }
        want = after_block;
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
async fn a_client_is_told_where_to_tune_in_without_being_asked() {
    use rabbithole_core::Client;
    use rabbithole_proto::radio::{RadioStations, RadioStationsRequest};

    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("dj", "spin-spin-spin", Role::Admin)
        .await
        .unwrap();
    let radio = burrow.radio_addr.expect("radio enabled");

    // A guest: tuning in needs no account, and neither does finding out where.
    let mut guest = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("listener".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();

    // Nothing on the air yet. The port is already known, and it is the one
    // that actually bound (the config said 0, "any").
    let quiet: RadioStations = guest.request(&RadioStationsRequest).await.unwrap();
    assert_eq!(quiet.port, radio.port());
    assert_ne!(quiet.port, 0);
    assert_eq!(quiet.stream_base, "");
    assert!(quiet.stations.is_empty());

    // A DJ goes live on /live.
    let mut source = TcpStream::connect(radio).await.unwrap();
    let head = format!(
        "PUT /live HTTP/1.1\r\n\
         Authorization: Basic {}\r\n\
         ice-name: Warren FM\r\n\
         content-type: audio/mpeg\r\n\r\n",
        basic_auth("dj", "spin-spin-spin")
    );
    source.write_all(head.as_bytes()).await.unwrap();
    source.flush().await.unwrap();
    let ack = read_at_least(&mut source, 12).await;
    assert!(String::from_utf8_lossy(&ack).contains("200 OK"));

    let on: RadioStations = guest.request(&RadioStationsRequest).await.unwrap();
    let live = on
        .stations
        .iter()
        .find(|s| s.station == "live")
        .expect("the mount is listed");
    assert!(live.live);
    assert!(
        live.streaming,
        "a source is connected: there is audio to be had"
    );
    assert_eq!(live.name, "Warren FM");
    assert!(live.recent.is_empty(), "its first track is still playing");

    // An operator behind a TLS proxy names the public address, live.
    burrow
        .shared
        .config
        .set_key("radio_public_base", "https://radio.example.org/")
        .unwrap();
    let named: RadioStations = guest.request(&RadioStationsRequest).await.unwrap();
    assert_eq!(named.stream_base, "https://radio.example.org");

    // The DJ leaves. The station comes off the listing, and says so.
    drop(source);
    let mut off = named;
    for _ in 0..50 {
        off = guest.request(&RadioStationsRequest).await.unwrap();
        if off.stations.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(off.stations.is_empty(), "off the air: {:?}", off.stations);
    burrow.shutdown().await;
}

/// `count` structurally valid MP3 frames (MPEG-1 Layer III, 128 kbit/s, 44.1
/// kHz: 417 bytes and 26 ms each). `tag` goes in the silent bodies so two
/// tracks are different files with different content hashes.
fn mp3_of(count: usize, tag: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(count * 417);
    for _ in 0..count {
        out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend(std::iter::repeat_n(tag, 413));
    }
    out
}

#[tokio::test]
async fn a_library_station_streams_its_rotation_and_yields_to_a_dj() {
    use rabbithole_core::Client;
    use rabbithole_proto::radio::{RadioStationInfo, RadioStations, RadioStationsRequest};

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    // About three seconds of audio each.
    let (one, two) = (mp3_of(115, 0x11), mp3_of(115, 0x22));

    // First boot: put the music in a file area.
    {
        let burrow = Burrow::start(test_config(&dir)).await.unwrap();
        burrow
            .shared
            .auth
            .create_account("dj", "spin-spin-spin", Role::Admin)
            .await
            .unwrap();
        let mut dj = Client::connect(
            &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
            None,
            None,
            "e2e",
            "0",
        )
        .await
        .unwrap();
        dj.auth_password("dj", "spin-spin-spin").await.unwrap();
        dj.expect_welcome().await.unwrap();
        dj.area_create("music", "Music", "").await.unwrap();
        for (name, bytes) in [("one.mp3", &one), ("two.mp3", &two)] {
            let src = work.path().join(name);
            std::fs::write(&src, bytes).unwrap();
            dj.transfer_upload("music", None, name, &src, "audio/mpeg", "The Lagomorphs")
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    // Second boot: that area is a station.
    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("ambient".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    let radio = burrow.radio_addr.expect("radio enabled");

    // Tune in, with no in-band metadata, and hear the file itself: whole
    // frames, verbatim, from the top. Before the pump existed this was a 404:
    // a library station had a now-playing and no audio.
    let mut listener = TcpStream::connect(radio).await.unwrap();
    listener
        .write_all(b"GET /ambient HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    listener.flush().await.unwrap();
    let (head, mut heard) = read_head(&mut listener).await;
    assert!(
        head.starts_with("ICY 200 OK"),
        "a rotation streams: {head:?}"
    );
    assert!(head.to_ascii_lowercase().contains("audio/mpeg"), "{head:?}");
    while heard.len() < 10 * 417 {
        let more = read_at_least(&mut listener, 1).await;
        assert!(
            !more.is_empty(),
            "the stream ended after {} bytes",
            heard.len()
        );
        heard.extend_from_slice(&more);
    }
    // Whichever track the rotation is on, what arrives is a run of its frames.
    let whole = heard.len() / 417 * 417;
    assert!(
        one.windows(whole).any(|w| w == &heard[..whole])
            || two.windows(whole).any(|w| w == &heard[..whole])
            || heard[..whole]
                .chunks(417)
                .all(|f| f.starts_with(&[0xFF, 0xFB, 0x90, 0x00])),
        "frames arrive whole and in order"
    );
    assert!(
        heard.starts_with(&[0xFF, 0xFB, 0x90, 0x00]),
        "from a frame boundary"
    );

    let mut guest = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("listener".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    async fn ambient(guest: &mut Client) -> Option<RadioStationInfo> {
        let listing: RadioStations = guest.request(&RadioStationsRequest).await.unwrap();
        listing
            .stations
            .into_iter()
            .find(|s| s.station == "ambient")
    }

    // The listing says there is audio to be had, and whose turn it is.
    let now = ambient(&mut guest)
        .await
        .expect("the rotation is on the air");
    assert!(now.streaming, "Listen would work");
    assert!(!now.live);
    assert!(
        ["one.mp3", "two.mp3"].contains(&now.title.as_str()),
        "{now:?}"
    );

    // The rotation moves on when the audio ends, not on a nominal timer, and
    // remembers what it played.
    let mut moved = None;
    for _ in 0..80 {
        let s = ambient(&mut guest).await.unwrap();
        if !s.recent.is_empty() {
            moved = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let moved = moved.expect("a three-second track ends well inside eight seconds");
    assert_ne!(
        moved.recent[0].title, moved.title,
        "history is the track before"
    );

    // A DJ takes the air. A rotation's mount gives way; it is not a 403.
    let mut source = TcpStream::connect(radio).await.unwrap();
    let head = format!(
        "PUT /ambient HTTP/1.1\r\n\
         Authorization: Basic {}\r\n\
         ice-name: Live from the Warren\r\n\
         content-type: audio/mpeg\r\n\r\n",
        basic_auth("dj", "spin-spin-spin")
    );
    source.write_all(head.as_bytes()).await.unwrap();
    source.flush().await.unwrap();
    let ack = read_at_least(&mut source, 12).await;
    assert!(
        String::from_utf8_lossy(&ack).contains("200 OK"),
        "the DJ is let on: {:?}",
        String::from_utf8_lossy(&ack)
    );
    let on_air = ambient(&mut guest).await.unwrap();
    assert!(on_air.live, "a person has the air: {on_air:?}");

    // The DJ leaves, and the rotation picks itself back up.
    drop(source);
    let mut resumed = false;
    for _ in 0..80 {
        if ambient(&mut guest)
            .await
            .is_some_and(|s| !s.live && s.streaming)
        {
            resumed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(resumed, "the rotation came back on its own");

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

/// One Ogg page carrying `payload`. Enough of the container for the station
/// to pace it; the checksum is the listener's decoder's business.
fn ogg_page(granule: u64, payload: &[u8], beginning: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(u8::from(beginning) << 1);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut table = Vec::new();
    let mut left = payload.len();
    while left >= 255 {
        table.push(255u8);
        left -= 255;
    }
    table.push(left as u8);
    out.push(table.len() as u8);
    out.extend_from_slice(&table);
    out.extend_from_slice(payload);
    out
}

/// `seconds` of Opus: the identification and comment pages, then a page of
/// audio every tenth of a second.
fn opus_of(seconds: u64, tag: u8) -> Vec<u8> {
    let mut out = ogg_page(
        0,
        b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00",
        true,
    );
    out.extend(ogg_page(0, b"OpusTags\x00\x00\x00\x00", false));
    for tenth in 1..=seconds * 10 {
        out.extend(ogg_page(tenth * 4_800, &[tag; 120], false));
    }
    out
}

/// A library of Ogg files plays as an Ogg station: the listener is told it
/// is `audio/ogg` and hears the pages themselves. Before this, a rotation
/// of anything but MP3 was silently skipped and the station played nothing.
#[tokio::test]
async fn a_library_of_ogg_files_streams_as_an_ogg_station() {
    use rabbithole_core::Client;

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    let song = opus_of(3, 0x5A);

    {
        let burrow = Burrow::start(test_config(&dir)).await.unwrap();
        burrow
            .shared
            .auth
            .create_account("dj", "spin-spin-spin", Role::Admin)
            .await
            .unwrap();
        let mut dj = Client::connect(
            &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
            None,
            None,
            "e2e",
            "0",
        )
        .await
        .unwrap();
        dj.auth_password("dj", "spin-spin-spin").await.unwrap();
        dj.expect_welcome().await.unwrap();
        dj.area_create("music", "Music", "").await.unwrap();
        let src = work.path().join("one.opus");
        std::fs::write(&src, &song).unwrap();
        dj.transfer_upload(
            "music",
            None,
            "one.opus",
            &src,
            "audio/ogg",
            "The Lagomorphs",
        )
        .await
        .unwrap();
        burrow.shutdown().await;
    }

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("oggcast".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    let radio = burrow.radio_addr.expect("radio enabled");

    let mut listener = TcpStream::connect(radio).await.unwrap();
    listener
        .write_all(b"GET /oggcast HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    listener.flush().await.unwrap();
    let (head, mut heard) = read_head(&mut listener).await;
    assert!(
        head.starts_with("ICY 200 OK"),
        "a rotation streams: {head:?}"
    );
    assert!(
        head.to_ascii_lowercase().contains("audio/ogg"),
        "an Ogg station says so: {head:?}"
    );

    // What arrives is the file's own pages, from the top.
    let mut more = read_at_least(&mut listener, 200).await;
    heard.append(&mut more);
    assert!(heard.starts_with(b"OggS"), "pages, from the first one");
    assert!(
        song.windows(heard.len().min(120))
            .any(|w| w == &heard[..heard.len().min(120)]),
        "the audio is the file's, verbatim"
    );

    burrow.shutdown().await;
}
