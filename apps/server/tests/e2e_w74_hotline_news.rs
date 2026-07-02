//! Wave 7.4 end-to-end tests: threaded + flat news and file transactions on
//! the Hotline-compatible surface. A scripted vintage-style client logs in,
//! browses the news categories, posts + reads a threaded article, reads the
//! flat message board, browses the file library, and downloads a small file
//! over the HTXF bulk channel (control port + 1). Mirrors the structure of
//! `e2e_w73_hotline.rs`.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{Field, Handshake, HandshakeReply, Transaction, TransactionHeader};
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Hotline Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        hotline_enabled: true,
        hotline_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// A scripted Hotline client over a raw TCP socket.
struct Client {
    stream: TcpStream,
    next_id: u32,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(&Handshake::hotl().encode()).await.unwrap();
        let mut reply = [0u8; HandshakeReply::LEN];
        stream.read_exact(&mut reply).await.unwrap();
        assert!(HandshakeReply::decode(&reply).unwrap().is_ok());
        Client { stream, next_id: 1 }
    }

    fn take_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn send(&mut self, type_: u16, fields: Vec<Field>) -> u32 {
        let id = self.take_id();
        let txn = Transaction::request(type_, id, fields);
        self.stream.write_all(&txn.encode()).await.unwrap();
        id
    }

    async fn read_txn(&mut self) -> Transaction {
        let mut hdr = [0u8; TransactionHeader::LEN];
        self.stream.read_exact(&mut hdr).await.unwrap();
        let header = TransactionHeader::decode(&hdr).unwrap();
        let mut buf = hdr.to_vec();
        buf.resize(TransactionHeader::LEN + header.data_size as usize, 0);
        self.stream
            .read_exact(&mut buf[TransactionHeader::LEN..])
            .await
            .unwrap();
        Transaction::decode(&buf).unwrap()
    }

    async fn read_until(&mut self, type_: u16) -> Transaction {
        loop {
            let txn = tokio::time::timeout(Duration::from_secs(5), self.read_txn())
                .await
                .expect("timed out waiting for transaction");
            if txn.header.type_ == type_ {
                return txn;
            }
        }
    }

    async fn login(&mut self, user: &str, pass: &str, name: &str) -> Transaction {
        let fields = vec![
            Field::new(field::LOGIN, obfuscate(user)),
            Field::new(field::PASSWORD, obfuscate(pass)),
            Field::text(field::USER_NAME, name),
            Field::int(field::USER_ICON_ID, 200),
        ];
        let id = self.send(transaction::LOGIN, fields).await;
        let reply = self.read_until(transaction::LOGIN).await;
        assert_eq!(reply.header.id, id, "login reply echoes the request id");
        reply
    }
}

fn obfuscate(s: &str) -> Vec<u8> {
    s.bytes().map(|b| !b).collect()
}

fn field_bytes(txn: &Transaction, id: u16) -> Option<&[u8]> {
    txn.fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.data.as_slice())
}

fn field_text(txn: &Transaction, id: u16) -> Option<String> {
    field_bytes(txn, id).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// Encode a structured Hotline path: `count(2)` then per component
/// `rsvd(2) len(1) name(len)`.
fn encode_path(components: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(components.len() as u16).to_be_bytes());
    for c in components {
        out.extend_from_slice(&[0u8, 0u8]);
        let b = c.as_bytes();
        out.push(b.len() as u8);
        out.extend_from_slice(b);
    }
    out
}

/// Parse `NewsCategoryListData15` records into `(is_category, name)` pairs.
fn parse_cats(txn: &Transaction) -> Vec<(bool, String)> {
    txn.fields
        .iter()
        .filter(|f| f.id == field::NEWS_CAT_LIST_DATA_15)
        .filter_map(|f| {
            let d = &f.data;
            if d.len() < 4 {
                return None;
            }
            let type_ = u16::from_be_bytes([d[0], d[1]]);
            let is_cat = type_ == 3;
            let mut pos = 4; // type(2) + count(2)
            if is_cat {
                pos += 24; // guid(16) + add_sn(4) + delete_sn(4)
            }
            if pos >= d.len() {
                return None;
            }
            let name_len = d[pos] as usize;
            pos += 1;
            let name = String::from_utf8_lossy(&d[pos..(pos + name_len).min(d.len())]).into_owned();
            Some((is_cat, name))
        })
        .collect()
}

/// Parse a `NewsArtListData` blob into `(art_id, title)` pairs.
fn parse_arts(data: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    if data.len() < 4 {
        return out;
    }
    let mut pos = 4; // list id
    let name_len = data[pos] as usize;
    pos += 1 + name_len;
    let desc_len = data[pos] as usize;
    pos += 1 + desc_len;
    if pos + 4 > data.len() {
        return out;
    }
    let count = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
    pos += 4;
    for _ in 0..count {
        if pos + 22 > data.len() {
            break;
        }
        let art_id = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        pos += 4 + 8 + 4 + 4; // id, date, parent, flags
        pos += 2; // flavor count
        let title_len = data[pos] as usize;
        pos += 1;
        let title =
            String::from_utf8_lossy(&data[pos..(pos + title_len).min(data.len())]).into_owned();
        pos += title_len;
        let poster_len = data[pos] as usize;
        pos += 1 + poster_len;
        // one flavor: len(1) + flavor + size(2)
        let flav_len = data[pos] as usize;
        pos += 1 + flav_len + 2;
        out.push((art_id, title));
    }
    out
}

