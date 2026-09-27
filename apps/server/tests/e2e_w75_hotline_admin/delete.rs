//! RH-35 permanent removal over the real Hotline socket, with native and
//! telnet sessions proving that deletion uses the shared sign-out path.

use super::*;
use rabbithole_core::Client as NativeClient;
use rabbithole_proto::board::{BoardCreate, PostCreate};
use rabbithole_server_core::events::SignedEvent;
use rabbithole_server_core::Caps;
use rabbithole_store_server::repo::{AccountsRepo, AuditRepo, ClassesRepo, SessionsRepo};
use rabbithole_store_server::repo2::{InvitesRepo, KeysRepo, PersonasRepo, TotpRepo};
use rabbithole_store_server::repo4::PostsRepo;

const PASS: &str = "hotline-delete-fixture-password";

async fn start(dir: &std::path::Path) -> Burrow {
    let mut config = test_config(dir);
    config.telnet_enabled = true;
    config.telnet_addr = "127.0.0.1:0".parse().unwrap();
    config.ratelimit_enabled = false;
    Burrow::start(config).await.unwrap()
}

async fn hotline(b: &Burrow, login: &str) -> Client {
    let mut c = Client::connect(b.hotline_addr.unwrap()).await;
    assert_eq!(c.login(login, PASS, login).await.header.error, 0);
    c
}

async fn native(b: &Burrow, login: &str) -> (NativeClient, String) {
    let mut c = NativeClient::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "delete-test",
        "0",
    )
    .await
    .unwrap();
    let ok = c.auth_password(login, PASS).await.unwrap();
    c.expect_welcome().await.unwrap();
    (c, ok.token)
}

async fn delete(c: &mut Client, login: &str) -> Transaction {
    c.roundtrip(
        transaction::DELETE_USER,
        vec![Field::credential(field::USER_LOGIN, login)],
    )
    .await
}

async fn expect_text(socket: &mut TcpStream, text: &[u8]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut seen = Vec::new();
        let mut buf = [0; 4096];
        while !seen.windows(text.len()).any(|part| part == text) {
            let n = socket.read(&mut buf).await.unwrap();
            assert_ne!(
                n,
                0,
                "closed waiting for {:?}",
                String::from_utf8_lossy(text)
            );
            seen.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn permanent_delete_removes_credentials_and_all_live_surfaces_but_keeps_signed_history() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    let admin = b
        .shared
        .auth
        .create_account("operator", PASS, Role::Admin)
        .await
        .unwrap();
    let victim = b
        .shared
        .auth
        .create_account("alice", PASS, Role::User)
        .await
        .unwrap();
    let persona = PersonasRepo(&b.shared.pool)
        .create(victim.id, "OldAlice", false)
        .await
        .unwrap();
    let mut operator = hotline(&b, "operator").await;
    let mut legacy = hotline(&b, "alice").await;
    let (mut native_victim, token) = native(&b, "alice").await;
    let (mut native_admin, _) = native(&b, "operator").await;
    native_admin
        .request_ack(&BoardCreate::new("general", "General", 2))
        .await
        .unwrap();
    native_victim
        .request_ack(&PostCreate::new("general", "History", "This byline stays"))
        .await
        .unwrap();
    let before = PostsRepo(&b.shared.pool)
        .threads("general", 10)
        .await
        .unwrap()
        .remove(0)
        .0;
    let signed: SignedEvent = postcard::from_bytes(&before.event_blob).unwrap();
    signed.verify(&b.shared.server_key).unwrap();

    let mut telnet = TcpStream::connect(b.telnet_addr.unwrap()).await.unwrap();
    expect_text(&mut telnet, b"login: ").await;
    telnet.write_all(b"alice\r\n").await.unwrap();
    expect_text(&mut telnet, b"password: ").await;
    telnet
        .write_all(format!("{PASS}\r\n").as_bytes())
        .await
        .unwrap();
    expect_text(&mut telnet, b"Command: ").await;
    assert_eq!(
        b.shared
            .presence
            .snapshot()
            .iter()
            .filter(|p| p.account_id == victim.id)
            .count(),
        3
    );

    // Additional credentials and unused invitations must leave with the row.
    TotpRepo(&b.shared.pool)
        .begin(victim.id, b"fixture secret")
        .await
        .unwrap();
    KeysRepo(&b.shared.pool)
        .add(victim.id, &[42; 32])
        .await
        .unwrap();
    InvitesRepo(&b.shared.pool)
        .create("alice-unused", victim.id, 3600)
        .await
        .unwrap();
    assert_eq!(delete(&mut operator, "ALICE").await.header.error, 0);
    assert!(AccountsRepo(&b.shared.pool)
        .by_id(victim.id)
        .await
        .unwrap()
        .is_none());
    assert!(PersonasRepo(&b.shared.pool)
        .by_id(persona.id)
        .await
        .unwrap()
        .is_none());
    assert!(PersonasRepo(&b.shared.pool)
        .for_account(victim.id)
        .await
        .unwrap()
        .is_empty());
    assert!(TotpRepo(&b.shared.pool)
        .get(victim.id)
        .await
        .unwrap()
        .is_none());
    assert!(KeysRepo(&b.shared.pool)
        .for_account(victim.id)
        .await
        .unwrap()
        .is_empty());
    assert!(!InvitesRepo(&b.shared.pool)
        .list(100)
        .await
        .unwrap()
        .iter()
        .any(|(code, _, _, _)| code == "alice-unused"));
    assert_eq!(
        SessionsRepo(&b.shared.pool)
            .revoke_account(victim.id)
            .await
            .unwrap(),
        0
    );
    assert!(b
        .shared
        .auth
        .login_password("alice", PASS, None)
        .await
        .is_err());
    assert!(b.shared.auth.login_resume(&token).await.is_err());

    let notice = legacy.read_until(transaction::DISCONNECT_MSG).await;
    assert_eq!(
        field_text(&notice, field::DATA).as_deref(),
        Some("account removed")
    );
    legacy.expect_closed().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while matches!(native_victim.next_push().await, Ok(Some(_))) {}
    })
    .await
    .expect("native session closes");
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), telnet.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&tail).contains("account removed"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while b
            .shared
            .presence
            .snapshot()
            .iter()
            .any(|p| p.account_id == victim.id)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let after = PostsRepo(&b.shared.pool)
        .by_id(&before.event_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.event_blob, before.event_blob,
        "signed attribution is unchanged"
    );
    assert_eq!(after.author, before.author);
    assert_eq!(after.body, "This byline stays");
    for retired in ["ALICE", "oldalice"] {
        assert!(AccountsRepo(&b.shared.pool)
            .name_is_retired(retired)
            .await
            .unwrap());
        assert!(b
            .shared
            .auth
            .create_account(retired, PASS, Role::User)
            .await
            .is_err());
        assert!(native_admin.persona_create(retired).await.is_err());
    }
    assert_ne!(
        delete(&mut operator, "alice").await.header.error,
        0,
        "repeat delete reports absence"
    );
    let audit = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let entries = AuditRepo(&b.shared.pool).recent(50).await.unwrap();
            if let Some(entry) = entries.into_iter().find(|e| e.action == "account-delete") {
                break entry;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(audit.actor, admin.login);
    assert_eq!(audit.detail, "alice via=hotline permanent");
    operator.close().await;
    b.shutdown().await;
}

#[tokio::test]
async fn delete_protects_self_and_role_ordering_and_can_remove_disabled_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    for (name, role) in [
        ("root", Role::Superuser),
        ("second-root", Role::Superuser),
        ("admin", Role::Admin),
        ("peer", Role::Admin),
        ("disabled", Role::User),
    ] {
        b.shared
            .auth
            .create_account(name, PASS, role)
            .await
            .unwrap();
    }
    let mut admin = hotline(&b, "admin").await;
    let mut root = hotline(&b, "root").await;
    let (mut peer, token) = native(&b, "peer").await;
    for target in ["admin", "peer", "root", "second-root", "missing"] {
        assert_ne!(
            delete(&mut admin, target).await.header.error,
            0,
            "refuse {target}"
        );
    }
    assert_ne!(
        delete(&mut root, "root").await.header.error,
        0,
        "superusers cannot remove themselves"
    );
    assert!(
        peer.who().await.is_ok(),
        "refused delete leaves the target connected"
    );
    assert!(
        b.shared.auth.login_resume(&token).await.is_ok(),
        "refused delete leaves saved credentials intact"
    );
    // A superuser may remove another superuser while an enabled keeper remains.
    assert_eq!(delete(&mut root, "second-root").await.header.error, 0);
    AccountsRepo(&b.shared.pool)
        .admin_set("disabled", None, None, Some(true))
        .await
        .unwrap();
    assert_eq!(
        delete(&mut admin, "disabled").await.header.error,
        0,
        "old soft-deleted rows can be removed"
    );
    assert!(AccountsRepo(&b.shared.pool)
        .by_login("disabled")
        .await
        .unwrap()
        .is_none());
    assert!(AccountsRepo(&b.shared.pool)
        .by_login("root")
        .await
        .unwrap()
        .is_some());
    admin.close().await;
    root.close().await;
    b.shutdown().await;
}

