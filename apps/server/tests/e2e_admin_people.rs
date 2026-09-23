//! People, as an operator manages them over the wire: who may do what to
//! whom, and that a password reset or a disable really puts someone out.

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_proto::admin::{
    AccountCreate, AccountList, AccountListRequest, AccountPasswordSet, AccountSet,
    AccountTotpReset, AuditList, AuditListRequest, ClassSet, InviteCode, InviteCreate, InviteList,
    InviteListRequest, InviteRevoke,
};
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Caps, Role, ServerConfig};
use rabbithole_store_server::repo::{AccountsRepo, AuditRepo};
use rabbithole_store_server::repo2::TotpRepo;

const PW: &str = "pw-pw-pw-pw";

async fn start(dir: &std::path::Path) -> Burrow {
    let config = ServerConfig {
        name: "People Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        registration_mode: "invite".into(),
        ..ServerConfig::default()
    };
    let burrow = Burrow::start(config).await.unwrap();
    for (login, role) in [
        ("root", Role::Superuser),
        ("ada", Role::Admin),
        ("bea", Role::Admin),
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

async fn connect(burrow: &Burrow) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    Client::connect(&url, None, None, "e2e", "0").await.unwrap()
}

async fn login_with(burrow: &Burrow, user: &str, password: &str) -> Result<Client, ClientError> {
    let mut c = connect(burrow).await;
    c.auth_password(user, password).await?;
    c.expect_welcome().await?;
    Ok(c)
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    login_with(burrow, user, PW).await.unwrap()
}

fn refused<T: std::fmt::Debug>(r: Result<T, ClientError>, code: ErrorCode) {
    match r {
        Err(ClientError::Refused(got)) if got == code => {}
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

async fn role_of(burrow: &Burrow, login: &str) -> u8 {
    AccountsRepo(&burrow.shared.pool)
        .by_login(login)
        .await
        .unwrap()
        .unwrap()
        .role
}

#[tokio::test]
async fn an_operator_makes_an_account_within_their_own_standing() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;

    ada.request_ack(&AccountCreate::new(
        "carol",
        "carols-password",
        Role::User as u8,
    ))
    .await
    .unwrap();
    // The account is real: it signs in, with the role it was given.
    let carol = login_with(&burrow, "carol", "carols-password").await;
    assert!(carol.is_ok(), "{:?}", carol.err());
    assert_eq!(role_of(&burrow, "carol").await, Role::User as u8);

    // A co-admin is within an admin's gift. A superuser is not.
    ada.request_ack(&AccountCreate::new(
        "dan",
        "dans-password",
        Role::Admin as u8,
    ))
    .await
    .unwrap();
    refused(
        ada.request_ack(&AccountCreate::new(
            "eve",
            "eves-password",
            Role::Superuser as u8,
        ))
        .await,
        ErrorCode::Forbidden,
    );
    // Taken, weak, malformed, or not a role at all.
    refused(
        ada.request_ack(&AccountCreate::new("carol", "another-password", 1))
            .await,
        ErrorCode::AlreadyExists,
    );
    refused(
        ada.request_ack(&AccountCreate::new("frank", "short", 1))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        ada.request_ack(&AccountCreate::new("has space", "good-password", 1))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        ada.request_ack(&AccountCreate::new("grace", "good-password", 9))
            .await,
        ErrorCode::BadRequest,
    );
    // A member makes nobody.
    let mut alice = login(&burrow, "alice").await;
    refused(
        alice
            .request_ack(&AccountCreate::new("mallory", "good-password", 1))
            .await,
        ErrorCode::Forbidden,
    );

    // No password ever reaches the audit log.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let audit = AuditRepo(&burrow.shared.pool).recent(50).await.unwrap();
    assert!(audit
        .iter()
        .any(|a| a.action == "account-create" && a.detail.starts_with("carol ")));
    assert!(
        audit.iter().all(|a| !a.detail.contains("password")),
        "{audit:?}"
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn nobody_promotes_past_themselves_or_touches_a_peer_or_themselves() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;

    let set_role = |login: &str, role: Role| {
        let mut set = AccountSet::new(login);
        set.role = Some(role as u8);
        set
    };
    let disable = |login: &str| {
        let mut set = AccountSet::new(login);
        set.disabled = Some(true);
        set
    };

    // The hole this closes: an account admin making a superuser.
    refused(
        ada.request_ack(&set_role("alice", Role::Superuser)).await,
        ErrorCode::Forbidden,
    );
    refused(
        ada.request_ack(&set_role("ada", Role::Superuser)).await,
        ErrorCode::Forbidden,
    );
    assert_eq!(role_of(&burrow, "alice").await, Role::User as u8);
    assert_eq!(role_of(&burrow, "ada").await, Role::Admin as u8);

    // Below herself: fine, up to her own role.
    ada.request_ack(&set_role("alice", Role::Moderator))
        .await
        .unwrap();
    assert_eq!(role_of(&burrow, "alice").await, Role::Moderator as u8);

    // A peer, someone above, herself, a ghost, a role that is not one.
    refused(ada.request_ack(&disable("bea")).await, ErrorCode::Forbidden);
    refused(
        ada.request_ack(&disable("root")).await,
        ErrorCode::Forbidden,
    );
    refused(ada.request_ack(&disable("ada")).await, ErrorCode::Forbidden);
    refused(
        ada.request_ack(&disable("nobody")).await,
        ErrorCode::NotFound,
    );
    let mut bad = AccountSet::new("alice");
    bad.role = Some(77);
    refused(ada.request_ack(&bad).await, ErrorCode::BadRequest);

    // The superuser is exempt from the ordering, and from nothing else.
    let mut root = login(&burrow, "root").await;
    root.request_ack(&set_role("bea", Role::User))
        .await
        .unwrap();
    assert_eq!(role_of(&burrow, "bea").await, Role::User as u8);
    refused(
        root.request_ack(&disable("root")).await,
        ErrorCode::Forbidden,
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn nobody_grants_a_capability_they_do_not_hold() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;

    let held = Role::Admin.default_caps().0;
    let not_held = !held & (1 << 63);
    assert_ne!(not_held, 0, "the test needs a bit an admin lacks");

    // Within what she holds: fine.
    ada.request_ack(&ClassSet::new(
        "helpers",
        Caps::CHAT_READ.0 | Caps::CHAT_SEND.0,
    ))
    .await
    .unwrap();
    // One bit beyond it: refused, whichever class it is aimed at.
    refused(
        ada.request_ack(&ClassSet::new("helpers", Caps::CHAT_READ.0 | not_held))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        ada.request_ack(&ClassSet::new("admin", u64::MAX)).await,
        ErrorCode::Forbidden,
    );
    // A superuser holds everything.
    let mut root = login(&burrow, "root").await;
    root.request_ack(&ClassSet::new("helpers", u64::MAX))
        .await
        .unwrap();
    burrow.shutdown().await;
}

#[tokio::test]
async fn a_new_password_puts_the_old_one_out_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;

    // Alice is signed in, and holds a token that would resume her.
    let mut alice = connect(&burrow).await;
    let ok = alice.auth_password("alice", PW).await.unwrap();
    alice.expect_welcome().await.unwrap();
    let token = ok.token.clone();
    assert!(!token.is_empty());

    let mut ada = login(&burrow, "ada").await;
    refused(
        ada.request_ack(&AccountPasswordSet::new("alice", "short"))
            .await,
        ErrorCode::BadRequest,
    );
    ada.request_ack(&AccountPasswordSet::new("alice", "a-brand-new-password"))
        .await
        .unwrap();

    // The session she had open is closed under her.
    let gone = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match alice.ping().await {
                Err(_) => break,
                Ok(()) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    })
    .await;
    assert!(gone.is_ok(), "the open session outlived the password");

    // The token no longer resumes, the old password no longer works, the new one does.
    let mut again = connect(&burrow).await;
    assert!(again.auth_resume(&token, 0).await.is_err());
    assert!(login_with(&burrow, "alice", PW).await.is_err());
    assert!(login_with(&burrow, "alice", "a-brand-new-password")
        .await
        .is_ok());

    // Not a peer's, not her own, not from a member.
    refused(
        ada.request_ack(&AccountPasswordSet::new("bea", "a-brand-new-password"))
            .await,
        ErrorCode::Forbidden,
    );
    refused(
        ada.request_ack(&AccountPasswordSet::new("ada", "a-brand-new-password"))
            .await,
        ErrorCode::Forbidden,
    );
    let mut mo = login(&burrow, "mo").await;
    refused(
        mo.request_ack(&AccountPasswordSet::new("alice", "a-brand-new-password"))
            .await,
        ErrorCode::Forbidden,
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn disabling_an_account_closes_the_door_behind_it() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut alice = login(&burrow, "alice").await;
    let mut ada = login(&burrow, "ada").await;

    let mut off = AccountSet::new("alice");
    off.disabled = Some(true);
    ada.request_ack(&off).await.unwrap();

    let gone = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while alice.ping().await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "a disabled account stayed connected");
    assert!(login_with(&burrow, "alice", PW).await.is_err());

    // And back.
    let mut on = AccountSet::new("alice");
    on.disabled = Some(false);
    ada.request_ack(&on).await.unwrap();
    assert!(login_with(&burrow, "alice", PW).await.is_ok());

    let listed: AccountList = ada.request(&AccountListRequest::new(0, 50)).await.unwrap();
    assert!(listed
        .accounts
        .iter()
        .any(|a| a.login == "alice" && !a.disabled));
    burrow.shutdown().await;
}

#[tokio::test]
async fn two_factor_can_be_cleared_for_someone_who_lost_it() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;

    refused(
        ada.request_ack(&AccountTotpReset::new("alice")).await,
        ErrorCode::NotFound,
    );
    let alice = AccountsRepo(&burrow.shared.pool)
        .by_login("alice")
        .await
        .unwrap()
        .unwrap();
    TotpRepo(&burrow.shared.pool)
        .begin(alice.id, b"0123456789abcdefghij")
        .await
        .unwrap();
    ada.request_ack(&AccountTotpReset::new("alice"))
        .await
        .unwrap();
    assert!(TotpRepo(&burrow.shared.pool)
        .get(alice.id)
        .await
        .unwrap()
        .is_none());
    refused(
        ada.request_ack(&AccountTotpReset::new("bea")).await,
        ErrorCode::Forbidden,
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn invitations_are_listed_and_an_unused_one_can_be_withdrawn() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;

    let kept: InviteCode = ada.request(&InviteCreate::new(3600)).await.unwrap();
    let spare: InviteCode = ada.request(&InviteCreate::new(3600)).await.unwrap();

    // Someone uses one.
    let mut newcomer = connect(&burrow).await;
    newcomer
        .register("zed", "zeds-password", Some(kept.code.clone()))
        .await
        .unwrap();

    let list: InviteList = ada.request(&InviteListRequest).await.unwrap();
    let of = |code: &str| {
        list.invites
            .iter()
            .find(|i| i.code == code)
            .unwrap()
            .clone()
    };
    assert_eq!(of(&kept.code).used_by.as_deref(), Some("zed"));
    assert_eq!(of(&kept.code).created_by, "ada");
    assert_eq!(of(&spare.code).used_by, None);

    // The spare is withdrawn and stops working; the used one is history.
    ada.request_ack(&InviteRevoke::new(spare.code.clone()))
        .await
        .unwrap();
    refused(
        ada.request_ack(&InviteRevoke::new(spare.code.clone()))
            .await,
        ErrorCode::NotFound,
    );
    refused(
        ada.request_ack(&InviteRevoke::new(kept.code.clone())).await,
        ErrorCode::NotFound,
    );
    let mut late = connect(&burrow).await;
    assert!(late
        .register("yan", "yans-password", Some(spare.code.clone()))
        .await
        .is_err());

    let mut alice = login(&burrow, "alice").await;
    refused(
        alice.request::<_, InviteList>(&InviteListRequest).await,
        ErrorCode::Forbidden,
    );
    burrow.shutdown().await;
}

#[tokio::test]
async fn the_audit_log_is_read_over_the_wire_by_those_allowed_to() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut ada = login(&burrow, "ada").await;
    ada.request_ack(&AccountCreate::new(
        "carol",
        "carols-password",
        Role::User as u8,
    ))
    .await
    .unwrap();
    ada.request_ack(&AccountCreate::new(
        "dave",
        "daves-password",
        Role::User as u8,
    ))
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let log: AuditList = ada.request(&AuditListRequest::new(50)).await.unwrap();
    let made = log
        .entries
        .iter()
        .find(|e| e.action == "account-create" && e.detail.starts_with("carol"))
        .expect("the creation is on record");
    assert_eq!(made.actor, "ada");
    assert!(made.at > 0);
    assert!(log.entries.iter().all(|e| !e.detail.contains("password")));
    // Oldest first, and a limit is a limit.
    let two: AuditList = ada.request(&AuditListRequest::new(2)).await.unwrap();
    assert_eq!(two.entries.len(), 2);
    assert!(two.entries[0].at <= two.entries[1].at);

    // Moderators hold AUDIT_READ by default: they act in this log.
    let mut mo = login(&burrow, "mo").await;
    let seen: AuditList = mo.request(&AuditListRequest::new(10)).await.unwrap();
    assert!(!seen.entries.is_empty());
    let mut alice = login(&burrow, "alice").await;
    refused(
        alice
            .request::<_, AuditList>(&AuditListRequest::new(10))
            .await,
        ErrorCode::Forbidden,
    );
    burrow.shutdown().await;
}

/// Removing an account for good: the standing order applies, a burrow
/// keeps its last administrator, the person is signed out and gone, and
/// what they wrote stays under the name they wrote it with.
#[tokio::test]
async fn an_account_can_be_removed_for_good_within_the_same_standing() {
    use rabbithole_proto::admin::AccountDelete;
    use rabbithole_proto::board::{BoardCreate, PostCreate};

    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut root = login(&burrow, "root").await;
    let mut ada = login(&burrow, "ada").await;
    let mut alice = login(&burrow, "alice").await;

    // Alice writes something before she goes.
    root.request_ack(&BoardCreate::new("general", "General", 2))
        .await
        .unwrap();
    alice
        .request_ack(&PostCreate::new("general", "Hello", "I was here"))
        .await
        .unwrap();

    // A moderator may not remove anybody.
    let mut mo = login(&burrow, "mo").await;
    refused(
        mo.request_ack(&AccountDelete::new("alice")).await,
        ErrorCode::Forbidden,
    );
    // Nor an admin their peer, nor themselves.
    refused(
        ada.request_ack(&AccountDelete::new("bea")).await,
        ErrorCode::Forbidden,
    );
    refused(
        ada.request_ack(&AccountDelete::new("ada")).await,
        ErrorCode::Forbidden,
    );
    // Nor anybody an account that is not there.
    refused(
        ada.request_ack(&AccountDelete::new("nobody")).await,
        ErrorCode::NotFound,
    );

    // An admin removes somebody below them: they are signed out and gone.
    ada.request_ack(&AccountDelete::new("alice")).await.unwrap();
    assert!(AccountsRepo(&burrow.shared.pool)
        .by_login("alice")
        .await
        .unwrap()
        .is_none());
    assert!(
        login_with(&burrow, "alice", PW).await.is_err(),
        "a removed account cannot sign in"
    );
    // What she wrote stays, under the name she wrote it with.
    let threads = rabbithole_store_server::repo4::PostsRepo(&burrow.shared.pool)
        .threads("general", 50)
        .await
        .unwrap();
    assert!(
        threads.iter().any(|(p, _, _)| p.subject == "Hello"),
        "her post is still there: {threads:?}"
    );
    assert!(
        threads
            .iter()
            .any(|(p, _, _)| p.author.starts_with("alice")),
        "under the name she wrote it with"
    );

    // Her name is hers still: nobody else can take the byline on what she
    // wrote, by account or by persona.
    refused(
        ada.request_ack(&AccountCreate::new("alice", "pw-pw-pw-pw", 1))
            .await,
        ErrorCode::AlreadyExists,
    );

    // The last one who can keep the burrow stays, whoever asks. Disabled,
    // they keep nothing, so they can go.
    root.request_ack(&AccountDelete::new("bea")).await.unwrap();
    let mut disable_ada = AccountSet::new("ada");
    disable_ada.disabled = Some(true);
    root.request_ack(&disable_ada).await.unwrap();
    root.request_ack(&AccountDelete::new("ada")).await.unwrap();
    refused(
        root.request_ack(&AccountDelete::new("root")).await,
        ErrorCode::Forbidden,
    );

    let audit = AuditRepo(&burrow.shared.pool).recent(50).await.unwrap();
    assert!(audit.iter().any(|a| a.action == "account-delete"));
    burrow.shutdown().await;
}
