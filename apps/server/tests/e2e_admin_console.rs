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
    assert!(!applied.applied_live);
    // A value the key refuses changes nothing and says so.
    let refused = root
        .request::<_, ConfigApplied>(&ConfigSet::new("registration_mode", "whenever"))
        .await;
    assert!(matches!(
        refused,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

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
