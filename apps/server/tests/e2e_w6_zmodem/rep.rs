//! Scripted REP reader over real TCP/telnet, including shell recovery.
use super::*;
use rabbithole_legacy_qwk::{zip_store, ReplyMessage, ReplyPacket};
use rabbithole_legacy_zmodem::Header;
use rabbithole_server_core::Caps;
use rabbithole_store_server::repo::{Account, AccountsRepo, ClassesRepo};

async fn fixture(path: &Path) -> (Burrow, Account, String, ZClient) {
    let mut cfg = test_config(path);
    cfg.qwk_enabled = true;
    let b = Burrow::start(cfg).await.unwrap();
    let account = b
        .shared
        .auth
        .create_account("alice", "password", Role::User)
        .await
        .unwrap();
    b.shared
        .boards
        .create_board("general", "General", "", 2, None, 0)
        .await
        .unwrap();
    let packet = burrow::qwk::build_for(&b.shared, &account).await.unwrap();
    let mut client = ZClient::connect(b.telnet_addr.unwrap()).await;
    client.login("alice", "password").await;
    (b, account, packet.export_id, client)
}

fn archive(subject: &str) -> Vec<u8> {
    let bytes = ReplyPacket {
        header: "ZMODEMWA".into(),
        replies: vec![ReplyMessage::new(
            1,
            "ALL",
            "FORGED",
            subject,
            "Offline reply body",
        )],
    }
    .encode();
    zip_store(&[("ZMODEMWA.MSG", &bytes)])
}

async fn ready(c: &mut ZClient, id: &str) {
    c.send_line(&format!("qwk-reply {id}")).await;
    c.expect(b"Ready for REP via ZMODEM.").await;
}

async fn offer(c: &mut ZClient, info: &FileInfo) {
    c.send_raw(&Header::new(FrameType::Zrqinit).encode(HeaderFormat::Hex))
        .await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrinit);
    c.send_raw(&Header::new(FrameType::Zfile).encode(HeaderFormat::Bin32))
        .await;
    c.send_raw(&encode_subpacket(&info.encode().unwrap(), FrameEnd::Zcrcw, true).unwrap())
        .await;
}

async fn file_data(c: &mut ZClient, bytes: &[u8]) {
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrpos);
    c.send_raw(&Header::with_pos(FrameType::Zdata, 0).encode(HeaderFormat::Bin32))
        .await;
    let mut chunks = bytes.chunks(1024).peekable();
    while let Some(chunk) = chunks.next() {
        let end = if chunks.peek().is_some() {
            FrameEnd::Zcrcg
        } else {
            FrameEnd::Zcrce
        };
        c.send_raw(&encode_subpacket(chunk, end, true).unwrap())
            .await;
    }
    c.send_raw(&Header::with_pos(FrameType::Zeof, bytes.len() as u32).encode(HeaderFormat::Bin32))
        .await;
}

async fn prompt_works(c: &mut ZClient) {
    c.expect(b"Command: ").await;
    c.send_line("help").await;
    c.expect(b"qwk-reply <export_id> upload REP replies").await;
    c.expect(b"Command: ").await;
}

