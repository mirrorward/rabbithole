//! Wave 8 end-to-end tests: server theme-bundle application — an admin
//! uploads/activates a bundle every client then receives (signed) via the
//! welcome/theme path, with hard validation rails and the per-account
//! disable safety valve.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    ConfigApplied, ConfigSet, ThemeBundleClear, ThemeBundleGet, ThemeBundleInfo, ThemeBundleSet,
};
use rabbithole_proto::blob::{BlobPurpose, BlobPut, BlobRef};
use rabbithole_proto::welcome::{
    ThemeBundle, ThemeChanged, ThemePrefGet, ThemePrefSet, ThemePrefState,
};
use rabbithole_proto::{Capability, CapabilitySet, ErrorCode, Hello, HelloAck};
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "W8 Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    login_with_updates(burrow, user, true).await
}

async fn login_with_updates(burrow: &Burrow, user: &str, updates: bool) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    if updates {
        // The general core client doesn't consume live theme updates. This
        // fixture explicitly offers the optional capability before auth.
        let ack: HelloAck = c
            .request(&Hello::new(
                "e2e-theme-updates",
                "0",
                CapabilitySet(vec![Capability::new(
                    rabbithole_proto::hello::caps::SERVER_THEME_UPDATES,
                )]),
            ))
            .await
            .unwrap();
        assert!(ack
            .capabilities
            .contains(rabbithole_proto::hello::caps::SERVER_THEME_UPDATES));
    }
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn start(dir: &std::path::Path) -> Burrow {
    let burrow = Burrow::start(test_config(dir)).await.unwrap();
    for (login, role) in [
        ("root", Role::Admin),
        ("alice", Role::User),
        ("bob", Role::User),
    ] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    burrow
}

/// A bundle that clears every rail: per-mode accents (the shared v1 accent
/// alone cannot pass 4.5:1 on both backgrounds), logo art, metric token.
fn good_bundle() -> ThemeBundle {
    let mut b = ThemeBundle::new("Wonderland");
    b.accent_rgb = Some([0x2b, 0x63, 0xd8]);
    b.logo_ansi = Some("== Wonderland ==".into());
    b.tokens_light = vec![("--rh-accent".into(), "#2b63d8".into())];
    b.tokens_dark = vec![("--rh-accent".into(), "#6c9cff".into())];
    b.tokens_shared = vec![("--rh-radius".into(), ".5rem".into())];
    b
}

fn encode(bundle: &ThemeBundle) -> Vec<u8> {
    postcard::to_allocvec(bundle).unwrap()
}

async fn next_theme_change(client: &mut Client) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let push = client
                .next_push()
                .await
                .unwrap()
                .expect("session stays open");
            if let Some(changed) = push.decode::<ThemeChanged>() {
                changed.unwrap();
                assert!(push.payload.0.is_empty(), "the push carries no theme bytes");
                break;
            }
        }
    })
    .await
    .expect("connected client should receive a theme invalidation");
}

async fn no_theme_change(client: &mut Client) {
    let result = tokio::time::timeout(std::time::Duration::from_millis(150), async {
        loop {
            let push = client
                .next_push()
                .await
                .unwrap()
                .expect("session stays open");
            assert!(
                push.decode::<ThemeChanged>().is_none(),
                "unrelated/refused operation must not invalidate this session's theme"
            );
        }
    })
    .await;
    assert!(
        result.is_err(),
        "session should remain open without a theme change"
    );
}

/// Already-connected clients refresh on publish and clear; the notification
/// never bypasses the account opt-out gate or substitutes unsigned theme data.
#[tokio::test]
async fn connected_clients_refresh_on_publish_and_clear() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let _: ThemePrefState = bob.request(&ThemePrefSet::new(true)).await.unwrap();
    next_theme_change(&mut bob).await;

    let _: ThemeBundleInfo = root
        .request(&ThemeBundleSet::new(encode(&good_bundle()), vec![]))
        .await
        .unwrap();
    next_theme_change(&mut alice).await;
    next_theme_change(&mut bob).await;
    // Client::theme verifies the fetched signature against HelloAck.server_key.
    let served = alice.theme().await.unwrap().expect("signed theme served");
    assert_eq!(served.name, "Wonderland");
    assert_eq!(served.tokens_light, good_bundle().tokens_light);
    assert!(
        bob.theme().await.unwrap().is_none(),
        "opt-out remains enforced"
    );

    root.request_ack(&ThemeBundleClear).await.unwrap();
    next_theme_change(&mut alice).await;
    next_theme_change(&mut bob).await;
    assert!(
        alice.theme().await.unwrap().is_none(),
        "clear takes effect live"
    );
    assert!(bob.theme().await.unwrap().is_none());
    burrow.shutdown().await;
}

