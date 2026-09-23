//! The admin console's contract with a burrow: the config describes itself, a
//! change is kept across a restart, and a credential is never read back.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    config_flag, config_kind, ConfigApplied, ConfigDescribeRequest, ConfigDescription, ConfigSet,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo::AuditRepo;

async fn start(dir: &std::path::Path) -> Burrow {
    let path = dir.join("burrow.toml");
    std::fs::write(
        &path,
        "# the operator's own notes\nname = \"Described Warren\"\n",
    )
    .unwrap();
    let mut config = ServerConfig::load(Some(&path)).unwrap();
    config.quic_addr = "127.0.0.1:0".parse().unwrap();
    config.ws_addr = "127.0.0.1:0".parse().unwrap();
    config.data_dir = dir.to_path_buf();
    let burrow = Burrow::start(config).await.unwrap();
    for (login, role) in [("root", Role::Admin), ("alice", Role::User)] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    burrow
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

#[tokio::test]
async fn a_burrow_describes_its_settings_to_an_admin_and_to_nobody_else() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;

    let mut alice = login(&burrow, "alice").await;
    let refused = alice
        .request::<_, ConfigDescription>(&ConfigDescribeRequest)
        .await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::Forbidden))),
        "a member was told the config: {refused:?}"
    );

    let mut root = login(&burrow, "root").await;
    let described: ConfigDescription = root.request(&ConfigDescribeRequest).await.unwrap();
    let of = |key: &str| {
        described
            .entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("{key} is not described"))
            .clone()
    };
    assert!(described.entries.len() > 90);

    let name = of("name");
    assert_eq!(name.value, "Described Warren");
    assert_eq!(name.default, "An Unnamed Burrow");
    assert_eq!(name.kind, config_kind::TEXT);
    assert!(name.has(config_flag::LIVE));

    assert_eq!(of("guest_enabled").kind, config_kind::BOOL);
    assert_eq!(of("chat_max_len").kind, config_kind::NUMBER);
    let mode = of("registration_mode");
    assert_eq!(mode.kind, config_kind::CHOICE);
    assert_eq!(mode.choices, ["open", "invite", "closed"]);
    assert!(of("data_dir").has(config_flag::READ_ONLY));
    assert!(!of("quic_addr").has(config_flag::LIVE));

    burrow.shutdown().await;
}

#[tokio::test]
async fn a_saved_setting_is_in_the_file_and_a_password_is_never_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;

    let applied: ConfigApplied = root
        .request(&ConfigSet::new("motd", "Mind the gap"))
        .await
        .unwrap();
    assert!(applied.applied_live);
    let applied: ConfigApplied = root
        .request(&ConfigSet::new("radio_source_password", "hunter2-hunter2"))
        .await
        .unwrap();
    // Read per source connection, so it takes effect at once.
    assert!(applied.applied_live);
    // A value the key refuses changes nothing and says so.
    let refused = root
        .request::<_, ConfigApplied>(&ConfigSet::new("registration_mode", "whenever"))
        .await;
    assert!(matches!(
        refused,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    // The console is told which keys it may not set; the burrow enforces
    // it rather than trusting the client. `data_dir` is what every other
    // path resolves against and what the HTTP server keeps to itself: moved
    // from a console, the real folder, signing key and all, would stop
    // counting as the burrow's own.
    let refused = root
        .request::<_, ConfigApplied>(&ConfigSet::new("data_dir", "/tmp/somewhere-else"))
        .await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::Forbidden))),
        "a console moved the data directory: {refused:?}"
    );
    // And the web root cannot be aimed at the burrow's own folders.
    let refused = root
        .request::<_, ConfigApplied>(&ConfigSet::new(
            "http_web_root",
            dir.path().to_str().unwrap(),
        ))
        .await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::BadRequest))),
        "the data folder was accepted as a web root: {refused:?}"
    );

    // In the file, beside what the operator wrote there.
    let text = std::fs::read_to_string(dir.path().join("burrow.toml")).unwrap();
    assert!(text.contains("# the operator's own notes"), "{text}");
    assert!(text.contains("motd = \"Mind the gap\""), "{text}");
    assert!(!text.contains("whenever"), "{text}");
    // The test's own overrides (a port of 0) were never written.
    assert!(!text.contains("quic_addr"), "{text}");

    // Described, not disclosed.
    let described: ConfigDescription = root.request(&ConfigDescribeRequest).await.unwrap();
    let pw = described
        .entries
        .iter()
        .find(|e| e.key == "radio_source_password")
        .unwrap();
    assert!(pw.has(config_flag::SECRET) && pw.has(config_flag::SET));
    assert!(pw.value.is_empty());
    assert!(described
        .entries
        .iter()
        .all(|e| !e.value.contains("hunter2")));

    // The audit log says it changed, and not what to.
    let audit = AuditRepo(&burrow.shared.pool).recent(50).await.unwrap();
    let lines: Vec<String> = audit.iter().map(|a| a.detail.clone()).collect();
    assert!(lines.iter().any(|l| l == "motd=Mind the gap"), "{lines:?}");
    assert!(
        lines.iter().any(|l| l == "radio_source_password=(set)"),
        "{lines:?}"
    );
    assert!(lines.iter().all(|l| !l.contains("hunter2")), "{lines:?}");

    // The restart.
    burrow.shutdown().await;
    let again = ServerConfig::load(Some(&dir.path().join("burrow.toml"))).unwrap();
    assert_eq!(again.motd, "Mind the gap");
    assert_eq!(again.radio_source_password, "hunter2-hunter2");
    assert_eq!(again.name, "Described Warren");
}

