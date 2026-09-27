//! Real REP ZIP import through ctl and the shared service, with durable export
//! provenance. This is a local scripted reader fixture, not external-reader QA.
use burrow::{
    qwk::{build_for, ingest_rep_archive_for},
    Burrow,
};
use rabbithole_legacy_qwk::{zip_store, ControlDat, ReplyMessage, ReplyPacket};
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::{
    repo::{Account, AccountsRepo},
    repo4::PostsRepo,
};
use serde_json::{json, Value};
fn config(path: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "QWK Warren".into(),
        data_dir: path.into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        qwk_enabled: true,
        ..Default::default()
    }
}
async fn start(path: &std::path::Path) -> (Burrow, Account) {
    let b = Burrow::start(config(path)).await.unwrap();
    let a = b
        .shared
        .auth
        .create_account("alice", "password", Role::User)
        .await
        .unwrap();
    for slug in ["alpha", "beta"] {
        b.shared
            .boards
            .create_board(slug, slug, "", 2, None, 0)
            .await
            .unwrap();
    }
    (b, a)
}
fn archive(bbs: &str, replies: Vec<ReplyMessage>) -> Vec<u8> {
    zip_store(&[(
        &format!("{bbs}.MSG"),
        &ReplyPacket {
            header: bbs.into(),
            replies,
        }
        .encode(),
    )])
}
fn deflated_archive(replies: Vec<ReplyMessage>) -> Vec<u8> {
    use std::io::Write;
    let member = ReplyPacket {
        header: "QWKWARRE".into(),
        replies,
    }
    .encode();
    let original = zip_store(&[("QWKWARRE.MSG", &member)]);
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&member).unwrap();
    let compressed = encoder.finish().unwrap();
    let data = 30 + "QWKWARRE.MSG".len();
    let central = data + member.len();
    let mut out = original[..data].to_vec();
    out[8..10].copy_from_slice(&8u16.to_le_bytes());
    out[18..22].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
    out.extend(&compressed);
    let new_central = out.len();
    let mut directory = original[central..original.len() - 22].to_vec();
    directory[10..12].copy_from_slice(&8u16.to_le_bytes());
    directory[20..24].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
    out.extend(directory);
    let mut end = original[original.len() - 22..].to_vec();
    end[16..20].copy_from_slice(&(new_central as u32).to_le_bytes());
    out.extend(end);
    out
}
fn reply(subject: &str) -> ReplyMessage {
    ReplyMessage::new(1, "ALL", "FORGED AUTHOR", subject, "A reply body")
}
async fn post(b: &Burrow, subject: &str, at: i64) -> [u8; 32] {
    b.shared
        .boards
        .post(
            "alpha",
            None,
            "seed@home",
            &[7; 32],
            subject,
            "body",
            "text/plain",
            at,
        )
        .await
        .unwrap()
        .event_id
}
async fn ctl(b: &Burrow, path: &std::path::Path, id: Option<&str>, login: &str) -> Value {
    let mut request = json!({"cmd":"qwk-ingest","login":login,"path":path});
    if let Some(id) = id {
        request["export_id"] = id.into();
    }
    burrow::ctl::handle(&b.shared, &request).await
}
#[tokio::test]
async fn ctl_archive_needs_exact_account_export_and_identity_and_bounds() {
    let work = tempfile::tempdir().unwrap();
    let (b, a) = start(&work.path().join("srv")).await;
    let built = burrow::ctl::handle(&b.shared, &json!({"cmd":"qwk-build","login":"alice"})).await;
    assert_eq!(built["ok"], true, "{built}");
    let id = built["data"]["export_id"].as_str().unwrap();
    let path = work.path().join("mail.REP");
    std::fs::write(&path, archive("QWKWARRE", vec![reply("good")])).unwrap();
    assert_eq!(ctl(&b, &path, None, "alice").await["ok"], false);
    assert_eq!(ctl(&b, &path, Some("missing"), "alice").await["ok"], false);
    b.shared
        .auth
        .create_account("bob", "password", Role::User)
        .await
        .unwrap();
    assert_eq!(ctl(&b, &path, Some(id), "bob").await["ok"], false);
    std::fs::write(&path, archive("OTHER", vec![reply("bad id")])).unwrap();
    assert_eq!(ctl(&b, &path, Some(id), "alice").await["ok"], false);
    std::fs::write(
        &path,
        deflated_archive(vec![
            reply("good"),
            ReplyMessage::new(99, "ALL", "A", "Unknown", "body"),
        ]),
    )
    .unwrap();
    let accepted = ctl(&b, &path, Some(id), "alice").await;
    assert_eq!(accepted["data"]["accepted"], 1, "{accepted}");
    assert_eq!(accepted["data"]["rejected"].as_array().unwrap().len(), 1);
    let rows = b.shared.boards.threads("alpha", 10).await.unwrap();
    assert!(rows[0].0.author.starts_with("alice@"));
    assert!(!rows[0].0.event_blob.is_empty());
    // No malformed ZIP fallback, even with an otherwise-legacy extension.
    let raw = work.path().join("mail.MSG");
    std::fs::write(&raw, b"PKbroken").unwrap();
    assert_eq!(ctl(&b, &raw, None, "alice").await["ok"], false);
    std::fs::write(
        &path,
        vec![0; rabbithole_legacy_qwk::rep_archive::MAX_ARCHIVE_BYTES + 1],
    )
    .unwrap();
    let large = ctl(&b, &path, Some(id), "alice").await;
    assert_eq!(large["ok"], false);
    assert!(large["error"].as_str().unwrap().contains("8 MiB"));
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", None, None, Some(true))
        .await
        .unwrap();
    assert!(ingest_rep_archive_for(
        &b.shared,
        &a,
        id,
        &archive("QWKWARRE", vec![reply("disabled")])
    )
    .await
    .is_err());
    b.shutdown().await;
}
#[tokio::test]
async fn export_survives_restart_and_board_and_article_renumbering() {
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("srv");
    let (b, a) = start(&data).await;
    let parent = post(&b, "Original parent", 2000).await;
    let built = build_for(&b.shared, &a).await.unwrap();
    let original_bytes = std::fs::read(&built.packet_path).unwrap();
    b.shared
        .boards
        .create_board("aardvark", "Earlier", "", 2, None, 0)
        .await
        .unwrap();
    post(&b, "Inserted earlier", 1000).await;
    let newer = build_for(&b.shared, &a).await.unwrap();
    assert_ne!(built.packet_path, newer.packet_path);
    assert_eq!(std::fs::read(&built.packet_path).unwrap(), original_bytes);
    let mut r = reply("Under frozen parent");
    r.reference = 1;
    let zip = archive("QWKWARRE", vec![r]);
    b.shutdown().await;
    let b = Burrow::start(config(&data)).await.unwrap();
    let report = ingest_rep_archive_for(&b.shared, &a, &built.export_id, &zip)
        .await
        .unwrap();
    assert_eq!(report.accepted, 1, "{report:?}");
    let thread = b.shared.boards.thread(&parent, 20).await.unwrap();
    let imported = thread
        .iter()
        .find(|p| p.subject == "Under frozen parent")
        .unwrap();
    assert_eq!(imported.parent_id, Some(parent));
    assert!(b
        .shared
        .boards
        .threads("aardvark", 10)
        .await
        .unwrap()
        .is_empty());
    let report = ingest_rep_archive_for(&b.shared, &a, &built.export_id, &zip)
        .await
        .unwrap();
    assert_eq!(report.duplicates, 1);
    b.shutdown().await;
}
#[tokio::test]
async fn deleted_recreated_boards_and_missing_held_or_tombstoned_parents_fail_closed() {
    use rabbithole_proto::admin::subject_kind;
    let work = tempfile::tempdir().unwrap();
    let (b, a) = start(&work.path().join("srv")).await;
    let first = post(&b, "First", 1000).await;
    let second = post(&b, "Second", 2000).await;
    let third = post(&b, "Third", 3000).await;
    let built = build_for(&b.shared, &a).await.unwrap();
    PostsRepo(&b.shared.pool)
        .delete_thread(&first)
        .await
        .unwrap();
    b.shared
        .moderation
        .quarantine_set(subject_kind::POST, &second, "held", "operator")
        .await
        .unwrap();
    b.shared
        .boards
        .tombstone(third, "seed@home", &[7; 32], 4000)
        .await
        .unwrap();
    let mut replies = Vec::new();
    for reference in [1, 2, 3, 999] {
        let mut r = reply(&format!("missing {reference}"));
        r.reference = reference;
        replies.push(r);
    }
    let report = ingest_rep_archive_for(
        &b.shared,
        &a,
        &built.export_id,
        &archive("QWKWARRE", replies),
    )
    .await
    .unwrap();
    assert_eq!(report.accepted, 0);
    assert_eq!(report.rejected.len(), 4);
    b.shared.boards.delete_board("beta").await.unwrap();
    b.shared
        .boards
        .create_board("beta", "Replacement", "", 2, None, 0)
        .await
        .unwrap();
    let mut r = reply("Old beta");
    r.conference = 2;
    let report = ingest_rep_archive_for(
        &b.shared,
        &a,
        &built.export_id,
        &archive("QWKWARRE", vec![r]),
    )
    .await
    .unwrap();
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.accepted, 0);
    assert!(b
        .shared
        .boards
        .threads("beta", 10)
        .await
        .unwrap()
        .is_empty());
    b.shutdown().await;
}
#[tokio::test]
async fn concurrent_builds_remain_distinct_and_eviction_is_bounded() {
    let work = tempfile::tempdir().unwrap();
    let (b, a) = start(&work.path().join("srv")).await;
    post(&b, "Message", 1000).await;
    let (one, two) = tokio::join!(build_for(&b.shared, &a), build_for(&b.shared, &a));
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_ne!(one.export_id, two.export_id);
    assert_ne!(one.packet_path, two.packet_path);
    for build in [&one, &two] {
        assert!(build.packet_path.is_file());
        let ctl = ControlDat::parse(&std::fs::read(build.spool_dir.join("CONTROL.DAT")).unwrap())
            .unwrap();
        assert_eq!(ctl.conferences, build.conferences);
    }
    // Both concurrent artifacts retain their own original bytes until eviction.
    let first = std::fs::read(&one.packet_path).unwrap();
    assert!(!first.is_empty());
    for _ in 0..8 {
        build_for(&b.shared, &a).await.unwrap();
    }
    assert!(!one.spool_dir.exists());
    assert!(!two.spool_dir.exists());
    let report = ingest_rep_archive_for(
        &b.shared,
        &a,
        &one.export_id,
        &archive("QWKWARRE", vec![reply("evicted")]),
    )
    .await;
    assert!(report.is_err());
    let dirs = std::fs::read_dir(one.spool_dir.parent().unwrap())
        .unwrap()
        .count();
    assert_eq!(dirs, 8);
    b.shutdown().await;
}

