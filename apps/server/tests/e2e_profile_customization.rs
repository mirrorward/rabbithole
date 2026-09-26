//! The profile editor's public-icon flow, through real WebSocket sessions:
//! upload -> active-persona update -> another person's profile/blob reads.
//! No profile or icon change is broadcast to another burrow or persona.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::blob::BlobPurpose;
use rabbithole_proto::persona::{PersonaUpdate, Profile};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};

fn config(path: &std::path::Path) -> ServerConfig {
    ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: path.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn connect(burrow: &Burrow) -> Client {
    Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "profile-customization-test",
        "0",
    )
    .await
    .unwrap()
}

async fn sign_in(burrow: &Burrow) -> Client {
    let mut client = connect(burrow).await;
    client.auth_password("alice", "wonderland").await.unwrap();
    client.expect_welcome().await.unwrap();
    client
}

#[tokio::test]
async fn public_icon_and_profile_persist_only_on_the_chosen_burrow_and_persona() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = Burrow::start(config(first_dir.path())).await.unwrap();
    let second = Burrow::start(config(second_dir.path())).await.unwrap();
    for burrow in [&first, &second] {
        burrow
            .shared
            .auth
            .create_account("alice", "wonderland", Role::User)
            .await
            .unwrap();
    }

    let mut alice = sign_in(&first).await;
    let mut elsewhere = sign_in(&second).await;
    let alternate = alice.persona_create("Night Owl").await.unwrap().persona;
    alice.persona_switch(alternate.id).await.unwrap();
    let list = alice.personas().await.unwrap();
    let active = list
        .personas
        .iter()
        .find(|p| p.id == list.active_id)
        .unwrap();
    assert_eq!(active.id, alternate.id);

    // A real checked-in PNG; UI tests separately verify the generated
    // sprite's PNG pixels, content hash and upload request encoding.
    let png = include_bytes!("../../../crates/ui-web/assets/icon-192.png").to_vec();
    let avatar = alice
        .blob_put(BlobPurpose::Avatar, png.clone())
        .await
        .unwrap();
    assert_eq!(avatar, *blake3::hash(&png).as_bytes());
    let mut update = PersonaUpdate::default();
    update.id = active.id;
    update.profile = Some(Profile::new(
        Some("Seattle".into()),
        Some("ANSI art and night radio".into()),
        Some("One more track.".into()),
        Some("Drawing a new icon pack.".into()),
        Some("they/them".into()),
    ));
    update.avatar = Some(Some(avatar));
    update.directory_visible = Some(true);
    let saved = alice.persona_update(&update).await.unwrap().persona;
    assert_eq!(saved.id, active.id);
    assert_eq!(saved.avatar, Some(avatar));

    let mut observer = connect(&first).await;
    observer.auth_guest(Some("observer".into())).await.unwrap();
    observer.expect_welcome().await.unwrap();
    let visible = observer.profile_get("Night Owl").await.unwrap();
    assert_eq!(visible.profile, update.profile.clone().unwrap());
    assert_eq!(visible.avatar, Some(avatar));
    assert_eq!(observer.blob_get(avatar).await.unwrap(), png);
    assert!(matches!(
        observer.persona_update(&update).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));

    // Another persona on this burrow and the same account on another
    // burrow keep their own public appearance and profile.
    let original = alice.profile_get("alice").await.unwrap();
    assert_eq!(original.profile, Profile::default());
    assert_eq!(original.avatar, None);
    let other = elsewhere.profile_get("alice").await.unwrap();
    assert_eq!(other.profile, Profile::default());
    assert_eq!(other.avatar, None);
    assert!(matches!(
        elsewhere.blob_get(avatar).await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // A server restart retains the public profile and uploaded bytes.
    drop(alice);
    drop(observer);
    first.shutdown().await;
    let restarted = Burrow::start(config(first_dir.path())).await.unwrap();
    let mut alice = sign_in(&restarted).await;
    let after_restart = alice.profile_get("Night Owl").await.unwrap();
    assert_eq!(after_restart.avatar, Some(avatar));
    assert_eq!(after_restart.profile.pronouns.as_deref(), Some("they/them"));
    assert_eq!(alice.blob_get(avatar).await.unwrap(), png);

    // Clear fields and remove the icon explicitly, then hide the profile.
    let mut clear = PersonaUpdate::default();
    clear.id = alternate.id;
    clear.profile = Some(Profile::new(None, None, Some(String::new()), None, None));
    clear.avatar = Some(None);
    let cleared = alice.persona_update(&clear).await.unwrap().persona;
    assert_eq!(cleared.avatar, None);
    assert_eq!(cleared.profile.quote.as_deref(), Some(""));
    assert_eq!(cleared.profile.location.as_deref(), Some("Seattle"));
    clear.directory_visible = Some(false);
    alice.persona_update(&clear).await.unwrap();
    assert!(matches!(
        alice.profile_get("Night Owl").await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    restarted.shutdown().await;
    second.shutdown().await;
}