#[tokio::test]
async fn rep_import_reports_signs_deduplicates_without_file_upload_or_http() {
    let work = tempfile::tempdir().unwrap();
    let (b, a, id, mut c) = fixture(work.path()).await;
    // This real account can post but cannot upload library files. A custom
    // class is additive, so use the Guest role and explicitly grant BOARD_POST.
    let class = ClassesRepo(&b.shared.pool)
        .upsert("reply-only", Caps::BOARD_POST.0)
        .await
        .unwrap();
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", Some(Role::Guest as u8), Some(Some(class)), None)
        .await
        .unwrap();
    b.shared.classes.reload(&b.shared.pool).await.unwrap();
    // Reauthenticate with those restricted caps, so a stale original User
    // session cannot accidentally satisfy an incorrect FILE_UPLOAD gate.
    drop(c);
    c = ZClient::connect(b.telnet_addr.unwrap()).await;
    c.login("alice", "password").await;
    let bytes = ReplyPacket {
        header: "ZMODEMWA".into(),
        replies: vec![
            ReplyMessage::new(1, "ALL", "FORGED", "Accepted", "Offline reply body"),
            ReplyMessage::new(99, "ALL", "FORGED", "\x1b[2JBad\nsubject", "Bad conference"),
        ],
    }
    .encode();
    let bytes = zip_store(&[("ZMODEMWA.MSG", &bytes)]);
    for (accepted, duplicate) in [(1, 0), (0, 1)] {
        ready(&mut c, &id).await;
        client_send(
            &mut c,
            FileInfo {
                length: Some(bytes.len() as u64),
                ..FileInfo::new("replies.REP")
            },
            &bytes,
            0,
        )
        .await;
        c.expect(
            format!("REP import: {accepted} accepted, {duplicate} duplicate, 1 rejected.")
                .as_bytes(),
        )
        .await;
        let start = c.pos;
        prompt_works(&mut c).await;
        assert!(!c.data[start..c.pos].contains(&0x1b));
    }
    let rows = b.shared.boards.threads("general", 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].0.author.starts_with("alice@"));
    assert!(!rows[0].0.event_blob.is_empty());
    assert!(b.shared.files.areas().await.unwrap().is_empty());
    assert!(b.shared.config.read().files_http_base.is_empty());
    assert!(a.id > 0);
    b.shutdown().await;
}