/// New notifications remain silent for an older peer that did not offer the
/// capability; its existing on-demand ThemeGet continues working.
#[tokio::test]
async fn clients_without_the_capability_do_not_receive_theme_updates() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let mut old_client = login_with_updates(&burrow, "alice", false).await;
    let _: ThemeBundleInfo = root
        .request(&ThemeBundleSet::new(encode(&good_bundle()), vec![]))
        .await
        .unwrap();
    no_theme_change(&mut old_client).await;
    assert!(old_client.theme().await.unwrap().is_some());
    root.request_ack(&ThemeBundleClear).await.unwrap();
    no_theme_change(&mut old_client).await;
    assert!(old_client.theme().await.unwrap().is_none());
    burrow.shutdown().await;
}

/// A preference refresh reaches every session of that account, and nobody
/// else. A second sign-in must not retain the old theme after an opt-out.
#[tokio::test]
async fn preference_changes_only_refresh_the_same_accounts_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let _: ThemeBundleInfo = root
        .request(&ThemeBundleSet::new(encode(&good_bundle()), vec![]))
        .await
        .unwrap();
    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    let mut other_bob = login(&burrow, "bob").await;

    for disabled in [true, false] {
        let _: ThemePrefState = bob.request(&ThemePrefSet::new(disabled)).await.unwrap();
        next_theme_change(&mut bob).await;
        next_theme_change(&mut other_bob).await;
        assert_eq!(other_bob.theme().await.unwrap().is_none(), disabled);
        no_theme_change(&mut alice).await;
        assert!(alice.theme().await.unwrap().is_some());
    }
    burrow.shutdown().await;
}

/// Both operator surfaces invalidate after a successful mutation. Refused
/// bundles/config values and unrelated settings neither change nor refresh it.
#[tokio::test]
async fn ctl_and_admin_config_changes_refresh_without_signalling_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let mut alice = login(&burrow, "alice").await;
    let response = burrow::ctl::handle(
        &burrow.shared,
        &serde_json::json!({"cmd": "config-set", "key": "theme_accent", "value": "2b63d8"}),
    )
    .await;
    assert_eq!(response["ok"], true);
    next_theme_change(&mut alice).await;
    assert_eq!(
        alice.theme().await.unwrap().unwrap().accent_rgb,
        Some([0x2b, 0x63, 0xd8])
    );

    let _: ConfigApplied = root
        .request(&ConfigSet::new("theme_name", "New Wonderland"))
        .await
        .unwrap();
    next_theme_change(&mut alice).await;
    assert_eq!(alice.theme().await.unwrap().unwrap().name, "New Wonderland");

    assert!(matches!(
        alice.request_ack(&ThemeBundleClear).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&ThemeBundleSet::new(vec![255], vec![]))
            .await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));
    let refused = burrow::ctl::handle(
        &burrow.shared,
        &serde_json::json!({"cmd": "config-set", "key": "theme_accent", "value": "no-color"}),
    )
    .await;
    assert_eq!(refused["ok"], false);
    let _: ConfigApplied = root
        .request(&ConfigSet::new("motd", "unrelated"))
        .await
        .unwrap();
    no_theme_change(&mut alice).await;
    let unchanged = alice.theme().await.unwrap().unwrap();
    assert_eq!(unchanged.name, "New Wonderland");
    assert_eq!(unchanged.accent_rgb, Some([0x2b, 0x63, 0xd8]));

    let cleared =
        burrow::ctl::handle(&burrow.shared, &serde_json::json!({"cmd": "theme-clear"})).await;
    assert_eq!(cleared["ok"], true);
    next_theme_change(&mut alice).await;
    assert!(alice.theme().await.unwrap().is_none());
    burrow.shutdown().await;
}