// ---------------------------------------------------------------------------
// Surfaces start and stop while the burrow runs.
// ---------------------------------------------------------------------------

use rabbithole_proto::admin::{surface_state, SurfaceInfo, SurfaceStatus, SurfaceStatusRequest};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

async fn surface(root: &mut Client, key: &str) -> SurfaceInfo {
    let status: SurfaceStatus = root.request(&SurfaceStatusRequest).await.unwrap();
    status
        .surfaces
        .into_iter()
        .find(|s| s.key == key)
        .unwrap_or_else(|| panic!("{key} is not a reported surface"))
}

async fn set(root: &mut Client, key: &str, value: &str) -> bool {
    let applied: ConfigApplied = root.request(&ConfigSet::new(key, value)).await.unwrap();
    applied.applied_live
}

/// The first line a freshly accepted connection is greeted with.
async fn greeting(addr: &str) -> String {
    let mut sock = TcpStream::connect(addr).await.expect("the surface accepts");
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), sock.read(&mut buf))
        .await
        .expect("a greeting arrives")
        .unwrap();
    String::from_utf8_lossy(&buf[..n]).to_string()
}

#[tokio::test]
async fn turning_a_gateway_on_starts_it_and_turning_it_off_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;

    assert!(burrow.nntp_addr.is_none(), "nothing asked for it at boot");
    assert_eq!(
        surface(&mut root, "nntp_enabled").await.state,
        surface_state::OFF
    );

    // Any free port, then on. Both are live now, and said to be.
    assert!(set(&mut root, "nntp_addr", "127.0.0.1:0").await);
    assert!(set(&mut root, "nntp_enabled", "true").await);

    // By the time "applied" came back, it was listening.
    let up = surface(&mut root, "nntp_enabled").await;
    assert_eq!(up.state, surface_state::LISTENING, "{up:?}");
    assert!(
        up.addr.starts_with("127.0.0.1:") && !up.addr.ends_with(":0"),
        "{up:?}"
    );
    let hello = greeting(&up.addr).await;
    assert!(hello.starts_with("20"), "an NNTP greeting: {hello:?}");

    // Off again: the port is closed, not merely ignored.
    assert!(set(&mut root, "nntp_enabled", "false").await);
    assert_eq!(
        surface(&mut root, "nntp_enabled").await.state,
        surface_state::OFF
    );
    assert!(
        TcpStream::connect(&up.addr).await.is_err(),
        "the listener is gone"
    );

    // A member may not ask what is listening.
    let mut alice = login(&burrow, "alice").await;
    let refused = alice
        .request::<_, SurfaceStatus>(&SurfaceStatusRequest)
        .await;
    assert!(matches!(
        refused,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    burrow.shutdown().await;
}

#[tokio::test]
async fn a_gateway_that_cannot_start_says_why_and_the_burrow_carries_on() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;

    // Someone else already has the port.
    let squatter = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken = squatter.local_addr().unwrap().to_string();

    set(&mut root, "telnet_addr", &taken).await;
    set(&mut root, "telnet_enabled", "true").await;
    let failed = surface(&mut root, "telnet_enabled").await;
    assert_eq!(failed.state, surface_state::FAILED, "{failed:?}");
    assert!(!failed.detail.is_empty(), "it says why: {failed:?}");
    assert!(failed.addr.is_empty());

    // The burrow is still here, and still answering.
    let described: ConfigDescription = root.request(&ConfigDescribeRequest).await.unwrap();
    assert!(described
        .entries
        .iter()
        .any(|e| e.key == "telnet_enabled" && e.value == "true"));

    // Moving it to a free port is all it takes: no restart, no off-and-on.
    set(&mut root, "telnet_addr", "127.0.0.1:0").await;
    let up = surface(&mut root, "telnet_enabled").await;
    assert_eq!(up.state, surface_state::LISTENING, "{up:?}");

    // And a surface that failed is retried when anything changes, because the
    // port it wanted may have come free.
    set(&mut root, "finger_addr", &taken).await;
    set(&mut root, "finger_enabled", "true").await;
    assert_eq!(
        surface(&mut root, "finger_enabled").await.state,
        surface_state::FAILED
    );
    drop(squatter);
    set(&mut root, "motd", "anything at all").await;
    let finger = surface(&mut root, "finger_enabled").await;
    assert_eq!(finger.state, surface_state::LISTENING, "{finger:?}");
    assert_eq!(finger.addr, taken);

    burrow.shutdown().await;
}

