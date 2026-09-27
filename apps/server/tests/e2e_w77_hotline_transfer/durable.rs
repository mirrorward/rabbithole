//! RH33 real control/HTXF fixtures; all data lives in isolated temporary roots.
use super::*;
use rabbithole_store_server::repo::AccountsRepo;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;

const CHILD_ENV: &str = "RABBITHOLE_HOTLINE_STAGE_FIXTURE";

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
                .create_account(name, "hunter2hunter2", Role::User)
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
    println!("HOTLINE-STAGING-READY {}", b.hotline_addr.unwrap());
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
            if let Some((_, addr)) = line.split_once("HOTLINE-STAGING-READY ") {
                break addr.parse().unwrap();
            }
        }
    })
    .await
    .unwrap();
    (child, addr)
}

async fn login(addr: std::net::SocketAddr, name: &str) -> Client {
    let mut c = Client::connect(addr).await;
    assert_eq!(c.login(name, "hunter2hunter2", name).await.header.error, 0);
    c
}

fn offset(reply: &Transaction) -> usize {
    assert_eq!(reply.header.error, 0, "{reply:?}");
    field_bytes(reply, field::FILE_RESUME_DATA)
        .map(|raw| {
            FileResumeData::decode(raw)
                .unwrap()
                .data_fork_offset()
                .unwrap() as usize
        })
        .unwrap_or(0)
}

/// Unlike the deliberately tolerant existing refusal helper, a successful
/// checkpoint must observe server FIN: it is sent only after durable parking.
async fn send_settled(addr: std::net::SocketAddr, refnum: u32, bytes: &[u8]) {
    let mut sock = TcpStream::connect(htxf_addr(addr)).await.unwrap();
    let mut header = b"HTXF".to_vec();
    header.extend_from_slice(&refnum.to_be_bytes());
    header.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    header.extend_from_slice(&[0; 4]);
    sock.write_all(&header).await.unwrap();
    sock.write_all(bytes).await.unwrap();
    sock.shutdown().await.unwrap();
    let mut sink = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), sock.read_to_end(&mut sink))
        .await
        .expect("HTXF must settle before server FIN")
        .unwrap();
}

async fn park(c: &mut Client, addr: std::net::SocketAddr, name: &str, bytes: &[u8], count: usize) {
    let full = client_ffo(name, "durable metadata", bytes, 0);
    let reply = negotiate_upload(c, name, &["warez"], Some(full.len() as u32), false).await;
    assert_eq!(offset(&reply), 0);
    let envelope = full.len() - bytes.len();
    send_settled(
        addr,
        field_int(&reply, field::REF_NUM).unwrap(),
        &full[..envelope + count],
    )
    .await;
}

#[tokio::test]
async fn killed_daemon_resumes_only_the_authenticated_target_and_exact_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let data = content(96_000);
    let (mut daemon, addr) = child(dir.path()).await;
    let mut alice = login(addr, "alice").await;
    park(&mut alice, addr, "restart.bin", &data, 30_000).await;
    daemon.kill().await.unwrap();
    daemon.wait().await.unwrap();
    drop(alice);
    let (mut daemon, addr) = child(dir.path()).await;
    let mut bob = login(addr, "bob").await;
    let other = negotiate_upload(&mut bob, "restart.bin", &["warez"], None, true).await;
    assert_eq!(
        offset(&other),
        0,
        "another account cannot inherit the prefix"
    );
    let mut alice = login(addr, "alice").await;
    let reply = negotiate_upload(&mut alice, "restart.bin", &["WAREZ"], None, true).await;
    assert_eq!(
        offset(&reply),
        30_000,
        "canonical casing preserves the authorized target"
    );
    send_settled(
        addr,
        field_int(&reply, field::REF_NUM).unwrap(),
        &client_ffo("restart.bin", "resumed", &data, offset(&reply)),
    )
    .await;
    daemon.kill().await.unwrap();
    daemon.wait().await.unwrap();
    drop((alice, bob));
    let b = Burrow::start(test_config(dir.path())).await.unwrap();
    let node = b
        .shared
        .files
        .node_by_path("warez", "restart.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.blob_id, Some(*blake3::hash(&data).as_bytes()));
    assert_eq!(node.comment, "resumed");
    assert_eq!(
        b.shared
            .blobs
            .get(&rabbithole_blobs::BlobId(node.blob_id.unwrap()))
            .unwrap(),
        data
    );
    b.shutdown().await;
}

