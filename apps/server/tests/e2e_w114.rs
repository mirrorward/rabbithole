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

/// Keep the initial response body and read until enough complete FLAC frames
/// have arrived. One deadline covers all reads, regardless of TCP chunk sizes.
async fn read_flac_frames(
    sock: &mut TcpStream,
    heard: &mut Vec<u8>,
    want: usize,
) -> Vec<rabbithole_radio::flac::Frame> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut chunk = [0u8; 4096];
        loop {
            let frames = rabbithole_radio::flac::frames(heard);
            if frames.len() >= want {
                break frames;
            }
            let n = sock.read(&mut chunk).await.expect("socket readable");
            assert!(n > 0, "FLAC stream ended before {want} complete frames");
            heard.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{want} complete FLAC frames should arrive within 5 seconds"))
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
    use rabbithole_proto::radio::{
        RadioOffer, RadioOfferRequest, RadioRequest, RadioRequests, RadioRequestsRequest,
        RadioStationInfo, RadioStations, RadioStationsRequest,
    };

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    // About three seconds of audio each.
    let (one, two, three) = (mp3_of(115, 0x11), mp3_of(115, 0x22), mp3_of(115, 0x33));

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
        for (name, bytes) in [("one.mp3", &one), ("two.mp3", &two), ("three.mp3", &three)] {
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
            || three.windows(whole).any(|w| w == &heard[..whole])
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
        ["one.mp3", "two.mp3", "three.mp3"].contains(&now.title.as_str()),
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

    // Somebody asks for a song, and a DJ takes the air before it comes up.
    let mut asker = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    asker.auth_password("dj", "spin-spin-spin").await.unwrap();
    asker.expect_welcome().await.unwrap();
    let offer: RadioOffer = asker
        .request(&RadioOfferRequest::new("ambient", ""))
        .await
        .unwrap();
    let wanted = offer.tracks[0].clone();
    let _: RadioRequests = asker
        .request(&RadioRequest::new("ambient", wanted.id))
        .await
        .unwrap();

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

    // The request is not spent while the DJ has the air: nobody would hear
    // it. (Unless the song before it ended first, a moment before the DJ
    // arrived, and it was already playing when they took over.)
    async fn waiting(asker: &mut Client) -> RadioRequests {
        asker
            .request(&RadioRequestsRequest::new("ambient"))
            .await
            .unwrap()
    }
    let during = waiting(&mut asker).await;
    assert!(during.dj_live, "{during:?}");
    let still_to_come = during.queue.iter().any(|q| q.id == wanted.id);
    if still_to_come {
        tokio::time::sleep(Duration::from_millis(800)).await;
        let later = waiting(&mut asker).await;
        assert!(
            later.queue.iter().any(|q| q.id == wanted.id),
            "a request waits out the DJ rather than being played to nobody: {later:?}"
        );
    }

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
    // And what was asked for is what comes back first — announced as the
    // DJ goes, not after the song they talked over is said to play again.
    if still_to_come {
        let back = ambient(&mut guest).await.unwrap();
        assert_eq!(
            back.title, wanted.title,
            "{} plays once the DJ has gone",
            wanted.title
        );
    }

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

/// An operator can see what each station is doing, including the tracks its
/// rotation could not play. Before this, a station that was silent because
/// its music is the wrong kind looked exactly like one that was fine.
#[tokio::test]
async fn an_operator_sees_what_a_station_is_doing_and_what_it_left_out() {
    use rabbithole_core::Client;
    use rabbithole_proto::radio::{RadioStatus, RadioStatusRequest};
    use rabbithole_proto::ErrorCode;

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    let song = opus_of(2, 0x31);

    {
        let burrow = Burrow::start(test_config(&dir)).await.unwrap();
        for (who, role) in [("boss", Role::Admin), ("listener", Role::User)] {
            burrow
                .shared
                .auth
                .create_account(who, "pw-pw-pw-pw", role)
                .await
                .unwrap();
        }
        let mut dj = Client::connect(
            &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
            None,
            None,
            "e2e",
            "0",
        )
        .await
        .unwrap();
        dj.auth_password("boss", "pw-pw-pw-pw").await.unwrap();
        dj.expect_welcome().await.unwrap();
        dj.area_create("music", "Music", "").await.unwrap();
        // One playable track, and one that is not audio at all.
        for (name, bytes, mime) in [
            ("one.opus", song.clone(), "audio/ogg"),
            ("two.mp3", b"this is not an mp3".to_vec(), "audio/mpeg"),
        ] {
            let src = work.path().join(name);
            std::fs::write(&src, &bytes).unwrap();
            dj.transfer_upload("music", None, name, &src, mime, "")
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("oggcast".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();

    let mut boss = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    boss.auth_password("boss", "pw-pw-pw-pw").await.unwrap();
    boss.expect_welcome().await.unwrap();

    // Give the rotation a moment to reach the track it cannot play.
    let mut status: RadioStatus = boss.request(&RadioStatusRequest).await.unwrap();
    for _ in 0..40 {
        if status.stations.iter().any(|s| !s.left_out.is_empty()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        status = boss.request(&RadioStatusRequest).await.unwrap();
    }
    // A library of both kinds is a mount of each: the file named .mp3 on
    // the bare mount, the Opus file beside it.
    let by_slug = |slug: &str| {
        status
            .stations
            .iter()
            .find(|s| s.station == slug)
            .unwrap_or_else(|| panic!("a station at {slug}: {:?}", status.stations))
            .clone()
    };
    let ogg = by_slug("oggcast.ogg");
    assert_eq!(ogg.area, "music", "where its music comes from");
    assert_eq!(ogg.tracks, 1);
    assert_eq!(
        ogg.content_type, "audio/ogg",
        "it settled on what it could play: {ogg:?}"
    );
    // The bare mount has the file that claims to be an MP3 and is not.
    let station = by_slug("oggcast");
    assert_eq!(station.tracks, 1);
    let left = station.left_out.first().expect("the one it cannot play");
    assert_eq!(left.title, "two.mp3");
    assert!(
        left.reason.contains("not audio") || left.reason.contains("not what"),
        "said for a person: {:?}",
        left.reason
    );

    // It is the operator's view, not everybody's.
    let mut listener = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    listener
        .auth_password("listener", "pw-pw-pw-pw")
        .await
        .unwrap();
    listener.expect_welcome().await.unwrap();
    let refused = listener
        .request::<_, RadioStatus>(&RadioStatusRequest)
        .await;
    assert!(
        matches!(
            refused,
            Err(rabbithole_core::ClientError::Refused(ErrorCode::Forbidden))
        ),
        "not for everyone: {refused:?}"
    );

    burrow.shutdown().await;
}

/// A library holding both kinds plays both: the MP3 files where they have
/// always been, and the Ogg files beside them. Nothing is left out for
/// being the wrong kind, and a listener picks which to tune in to.
#[tokio::test]
async fn a_library_of_both_kinds_plays_on_a_mount_each() {
    use rabbithole_core::Client;

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    let mp3 = mp3_of(115, 0x44);
    let opus = opus_of(3, 0x77);

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
        for (name, bytes, mime) in [
            ("one.mp3", &mp3, "audio/mpeg"),
            ("two.opus", &opus, "audio/ogg"),
        ] {
            let src = work.path().join(name);
            std::fs::write(&src, bytes).unwrap();
            dj.transfer_upload("music", None, name, &src, mime, "")
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("mixed".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    let radio = burrow.radio_addr.expect("radio enabled");

    // The bare mount is the MP3 one, as it has always been.
    let mut plain = TcpStream::connect(radio).await.unwrap();
    plain
        .write_all(b"GET /mixed HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    plain.flush().await.unwrap();
    let (head, _heard) = read_head(&mut plain).await;
    assert!(head.starts_with("ICY 200 OK"), "{head:?}");
    assert!(
        head.to_ascii_lowercase().contains("audio/mpeg"),
        "the bare mount stays MP3: {head:?}"
    );

    // And the Ogg files are beside it, on their own mount, at the same time.
    let mut ogg = TcpStream::connect(radio).await.unwrap();
    ogg.write_all(b"GET /mixed.ogg HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    ogg.flush().await.unwrap();
    let (head, mut heard) = read_head(&mut ogg).await;
    assert!(head.starts_with("ICY 200 OK"), "{head:?}");
    assert!(
        head.to_ascii_lowercase().contains("audio/ogg"),
        "beside it, an Ogg mount: {head:?}"
    );
    let mut more = read_at_least(&mut ogg, 64).await;
    heard.append(&mut more);
    assert!(heard.starts_with(b"OggS"), "pages, from the first one");

    burrow.shutdown().await;
}

/// A library of FLAC files plays as a FLAC station: the listener is told it
/// is `audio/flac` and is given the mount's own stream — one set of headers,
/// then the file's frames. The file is the one the reference encoder wrote,
/// so what arrives can be checked against it frame for frame.
#[tokio::test]
async fn a_library_of_flac_files_streams_as_a_flac_station() {
    use rabbithole_core::Client;

    const REFERENCE_FLAC: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");

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
        let src = work.path().join("tone.flac");
        std::fs::write(&src, REFERENCE_FLAC).unwrap();
        dj.transfer_upload("music", None, "tone.flac", &src, "audio/flac", "")
            .await
            .unwrap();
        burrow.shutdown().await;
    }

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("lossless".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    let radio = burrow.radio_addr.expect("radio enabled");

    let mut listener = TcpStream::connect(radio).await.unwrap();
    listener
        .write_all(b"GET /lossless HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    listener.flush().await.unwrap();
    let (head, mut heard) = read_head(&mut listener).await;
    assert!(
        head.starts_with("ICY 200 OK"),
        "a rotation streams: {head:?}"
    );
    assert!(
        head.to_ascii_lowercase().contains("audio/flac"),
        "a FLAC station says so: {head:?}"
    );

    // What arrives is the mount's own stream: one `fLaC` magic and one
    // STREAMINFO, said once for the whole night, and then the file's
    // frames — not the file, which would put a second set of headers in
    // the middle of the stream at every track and stop a decoder dead.
    let info = rabbithole_radio::flac::playable(REFERENCE_FLAC).unwrap();
    let theirs = rabbithole_radio::flac::frames(REFERENCE_FLAC);
    // TCP may split a cycle or coalesce several. Read complete frames, counting
    // the body read_head already received, under one deadline. Deliberately
    // collect two cycles so extra rotation data is always part of this check.
    let frames = read_flac_frames(&mut listener, &mut heard, theirs.len() * 2).await;
    let head = rabbithole_radio::flac::stream_headers(
        info.sample_rate,
        info.channels,
        info.bits_per_sample,
    );
    assert_eq!(
        &heard[..head.len()],
        &head[..],
        "the mount says what its stream is, once"
    );
    assert_eq!(
        heard.windows(4).filter(|w| *w == b"fLaC").count(),
        1,
        "and does not say it again"
    );
    // And the first complete cycle is the audio of that file, frame for frame:
    // the same bytes, renumbered to carry on from where the mount is. Further
    // cycles received in the same read are valid ongoing station output.
    let first_cycle = &frames[..theirs.len()];
    assert_eq!(frames[0].offset, head.len(), "audio, straight after");
    assert!(
        frames
            .windows(2)
            .all(|w| w[0].offset + w[0].len == w[1].offset),
        "complete frames stay back to back across rotations"
    );
    assert_eq!(
        first_cycle
            .iter()
            .map(|f| u64::from(f.samples))
            .sum::<u64>(),
        info.total_samples
    );
    for (ours, theirs) in first_cycle.iter().zip(&theirs) {
        assert_eq!(
            ours.samples, theirs.samples,
            "the frame keeps its sample count"
        );
        assert_eq!(
            ours.sample_rate, theirs.sample_rate,
            "the sample rate stays put"
        );
        assert_eq!(
            &heard[ours.offset + ours.header..ours.offset + ours.len - 2],
            &REFERENCE_FLAC[theirs.offset + theirs.header..theirs.offset + theirs.len - 2],
            "the audio inside a frame is the encoder's own, untouched"
        );
    }

    burrow.shutdown().await;
}

/// A station is one stream, not a file after a file. Two tracks play as one
/// run of frames under one set of headers, numbered so they carry on across
/// the join — a second `fLaC` magic mid-stream is what stops a native FLAC
/// player at the end of the first song. A track of another form is not this
/// stream, so it is left out with a word about why.
#[tokio::test]
async fn a_flac_station_carries_on_across_a_track_change() {
    use rabbithole_core::Client;

    const EIGHT_K: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");
    const FORTY_FOUR_K: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-44k-stereo.flac");

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");

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
        for (name, bytes) in [
            ("a-tone.flac", EIGHT_K),
            ("b-tone.flac", EIGHT_K),
            ("c-other.flac", FORTY_FOUR_K),
        ] {
            let src = work.path().join(name);
            std::fs::write(&src, bytes).unwrap();
            dj.transfer_upload("music", None, name, &src, "audio/flac", "")
                .await
                .unwrap();
        }
        burrow.shutdown().await;
    }

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("lossless".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    let radio = burrow.radio_addr.expect("radio enabled");

    let mut listener = TcpStream::connect(radio).await.unwrap();
    listener
        .write_all(b"GET /lossless HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    listener.flush().await.unwrap();
    let (head, mut heard) = read_head(&mut listener).await;
    assert!(head.starts_with("ICY 200 OK"), "{head:?}");

    // Both 8 kHz tracks, which is every frame of that file twice.
    let info = rabbithole_radio::flac::playable(EIGHT_K).unwrap();
    let theirs = rabbithole_radio::flac::frames(EIGHT_K);
    let frames = read_flac_frames(&mut listener, &mut heard, theirs.len() * 2).await;

    assert_eq!(
        heard.windows(4).filter(|w| *w == b"fLaC").count(),
        1,
        "one set of headers for the whole stream"
    );
    assert!(
        frames.len() >= theirs.len() * 2,
        "both tracks, as frames: {} of {}",
        frames.len(),
        theirs.len() * 2
    );
    assert!(
        frames
            .windows(2)
            .all(|w| w[0].offset + w[0].len == w[1].offset),
        "back to back, with nothing between the songs"
    );
    assert!(
        frames.iter().all(|f| f.sample_rate == info.sample_rate),
        "one form, all the way through"
    );

    // The numbers count samples across the join, so no decoder is told to
    // go back to the beginning of a song it has already played.
    let numbers: Vec<u64> = frames
        .iter()
        .map(|f| {
            let lead = heard[f.offset + 4];
            let (mut v, follow) = match lead {
                0x00..=0x7F => (u64::from(lead), 0),
                0xC0..=0xDF => (u64::from(lead & 0x1F), 1),
                0xE0..=0xEF => (u64::from(lead & 0x0F), 2),
                0xF0..=0xF7 => (u64::from(lead & 0x07), 3),
                0xF8..=0xFB => (u64::from(lead & 0x03), 4),
                0xFC..=0xFD => (u64::from(lead & 0x01), 5),
                _ => (0, 6),
            };
            for i in 0..follow {
                v = (v << 6) | u64::from(heard[f.offset + 5 + i] & 0x3F);
            }
            v
        })
        .collect();
    assert!(
        numbers.windows(2).all(|w| w[1] > w[0]),
        "always forwards: {numbers:?}"
    );
    assert_eq!(
        numbers[theirs.len()],
        numbers[0] + info.total_samples,
        "the second song starts where the first one ended"
    );

    // Receiving the second song does not mean the pump has examined the
    // incompatible third track yet. Wait for that specific diagnostic.
    let reason = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(reason) = burrow::radio::station_status(&burrow.shared)
                .iter()
                .find(|s| s.station == "lossless")
                .and_then(|s| s.left_out.iter().find(|l| l.title == "c-other.flac"))
                .map(|left| left.reason.clone())
            {
                break reason;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the incompatible track should be reported within 5 seconds");
    assert_eq!(
        reason,
        "not what this station is sending (audio/flac, 8000 Hz, mono, 16 bit)"
    );

    burrow.shutdown().await;
}

/// A download that stopped after the metadata still says what it was going
/// to be. It does not get a say in what the station sends: the files that
/// have audio where their headers end decide that, however many of the
/// others there are.
#[tokio::test]
async fn a_stalled_download_does_not_decide_what_a_station_sends() {
    const EIGHT_K: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");
    const FORTY_FOUR_K: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-44k-stereo.flac");

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    let headers = &FORTY_FOUR_K[..rabbithole_radio::flac::playable(FORTY_FOUR_K)
        .unwrap()
        .audio_at];
    library_of(
        &work,
        &dir,
        &[
            ("a-stopped.flac", headers),
            ("b-stopped.flac", headers),
            ("c-stopped.flac", headers),
            ("d-good.flac", EIGHT_K),
        ],
    )
    .await;

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("lossless".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    assert_eq!(
        burrow.shared.radio.expected_sound("lossless"),
        Some(burrow::radio::Sound::Flac(burrow::radio::Form {
            rate: 8_000,
            channels: 1,
            bits: 16,
        })),
        "three files say 44.1 and hold no audio; one holds some"
    );
    burrow.shutdown().await;
}

/// And when the vote goes to a form that then turns out never to play — the
/// files that hold it are corrupt past their first header — the station
/// does not stand silent over the rest of the library. A rotation that
/// leaves out everything takes the next track that can play instead.
#[tokio::test]
async fn a_station_does_not_stay_silent_over_a_form_that_never_plays() {
    const EIGHT_K: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-8k-mono.flac");
    const FORTY_FOUR_K: &[u8] =
        include_bytes!("../../../crates/radio/tests/fixtures/reference-44k-stereo.flac");

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");

    // A 44.1 kHz file whose first frame header is whole and whose audio is
    // not: it looks playable from the front and walks to nothing.
    let torn = {
        let mut bytes = FORTY_FOUR_K.to_vec();
        let frame = rabbithole_radio::flac::frames(FORTY_FOUR_K)[0];
        bytes[frame.offset + frame.header + 4] ^= 0xFF;
        bytes
    };
    assert!(
        rabbithole_radio::flac::playable(&torn).is_some(),
        "still says what it is"
    );
    assert!(
        rabbithole_radio::flac::frames(&torn).is_empty(),
        "and has nothing to send"
    );

    library_of(
        &work,
        &dir,
        &[
            ("a-torn.flac", &torn),
            ("b-torn.flac", &torn),
            ("c-torn.flac", &torn),
            ("d-good.flac", EIGHT_K),
        ],
    )
    .await;

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("lossless".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    assert_eq!(
        burrow.shared.radio.expected_sound("lossless"),
        Some(burrow::radio::Sound::Flac(burrow::radio::Form {
            rate: 44_100,
            channels: 2,
            bits: 16,
        })),
        "three of them show a frame where their headers end, so they win the vote"
    );
    let radio = burrow.radio_addr.expect("radio enabled");

    // Under that form nothing plays at all. The station is not left so.
    let mut listener = TcpStream::connect(radio).await.unwrap();
    listener
        .write_all(b"GET /lossless HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    listener.flush().await.unwrap();
    let (head, mut heard) = read_head(&mut listener).await;
    assert!(head.starts_with("ICY 200 OK"), "{head:?}");
    let mut more = read_at_least(&mut listener, 512).await;
    heard.append(&mut more);
    let info = rabbithole_radio::flac::playable(EIGHT_K).unwrap();
    let want = rabbithole_radio::flac::stream_headers(
        info.sample_rate,
        info.channels,
        info.bits_per_sample,
    );
    assert_eq!(
        heard.get(..want.len()),
        Some(&want[..]),
        "8 kHz mono: the one it can actually play"
    );

    // And the operator can see what it could not play on the way there.
    let status = burrow::radio::station_status(&burrow.shared);
    let station = status
        .iter()
        .find(|s| s.station == "lossless")
        .expect("the station");
    assert!(
        station.left_out.iter().any(|l| l.title == "a-torn.flac"),
        "{:?}",
        station.left_out
    );

    burrow.shutdown().await;
}

/// A burrow whose `music` area holds exactly these files.
async fn library_of(work: &tempfile::TempDir, dir: &std::path::Path, files: &[(&str, &[u8])]) {
    use rabbithole_core::Client;

    let burrow = Burrow::start(test_config(dir)).await.unwrap();
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
    for (name, bytes) in files {
        let src = work.path().join(name);
        std::fs::write(&src, bytes).unwrap();
        let mime = burrow::radio::sound_of_name(name, "")
            .expect("fixture has a supported audio suffix")
            .content_type();
        dj.transfer_upload("music", None, name, &src, mime, "")
            .await
            .unwrap();
    }
    burrow.shutdown().await;
}

/// Listeners steer a station by asking for songs. What a station's rotation
/// offers is open to anybody; asking and joining in take an account that
/// may talk, one vote each, and a few waiting per person — enough to ask
/// for a song or two, not enough to take the evening over. And what is
/// asked for is what plays next.
#[tokio::test]
async fn a_listener_asks_for_a_song_and_it_plays_next() {
    use rabbithole_core::{Client, ClientError};
    use rabbithole_proto::radio::{
        RadioOffer, RadioOfferRequest, RadioRequest, RadioRequestVote, RadioRequests,
        RadioRequestsRequest,
    };
    use rabbithole_proto::ErrorCode;

    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for login in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    // A rotation of five, the way a library station is installed.
    let tracks: Vec<rabbithole_radio::Track> = (1..=5)
        .map(|n| {
            rabbithole_radio::Track::new(
                rabbithole_radio::TrackId(n),
                format!("song-{n}.mp3"),
                "The Lagomorphs",
                180_000,
                rabbithole_radio::BlobId([n as u8; 32]),
            )
        })
        .collect();
    let sound = burrow::radio::sound_of_tracks(&tracks);
    burrow
        .shared
        .radio
        .install_program("jukebox", "Jukebox", "music", tracks, sound);

    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let signed_in = |login: &'static str| {
        let url = url.clone();
        async move {
            let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
            c.auth_password(login, "pw-pw-pw-pw").await.unwrap();
            c.expect_welcome().await.unwrap();
            c
        }
    };
    let mut alice = signed_in("alice").await;
    let mut bob = signed_in("bob").await;

    // Nothing waiting yet; the station takes requests, and no DJ has it.
    let seen: RadioRequests = alice
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert!(seen.requestable && !seen.dj_live);
    assert!(seen.queue.is_empty());
    // What can be asked for is the rotation, less what is playing now.
    assert_eq!(
        burrow.shared.radio.now_playing("jukebox").unwrap().title,
        "song-1.mp3"
    );
    let offer: RadioOffer = alice
        .request(&RadioOfferRequest::new("jukebox", ""))
        .await
        .unwrap();
    let offered: Vec<u64> = offer.tracks.iter().map(|t| t.id).collect();
    assert_eq!(offered, [2, 3, 4, 5]);
    assert_eq!(offer.more, 0);
    let playing = alice
        .request::<_, RadioRequests>(&RadioRequest::new("jukebox", 1))
        .await;
    assert!(
        matches!(playing, Err(ClientError::Refused(ErrorCode::AlreadyExists))),
        "what is playing now is not asked for: {playing:?}"
    );

    // Alice asks for song 4; it is waiting, and it is hers.
    let after: RadioRequests = alice
        .request(&RadioRequest::new("jukebox", 4))
        .await
        .unwrap();
    assert_eq!(after.queue.len(), 1);
    assert_eq!(after.queue[0].title, "song-4.mp3");
    assert_eq!(after.queue[0].votes, 1);
    assert!(after.queue[0].mine, "she asked for it");

    // Bob sees it as somebody else's, and joins in; voting twice is one vote.
    let his: RadioRequests = bob
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert!(!his.queue[0].mine);
    let _: RadioRequests = bob
        .request(&RadioRequestVote::new("jukebox", 4))
        .await
        .unwrap();
    let again: RadioRequests = bob
        .request(&RadioRequestVote::new("jukebox", 4))
        .await
        .unwrap();
    assert_eq!(
        again.queue[0].votes, 2,
        "one vote each, however often it is sent"
    );
    assert!(again.queue[0].mine, "and now it is his too");

    // Asking for what is already waiting is joining in, not a second copy.
    let same: RadioRequests = bob.request(&RadioRequest::new("jukebox", 4)).await.unwrap();
    assert_eq!(same.queue.len(), 1);

    // Three waiting each, and no more — said as its own reason, not as the
    // posting budget running out.
    let _: RadioRequests = alice
        .request(&RadioRequest::new("jukebox", 2))
        .await
        .unwrap();
    let _: RadioRequests = alice
        .request(&RadioRequest::new("jukebox", 3))
        .await
        .unwrap();
    let too_many = alice
        .request::<_, RadioRequests>(&RadioRequest::new("jukebox", 5))
        .await;
    assert!(
        matches!(too_many, Err(ClientError::Refused(ErrorCode::TooLarge))),
        "a fourth of hers waiting is refused: {too_many:?}"
    );
    // A vote for something nobody asked for is not a request.
    let stray = bob
        .request::<_, RadioRequests>(&RadioRequestVote::new("jukebox", 5))
        .await;
    assert!(matches!(
        stray,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // Something that is not in the rotation cannot be asked for.
    let nonsense = alice
        .request::<_, RadioRequests>(&RadioRequest::new("jukebox", 99))
        .await;
    assert!(matches!(
        nonsense,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // The most wanted plays next, whatever the rotation would have picked.
    burrow.shared.radio.advance("jukebox", 1);
    let late = bob
        .request::<_, RadioRequests>(&RadioRequestVote::new("jukebox", 4))
        .await;
    assert!(
        matches!(late, Err(ClientError::Refused(ErrorCode::AlreadyExists))),
        "a vote for what has just started is told it is playing: {late:?}"
    );
    assert_eq!(
        burrow.shared.radio.now_playing("jukebox").unwrap().title,
        "song-4.mp3",
        "two votes beat one"
    );
    let left: RadioRequests = alice
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert_eq!(left.queue.len(), 2, "and it is no longer waiting");

    // A guest can see what is waiting but cannot ask.
    let mut guest = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    guest.auth_guest(Some("visitor".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    let looked: RadioRequests = guest
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert_eq!(looked.queue.len(), 2);
    let refused = guest
        .request::<_, RadioRequests>(&RadioRequest::new("jukebox", 5))
        .await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::Forbidden))),
        "a guest may look but not ask: {refused:?}"
    );

    burrow.shutdown().await;
}

/// A station offers only what it can play, and a library bigger than a
/// reply is looked through, not sent whole: a page of it, how many more
/// there are, and a search to reach them. What cannot go out — audio of a
/// kind the burrow does not stream, a track it had to leave out, a file a
/// moderator is holding back — is neither offered nor taken, so nothing
/// waits in the queue only to be passed over without a word.
#[tokio::test]
async fn a_station_offers_only_what_it_can_play_a_page_at_a_time() {
    use rabbithole_core::{Client, ClientError};
    use rabbithole_proto::admin::subject_kind;
    use rabbithole_proto::radio::{
        RadioOffer, RadioOfferRequest, RadioRequest, RadioRequestVote, RadioRequests,
        RadioRequestsRequest,
    };
    use rabbithole_proto::ErrorCode;

    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for login in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    // Two hundred and fifty songs, a long comment on one of them, and one
    // file this burrow cannot send.
    let mut tracks: Vec<rabbithole_radio::Track> = (1..=250u64)
        .map(|n| {
            let artist = if n == 7 {
                "x".repeat(10_000)
            } else {
                "The Lagomorphs".into()
            };
            let mut blob = [0u8; 32];
            blob[..8].copy_from_slice(&n.to_le_bytes());
            rabbithole_radio::Track::new(
                rabbithole_radio::TrackId(n),
                format!("song-{n}.mp3"),
                artist,
                180_000,
                rabbithole_radio::BlobId(blob),
            )
        })
        .collect();
    tracks.push(rabbithole_radio::Track::new(
        rabbithole_radio::TrackId(999),
        "memo.m4a",
        "",
        180_000,
        rabbithole_radio::BlobId([9u8; 32]),
    ));
    let sound = burrow::radio::sound_of_tracks(&tracks);
    let radio = &burrow.shared.radio;
    radio.install_program("jukebox", "Jukebox", "music", tracks, sound);
    radio.set_unsendable("jukebox", [rabbithole_radio::TrackId(999)]);

    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut alice = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    alice.auth_password("alice", "pw-pw-pw-pw").await.unwrap();
    alice.expect_welcome().await.unwrap();
    // Asking is spent from the posting budget, so a second listener takes
    // the asks past the first few.
    let mut bob = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    bob.auth_password("bob", "pw-pw-pw-pw").await.unwrap();
    bob.expect_welcome().await.unwrap();
    let offer = |search: &'static str| RadioOfferRequest::new("jukebox", search);

    // A page, and how many more: 249 it can play (song 1 is playing).
    let first: RadioOffer = alice.request(&offer("")).await.unwrap();
    assert_eq!(first.tracks.len(), burrow::radio::OFFER_SHOWN);
    assert_eq!(first.more as usize, 249 - burrow::radio::OFFER_SHOWN);
    assert!(first.tracks.iter().all(|t| t.id != 1 && t.id != 999));
    let seventh = first.tracks.iter().find(|t| t.id == 7).unwrap();
    assert!(
        seventh.artist.chars().count() <= 200,
        "an uploader's essay is cut to size"
    );

    // A search reaches the rest, and says which search it answers.
    let found: RadioOffer = alice.request(&offer("SONG-24")).await.unwrap();
    assert_eq!(found.search, "SONG-24");
    let ids: Vec<u64> = found.tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids, [24, 240, 241, 242, 243, 244, 245, 246, 247, 248, 249]);
    assert_eq!(found.more, 0);

    // What cannot be sent is neither offered nor taken.
    let memo: RadioOffer = alice.request(&offer("memo")).await.unwrap();
    assert!(memo.tracks.is_empty());
    let refused = alice
        .request::<_, RadioRequests>(&RadioRequest::new("jukebox", 999))
        .await;
    assert!(matches!(
        refused,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // Nor a track the station found it could not play when its turn came —
    // until it does play, when it is offered again.
    radio.cannot_play("jukebox", rabbithole_radio::TrackId(30));
    let thirty: RadioOffer = alice.request(&offer("song-30")).await.unwrap();
    assert!(thirty.tracks.is_empty(), "{thirty:?}");
    radio.can_play("jukebox", rabbithole_radio::TrackId(30), "song-30.mp3");
    let thirty: RadioOffer = alice.request(&offer("song-30")).await.unwrap();
    assert_eq!(thirty.tracks.len(), 1, "it played, so it can be asked for");

    // Somebody asks for songs 5, 6 and 7; then a moderator holds 5's file
    // back. It leaves the queue as everybody sees it and the offer, cannot
    // be asked for or voted for, and does not use up one of her three,
    // until it is let through.
    for n in [5, 6, 7] {
        let _: RadioRequests = alice
            .request(&RadioRequest::new("jukebox", n))
            .await
            .unwrap();
    }
    let mut blob = [0u8; 32];
    blob[..8].copy_from_slice(&5u64.to_le_bytes());
    burrow
        .shared
        .moderation
        .quarantine_set(subject_kind::FILE, &blob, "under review", "mo")
        .await
        .unwrap();
    let waiting: RadioRequests = alice
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert!(
        waiting.queue.iter().all(|q| q.id != 5),
        "a held file's name is not shown"
    );
    let _: RadioRequests = alice
        .request(&RadioRequest::new("jukebox", 8))
        .await
        .expect("what she cannot see does not count against her");
    let voted = bob
        .request::<_, RadioRequests>(&RadioRequestVote::new("jukebox", 5))
        .await;
    assert!(matches!(
        voted,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));
    let five: RadioOffer = alice.request(&offer("song-5")).await.unwrap();
    assert!(five.tracks.iter().all(|t| t.id != 5));
    let again = bob
        .request::<_, RadioRequests>(&RadioRequest::new("jukebox", 5))
        .await;
    assert!(matches!(
        again,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));
    burrow
        .shared
        .moderation
        .quarantine_clear(subject_kind::FILE, &blob, "mo")
        .await
        .unwrap();
    let back: RadioRequests = alice
        .request(&RadioRequestsRequest::new("jukebox"))
        .await
        .unwrap();
    assert!(
        back.queue.iter().any(|q| q.id == 5),
        "let through, it is waiting again"
    );

    // A station nobody has heard of is not there to ask about.
    let nowhere = alice
        .request::<_, RadioOffer>(&RadioOfferRequest::new("nowhere", ""))
        .await;
    assert!(matches!(
        nowhere,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    burrow.shutdown().await;
}

/// A station follows its folder while the burrow runs. Its rotation used to
/// be read once, at startup, so a song uploaded to the station's folder
/// could not be played or asked for until a restart, and a folder that was
/// empty at startup stayed a silent station for good.
#[tokio::test]
async fn a_song_added_to_a_stations_folder_takes_its_turn_without_a_restart() {
    use rabbithole_core::Client;
    use rabbithole_proto::radio::{RadioOffer, RadioOfferRequest, RadioRequest, RadioRequests};

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    // One song in "music"; "later" exists and is empty.
    library_of(&work, &dir, &[]).await;
    {
        let burrow = Burrow::start(test_config(&dir)).await.unwrap();
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
        dj.area_create("later", "Later", "").await.unwrap();
        let src = work.path().join("one.mp3");
        // Thirty seconds of audio; the entire later startup/upload/request
        // phase has a shorter deadline, so a legitimate track boundary cannot
        // race the assertion that refreshing preserves the song on the air.
        std::fs::write(&src, mp3_of(1_150, 0x11)).unwrap();
        dj.transfer_upload("music", None, "one.mp3", &src, "audio/mpeg", "")
            .await
            .unwrap();
        burrow.shutdown().await;
    }

    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("jukebox".into(), "music".into());
    config
        .radio_library_areas
        .insert("nightshift".into(), "later".into());
    let action_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let burrow = Burrow::start(config).await.unwrap();
    tokio::time::timeout_at(action_deadline, async {
        let radio = burrow.radio_addr.expect("radio enabled");
        assert_eq!(burrow.shared.radio.track_count("jukebox"), 1);
        assert_eq!(burrow.shared.radio.track_count("nightshift"), 0);
        assert!(
            !burrow.shared.radio.is_pumped("nightshift"),
            "nothing to play"
        );

        burrow
            .shared
            .auth
            .create_account("alice", "pw-pw-pw-pw", Role::User)
            .await
            .unwrap();
        let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
        let mut dj = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
        dj.auth_password("dj", "spin-spin-spin").await.unwrap();
        dj.expect_welcome().await.unwrap();
        let mut alice = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
        alice.auth_password("alice", "pw-pw-pw-pw").await.unwrap();
        alice.expect_welcome().await.unwrap();

        // Only the song on the air, so nothing to ask for yet.
        let offer: RadioOffer = alice
            .request(&RadioOfferRequest::new("jukebox", ""))
            .await
            .unwrap();
        assert!(offer.tracks.is_empty(), "{offer:?}");

        // A second song lands in the folder, and a first one in the empty folder.
        for (area, name, tag) in [("music", "two.mp3", 0x22), ("later", "first.mp3", 0x33)] {
            let src = work.path().join(name);
            std::fs::write(&src, mp3_of(115, tag)).unwrap();
            dj.transfer_upload(area, None, name, &src, "audio/mpeg", "")
                .await
                .unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while (burrow.shared.radio.track_count("jukebox") < 2
            || !burrow.shared.radio.is_pumped("nightshift"))
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            burrow.shared.radio.track_count("jukebox"),
            2,
            "the new song is in the rotation"
        );

        // It can be asked for, by its name, and it is what plays next.
        let offer: RadioOffer = alice
            .request(&RadioOfferRequest::new("jukebox", ""))
            .await
            .unwrap();
        let two = offer
            .tracks
            .iter()
            .find(|t| t.title.contains("two"))
            .expect("the new song is offered");
        let asked: RadioRequests = alice
            .request(&RadioRequest::new("jukebox", two.id))
            .await
            .unwrap();
        assert_eq!(asked.queue.len(), 1);
        // The song that was on the air is still the one on the air.
        assert!(burrow
            .shared
            .radio
            .now_playing("jukebox")
            .unwrap()
            .title
            .contains("one"));

        // And the station that had nothing is on the air now, streaming.
        assert!(burrow.shared.radio.is_pumped("nightshift"));
        let mut listener = TcpStream::connect(radio).await.unwrap();
        listener
            .write_all(b"GET /nightshift HTTP/1.0\r\n\r\n")
            .await
            .unwrap();
        listener.flush().await.unwrap();
        let (head, _) = read_head(&mut listener).await;
        assert!(
            head.starts_with("ICY 200 OK"),
            "the folder that was empty plays now: {head:?}"
        );
    })
    .await
    .expect("startup, uploads and requests complete within 15s, before the first 30s song ends");

    burrow.shutdown().await;
}

#[tokio::test]
async fn adding_a_new_format_keeps_the_existing_stream_and_uses_a_companion_mount() {
    use rabbithole_core::Client;

    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("srv");
    let opus = opus_of(3, 0x77);
    library_of(&work, &dir, &[("one.opus", &opus)]).await;
    let mut config = test_config(&dir);
    config
        .radio_library_areas
        .insert("mixed".into(), "music".into());
    let burrow = Burrow::start(config).await.unwrap();
    let radio = burrow.radio_addr.unwrap();
    let mut listener = TcpStream::connect(radio).await.unwrap();
    listener
        .write_all(b"GET /mixed HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    let (head, _) = read_head(&mut listener).await;
    assert!(
        head.starts_with("ICY 200 OK") && head.contains("audio/ogg"),
        "{head}"
    );

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
    let src = work.path().join("two.mp3");
    std::fs::write(&src, mp3_of(115, 0x22)).unwrap();
    dj.transfer_upload("music", None, "two.mp3", &src, "audio/mpeg", "")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !burrow.shared.radio.is_pumped("mixed.mp3") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("new MP3 format should get its own pump without restarting");
    assert_eq!(
        burrow.shared.radio.expected_sound("mixed"),
        Some(burrow::radio::Sound::Ogg(0))
    );
    assert_eq!(burrow.shared.radio.track_count("mixed"), 1);
    assert_eq!(burrow.shared.radio.track_count("mixed.mp3"), 1);
    assert!(!burrow
        .shared
        .radio
        .program_slugs()
        .contains(&"mixed.ogg".into()));

    // The already-connected listener keeps receiving the original codec;
    // read beyond an entire old track so buffered startup bytes cannot pass.
    let heard = tokio::time::timeout(
        Duration::from_secs(10),
        read_at_least(&mut listener, opus.len() * 3),
    )
    .await
    .expect("original Ogg stream continues across rotation");
    assert!(
        heard.len() >= opus.len() * 3,
        "existing stream was disconnected"
    );
    assert!(heard.windows(120).any(|bytes| bytes == [0x77; 120]));
    assert!(!heard.windows(120).any(|bytes| bytes == [0x22; 120]));

    let mut mp3 = TcpStream::connect(radio).await.unwrap();
    mp3.write_all(b"GET /mixed.mp3 HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    let (head, mut heard) = read_head(&mut mp3).await;
    assert!(
        head.starts_with("ICY 200 OK") && head.contains("audio/mpeg"),
        "{head}"
    );
    heard.extend(read_at_least(&mut mp3, 417).await);
    assert!(
        heard.windows(413).any(|bytes| bytes == [0x22; 413]),
        "new mount streams the uploaded MP3"
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn a_new_library_companion_waits_for_its_existing_dj_on_both_source_surfaces() {
    use rabbithole_core::Client;

    for dedicated in [false, true] {
        let work = tempfile::tempdir().unwrap();
        let dir = work.path().join("srv");
        library_of(&work, &dir, &[("one.opus", &opus_of(3, 0x77))]).await;
        let mut config = test_config(&dir);
        config
            .radio_library_areas
            .insert("mixed".into(), "music".into());
        config.radio_source_enabled = dedicated;
        config.radio_source_addr = "127.0.0.1:0".parse().unwrap();
        config.radio_source_user = "source".into();
        config.radio_source_password = "source-password".into();
        let burrow = Burrow::start(config).await.unwrap();
        let radio = burrow.radio_addr.unwrap();
        let (source_addr, auth) = if dedicated {
            (
                burrow.radio_source_addr.unwrap(),
                basic_auth("source", "source-password"),
            )
        } else {
            (radio, basic_auth("dj", "spin-spin-spin"))
        };
        // A DJ already owns the future MP3 companion, and is sending Ogg.
        let mut source = TcpStream::connect(source_addr).await.unwrap();
        source.write_all(format!(
            "PUT /mixed.mp3 HTTP/1.0\r\nAuthorization: Basic {auth}\r\nice-name: Live set\r\ncontent-type: audio/ogg\r\n\r\n"
        ).as_bytes()).await.unwrap();
        assert!(read_head(&mut source).await.0.contains("200"));
        let mut listener = TcpStream::connect(radio).await.unwrap();
        listener
            .write_all(b"GET /mixed.mp3 HTTP/1.0\r\n\r\n")
            .await
            .unwrap();
        assert!(read_head(&mut listener).await.0.contains("audio/ogg"));

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
        let src = work.path().join("two.mp3");
        std::fs::write(&src, mp3_of(115, 0x22)).unwrap();
        dj.transfer_upload("music", None, "two.mp3", &src, "audio/mpeg", "")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !burrow.shared.radio.is_pumped("mixed.mp3") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("library companion should be installed while its DJ holds the mount");
        let current = burrow::radio::station_listing(&burrow.shared)
            .into_iter()
            .find(|s| s.station == "mixed.mp3")
            .expect("live station remains listed");
        assert!(
            current.live,
            "installing automation must not announce that the DJ left"
        );
        assert_eq!(current.title, "Live set");
        assert!(burrow.shared.radio.is_live("mixed.mp3"));
        assert_eq!(
            burrow.shared.radio.now_playing("mixed.mp3").unwrap().title,
            "Live set"
        );
        assert_eq!(burrow.shared.radio.program_content_type("mixed.mp3"), None);
        let marker = opus_of(1, 0x55);
        source.write_all(&marker).await.unwrap();
        assert_eq!(
            read_at_least(&mut listener, marker.len()).await,
            marker,
            "DJ keeps the existing stream"
        );

        drop(source);
        tokio::time::timeout(Duration::from_secs(5), async {
            while burrow.shared.radio.is_live("mixed.mp3")
                || burrow
                    .shared
                    .radio
                    .program_content_type("mixed.mp3")
                    .as_deref()
                    != Some("audio/mpeg")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("new automation starts after DJ departure");
        assert!(
            burrow
                .shared
                .radio
                .registry
                .get("mixed.mp3")
                .unwrap()
                .enabled
        );
        let mut automation = TcpStream::connect(radio).await.unwrap();
        automation
            .write_all(b"GET /mixed.mp3 HTTP/1.0\r\n\r\n")
            .await
            .unwrap();
        let (head, mut heard) = read_head(&mut automation).await;
        assert!(
            head.contains("audio/mpeg"),
            "automation declares its own codec after DJ departure: {head}"
        );
        heard.extend(read_at_least(&mut automation, 417).await);
        assert!(heard.windows(413).any(|bytes| bytes == [0x22; 413]));
        burrow.shutdown().await;
    }
}