#[tokio::test]
async fn a_gateway_saved_as_on_is_on_after_the_restart() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    set(&mut root, "finger_addr", "127.0.0.1:0").await;
    set(&mut root, "finger_enabled", "true").await;
    burrow.shutdown().await;

    // The same data directory and the same file, as a restart would find them.
    let mut config = ServerConfig::load(Some(&dir.path().join("burrow.toml"))).unwrap();
    assert!(config.finger_enabled, "the switch was saved");
    config.quic_addr = "127.0.0.1:0".parse().unwrap();
    config.ws_addr = "127.0.0.1:0".parse().unwrap();
    config.data_dir = dir.path().to_path_buf();
    let again = Burrow::start(config).await.unwrap();
    let addr = again.finger_addr.expect("finger came up with the burrow");
    assert_ne!(addr.port(), 0);
    again.shutdown().await;
}

/// The audit log says it holds every change an operator or moderator made.
/// It used to miss a good half of them: a board made, a file area made or
/// taken away, a folder, a hash refused, a report acted on, a moderator
/// removing somebody else's post. Each of those is a thing an operator
/// would want to find afterwards, and none of them was written down.
#[tokio::test]
async fn every_operator_change_is_on_the_record() {
    use rabbithole_proto::admin::{
        report_action, report_state, AuditList, AuditListRequest, DenyHashAdd, DenyHashRemove,
        ReportCreate, ReportList, ReportListRequest, ReportResolve,
    };
    use rabbithole_proto::board::{BoardCreate, PostCreate, PostDelete};
    use rabbithole_proto::filelib::{AreaCreate, AreaDelete, AreaUpdate, FolderCreate};

    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;

    // A board, a post on it by somebody else, and the moderator taking it
    // down: only the last is anybody's business but the author's.
    root.board_create(&BoardCreate::new("talk", "Talk", 2))
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;
    let post = alice
        .post(&PostCreate::new("talk", "Hello", "is it me"))
        .await
        .unwrap();
    root.request_ack(&PostDelete::new(post.id)).await.unwrap();

    // A file area, renamed, given a folder, then taken away.
    root.request_ack(&AreaCreate::new("shelf", "The Shelf"))
        .await
        .unwrap();
    root.request_ack(&AreaUpdate::new("shelf", "The Long Shelf", ""))
        .await
        .unwrap();
    root.request_ack(&FolderCreate::new("shelf", None, "papers"))
        .await
        .unwrap();

    // A hash refused and allowed again.
    root.request_ack(&DenyHashAdd::new([7u8; 32], "malware"))
        .await
        .unwrap();
    root.request_ack(&DenyHashRemove::new([7u8; 32]))
        .await
        .unwrap();

    // A report, acted on.
    alice
        .request_ack(&ReportCreate::new(0, post.id.to_vec(), "rude"))
        .await
        .unwrap();
    let open: ReportList = root
        .request(&ReportListRequest::new(Some(report_state::OPEN), 0, 10))
        .await
        .unwrap();
    let report = open.reports.first().expect("the report is queued");
    root.request_ack(&ReportResolve::new(
        report.id,
        report_action::RESOLVE,
        "dealt with",
    ))
    .await
    .unwrap();

    // An empty one, made and taken away again.
    root.request_ack(&AreaCreate::new("attic", "The Attic"))
        .await
        .unwrap();
    root.request_ack(&AreaDelete::new("attic")).await.unwrap();

    // The audit log is written from a task of its own, so give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let log: AuditList = root.request(&AuditListRequest::new(100)).await.unwrap();
    let actions: Vec<&str> = log.entries.iter().map(|e| e.action.as_str()).collect();
    for want in [
        "board-create",
        "post-delete",
        "area-create",
        "area-update",
        "area-delete",
        "folder-create",
        "deny-hash-add",
        "deny-hash-remove",
        "report-resolve",
    ] {
        assert!(
            actions.contains(&want),
            "{want} is not on the record: {actions:?}"
        );
    }
    // And the one thing that is not an operator's doing is not in it:
    // alice taking down her own post would have been her own business.
    let removal = log
        .entries
        .iter()
        .find(|e| e.action == "post-delete")
        .unwrap();
    assert_eq!(removal.actor, "root");
    assert!(
        removal.detail.contains("Hello") && removal.detail.contains("talk"),
        "{:?}",
        removal.detail
    );

    burrow.shutdown().await;
}