/// Admin applies a bundle (art travels as blob refs, v1-style); a fresh
/// login fetches it, verifies the server signature, and sees the tokens.
#[tokio::test]
async fn admin_applies_bundle_and_fresh_login_sees_it() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;

    // Art rides the existing blob path first, like avatars/banners.
    let banner: BlobRef = root
        .request(&BlobPut::new(BlobPurpose::ThemeAsset, vec![0xB0; 2048]))
        .await
        .unwrap();
    let icon: BlobRef = root
        .request(&BlobPut::new(BlobPurpose::Avatar, vec![0x1C; 512]))
        .await
        .unwrap();

    let mut bundle = good_bundle();
    bundle.banner = Some(banner.id);
    bundle.icons = vec![("dm".into(), icon.id)];

    let info: ThemeBundleInfo = root
        .request(&ThemeBundleSet::new(encode(&bundle), vec![]))
        .await
        .unwrap();
    assert!(info.present);
    assert_eq!(info.name, "Wonderland");
    assert_eq!(info.applied_by, "root");
    assert!(info.applied_at_unix > 0);
    assert_ne!(info.id, [0u8; 32]);
    assert_eq!(info.accent_rgb, Some([0x2b, 0x63, 0xd8]));
    assert!(info.has_logo && info.has_banner);
    assert_eq!((info.icons, info.tokens_light), (1, 1));
    assert_eq!((info.tokens_dark, info.tokens_shared), (1, 1));

    // A fresh login sees the themed welcome bundle — signature verified
    // client-side against the server key from HelloAck.
    let mut alice = login(&burrow, "alice").await;
    let served = alice.theme().await.unwrap().expect("signed theme served");
    assert_eq!(served.name, "Wonderland");
    assert_eq!(served.accent_rgb, Some([0x2b, 0x63, 0xd8]));
    assert_eq!(served.logo_ansi.as_deref(), Some("== Wonderland =="));
    assert_eq!(served.banner, Some(banner.id));
    assert_eq!(served.icons, vec![("dm".to_string(), icon.id)]);
    assert_eq!(
        served.tokens_light,
        vec![("--rh-accent".to_string(), "#2b63d8".to_string())]
    );
    assert_eq!(
        served.tokens_dark,
        vec![("--rh-accent".to_string(), "#6c9cff".to_string())]
    );
    assert_eq!(
        served.tokens_shared,
        vec![("--rh-radius".to_string(), ".5rem".to_string())]
    );

    // ThemeBundleGet reports the same content id; ctl theme-status agrees.
    let again: ThemeBundleInfo = root.request(&ThemeBundleGet).await.unwrap();
    assert_eq!(again.id, info.id);
    let status =
        burrow::ctl::handle(&burrow.shared, &serde_json::json!({"cmd": "theme-status"})).await;
    assert_eq!(status["ok"], true);
    assert_eq!(status["data"]["present"], true);
    assert_eq!(status["data"]["id"], hex::encode(info.id));
    assert_eq!(status["data"]["name"], "Wonderland");
    assert_eq!(status["data"]["icons"], 1);

    burrow.shutdown().await;
}