#[tokio::test]
async fn delete_checks_live_caller_standing_enabled_state_and_class() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .auth
        .create_account("operator", PASS, Role::Admin)
        .await
        .unwrap();
    let victim = b
        .shared
        .auth
        .create_account("victim", PASS, Role::User)
        .await
        .unwrap();
    let mut operator = hotline(&b, "operator").await;
    let (mut victim_client, token) = native(&b, "victim").await;
    let accounts = AccountsRepo(&b.shared.pool);
    // Do not publish a Kick here: exercise the still-open stale session.
    accounts
        .admin_set("operator", Some(Role::User as u8), Some(None), None)
        .await
        .unwrap();
    assert_ne!(
        delete(&mut operator, "victim").await.header.error,
        0,
        "demoted caller"
    );
    accounts
        .admin_set("operator", Some(Role::Admin as u8), None, Some(true))
        .await
        .unwrap();
    assert_ne!(
        delete(&mut operator, "victim").await.header.error,
        0,
        "disabled caller"
    );
    let class = ClassesRepo(&b.shared.pool)
        .upsert("account-operator", Caps::ACCOUNT_ADMIN.0)
        .await
        .unwrap();
    accounts
        .admin_set(
            "operator",
            Some(Role::Moderator as u8),
            Some(Some(class)),
            Some(false),
        )
        .await
        .unwrap();
    // The previous login-time Admin role cannot override the live class revoke.
    ClassesRepo(&b.shared.pool)
        .set_mask("account-operator", 0)
        .await
        .unwrap();
    assert_ne!(
        delete(&mut operator, "victim").await.header.error,
        0,
        "class permission removed"
    );
    assert!(accounts.by_id(victim.id).await.unwrap().is_some());
    assert!(victim_client.who().await.is_ok());
    assert!(b.shared.auth.login_resume(&token).await.is_ok());
    ClassesRepo(&b.shared.pool)
        .set_mask("account-operator", Caps::ACCOUNT_ADMIN.0)
        .await
        .unwrap();
    assert_eq!(
        delete(&mut operator, "victim").await.header.error,
        0,
        "fresh class grants apply"
    );
    operator.close().await;
    b.shutdown().await;
}
