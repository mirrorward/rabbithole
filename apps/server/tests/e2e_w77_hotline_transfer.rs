//! Wave 7.7 end-to-end tests: HTXF **upload** and fork-offset **resume** on
//! the Hotline-compatible surface. A scripted vintage-style client flattens
//! a file and uploads it over the HTXF bulk channel (control port + 1),
//! downloads it back byte-identical, resumes interrupted transfers in both
//! directions, and exercises the finalize gates (hash-deny, quota, declared
//! size). Mirrors the structure of `e2e_w74_hotline_news.rs`.
//!
//! Determinism note: the server settles an upload's outcome (finalize or
//! park-for-resume) *before* it FINs the HTXF socket, so a client that
//! drains to EOF observes the settled state — no sleeps anywhere.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::flatten::{FORK_DATA, FORK_INFO};
use rabbithole_legacy_hotline::{
    Field, FileResumeData, FlatHeader, ForkHeader, Handshake, HandshakeReply, InfoFork,
    Transaction, TransactionHeader,
};
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

fn field_int(txn: &Transaction, id: u16) -> Option<u32> {
    field_bytes(txn, id)
        .map(rabbithole_legacy_hotline::read_int)
        .and_then(Result::ok)
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

/// Deterministic non-trivial content for transfer tests.
fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
}

/// Flatten a file the way a classic client does: FILP header, INFO fork,
/// then a DATA fork whose body starts at `data_offset` (0 = whole file).
fn client_ffo(name: &str, comment: &str, data: &[u8], data_offset: usize) -> Vec<u8> {
    let info = InfoFork::new(*b"BINA", *b"HTLC", name.as_bytes(), comment.as_bytes()).encode();
    let tail = &data[data_offset..];
    let mut out = FlatHeader { fork_count: 2 }.encode().to_vec();
    out.extend_from_slice(
        &ForkHeader {
            fork_type: FORK_INFO,
            data_size: info.len() as u32,
        }
        .encode(),
    );
    out.extend_from_slice(&info);
    out.extend_from_slice(
        &ForkHeader {
            fork_type: FORK_DATA,
            data_size: tail.len() as u32,
        }
        .encode(),
    );
    out.extend_from_slice(tail);
    out
}

/// Walk a flattened file object into `(fork_type, body)` pairs.
fn ffo_forks(buf: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    let flat = FlatHeader::decode(buf).expect("FILP header");
    let mut pos = FlatHeader::LEN;
    let mut out = Vec::new();
    for _ in 0..flat.fork_count {
        let fh = ForkHeader::decode(&buf[pos..]).expect("fork header");
        pos += ForkHeader::LEN;
        out.push((fh.fork_type, buf[pos..pos + fh.data_size as usize].to_vec()));
        pos += fh.data_size as usize;
    }
    out
}

/// Pull the DATA fork out of a flattened file object.
fn ffo_data_fork(buf: &[u8]) -> Vec<u8> {
    ffo_forks(buf)
        .into_iter()
        .find(|(t, _)| *t == FORK_DATA)
        .map(|(_, b)| b)
        .expect("no DATA fork in flattened file object")
}

/// The HTXF data address is the control port + 1.
fn htxf_addr(addr: std::net::SocketAddr) -> std::net::SocketAddr {
    std::net::SocketAddr::new(addr.ip(), addr.port() + 1)
}

/// Open the HTXF channel and send `bytes` for an upload reference, then FIN
/// and drain to the server's FIN — which the server sends only after the
/// upload's outcome (finalize / park / refuse) is settled, so returning from
/// here means the server state is observable. Write errors are tolerated:
/// a refusing server may close mid-send.
async fn htxf_send(addr: std::net::SocketAddr, refnum: u32, bytes: &[u8]) {
    let mut sock = TcpStream::connect(htxf_addr(addr)).await.unwrap();
    let mut hdr = Vec::new();
    hdr.extend_from_slice(b"HTXF");
    hdr.extend_from_slice(&refnum.to_be_bytes());
    hdr.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    hdr.extend_from_slice(&[0u8; 4]);
    let _ = sock.write_all(&hdr).await;
    let _ = sock.write_all(bytes).await;
    let _ = sock.shutdown().await;
    let mut sink = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), sock.read_to_end(&mut sink)).await;
}

