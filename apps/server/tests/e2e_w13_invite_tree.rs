//! Wave 13 end-to-end test: invite lineage surfaced through the `invite-tree`
//! ctl command. The tree logic + register wiring are unit-tested in
//! `rabbithole-server-core`; here we prove the operator-facing glue: a
//! multi-level downline reported breadth-first with correct depths and counts.

use burrow::Burrow;
use rabbithole_server_core::{RegistrationMode, Role, ServerConfig};
use rabbithole_store_server::repo::AccountsRepo;
use rabbithole_store_server::repo2::InvitesRepo;
use serde_json::json;

const PW: &str = "correct-horse-battery-staple";

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Invite Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn invite_tree_ctl_reports_the_downline() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    let shared = &burrow.shared;

    // Alice invites Bob; Bob invites Carol — a two-level tree.
    let alice = shared
        .auth
        .create_account("alice", PW, Role::User)
        .await
        .unwrap();
    InvitesRepo(&shared.pool)
        .create("code-bob", alice.id, 3600)
        .await
        .unwrap();
    shared
        .auth
        .register(RegistrationMode::Invite, "bob", PW, Some("code-bob"))
        .await
        .unwrap();

    let bob = AccountsRepo(&shared.pool)
        .by_login("bob")
        .await
        .unwrap()
        .unwrap();
    InvitesRepo(&shared.pool)
        .create("code-carol", bob.id, 3600)
        .await
        .unwrap();
    shared
        .auth
        .register(RegistrationMode::Invite, "carol", PW, Some("code-carol"))
        .await
        .unwrap();

    // The tree rooted at Alice: alice(0) → bob(1) → carol(2).
    let r = burrow::ctl::handle(shared, &json!({"cmd": "invite-tree", "login": "alice"})).await;
    assert_eq!(r["ok"], true, "{r}");
    let d = &r["data"];
    assert_eq!(d["count"], 3, "{d}");
    assert_eq!(d["invited"], 2, "the two accounts below alice");

    let tree = d["tree"].as_array().unwrap();
    let depth = |login: &str| {
        tree.iter()
            .find(|n| n["login"] == login)
            .unwrap_or_else(|| panic!("{login} missing: {d}"))["depth"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(depth("alice"), 0);
    assert_eq!(depth("bob"), 1);
    assert_eq!(depth("carol"), 2);
    // Breadth-first: the root leads.
    assert_eq!(tree[0]["login"], "alice");

    // A leaf reports just itself.
    let leaf = burrow::ctl::handle(shared, &json!({"cmd": "invite-tree", "login": "carol"})).await;
    assert_eq!(leaf["data"]["count"], 1, "{leaf}");
    assert_eq!(leaf["data"]["invited"], 0);

    // An unknown login is an empty tree, not an error.
    let missing =
        burrow::ctl::handle(shared, &json!({"cmd": "invite-tree", "login": "nobody"})).await;
    assert_eq!(missing["ok"], true, "{missing}");
    assert_eq!(missing["data"]["count"], 0);
}
