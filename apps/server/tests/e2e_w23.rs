//! Wave 2.3 end-to-end tests: welcome screen composition, signed theme
//! bundle (verify + contrast rail), keyword teleport.

use burrow::Burrow;
use rabbithole_core::theme::{self, Mode, ThemePack};
use rabbithole_core::Client;
use rabbithole_proto::welcome::{KeywordKind, WelcomeWidget};
use rabbithole_server_core::{Role, ServerConfig};

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "W23 Warren".into(),
        motd: "mind the gap".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let mut c = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    c.auth_password(user, "pw-pw-pw").await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

#[tokio::test]
async fn welcome_screen_composition() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .config
        .set_key("welcome_featured", "Grand Opening\nCome one, come all")
        .unwrap();
    burrow
        .shared
        .config
        .set_key("welcome_ticker", "now serving unbirthdays")
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;
    let _bob = login(&burrow, "bob").await;

    let screen = alice.welcome_screen().await.unwrap();
    let has_motd = screen
        .widgets
        .iter()
        .any(|w| matches!(w, WelcomeWidget::Motd(m) if m == "mind the gap"));
    let online = screen.widgets.iter().find_map(|w| match w {
        WelcomeWidget::OnlineNow { count, sample } => Some((*count, sample.clone())),
        _ => None,
    });
    let has_featured = screen
        .widgets
        .iter()
        .any(|w| matches!(w, WelcomeWidget::Featured { title, .. } if title == "Grand Opening"));
    let has_ticker = screen
        .widgets
        .iter()
        .any(|w| matches!(w, WelcomeWidget::Ticker(t) if t == "now serving unbirthdays"));

    assert!(has_motd);
    assert!(has_featured);
    assert!(has_ticker);
    let (count, sample) = online.expect("online widget present");
    assert_eq!(count, 2);
    assert!(sample.contains(&"alice".to_string()) && sample.contains(&"bob".to_string()));

    burrow.shutdown().await;
}

#[tokio::test]
async fn theme_bundle_signed_and_contrast_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;

    // No theme configured yet → None.
    assert!(alice.theme().await.unwrap().is_none());

    // Configure a bright accent + logo; client verifies the signature.
    burrow
        .shared
        .config
        .set_key("theme_accent", "ff8800")
        .unwrap();
    burrow
        .shared
        .config
        .set_key("theme_logo_ansi", "== W23 ==")
        .unwrap();
    let bundle = alice.theme().await.unwrap().expect("signed theme");
    assert_eq!(bundle.accent_rgb, Some([0xff, 0x88, 0x00]));
    assert_eq!(bundle.logo_ansi.as_deref(), Some("== W23 =="));

    // The client folds it into a dark palette — bright accent accepted.
    let pal = theme::resolve(ThemePack::Clean, Mode::Dark, Some(&bundle));
    assert_eq!(pal.accent, theme::Rgb(0xff, 0x88, 0x00));

    // A near-black accent is rejected by the contrast rail.
    burrow
        .shared
        .config
        .set_key("theme_accent", "0a0a0c")
        .unwrap();
    let dark_bundle = alice.theme().await.unwrap().unwrap();
    let pal2 = theme::resolve(ThemePack::Clean, Mode::Dark, Some(&dark_bundle));
    let builtin = theme::Palette::builtin(ThemePack::Clean, Mode::Dark);
    assert_eq!(pal2.accent, builtin.accent, "unreadable accent rejected");

    burrow.shutdown().await;
}

#[tokio::test]
async fn keyword_teleport() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(dir.path())).await.unwrap();
    burrow.shared.config.set_key("theme_accent", "").unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();

    let mut alice = login(&burrow, "alice").await;

    // Room match (the lobby always exists).
    let t = alice.keyword_go("lobby").await.unwrap();
    assert_eq!(t.kind, KeywordKind::Room);
    assert!(t.target.eq_ignore_ascii_case("lobby"));

    // User match (alice's own persona).
    let t = alice.keyword_go("alice").await.unwrap();
    assert_eq!(t.kind, KeywordKind::User);

    // Unknown falls through, echoing the query.
    let t = alice.keyword_go("nonesuch-place").await.unwrap();
    assert_eq!(t.kind, KeywordKind::Unknown);
    assert_eq!(t.target, "nonesuch-place");

    burrow.shutdown().await;
}