/// Every theme admin op is capability-gated: plain users are refused.
#[tokio::test]
async fn non_admin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut alice = login(&burrow, "alice").await;

    assert!(matches!(
        alice
            .request::<_, ThemeBundleInfo>(&ThemeBundleSet::new(encode(&good_bundle()), vec![]))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice.request_ack(&ThemeBundleClear).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    assert!(matches!(
        alice.request::<_, ThemeBundleInfo>(&ThemeBundleGet).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    // Nothing was applied.
    assert!(alice.theme().await.unwrap().is_none());

    burrow.shutdown().await;
}

/// The safety rails reject bad bundles at the door — nothing hot-applies.
#[tokio::test]
async fn invalid_bundles_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;

    let set = |bundle: &ThemeBundle| ThemeBundleSet::new(encode(bundle), vec![]);

    // A single shared accent can't clear 4.5:1 on both modes: rejected
    // (the computed ratio lands in the audit log server-side).
    let mut low = ThemeBundle::new("Blaze");
    low.accent_rgb = Some([0xff, 0x88, 0x00]);
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&set(&low)).await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    // Free-form CSS through the texture token: refused.
    let mut inject = good_bundle();
    inject.tokens_dark.push((
        "--rh-bg-image".into(),
        "url(https://evil.example/x)}body{display:none".into(),
    ));
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&set(&inject)).await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    // Unknown tokens and malformed colours: refused.
    let mut unknown = good_bundle();
    unknown
        .tokens_shared
        .push(("--rh-font-sans".into(), "Comic Sans MS".into()));
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&set(&unknown)).await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));
    let mut badhex = good_bundle();
    badhex.tokens_light = vec![("--rh-accent".into(), "blue".into())];
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&set(&badhex)).await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    // Oversized logo art: refused as too large.
    let mut fat = good_bundle();
    fat.logo_ansi = Some("x".repeat(64 * 1024 + 1));
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&set(&fat)).await,
        Err(ClientError::Refused(ErrorCode::TooLarge))
    ));

    // A banner blob that was never uploaded: refused as not found.
    let mut ghost = good_bundle();
    ghost.banner = Some([9u8; 32]);
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&set(&ghost)).await,
        Err(ClientError::Refused(ErrorCode::NotFound))
    ));

    // Garbage bytes: refused, never fatal.
    assert!(matches!(
        root.request::<_, ThemeBundleInfo>(&ThemeBundleSet::new(vec![1, 2, 3], vec![]))
            .await,
        Err(ClientError::Refused(ErrorCode::BadRequest))
    ));

    // After all those refusals, no theme is applied.
    let info: ThemeBundleInfo = root.request(&ThemeBundleGet).await.unwrap();
    assert!(!info.present);
    let mut alice = login(&burrow, "alice").await;
    assert!(alice.theme().await.unwrap().is_none());

    burrow.shutdown().await;
}

/// Clearing the bundle restores defaults for everyone.
#[tokio::test]
async fn clear_restores_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let mut alice = login(&burrow, "alice").await;

    let _: ThemeBundleInfo = root
        .request(&ThemeBundleSet::new(encode(&good_bundle()), vec![]))
        .await
        .unwrap();
    assert!(alice.theme().await.unwrap().is_some());

    root.request_ack(&ThemeBundleClear).await.unwrap();
    assert!(alice.theme().await.unwrap().is_none(), "defaults are back");
    let info: ThemeBundleInfo = root.request(&ThemeBundleGet).await.unwrap();
    assert!(!info.present);
    let status =
        burrow::ctl::handle(&burrow.shared, &serde_json::json!({"cmd": "theme-status"})).await;
    assert_eq!(status["data"]["present"], false);

    burrow.shutdown().await;
}

/// The user safety valve: an account with the disable preference set gets
/// default tokens while everyone else stays themed.
#[tokio::test]
async fn disabled_pref_user_gets_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let _: ThemeBundleInfo = root
        .request(&ThemeBundleSet::new(encode(&good_bundle()), vec![]))
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let mut bob = login(&burrow, "bob").await;
    assert!(bob.theme().await.unwrap().is_some(), "themed by default");
    let pref: ThemePrefState = bob.request(&ThemePrefGet).await.unwrap();
    assert!(!pref.disable_server_theme);

    // Bob opts out; only bob is affected, and the pref reads back.
    let pref: ThemePrefState = bob.request(&ThemePrefSet::new(true)).await.unwrap();
    assert!(pref.disable_server_theme);
    assert!(bob.theme().await.unwrap().is_none(), "safety valve engaged");
    assert!(alice.theme().await.unwrap().is_some(), "alice unaffected");
    let pref: ThemePrefState = bob.request(&ThemePrefGet).await.unwrap();
    assert!(pref.disable_server_theme);

    // Opting back in restores the served theme.
    let pref: ThemePrefState = bob.request(&ThemePrefSet::new(false)).await.unwrap();
    assert!(!pref.disable_server_theme);
    assert!(bob.theme().await.unwrap().is_some());

    // Guests have no stored preference: the pref surface refuses them.
    let mut guest = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    guest.auth_guest(Some("visitor".into())).await.unwrap();
    guest.expect_welcome().await.unwrap();
    assert!(matches!(
        guest
            .request::<_, ThemePrefState>(&ThemePrefSet::new(true))
            .await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    // …but guests do receive the server theme.
    assert!(guest.theme().await.unwrap().is_some());

    burrow.shutdown().await;
}
