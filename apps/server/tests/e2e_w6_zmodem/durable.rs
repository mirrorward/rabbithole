//! RH-27: a real daemon process is killed after an acknowledged checkpoint.
use super::*;
use rabbithole_legacy_zmodem::Header;
use rabbithole_store_server::repo::AccountsRepo;
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;

const CHILD_ENV: &str = "RABBITHOLE_ZMODEM_STAGE_FIXTURE";

#[tokio::test]
async fn staging_daemon_child() {
    let Some(path) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let b = Burrow::start(test_config(Path::new(&path))).await.unwrap();
    for name in ["alice", "bob"] {
        if AccountsRepo(&b.shared.pool)
            .by_login(name)
            .await
            .unwrap()
            .is_none()
        {
            b.shared
                .auth
                .create_account(name, "pw-pw-pw", Role::User)
                .await
                .unwrap();
        }
    }
    if b.shared.files.area("warez").await.is_err() {
        b.shared
            .files
            .create_area("warez", "Warez", "")
            .await
            .unwrap();
    }
    println!("STAGING-READY {}", b.telnet_addr.unwrap());
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    std::future::pending::<()>().await;
}

async fn child(path: &Path) -> (tokio::process::Child, std::net::SocketAddr) {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "durable::staging_daemon_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let addr = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let line = lines.next_line().await.unwrap().expect("child readiness");
            // libtest can print its test-name prefix on this same line.
            if let Some((_, addr)) = line.split_once("STAGING-READY ") {
                break addr.parse().unwrap();
            }
        }
    })
    .await
    .unwrap();
    (child, addr)
}

async fn login(addr: std::net::SocketAddr, who: &str) -> ZClient {
    let mut c = ZClient::connect(addr).await;
    c.login(who, "pw-pw-pw").await;
    c.enter_area("warez").await;
    c
}

async fn offer(c: &mut ZClient, info: &FileInfo) -> u32 {
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;
    c.send_raw(&Header::new(FrameType::Zrqinit).encode(HeaderFormat::Hex))
        .await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrinit);
    c.send_raw(&Header::new(FrameType::Zfile).encode(HeaderFormat::Bin32))
        .await;
    c.send_raw(&encode_subpacket(&info.encode().unwrap(), FrameEnd::Zcrcw, true).unwrap())
        .await;
    let pos = c.next_header().await.header;
    assert_eq!(pos.frame_type, FrameType::Zrpos);
    pos.pos()
}

async fn checkpoint(c: &mut ZClient, info: &FileInfo, bytes: &[u8]) {
    assert_eq!(offer(c, info).await, 0);
    c.send_raw(&Header::with_pos(FrameType::Zdata, 0).encode(HeaderFormat::Bin32))
        .await;
    let mut offset = 0;
    for chunk in bytes.chunks(rabbithole_legacy_zmodem::subpacket::MAX_PAYLOAD) {
        c.send_raw(&encode_subpacket(chunk, FrameEnd::Zcrcq, true).unwrap())
            .await;
        offset += chunk.len();
        let ack = c.next_header().await.header;
        assert_eq!(ack.frame_type, FrameType::Zack);
        assert_eq!(
            ack.pos(),
            offset as u32,
            "durable checkpoint precedes this ACK"
        );
    }
}

async fn cancel(c: &mut ZClient) {
    c.send_raw(&[0x18; 8]).await;
    c.expect(b"Transfer cancelled.").await;
    c.expect(b"files /warez> ").await;
}

#[tokio::test]
async fn killed_daemon_resumes_only_matching_authorized_offer_and_commits_exact_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server");
    let bytes = noisy(4000);
    let info = FileInfo {
        length: Some(bytes.len() as u64),
        mtime: Some(123),
        ..FileInfo::new("crash.bin")
    };
    let (mut process, addr) = child(&path).await;
    let mut alice = login(addr, "alice").await;
    checkpoint(&mut alice, &info, &bytes[..1700]).await;
    // No cancel/EOF/cleanup path runs. This kills the actual process while
    // the receiver is still waiting for more data on its active lease.
    process.kill().await.unwrap();
    process.wait().await.unwrap();
    drop(alice);
    let (mut process, addr) = child(&path).await;
    let mut bob = login(addr, "bob").await;
    assert_eq!(
        offer(&mut bob, &info).await,
        0,
        "another account cannot claim the prefix"
    );
    cancel(&mut bob).await;
    drop(bob);
    let mut alice = login(addr, "alice").await;
    alice.send_line("zput").await;
    alice.expect(b"Begin your send now").await;
    assert_eq!(client_send(&mut alice, info, &bytes, 0).await, 1700);
    alice.expect(b"Received crash.bin (4000 bytes).").await;
    alice.expect(b"files /warez> ").await;
    process.kill().await.unwrap();
    process.wait().await.unwrap();
    drop(alice);
    let b = Burrow::start(test_config(&path)).await.unwrap();
    let node = b
        .shared
        .files
        .node_by_path("warez", "crash.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.blob_id, Some(*blake3::hash(&bytes).as_bytes()));
    assert_eq!(
        b.shared
            .blobs
            .get(&rabbithole_blobs::BlobId(node.blob_id.unwrap()))
            .unwrap(),
        bytes
    );
    b.shutdown().await;
}

#[tokio::test]
async fn replacement_folder_never_inherits_old_checkpoint_or_active_upload() {
    let dir = tempfile::tempdir().unwrap();
    let b = Burrow::start(test_config(dir.path())).await.unwrap();
    b.shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    b.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let folder = b
        .shared
        .files
        .mkdir("warez", None, "drop", false)
        .await
        .unwrap();
    let mut c = login(b.telnet_addr.unwrap(), "alice").await;
    c.send_line("cd drop").await;
    c.expect(b"files /warez/drop> ").await;
    let bytes = noisy(1500);
    let info = FileInfo {
        length: Some(1500),
        ..FileInfo::new("same.bin")
    };
    checkpoint(&mut c, &info, &bytes[..700]).await;
    b.shared.files.delete(folder.id).await.unwrap();
    b.shared
        .files
        .mkdir("warez", None, "drop", false)
        .await
        .unwrap();
    // Complete the already active upload: the changed canonical identity
    // must be refused even though its visible path exists again.
    c.send_raw(&encode_subpacket(&bytes[700..], FrameEnd::Zcrce, true).unwrap())
        .await;
    c.send_raw(&Header::with_pos(FrameType::Zeof, 1500).encode(HeaderFormat::Hex))
        .await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrinit);
    c.send_raw(&Header::new(FrameType::Zfin).encode(HeaderFormat::Hex))
        .await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zfin);
    c.send_raw(b"OO").await;
    c.expect(b"destination changed during upload").await;
    c.expect(b"files /warez/drop> ").await;
    assert!(b
        .shared
        .files
        .node_by_path("warez", "drop/same.bin")
        .await
        .unwrap()
        .is_none());
    c.send_line("zput").await;
    c.expect(b"Begin your send now").await;
    assert_eq!(client_send(&mut c, info, &bytes, 0).await, 0);
    c.expect(b"Received same.bin (1500 bytes).").await;
    c.expect(b"files /warez/drop> ").await;
    b.shutdown().await;
}