/// Open the HTXF channel for a download reference and read exactly `len`
/// bytes (the negotiated transfer size), leaving the socket to drop.
async fn htxf_recv(addr: std::net::SocketAddr, refnum: u32, len: usize) -> Vec<u8> {
    let mut sock = TcpStream::connect(htxf_addr(addr)).await.unwrap();
    let mut hdr = Vec::new();
    hdr.extend_from_slice(b"HTXF");
    hdr.extend_from_slice(&refnum.to_be_bytes());
    hdr.extend_from_slice(&[0u8; 8]);
    sock.write_all(&hdr).await.unwrap();
    let mut buf = vec![0u8; len];
    tokio::time::timeout(Duration::from_secs(10), sock.read_exact(&mut buf))
        .await
        .expect("timed out reading HTXF payload")
        .unwrap();
    buf
}

/// Negotiate an upload; returns the reply transaction.
async fn negotiate_upload(
    client: &mut Client,
    name: &str,
    path: &[&str],
    declared: Option<u32>,
    resume: bool,
) -> Transaction {
    let mut fields = vec![
        Field::text(field::FILE_NAME, name),
        Field::new(field::FILE_PATH, encode_path(path)),
    ];
    if let Some(d) = declared {
        fields.push(Field::int(field::TRANSFER_SIZE, d));
    }
    if resume {
        fields.push(Field::int(field::FILE_TRANSFER_OPTIONS, 1));
    }
    client.send(transaction::UPLOAD_FILE, fields).await;
    client.read_until(transaction::UPLOAD_FILE).await
}

/// Negotiate a download; returns the reply transaction.
async fn negotiate_download(
    client: &mut Client,
    name: &str,
    path: &[&str],
    resume_offset: Option<u32>,
) -> Transaction {
    let mut fields = vec![
        Field::text(field::FILE_NAME, name),
        Field::new(field::FILE_PATH, encode_path(path)),
    ];
    if let Some(off) = resume_offset {
        fields.push(Field::new(
            field::FILE_RESUME_DATA,
            FileResumeData::for_data_offset(off).encode(),
        ));
    }
    client.send(transaction::DOWNLOAD_FILE, fields).await;
    client.read_until(transaction::DOWNLOAD_FILE).await
}

/// Boot a server with one file area and one member account, logged in.
async fn setup(work: &std::path::Path) -> (Burrow, Client, std::net::SocketAddr) {
    let burrow = Burrow::start(test_config(&work.join("srv"))).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "hunter2hunter2", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .files
        .create_area("warez", "Warez", "")
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
    (burrow, alice, addr)
}