#[tokio::test]
async fn rep_preflight_refuses_disabled_foreign_missing_and_revoked_export_owners() {
    let work = tempfile::tempdir().unwrap();
    let (b, _, id, mut c) = fixture(work.path()).await;
    c.send_line("qwk-reply").await;
    c.expect(b"Usage: qwk-reply").await;
    prompt_works(&mut c).await;
    b.shared.config.set_key("qwk_enabled", "false").unwrap();
    c.send_line(&format!("qwk-reply {id}")).await;
    c.expect(b"not enabled").await;
    prompt_works(&mut c).await;
    b.shared.config.set_key("qwk_enabled", "true").unwrap();
    let bob = b
        .shared
        .auth
        .create_account("bob", "password", Role::User)
        .await
        .unwrap();
    let foreign = burrow::qwk::build_for(&b.shared, &bob)
        .await
        .unwrap()
        .export_id;
    for id in [&foreign, "missing"] {
        c.send_line(&format!("qwk-reply {id}")).await;
        c.expect(b"missing, foreign, or expired QWK export id")
            .await;
        prompt_works(&mut c).await;
    }
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", Some(Role::Guest as u8), Some(None), None)
        .await
        .unwrap();
    c.send_line(&format!("qwk-reply {id}")).await;
    c.expect(b"permission to post board replies").await;
    prompt_works(&mut c).await;
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", Some(Role::User as u8), None, Some(true))
        .await
        .unwrap();
    c.send_line(&format!("qwk-reply {id}")).await;
    c.expect(b"enabled account").await;
    prompt_works(&mut c).await;
    assert!(b
        .shared
        .boards
        .threads("general", 10)
        .await
        .unwrap()
        .is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn rep_rechecks_account_and_feature_after_transfer() {
    let work = tempfile::tempdir().unwrap();
    let (b, _, id, mut c) = fixture(work.path()).await;
    let bytes = archive("Do not post");
    for disabled_account in [true, false] {
        ready(&mut c, &id).await;
        if disabled_account {
            AccountsRepo(&b.shared.pool)
                .admin_set("alice", None, None, Some(true))
                .await
                .unwrap();
        } else {
            b.shared.config.set_key("qwk_enabled", "false").unwrap();
        }
        client_send(&mut c, FileInfo::new("reply.rep"), &bytes, 0).await;
        c.expect(b"REP import refused:").await;
        prompt_works(&mut c).await;
        AccountsRepo(&b.shared.pool)
            .admin_set("alice", None, None, Some(false))
            .await
            .unwrap();
        b.shared.config.set_key("qwk_enabled", "true").unwrap();
    }
    assert!(b
        .shared
        .boards
        .threads("general", 10)
        .await
        .unwrap()
        .is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn rep_cancel_malformed_and_bad_size_restore_prompt_without_publication() {
    let work = tempfile::tempdir().unwrap();
    let (b, _, id, mut c) = fixture(work.path()).await;
    ready(&mut c, &id).await;
    offer(&mut c, &FileInfo::new("reply.rep")).await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrpos);
    c.send_raw(&Header::with_pos(FrameType::Zdata, 0).encode(HeaderFormat::Bin32))
        .await;
    c.send_raw(&encode_subpacket(b"partial data", FrameEnd::Zcrcg, true).unwrap())
        .await;
    c.send_raw(&[0x18; 5]).await;
    c.expect(b"Transfer cancelled.").await;
    prompt_works(&mut c).await;
    ready(&mut c, &id).await;
    client_send(&mut c, FileInfo::new("reply.rep"), b"PKbroken", 0).await;
    c.expect(b"REP import refused:").await;
    prompt_works(&mut c).await;
    ready(&mut c, &id).await;
    offer(&mut c, &FileInfo::new("corrupt.rep")).await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrpos);
    c.send_raw(&Header::with_pos(FrameType::Zdata, 0).encode(HeaderFormat::Bin32))
        .await;
    let mut corrupt = encode_subpacket(b"x", FrameEnd::Zcrce, true).unwrap();
    corrupt[0] = b'y'; // Preserve framing while invalidating the payload CRC.
    c.send_raw(&corrupt).await;
    c.expect(b"Transfer failed: bad subpacket").await;
    prompt_works(&mut c).await;
    ready(&mut c, &id).await;
    offer(
        &mut c,
        &FileInfo {
            length: Some(8 * 1024 * 1024 + 1),
            ..FileInfo::new("huge.rep")
        },
    )
    .await;
    c.expect(b"Upload refused: REP must contain between 1 byte and 8 MiB")
        .await;
    prompt_works(&mut c).await;
    ready(&mut c, &id).await;
    let bytes = archive("short file");
    offer(
        &mut c,
        &FileInfo {
            length: Some(bytes.len() as u64 + 1),
            ..FileInfo::new("short.rep")
        },
    )
    .await;
    file_data(&mut c, &bytes).await;
    c.expect(b"does not match its declared length").await;
    prompt_works(&mut c).await;
    assert!(b
        .shared
        .boards
        .threads("general", 10)
        .await
        .unwrap()
        .is_empty());
    b.shutdown().await;
}

#[tokio::test]
async fn rep_second_file_discards_complete_first_and_new_command_starts_clean() {
    let work = tempfile::tempdir().unwrap();
    let (b, _, id, mut c) = fixture(work.path()).await;
    let bytes = archive("Only after whole batch");
    ready(&mut c, &id).await;
    offer(&mut c, &FileInfo::new("first.rep")).await;
    file_data(&mut c, &bytes).await;
    assert_eq!(c.next_header().await.header.frame_type, FrameType::Zrinit);
    c.send_raw(&Header::new(FrameType::Zfile).encode(HeaderFormat::Bin32))
        .await;
    c.send_raw(
        &encode_subpacket(
            &FileInfo::new("second.rep").encode().unwrap(),
            FrameEnd::Zcrcw,
            true,
        )
        .unwrap(),
    )
    .await;
    c.expect(b"send exactly one REP archive per command").await;
    prompt_works(&mut c).await;
    assert!(b
        .shared
        .boards
        .threads("general", 10)
        .await
        .unwrap()
        .is_empty());
    ready(&mut c, &id).await;
    assert_eq!(
        client_send(&mut c, FileInfo::new("valid.rep"), &bytes, 0).await,
        0
    );
    c.expect(b"REP import: 1 accepted, 0 duplicate, 0 rejected.")
        .await;
    prompt_works(&mut c).await;
    b.shutdown().await;
}
