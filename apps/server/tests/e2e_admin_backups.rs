//! Backups over the wire: an operator makes, lists, checks and removes
//! snapshots from the console. A snapshot is the whole burrow, so the door is
//! narrower than `CONFIG_ADMIN` alone.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    AccountSet, BackupCreate, BackupDelete, BackupList, BackupListRequest, BackupMade,
    BackupVerified, BackupVerify, ClassSet, PeerList, PeerListRequest,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Caps, Role, ServerConfig};
use rabbithole_store_server::repo::AuditRepo;

const PW: &str = "pw-pw-pw";

async fn start(dir: &std::path::Path) -> Burrow {
    let config = ServerConfig {
        name: "Kept Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(config).await.unwrap();
    for (login, role) in [
        ("root", Role::Admin),
        ("mo", Role::Moderator),
        ("alice", Role::User),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, PW, role)
            .await
            .unwrap();
    }
    burrow
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    c.auth_password(user, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn an_operator_makes_checks_and_removes_a_snapshot_from_the_console() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;

    let mut alice = login(&burrow, "alice").await;
    refused(
        alice.request::<_, BackupList>(&BackupListRequest).await,
        ErrorCode::Forbidden,
    );
    refused(
        alice.request::<_, BackupMade>(&BackupCreate).await,
        ErrorCode::Forbidden,
    );

    // The folder is the burrow's own, and empty until asked.
    let mut root = login(&burrow, "root").await;
    let list: BackupList = root.request(&BackupListRequest).await.unwrap();
    assert!(list.snapshots.is_empty());
    assert_eq!(
        std::path::Path::new(&list.dir),
        dir.path().join("backups"),
        "a relative backup_dir resolves under the data directory"
    );

    // Made, listed, and whole.
    let made: BackupMade = root.request(&BackupCreate).await.unwrap();
    let name = made.snapshot.name.clone();
    assert!(name.starts_with("snapshot-"), "{name}");
    assert!(
        made.snapshot.files >= 2,
        "the database and the identity: {made:?}"
    );
    assert!(made.snapshot.total_bytes > 0);
    assert_eq!(made.snapshot.version, env!("CARGO_PKG_VERSION"));
    let list: BackupList = root.request(&BackupListRequest).await.unwrap();
    assert_eq!(list.snapshots, vec![made.snapshot.clone()]);

    // A snapshot is the whole burrow: the database it copies holds every
    // password hash and every private message. `VACUUM INTO` writes a fresh
    // file at whatever the umask says, so it is kept to its owner on
    // purpose rather than by luck.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let snap = dir.path().join("backups").join(&name);
        let mode =
            |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(snap.clone()),
            0o700,
            "the snapshot folder is the owner's"
        );
        assert_eq!(mode(snap.join("burrow.db")), 0o600, "so is the database");
        assert_eq!(
            mode(snap.join("identity").join("server_ed25519.seed")),
            0o600,
            "and the signing seed keeps what it had"
        );
    }

    let checked: BackupVerified = root.request(&BackupVerify::new(&name)).await.unwrap();
    assert!(checked.ok, "{checked:?}");
    assert_eq!(checked.detail, "ok");
    assert_eq!(checked.files, made.snapshot.files);
    assert_eq!(checked.total_bytes, made.snapshot.total_bytes);

    // A snapshot that was tampered with is reported, not hidden.
    let db = dir.path().join("backups").join(&name).join("burrow.db");
    let mut bytes = std::fs::read(&db).unwrap();
    bytes[100] ^= 0xff;
    std::fs::write(&db, bytes).unwrap();
    let checked: BackupVerified = root.request(&BackupVerify::new(&name)).await.unwrap();
    assert!(!checked.ok, "{checked:?}");
    assert!(checked.detail.contains("mismatch"), "{checked:?}");

    // Names are the burrow's own; nothing else in the folder is reachable.
    refused(
        root.request::<_, BackupVerified>(&BackupVerify::new("snapshot-nope"))
            .await,
        ErrorCode::NotFound,
    );
    refused(
        root.request::<_, BackupVerified>(&BackupVerify::new("../identity"))
            .await,
        ErrorCode::NotFound,
    );
    std::fs::write(dir.path().join("backups").join("notes.txt"), "mine").unwrap();
    refused(
        root.request_ack(&BackupDelete::new("notes.txt")).await,
        ErrorCode::NotFound,
    );

    root.request_ack(&BackupDelete::new(&name)).await.unwrap();
    assert!(!dir.path().join("backups").join(&name).exists());
    assert!(dir.path().join("backups").join("notes.txt").exists());
    let list: BackupList = root.request(&BackupListRequest).await.unwrap();
    assert!(list.snapshots.is_empty());
    refused(
        root.request_ack(&BackupDelete::new(&name)).await,
        ErrorCode::NotFound,
    );

    let audit = AuditRepo(&burrow.shared.pool).recent(50).await.unwrap();
    assert!(audit
        .iter()
        .any(|row| row.actor == "root" && row.action == "backup" && row.detail.contains(&name)));
    assert!(audit
        .iter()
        .any(|row| row.actor == "root" && row.action == "backup-delete" && row.detail == name));

    burrow.shutdown().await;
}

#[tokio::test]
async fn config_admin_through_a_class_does_not_reach_the_backups() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;

    // A moderator handed CONFIG_ADMIN by class can read the peers…
    let mut root = login(&burrow, "root").await;
    root.request_ack(&ClassSet::new("ops", Caps::CONFIG_ADMIN.0))
        .await
        .unwrap();
    let mut set = AccountSet::new("mo");
    set.class = Some("ops".into());
    root.request_ack(&set).await.unwrap();
    let mut mo = login(&burrow, "mo").await;
    mo.request::<_, PeerList>(&PeerListRequest)
        .await
        .expect("CONFIG_ADMIN reads the peer list");

    // …and not the snapshots: that door is the Admin role's.
    refused(
        mo.request::<_, BackupList>(&BackupListRequest).await,
        ErrorCode::Forbidden,
    );
    refused(
        mo.request::<_, BackupMade>(&BackupCreate).await,
        ErrorCode::Forbidden,
    );
    assert!(!dir.path().join("backups").exists(), "nothing was written");

    burrow.shutdown().await;
}