#[tokio::test]
async fn current_permissions_and_concurrent_archive_replays_use_shared_posting_path() {
    let work = tempfile::tempdir().unwrap();
    let (b, a) = start(&work.path().join("srv")).await;
    let build = build_for(&b.shared, &a).await.unwrap();
    let bytes = archive("QWKWARRE", vec![reply("Only once")]);
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", Some(Role::Guest as u8), Some(None), None)
        .await
        .unwrap();
    assert!(matches!(
        ingest_rep_archive_for(&b.shared, &a, &build.export_id, &bytes).await,
        Err(burrow::qwk::QwkGateError::Forbidden)
    ));
    assert!(b
        .shared
        .boards
        .threads("alpha", 10)
        .await
        .unwrap()
        .is_empty());
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", Some(Role::User as u8), Some(a.class_id), None)
        .await
        .unwrap();
    let mut events = b.shared.bus.subscribe();
    let (one, two) = tokio::join!(
        ingest_rep_archive_for(&b.shared, &a, &build.export_id, &bytes),
        ingest_rep_archive_for(&b.shared, &a, &build.export_id, &bytes)
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.accepted + two.accepted, 1);
    assert_eq!(one.duplicates + two.duplicates, 1);
    assert!(one.rejected.is_empty() && two.rejected.is_empty());
    let rows = b.shared.boards.threads("alpha", 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let event: rabbithole_server_core::events::SignedEvent =
        postcard::from_bytes(&rows[0].0.event_blob).unwrap();
    event
        .verify(
            &rabbithole_identity::keys::IdentityKey::from_seed(&b.shared.server_signing_seed)
                .public()
                .0,
        )
        .unwrap();
    let mut announced = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let rabbithole_server_core::ServerEvent::BoardPost { id, .. } = event {
            announced.push(id)
        }
    }
    assert_eq!(announced, [rows[0].0.event_id]);
    b.shutdown().await;
}