/// Pull the DATA fork out of a flattened file object.
fn ffo_data_fork(buf: &[u8]) -> Vec<u8> {
    assert_eq!(&buf[0..4], b"FILP", "flattened file object magic");
    let fork_count = u16::from_be_bytes([buf[22], buf[23]]) as usize;
    let mut pos = 24;
    for _ in 0..fork_count {
        let fork_type = &buf[pos..pos + 4];
        let size = u32::from_be_bytes([buf[pos + 12], buf[pos + 13], buf[pos + 14], buf[pos + 15]])
            as usize;
        let body = &buf[pos + 16..pos + 16 + size];
        if fork_type == b"DATA" {
            return body.to_vec();
        }
        pos += 16 + size;
    }
    panic!("no DATA fork in flattened file object");
}

#[tokio::test]
async fn hotline_news_category_post_and_read_roundtrip() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "hunter2hunter2", Role::User)
        .await
        .unwrap();

    // A news bundle (folder) with a postable category beneath it.
    burrow
        .shared
        .boards
        .create_board("main", "Main", "top bundle", 0, None, 0)
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("main.general", "General", "chatter", 2, Some("main"), 0)
        .await
        .unwrap();

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut alice = Client::connect(addr).await;
    assert_eq!(
        alice
            .login("alice", "hunter2hunter2", "Alice")
            .await
            .header
            .error,
        0
    );

    // Root news list shows the bundle.
    alice
        .send(transaction::GET_NEWS_CAT_NAME_LIST, vec![])
        .await;
    let cats = parse_cats(&alice.read_until(transaction::GET_NEWS_CAT_NAME_LIST).await);
    assert!(
        cats.iter().any(|(is_cat, n)| !is_cat && n == "main"),
        "root shows the bundle: {cats:?}"
    );

    // Drilling into the bundle shows the postable category.
    alice
        .send(
            transaction::GET_NEWS_CAT_NAME_LIST,
            vec![Field::new(field::NEWS_PATH, encode_path(&["main"]))],
        )
        .await;
    let cats = parse_cats(&alice.read_until(transaction::GET_NEWS_CAT_NAME_LIST).await);
    assert!(
        cats.iter()
            .any(|(is_cat, n)| *is_cat && n == "main.general"),
        "bundle shows the category: {cats:?}"
    );

    // Post a threaded article to the category.
    alice
        .send(
            transaction::POST_NEWS_ART,
            vec![
                Field::new(field::NEWS_PATH, encode_path(&["main.general"])),
                Field::int(field::NEWS_ART_ID, 0), // new top-level thread
                Field::text(field::NEWS_ART_TITLE, "Tea Party"),
                Field::text(field::NEWS_ART_DATA_FLAV, "text/plain"),
                Field::new(field::NEWS_ART_DATA, b"we're all mad here".to_vec()),
            ],
        )
        .await;
    let posted = alice.read_until(transaction::POST_NEWS_ART).await;
    assert_eq!(posted.header.error, 0, "article accepted");

    // The article shows up in the category's article list.
    alice
        .send(
            transaction::GET_NEWS_ART_NAME_LIST,
            vec![Field::new(field::NEWS_PATH, encode_path(&["main.general"]))],
        )
        .await;
    let list = alice.read_until(transaction::GET_NEWS_ART_NAME_LIST).await;
    let arts = parse_arts(field_bytes(&list, field::NEWS_ART_LIST_DATA).expect("art list data"));
    let (art_id, _) = arts
        .iter()
        .find(|(_, title)| title == "Tea Party")
        .cloned()
        .expect("posted article present in list");

    // Reading the article returns the body.
    alice
        .send(
            transaction::GET_NEWS_ART_DATA,
            vec![
                Field::new(field::NEWS_PATH, encode_path(&["main.general"])),
                Field::int(field::NEWS_ART_ID, art_id),
            ],
        )
        .await;
    let art = alice.read_until(transaction::GET_NEWS_ART_DATA).await;
    assert_eq!(
        field_text(&art, field::NEWS_ART_TITLE).as_deref(),
        Some("Tea Party")
    );
    assert_eq!(
        field_text(&art, field::NEWS_ART_DATA).as_deref(),
        Some("we're all mad here"),
        "article body round-trips"
    );

    // The native board service sees the same post (shared subsystem).
    let threads = burrow
        .shared
        .boards
        .threads("main.general", 10)
        .await
        .unwrap();
    assert!(
        threads
            .iter()
            .any(|(root, _, _)| root.subject == "Tea Party"),
        "post landed in the shared board"
    );

    // Flat-news projection: GetMsgs carries the article body.
    alice.send(transaction::GET_MESSAGES, vec![]).await;
    let msgs = alice.read_until(transaction::GET_MESSAGES).await;
    let text = field_text(&msgs, field::DATA).unwrap_or_default();
    assert!(
        text.contains("we're all mad here"),
        "flat news shows the message: {text:?}"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_file_browse_and_download() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "swordfish-swordfish", Role::User)
        .await
        .unwrap();

    // A file area with one small file backed by a real blob.
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let content = b"down the rabbit hole we go".to_vec();
    let blob_id = burrow.shared.blobs.put(&content).unwrap();
    burrow
        .shared
        .files
        .add_file(
            "warez",
            None,
            "readme.txt",
            &blob_id.0,
            content.len() as i64,
            "text/plain",
            "doc",
            "the readme",
            "bob@home",
            1,
        )
        .await
        .unwrap();

    let addr = burrow.hotline_addr.expect("hotline enabled");
    let mut bob = Client::connect(addr).await;
    assert_eq!(
        bob.login("bob", "swordfish-swordfish", "Bob")
            .await
            .header
            .error,
        0
    );

    // Root browse lists the area as a folder.
    bob.send(transaction::GET_FILE_NAME_LIST, vec![]).await;
    let root = bob.read_until(transaction::GET_FILE_NAME_LIST).await;
    let names: Vec<String> = root
        .fields
        .iter()
        .filter(|f| f.id == field::FILE_NAME_WITH_INFO)
        .map(|f| {
            let d = &f.data;
            let name_len = u16::from_be_bytes([d[18], d[19]]) as usize;
            String::from_utf8_lossy(&d[20..20 + name_len]).into_owned()
        })
        .collect();
    assert!(
        names.contains(&"warez".to_string()),
        "area listed: {names:?}"
    );

    // Browsing the area lists the file.
    bob.send(
        transaction::GET_FILE_NAME_LIST,
        vec![Field::new(field::FILE_PATH, encode_path(&["warez"]))],
    )
    .await;
    let listing = bob.read_until(transaction::GET_FILE_NAME_LIST).await;
    let file_names: Vec<String> = listing
        .fields
        .iter()
        .filter(|f| f.id == field::FILE_NAME_WITH_INFO)
        .map(|f| {
            let d = &f.data;
            let name_len = u16::from_be_bytes([d[18], d[19]]) as usize;
            String::from_utf8_lossy(&d[20..20 + name_len]).into_owned()
        })
        .collect();
    assert!(
        file_names.contains(&"readme.txt".to_string()),
        "file listed: {file_names:?}"
    );

    // File info reports the size.
    bob.send(
        transaction::GET_FILE_INFO,
        vec![
            Field::text(field::FILE_NAME, "readme.txt"),
            Field::new(field::FILE_PATH, encode_path(&["warez"])),
        ],
    )
    .await;
    let info = bob.read_until(transaction::GET_FILE_INFO).await;
    assert_eq!(
        field_text(&info, field::FILE_NAME).as_deref(),
        Some("readme.txt")
    );
    let size = field_bytes(&info, field::FILE_SIZE)
        .map(rabbithole_legacy_hotline::read_int)
        .and_then(Result::ok)
        .unwrap();
    assert_eq!(size as usize, content.len(), "reported file size");

    // Negotiate a download.
    bob.send(
        transaction::DOWNLOAD_FILE,
        vec![
            Field::text(field::FILE_NAME, "readme.txt"),
            Field::new(field::FILE_PATH, encode_path(&["warez"])),
        ],
    )
    .await;
    let dl = bob.read_until(transaction::DOWNLOAD_FILE).await;
    assert_eq!(dl.header.error, 0, "download authorized");
    let refnum = field_bytes(&dl, field::REF_NUM)
        .map(rabbithole_legacy_hotline::read_int)
        .and_then(Result::ok)
        .unwrap();
    let transfer_size = field_bytes(&dl, field::TRANSFER_SIZE)
        .map(rabbithole_legacy_hotline::read_int)
        .and_then(Result::ok)
        .unwrap() as usize;

    // Pull the flattened file object over the HTXF channel (control port + 1).
    let htxf_addr = std::net::SocketAddr::new(addr.ip(), addr.port() + 1);
    let mut htxf = TcpStream::connect(htxf_addr).await.unwrap();
    let mut hdr = Vec::new();
    hdr.extend_from_slice(b"HTXF");
    hdr.extend_from_slice(&refnum.to_be_bytes());
    hdr.extend_from_slice(&[0u8; 8]); // size + rsvd
    htxf.write_all(&hdr).await.unwrap();

    let mut ffo = vec![0u8; transfer_size];
    htxf.read_exact(&mut ffo).await.unwrap();
    let data = ffo_data_fork(&ffo);
    assert_eq!(data, content, "downloaded bytes match the stored file");

    // The download was recorded against the file.
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "readme.txt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.downloads, 1, "download counted");

    burrow.shutdown().await;
}
