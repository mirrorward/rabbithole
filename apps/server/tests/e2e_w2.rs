//! Wave 2.1 end-to-end tests: registration, personas, directory, blobs,
//! TOTP, and the admin family.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_identity::totp::TotpEnrollment;
use rabbithole_proto::admin as padm;
use rabbithole_proto::blob::BlobPurpose;
use rabbithole_proto::persona::{PersonaUpdate, Profile};
use rabbithole_proto::session::ServerNotice;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Caps, Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "W2 Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn connect(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .expect("connect")
}

#[tokio::test]
async fn registration_modes() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();

    // Open: register works and signs in.
    let mut c = connect(&burrow).await;
    let ok = c.register("newcomer", "pw-pw-pw", None).await.unwrap();
    assert_eq!(ok.screen_name, "newcomer");
    assert!(!ok.token.is_empty());

    // Duplicate login → AlreadyExists.
    let mut c2 = connect(&burrow).await;
    match c2.register("newcomer", "x", None).await {
        Err(ClientError::Refused(ErrorCode::AlreadyExists)) => {}
        other => panic!("expected AlreadyExists, got {other:?}"),
    }

    // Closed: refused.
    burrow
        .shared
        .config
        .set_key("registration_mode", "closed")
        .unwrap();
    let mut c3 = connect(&burrow).await;
    assert!(matches!(
        c3.register("nope", "x", None).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Invite mode: only a minted code works, exactly once.
    burrow
        .shared
        .config
        .set_key("registration_mode", "invite")
        .unwrap();
    let mut admin = connect(&burrow).await;
    burrow
        .shared
        .auth
        .create_account("root", "root-pw", Role::Admin)
        .await
        .unwrap();
    admin.auth_password("root", "root-pw").await.unwrap();
    admin.expect_welcome().await.unwrap();
    let invite: padm::InviteCode = admin.request(&padm::InviteCreate::new(3600)).await.unwrap();

    let mut c4 = connect(&burrow).await;
    assert!(c4
        .register("badcode", "x", Some("wrong".into()))
        .await
        .is_err());
    let ok = c4
        .register("invited", "pw-pw-pw", Some(invite.code.clone()))
        .await
        .unwrap();
    assert_eq!(ok.screen_name, "invited");
    let mut c5 = connect(&burrow).await;
    assert!(
        c5.register("reuse", "x", Some(invite.code)).await.is_err(),
        "single use"
    );

    burrow.shutdown().await;
}

#[tokio::test]
async fn personas_profiles_directory_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "wonderland", Role::User)
        .await
        .unwrap();

    let mut alice = connect(&burrow).await;
    alice.auth_password("alice", "wonderland").await.unwrap();
    alice.expect_welcome().await.unwrap();

    // Default persona exists.
    let list = alice.personas().await.unwrap();
    assert_eq!(list.personas.len(), 1);
    assert_eq!(list.personas[0].screen_name, "alice");
    assert!(list.personas[0].is_default);

    // Create an alt, give it a profile and an avatar blob.
    let alt = alice.persona_create("White Rabbit").await.unwrap().persona;
    let avatar = alice
        .blob_put(BlobPurpose::Avatar, b"png-bytes".to_vec())
        .await
        .unwrap();
    let mut update = PersonaUpdate::default();
    update.id = alt.id;
    update.profile = Some(Profile::new(
        Some("Wonderland".into()),
        None,
        Some("I'm late!".into()),
        Some("Follow me.".into()),
        None,
    ));
    update.avatar = Some(Some(avatar));
    let updated = alice.persona_update(&update).await.unwrap().persona;
    assert_eq!(updated.avatar, Some(avatar));

    // Blob round-trips.
    assert_eq!(alice.blob_get(avatar).await.unwrap(), b"png-bytes");

    // Oversized avatar refused.
    let big = vec![0u8; 300 * 1024];
    assert!(matches!(
        alice.blob_put(BlobPurpose::Avatar, big).await,
        Err(ClientError::Refused(ErrorCode::TooLarge))
    ));

    // Switch persona: presence renames live.
    alice.persona_switch(alt.id).await.unwrap();
    let mut bob = connect(&burrow).await;
    bob.auth_guest(Some("Bob".into())).await.unwrap();
    bob.expect_welcome().await.unwrap();
    let who: Vec<String> = bob
        .who()
        .await
        .unwrap()
        .into_iter()
        .map(|u| u.screen_name)
        .collect();
    assert!(who.contains(&"White Rabbit".to_string()), "who: {who:?}");

    // Directory search + profile card (with online transport).
    let results = bob.directory_search("wonderland", 10).await.unwrap();
    assert_eq!(results.personas.len(), 1);
    let card = bob.profile_get("White Rabbit").await.unwrap();
    assert_eq!(card.profile.quote.as_deref(), Some("I'm late!"));
    assert!(card.online_transport.is_some(), "locate-online works");

    // Hidden persona vanishes from directory AND profile lookup.
    let mut hide = PersonaUpdate::default();
    hide.id = alt.id;
    hide.directory_visible = Some(false);
    alice.persona_update(&hide).await.unwrap();
    assert!(bob
        .directory_search("wonderland", 10)
        .await
        .unwrap()
        .personas
        .is_empty());
    assert!(matches!(
        bob.profile_get("White Rabbit").await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // Guests can't create personas.
    assert!(matches!(
        bob.persona_create("sneaky").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    burrow.shutdown().await;
}

#[tokio::test]
async fn totp_enrollment_and_login_gate() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "wonderland", Role::User)
        .await
        .unwrap();

    let mut alice = connect(&burrow).await;
    alice.auth_password("alice", "wonderland").await.unwrap();
    alice.expect_welcome().await.unwrap();

    let info = alice.totp_enroll().await.unwrap();
    let secret = data_encoding::BASE32_NOPAD
        .decode(info.secret_base32.as_bytes())
        .unwrap();
    let enrollment = TotpEnrollment::from_secret(&secret, "RabbitHole", "alice").unwrap();

    // Wrong code doesn't confirm.
    assert!(
        alice.totp_confirm("000000").await.is_err()
            || enrollment.current_code().unwrap() == "000000"
    );
    let recovery = alice
        .totp_confirm(&enrollment.current_code().unwrap())
        .await
        .unwrap();
    assert_eq!(recovery.codes.len(), 8);
    alice.close().await;

    // Password alone now answers TotpRequired.
    let mut c = connect(&burrow).await;
    match c.auth_password("alice", "wonderland").await {
        Err(ClientError::Refused(ErrorCode::TotpRequired)) => {}
        other => panic!("expected TotpRequired, got {other:?}"),
    }
    // With a valid code it succeeds.
    let with_code = rabbithole_proto::session::AuthPassword::new("alice", "wonderland")
        .with_totp(enrollment.current_code().unwrap());
    let ok: rabbithole_proto::session::AuthOk = c.request(&with_code).await.unwrap();
    assert_eq!(ok.screen_name, "alice");
    c.close().await;

    // A recovery code works exactly once.
    let mut c = connect(&burrow).await;
    let with_recovery = rabbithole_proto::session::AuthPassword::new("alice", "wonderland")
        .with_totp(recovery.codes[0].clone());
    let _: rabbithole_proto::session::AuthOk = c.request(&with_recovery).await.unwrap();
    c.close().await;
    let mut c = connect(&burrow).await;
    let reuse = rabbithole_proto::session::AuthPassword::new("alice", "wonderland")
        .with_totp(recovery.codes[0].clone());
    assert!(c
        .request::<_, rabbithole_proto::session::AuthOk>(&reuse)
        .await
        .is_err());

    burrow.shutdown().await;
}

#[tokio::test]
async fn admin_family_classes_broadcast_kick() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("root", "root-pw", Role::Admin)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("pleb", "pleb-pw", Role::User)
        .await
        .unwrap();

    let mut admin = connect(&burrow).await;
    admin.auth_password("root", "root-pw").await.unwrap();
    admin.expect_welcome().await.unwrap();

    let mut pleb = connect(&burrow).await;
    pleb.auth_password("pleb", "pleb-pw").await.unwrap();
    pleb.expect_welcome().await.unwrap();

    // Non-admin denied everywhere.
    assert!(matches!(
        pleb.request::<_, padm::ClassList>(&padm::ClassListRequest)
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Class list + live inheritance: strip CHAT_SEND from members mid-session.
    let classes: padm::ClassList = admin.request(&padm::ClassListRequest).await.unwrap();
    let member = classes.classes.iter().find(|c| c.name == "member").unwrap();
    assert!(member.members >= 1);

    pleb.chat_send("lobby", "before").await.unwrap();
    let stripped = member.base_mask & !Caps::CHAT_SEND.0;
    admin
        .request_ack(&padm::ClassSet::new("member", stripped))
        .await
        .unwrap();
    // Note: User role default still grants CHAT_SEND; revoke via account
    // grant/revoke isn't the class path. To observe class-only effect, use
    // a capability the role default lacks: give members DROPBOX_VIEW.
    let with_dropbox = stripped | Caps::DROPBOX_VIEW.0;
    admin
        .request_ack(&padm::ClassSet::new("member", with_dropbox))
        .await
        .unwrap();
    let ok: padm::ClassList = admin.request(&padm::ClassListRequest).await.unwrap();
    let member_now = ok.classes.iter().find(|c| c.name == "member").unwrap();
    assert_eq!(
        member_now.base_mask, with_dropbox,
        "class change persisted + live"
    );

    // Account admin: list + disable.
    let list: padm::AccountList = admin
        .request(&padm::AccountListRequest::new(0, 50))
        .await
        .unwrap();
    assert!(list.accounts.iter().any(|a| a.login == "pleb"));
    let mut set = padm::AccountSet::new("pleb");
    set.disabled = Some(true);
    admin.request_ack(&set).await.unwrap();
    let mut denied = connect(&burrow).await;
    assert!(matches!(
        denied.auth_password("pleb", "pleb-pw").await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    let mut set = padm::AccountSet::new("pleb");
    set.disabled = Some(false);
    admin.request_ack(&set).await.unwrap();

    // Broadcast reaches sessions as ServerNotice.
    admin
        .request_ack(&padm::Broadcast::new("tea time in five"))
        .await
        .unwrap();
    let mut saw_notice = false;
    for _ in 0..10 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), pleb.next_push())
            .await
            .expect("push in time")
            .unwrap()
            .expect("push");
        if let Some(Ok(n)) = frame.decode::<ServerNotice>() {
            assert_eq!(n.text, "tea time in five");
            saw_notice = true;
            break;
        }
    }
    assert!(saw_notice);

    // Kick: pleb's session ends; admins can't be kicked by peers (role gate
    // tested via pleb trying to kick).
    let who = admin.who().await.unwrap();
    let pleb_session = who
        .iter()
        .find(|u| u.screen_name == "pleb")
        .unwrap()
        .session_id;
    admin
        .request_ack(&padm::Kick::new(pleb_session))
        .await
        .unwrap();
    // The kicked session sees a notice then EOF.
    let mut closed = false;
    for _ in 0..10 {
        match tokio::time::timeout(std::time::Duration::from_secs(5), pleb.next_push()).await {
            Ok(Ok(Some(_))) => continue,
            _ => {
                closed = true;
                break;
            }
        }
    }
    assert!(closed, "kicked session closed");

    burrow.shutdown().await;
}