#[tokio::test]
async fn guest_missing_disabled_and_deleted_builders_leave_no_artifacts() {
    use rabbithole_store_server::repo4::ReadMarksRepo;
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("srv");
    let (b, a) = start(&data).await;
    post(&b, "Unread", 1000).await;
    for id in [-1, i64::MAX] {
        let mut unknown = a.clone();
        unknown.id = id;
        unknown.role = Role::Guest as u8;
        unknown.class_id = None;
        assert!(matches!(
            build_for(&b.shared, &unknown).await,
            Err(burrow::qwk::QwkGateError::Forbidden)
        ));
    }
    AccountsRepo(&b.shared.pool)
        .admin_set("alice", None, None, Some(true))
        .await
        .unwrap();
    assert!(
        matches!(
            build_for(&b.shared, &a).await,
            Err(burrow::qwk::QwkGateError::Forbidden)
        ),
        "stale enabled caller is refreshed"
    );
    assert_eq!(
        ReadMarksRepo(&b.shared.pool)
            .get(a.id, "alpha")
            .await
            .unwrap(),
        0
    );
    assert!(AccountsRepo(&b.shared.pool)
        .delete(a.id, Role::Superuser as u8)
        .await
        .unwrap());
    assert!(matches!(
        build_for(&b.shared, &a).await,
        Err(burrow::qwk::QwkGateError::Forbidden)
    ));
    assert!(
        !data.join("qwk").exists(),
        "refused builds create no spool directories"
    );
    b.shutdown().await;
}