#[tokio::test]
async fn pending_resume_is_exclusive_and_changed_tail_or_explicit_fresh_start_cannot_mix_files() {
    let dir = tempfile::tempdir().unwrap();
    let (b, mut c, addr) = setup(dir.path()).await;
    let bytes = content(2_000);
    park(&mut c, addr, "changed.bin", &bytes, 700).await;
    let held = negotiate_upload(&mut c, "changed.bin", &["warez"], None, true).await;
    assert_eq!(offset(&held), 700);
    for resume in [true, false] {
        assert_ne!(
            negotiate_upload(&mut c, "changed.bin", &["warez"], None, resume)
                .await
                .header
                .error,
            0
        );
    }
    // The new tail implies a different full length; it cannot append to the
    // old prefix. A definitive rejection removes the checkpoint.
    htxf_send(
        addr,
        field_int(&held, field::REF_NUM).unwrap(),
        &client_ffo("changed.bin", "", &bytes[..1_900], 700),
    )
    .await;
    assert!(b
        .shared
        .files
        .node_by_path("warez", "changed.bin")
        .await
        .unwrap()
        .is_none());
    let fresh = negotiate_upload(&mut c, "changed.bin", &["warez"], None, true).await;
    assert_eq!(offset(&fresh), 0);
    send_settled(
        addr,
        field_int(&fresh, field::REF_NUM).unwrap(),
        &client_ffo("changed.bin", "", &bytes, 0),
    )
    .await;

    park(&mut c, addr, "reset.bin", &bytes, 700).await;
    let fresh = negotiate_upload(&mut c, "reset.bin", &["warez"], None, false).await;
    assert_eq!(offset(&fresh), 0);
    let replacement = b"short replacement";
    send_settled(
        addr,
        field_int(&fresh, field::REF_NUM).unwrap(),
        &client_ffo("reset.bin", "", replacement, 0),
    )
    .await;
    let node = b
        .shared
        .files
        .node_by_path("warez", "reset.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.blob_id, Some(*blake3::hash(replacement).as_bytes()));
    b.shutdown().await;
}

#[tokio::test]
async fn incomplete_or_invalid_fork_envelopes_never_publish() {
    let dir = tempfile::tempdir().unwrap();
    let (b, mut c, addr) = setup(dir.path()).await;
    let data = content(200);
    for (name, case, retained) in [
        ("duplicate.bin", 0, false),
        ("many.bin", 1, false),
        ("truncated.bin", 2, true),
        ("huge-aux.bin", 3, false),
        ("compression.bin", 4, false),
    ] {
        let mut bad = client_ffo(name, "", &data, 0);
        match case {
            0 => {
                bad[..FlatHeader::LEN].copy_from_slice(&FlatHeader { fork_count: 3 }.encode());
                bad.extend_from_slice(
                    &ForkHeader {
                        fork_type: FORK_DATA,
                        data_size: 0,
                    }
                    .encode(),
                );
            }
            1 => bad[..FlatHeader::LEN].copy_from_slice(&FlatHeader { fork_count: 9 }.encode()),
            2 => {
                bad[..FlatHeader::LEN].copy_from_slice(&FlatHeader { fork_count: 3 }.encode());
                bad.extend_from_slice(
                    &ForkHeader {
                        fork_type: *b"MACR",
                        data_size: 10,
                    }
                    .encode(),
                );
                bad.extend_from_slice(b"short");
            }
            3 => {
                bad[..FlatHeader::LEN].copy_from_slice(&FlatHeader { fork_count: 3 }.encode());
                bad.extend_from_slice(
                    &ForkHeader {
                        fork_type: *b"MACR",
                        data_size: 64 * 1024 * 1024 + 1,
                    }
                    .encode(),
                );
            }
            4 => bad[FlatHeader::LEN + 7] = 1,
            _ => unreachable!(),
        }
        let reply = negotiate_upload(&mut c, name, &["warez"], None, false).await;
        htxf_send(addr, field_int(&reply, field::REF_NUM).unwrap(), &bad).await;
        assert!(
            b.shared
                .files
                .node_by_path("warez", name)
                .await
                .unwrap()
                .is_none(),
            "{name}"
        );
        let retry = negotiate_upload(&mut c, name, &["warez"], None, true).await;
        assert_eq!(
            offset(&retry),
            if retained { data.len() } else { 0 },
            "{name}"
        );
        send_settled(
            addr,
            field_int(&retry, field::REF_NUM).unwrap(),
            &client_ffo(name, "", &data, offset(&retry)),
        )
        .await;
        assert!(b
            .shared
            .files
            .node_by_path("warez", name)
            .await
            .unwrap()
            .is_some());
    }
    b.shutdown().await;
}

#[tokio::test]
async fn resumed_upload_rechecks_final_quota_and_denied_hash_then_discards() {
    let dir = tempfile::tempdir().unwrap();
    let (b, mut c, addr) = setup(dir.path()).await;
    for (name, quota) in [("quota.bin", true), ("denied.bin", false)] {
        let data = content(if quota { 2_000 } else { 3_000 });
        park(&mut c, addr, name, &data, 700).await;
        let reply = negotiate_upload(&mut c, name, &["warez"], None, true).await;
        assert_eq!(offset(&reply), 700);
        if quota {
            b.shared
                .config
                .set_key("upload_quota_bytes", "100")
                .unwrap();
        } else {
            b.shared
                .moderation
                .deny_add(blake3::hash(&data).as_bytes(), "late deny", "keeper")
                .await
                .unwrap();
        }
        send_settled(
            addr,
            field_int(&reply, field::REF_NUM).unwrap(),
            &client_ffo(name, "", &data, 700),
        )
        .await;
        assert!(b
            .shared
            .files
            .node_by_path("warez", name)
            .await
            .unwrap()
            .is_none());
        b.shared.config.set_key("upload_quota_bytes", "0").unwrap();
        let retry = negotiate_upload(&mut c, name, &["warez"], None, true).await;
        assert_eq!(offset(&retry), 0);
        // Finish this otherwise pending ticket without retaining a reservation.
        htxf_send(
            addr,
            field_int(&retry, field::REF_NUM).unwrap(),
            &FlatHeader { fork_count: 0 }.encode(),
        )
        .await;
    }
    b.shutdown().await;
}

#[tokio::test]
async fn live_account_disable_blocks_pending_htxf_and_cached_control_session() {
    let dir = tempfile::tempdir().unwrap();
    let (b, mut c, addr) = setup(dir.path()).await;
    let data = content(500);
    let reply = negotiate_upload(&mut c, "disabled.bin", &["warez"], None, false).await;
    assert_eq!(offset(&reply), 0);
    // Direct fixture mutation leaves the existing socket alive, proving the
    // upload gates refresh account state independently of kick delivery.
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", None, None, Some(true))
        .await
        .unwrap();
    htxf_send(
        addr,
        field_int(&reply, field::REF_NUM).unwrap(),
        &client_ffo("disabled.bin", "", &data, 0),
    )
    .await;
    assert!(b
        .shared
        .files
        .node_by_path("warez", "disabled.bin")
        .await
        .unwrap()
        .is_none());
    assert_ne!(
        negotiate_upload(&mut c, "other.bin", &["warez"], None, false)
            .await
            .header
            .error,
        0
    );
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", None, None, Some(false))
        .await
        .unwrap();
    let retry = negotiate_upload(&mut c, "disabled.bin", &["warez"], None, true).await;
    assert_eq!(offset(&retry), 0);
    send_settled(
        addr,
        field_int(&retry, field::REF_NUM).unwrap(),
        &client_ffo("disabled.bin", "", &data, 0),
    )
    .await;
    assert!(b
        .shared
        .files
        .node_by_path("warez", "disabled.bin")
        .await
        .unwrap()
        .is_some());
    b.shutdown().await;
}