/// What a moderator is holding back is a list they can read and act on:
/// the console used to be able to hold nothing and see nothing held, while
/// every surface honoured the holds. Moderators only.
#[tokio::test]
async fn what_is_held_back_is_listed_to_a_moderator_and_can_be_let_through() {
    use rabbithole_proto::admin::{
        subject_kind, QuarantineClear, QuarantineList, QuarantineListRequest, QuarantineSet,
    };

    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let mut alice = login(&burrow, "alice").await;
    let blob = [7u8; 32];

    // Nothing held yet.
    let held: QuarantineList = root
        .request(&QuarantineListRequest::new(0, 50))
        .await
        .unwrap();
    assert!(held.held.is_empty());
    assert_eq!(held.total, 0);

    // A moderator holds a file back, and the list says what and why.
    root.request_ack(&QuarantineSet::new(
        subject_kind::FILE,
        blob.to_vec(),
        "under review",
    ))
    .await
    .unwrap();
    let held: QuarantineList = root
        .request(&QuarantineListRequest::new(0, 50))
        .await
        .unwrap();
    assert_eq!(held.held.len(), 1);
    assert_eq!(held.total, 1);
    let item = &held.held[0];
    assert_eq!(item.subject_kind, subject_kind::FILE);
    assert_eq!(item.subject_ref, blob.to_vec());
    assert_eq!(item.reason, "under review");
    assert_eq!(item.held_by, "root", "who held it");
    assert!(item.at_unix > 0, "and when");

    // Nobody else reads it.
    let refused = alice
        .request::<_, QuarantineList>(&QuarantineListRequest::new(0, 50))
        .await;
    assert!(
        matches!(refused, Err(ClientError::Refused(ErrorCode::Forbidden))),
        "{refused:?}"
    );

    // Let through, and it is off the list.
    root.request_ack(&QuarantineClear::new(subject_kind::FILE, blob.to_vec()))
        .await
        .unwrap();
    let held: QuarantineList = root
        .request(&QuarantineListRequest::new(0, 50))
        .await
        .unwrap();
    assert!(held.held.is_empty());

    burrow.shutdown().await;
}