#[tokio::test]
async fn hotline_upload_then_download_roundtrips_byte_identical() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, addr) = setup(work.path()).await;

    let data = content(200_000);
    let ffo = client_ffo("cake.bin", "eat me", &data, 0);

    // Negotiate: REF_NUM back, no resume field on a fresh upload.
    let up = negotiate_upload(
        &mut alice,
        "cake.bin",
        &["warez"],
        Some(ffo.len() as u32),
        false,
    )
    .await;
    assert_eq!(up.header.error, 0, "upload authorized: {up:?}");
    let refnum = field_int(&up, field::REF_NUM).expect("upload refnum");
    assert!(
        field_bytes(&up, field::FILE_RESUME_DATA).is_none(),
        "fresh upload carries no resume data"
    );

    // Send the flattened file object; returning means the outcome settled.
    htxf_send(addr, refnum, &ffo).await;

    // The node registered with the blob committed under its blake3.
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "cake.bin")
        .await
        .unwrap()
        .expect("uploaded file registered");
    assert_eq!(node.size as usize, data.len());
    assert_eq!(node.comment, "eat me", "INFO fork comment carried over");
    assert!(
        node.uploader.starts_with("Alice@"),
        "uploader attribution: {}",
        node.uploader
    );
    let expected_root = *blake3::hash(&data).as_bytes();
    assert_eq!(node.blob_id, Some(expected_root), "blob id is the blake3");

    // Download it back: byte-identical DATA fork.
    let dl = negotiate_download(&mut alice, "cake.bin", &["warez"], None).await;
    assert_eq!(dl.header.error, 0, "download authorized");
    let dl_ref = field_int(&dl, field::REF_NUM).unwrap();
    let transfer_size = field_int(&dl, field::TRANSFER_SIZE).unwrap() as usize;
    let ffo_back = htxf_recv(addr, dl_ref, transfer_size).await;
    assert_eq!(
        ffo_data_fork(&ffo_back),
        data,
        "round-trip is byte-identical"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_dropbox_upload_lands_hidden() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, addr) = setup(work.path()).await;
    burrow
        .shared
        .files
        .mkdir("warez", None, "inbox", true)
        .await
        .unwrap();

    let data = content(4_096);
    let ffo = client_ffo("secret.bin", "", &data, 0);
    let up = negotiate_upload(
        &mut alice,
        "secret.bin",
        &["warez", "inbox"],
        Some(ffo.len() as u32),
        false,
    )
    .await;
    assert_eq!(up.header.error, 0, "uploading INTO a drop box is allowed");
    let refnum = field_int(&up, field::REF_NUM).unwrap();
    htxf_send(addr, refnum, &ffo).await;

    // The file landed…
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "inbox/secret.bin")
        .await
        .unwrap()
        .expect("drop-box upload registered");
    assert_eq!(node.size as usize, data.len());

    // …but a plain member browsing the drop box sees nothing…
    alice
        .send(
            transaction::GET_FILE_NAME_LIST,
            vec![Field::new(
                field::FILE_PATH,
                encode_path(&["warez", "inbox"]),
            )],
        )
        .await;
    let listing = alice.read_until(transaction::GET_FILE_NAME_LIST).await;
    assert_eq!(listing.header.error, 0);
    assert!(
        !listing
            .fields
            .iter()
            .any(|f| f.id == field::FILE_NAME_WITH_INFO),
        "drop-box contents stay hidden"
    );

    // …and cannot pull the file back out.
    let dl = negotiate_download(&mut alice, "secret.bin", &["warez", "inbox"], None).await;
    assert_eq!(dl.header.error, 1, "drop-boxed content is not downloadable");

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_download_resume_from_offset_yields_identical_bytes() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, addr) = setup(work.path()).await;

    // Server-side file to download.
    let data = content(64_000);
    let blob_id = burrow.shared.blobs.put(&data).unwrap();
    burrow
        .shared
        .files
        .add_file(
            "warez",
            None,
            "bignum.bin",
            &blob_id.0,
            data.len() as i64,
            "application/octet-stream",
            "",
            "a big number",
            "alice@hotline-warren",
            1,
        )
        .await
        .unwrap();

    // First attempt: take the envelope plus a prefix of the DATA fork, then
    // abandon the connection (the classic mid-download interruption).
    let k = 20_000;
    let dl = negotiate_download(&mut alice, "bignum.bin", &["warez"], None).await;
    assert_eq!(dl.header.error, 0);
    let transfer_size = field_int(&dl, field::TRANSFER_SIZE).unwrap() as usize;
    assert_eq!(
        field_int(&dl, field::FILE_SIZE).unwrap() as usize,
        data.len()
    );
    let envelope = transfer_size - data.len();
    let first = htxf_recv(addr, field_int(&dl, field::REF_NUM).unwrap(), envelope + k).await;
    let got_prefix = first[envelope..].to_vec();
    assert_eq!(
        got_prefix,
        data[..k],
        "prefix bytes before the interruption"
    );

    // Resume from offset k: fresh FILP + INFO envelope, DATA fork = tail.
    let dl2 = negotiate_download(&mut alice, "bignum.bin", &["warez"], Some(k as u32)).await;
    assert_eq!(dl2.header.error, 0, "resume authorized");
    let transfer_size2 = field_int(&dl2, field::TRANSFER_SIZE).unwrap() as usize;
    assert_eq!(
        field_int(&dl2, field::FILE_SIZE).unwrap() as usize,
        data.len() - k,
        "FILE_SIZE counts the remaining DATA fork"
    );
    assert_eq!(
        transfer_size2,
        envelope + (data.len() - k),
        "the envelope is re-sent fresh, the DATA fork resumes"
    );
    let second = htxf_recv(
        addr,
        field_int(&dl2, field::REF_NUM).unwrap(),
        transfer_size2,
    )
    .await;
    let forks = ffo_forks(&second);
    let info = forks
        .iter()
        .find(|(t, _)| *t == FORK_INFO)
        .map(|(_, b)| InfoFork::decode(b).unwrap())
        .expect("INFO fork sent fresh on resume");
    assert_eq!(info.name, b"bignum.bin");
    let tail = ffo_data_fork(&second);
    assert_eq!(tail, data[k..], "resumed DATA fork starts at the offset");

    // Stitched together, the two reads are the whole file.
    let mut stitched = got_prefix;
    stitched.extend_from_slice(&tail);
    assert_eq!(stitched, data, "prefix + resumed tail is byte-identical");

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_upload_resume_completes_with_correct_hash() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, addr) = setup(work.path()).await;

    let data = content(96_000);
    let full = client_ffo("resume.bin", "", &data, 0);
    let envelope = full.len() - data.len();
    let k = 30_000;

    // Interrupted first attempt: envelope + first k data bytes, then FIN.
    let up = negotiate_upload(
        &mut alice,
        "resume.bin",
        &["warez"],
        Some(full.len() as u32),
        false,
    )
    .await;
    assert_eq!(up.header.error, 0);
    htxf_send(
        addr,
        field_int(&up, field::REF_NUM).unwrap(),
        &full[..envelope + k],
    )
    .await;

    // Nothing registered yet — the bytes are parked for resume.
    assert!(burrow
        .shared
        .files
        .node_by_path("warez", "resume.bin")
        .await
        .unwrap()
        .is_none());

    // Resume negotiation: the reply quotes the already-received DATA size.
    let up2 = negotiate_upload(&mut alice, "resume.bin", &["warez"], None, true).await;
    assert_eq!(up2.header.error, 0);
    let resume = field_bytes(&up2, field::FILE_RESUME_DATA)
        .map(FileResumeData::decode)
        .expect("resume data present")
        .expect("resume data parses");
    assert_eq!(
        resume.data_fork_offset(),
        Some(k as u32),
        "server reports the bytes it already holds"
    );

    // Send the tail as a flattened object whose DATA fork holds the rest.
    let offset = resume.data_fork_offset().unwrap() as usize;
    let tail_ffo = client_ffo("resume.bin", "", &data, offset);
    htxf_send(addr, field_int(&up2, field::REF_NUM).unwrap(), &tail_ffo).await;

    // Complete: the assembled file's blake3 is the committed blob id.
    let node = burrow
        .shared
        .files
        .node_by_path("warez", "resume.bin")
        .await
        .unwrap()
        .expect("resumed upload finalized");
    assert_eq!(node.size as usize, data.len());
    let expected_root = *blake3::hash(&data).as_bytes();
    assert_eq!(node.blob_id, Some(expected_root), "final blake3 matches");
    let stored = burrow
        .shared
        .blobs
        .get(&rabbithole_blobs::BlobId(expected_root))
        .unwrap();
    assert_eq!(stored, data, "stored bytes are the stitched original");

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_denied_hash_upload_refused_at_finalize() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, addr) = setup(work.path()).await;

    let data = content(8_192);
    burrow
        .shared
        .moderation
        .deny_add(blake3::hash(&data).as_bytes(), "known bad", "the-queen")
        .await
        .unwrap();

    let ffo = client_ffo("banned.bin", "", &data, 0);
    let up = negotiate_upload(
        &mut alice,
        "banned.bin",
        &["warez"],
        Some(ffo.len() as u32),
        false,
    )
    .await;
    assert_eq!(up.header.error, 0, "the open cannot know the hash yet");
    htxf_send(addr, field_int(&up, field::REF_NUM).unwrap(), &ffo).await;

    // Refused at the finalize gate: no node, no blob.
    assert!(
        burrow
            .shared
            .files
            .node_by_path("warez", "banned.bin")
            .await
            .unwrap()
            .is_none(),
        "denied content never registers"
    );
    assert!(
        burrow
            .shared
            .blobs
            .get(&rabbithole_blobs::BlobId(*blake3::hash(&data).as_bytes()))
            .is_err(),
        "denied content never reaches the blob store"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_quota_exceeded_upload_refused() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, _addr) = setup(work.path()).await;
    burrow
        .shared
        .config
        .set_key("upload_quota_bytes", "100")
        .unwrap();

    let up = negotiate_upload(&mut alice, "huge.bin", &["warez"], Some(5_000), false).await;
    assert_eq!(up.header.error, 1, "over-quota upload refused at the open");
    let msg = field_bytes(&up, field::ERROR_TEXT)
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    assert!(msg.contains("quota"), "names the quota: {msg:?}");

    burrow.shutdown().await;
}

#[tokio::test]
async fn hotline_oversized_vs_declared_upload_refused() {
    let work = tempfile::tempdir().unwrap();
    let (burrow, mut alice, addr) = setup(work.path()).await;

    // Declare a tiny transfer, then send a DATA fork claiming far more.
    let data = content(5_000);
    let ffo = client_ffo("liar.bin", "", &data, 0);
    let up = negotiate_upload(&mut alice, "liar.bin", &["warez"], Some(64), false).await;
    assert_eq!(up.header.error, 0);
    htxf_send(addr, field_int(&up, field::REF_NUM).unwrap(), &ffo).await;

    // Refused outright: nothing registered, and nothing parked for resume.
    assert!(
        burrow
            .shared
            .files
            .node_by_path("warez", "liar.bin")
            .await
            .unwrap()
            .is_none(),
        "oversized upload never registers"
    );
    let up2 = negotiate_upload(&mut alice, "liar.bin", &["warez"], None, true).await;
    assert_eq!(up2.header.error, 0);
    assert!(
        field_bytes(&up2, field::FILE_RESUME_DATA).is_none(),
        "no partial state survives a refused oversized upload"
    );

    burrow.shutdown().await;
}
