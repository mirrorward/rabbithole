//! RH-36 exact, shared Hotline access edits over real TCP connections.

use super::*;
use rabbithole_core::Client as NativeClient;
use rabbithole_server_core::Caps;
use rabbithole_store_server::repo::{AccountAccess, AccountsRepo, AuditRepo, ClassesRepo};

const PASS: &str = "access-fixture-password";
const NEXT_PASS: &str = "access-fixture-new-password";

async fn start(dir: &std::path::Path) -> Burrow {
    let mut config = test_config(dir);
    config.telnet_enabled = true;
    config.telnet_addr = "127.0.0.1:0".parse().unwrap();
    config.ratelimit_enabled = false;
    Burrow::start(config).await.unwrap()
}

async fn connect(b: &Burrow, login: &str) -> Client {
    let mut c = Client::connect(b.hotline_addr.unwrap()).await;
    assert_eq!(c.login(login, PASS, login).await.header.error, 0);
    c
}

async fn get(c: &mut Client, login: &str) -> AccessMask {
    let reply = c
        .roundtrip(
            transaction::GET_USER,
            vec![Field::credential(field::USER_LOGIN, login)],
        )
        .await;
    assert_eq!(reply.header.error, 0);
    field_mask(&reply, field::USER_ACCESS)
}

async fn set(c: &mut Client, login: &str, bytes: Vec<u8>, password: &str) -> Transaction {
    c.roundtrip(
        transaction::SET_USER,
        vec![
            Field::credential(field::USER_LOGIN, login),
            Field::new(field::USER_ACCESS, bytes),
            Field::credential(field::USER_PASSWORD, password),
        ],
    )
    .await
}

fn mutable_bits(mut mask: AccessMask) -> AccessMask {
    for bit in [
        Privilege::ShowInList,
        Privilege::AnyName,
        Privilege::ChangeOwnPassword,
    ] {
        mask.revoke(bit);
    }
    mask
}

async fn expect_text(socket: &mut TcpStream, text: &[u8]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut seen = Vec::new();
        let mut buf = [0; 4096];
        while !seen.windows(text.len()).any(|part| part == text) {
            let n = socket.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "session closed waiting for prompt");
            seen.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn non_role_creation_roundtrips_and_shared_permissions_enforce_each_choice() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .auth
        .create_account("operator", PASS, Role::Admin)
        .await
        .unwrap();
    let mut operator = connect(&b, "operator").await;
    let mask: AccessMask = [
        Privilege::ReadChat,
        Privilege::DownloadFiles,
        Privilege::ViewDropBoxes,
    ]
    .into_iter()
    .collect();
    let reply = operator
        .roundtrip(
            transaction::NEW_USER,
            vec![
                Field::credential(field::USER_LOGIN, "alice"),
                Field::credential(field::USER_PASSWORD, PASS),
                Field::new(field::USER_ACCESS, mask.to_bytes().to_vec()),
            ],
        )
        .await;
    assert_eq!(reply.header.error, 0);
    assert_eq!(mutable_bits(get(&mut operator, "alice").await), mask);
    let user = b
        .shared
        .auth
        .login_password("alice", PASS, None)
        .await
        .unwrap();
    assert_eq!(user.account.role, Role::User as u8);
    assert_ne!(user.account.grant_mask & Caps::DROPBOX_VIEW.0, 0);
    assert_ne!(user.account.revoke_mask & Caps::CHAT_SEND.0, 0);
    for (cap, allowed) in [
        (Caps::CHAT_READ, true),
        (Caps::CHAT_SEND, false),
        (Caps::FILE_DOWNLOAD, true),
        (Caps::FILE_UPLOAD, false),
        (Caps::DROPBOX_VIEW, true),
        (Caps::BOARD_POST, false),
        (Caps::DOOR_RUN, false),
        (Caps::CONFIG_ADMIN, false),
    ] {
        assert_eq!(
            b.shared.perms.allows(&user.subject, "", cap),
            allowed,
            "{cap:?}"
        );
    }
    let mut alice = connect(&b, "alice").await;
    let refused = alice
        .roundtrip(transaction::INVITE_NEW_CHAT, Vec::new())
        .await;
    assert_ne!(
        refused.header.error, 0,
        "native CHAT_CREATE_ROOM revoke is enforced by Hotline"
    );

    // A GET/SET roundtrip changes neither native standing nor masks, and does
    // not disconnect the target or expire a saved sign-in.
    let before = AccountsRepo(&b.shared.pool)
        .by_login("alice")
        .await
        .unwrap()
        .unwrap();
    let token = user.token.unwrap().encode();
    let roundtrip = get(&mut operator, "alice").await;
    assert_eq!(
        set(&mut operator, "alice", roundtrip.to_bytes().to_vec(), "")
            .await
            .header
            .error,
        0
    );
    assert_eq!(
        AccountsRepo(&b.shared.pool)
            .by_id(before.id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
    assert!(b.shared.auth.login_resume(&token).await.is_ok());
    assert_eq!(
        alice
            .roundtrip(transaction::GET_USER_NAME_LIST, Vec::new())
            .await
            .header
            .error,
        0
    );
    alice.close().await;
    operator.close().await;
    b.shutdown().await;
}

#[tokio::test]
async fn access_and_password_change_preserves_native_rights_and_closes_all_old_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .auth
        .create_account("operator", PASS, Role::Superuser)
        .await
        .unwrap();
    let alice = b
        .shared
        .auth
        .create_account("alice", PASS, Role::User)
        .await
        .unwrap();
    let mut initial = AccountAccess::from(&alice);
    initial.grant_mask = Caps::AUDIT_READ.0;
    initial.revoke_mask = Caps::DOOR_RUN.0;
    assert!(AccountsRepo(&b.shared.pool)
        .update_credentials_and_access(&alice, None, initial)
        .await
        .unwrap());
    let mut operator = connect(&b, "operator").await;
    let mut legacy = connect(&b, "alice").await;
    let mut native = NativeClient::connect(
        &format!("ws://{}", b.ws_addr),
        None,
        None,
        "access-test",
        "0",
    )
    .await
    .unwrap();
    let token = native.auth_password("alice", PASS).await.unwrap().token;
    native.expect_welcome().await.unwrap();
    let mut telnet = TcpStream::connect(b.telnet_addr.unwrap()).await.unwrap();
    expect_text(&mut telnet, b"login: ").await;
    telnet.write_all(b"alice\r\n").await.unwrap();
    expect_text(&mut telnet, b"password: ").await;
    telnet
        .write_all(format!("{PASS}\r\n").as_bytes())
        .await
        .unwrap();
    expect_text(&mut telnet, b"Command: ").await;

    let mut mask = get(&mut operator, "alice").await;
    mask.revoke(Privilege::UploadFiles);
    for bit in [
        Privilege::CreateUsers,
        Privilege::DeleteUsers,
        Privilege::OpenUsers,
        Privilege::ModifyUsers,
    ] {
        mask.grant(bit);
    }
    assert_eq!(
        set(&mut operator, "alice", mask.to_bytes().to_vec(), NEXT_PASS)
            .await
            .header
            .error,
        0
    );
    assert_eq!(
        mutable_bits(get(&mut operator, "alice").await),
        mutable_bits(mask)
    );
    assert!(b
        .shared
        .auth
        .login_password("alice", PASS, None)
        .await
        .is_err());
    assert!(b.shared.auth.login_resume(&token).await.is_err());
    let fresh = b
        .shared
        .auth
        .login_password("alice", NEXT_PASS, None)
        .await
        .unwrap();
    assert_eq!(fresh.account.id, alice.id);
    assert_eq!(fresh.account.role, Role::Admin as u8);
    let caps = fresh.subject.base_caps();
    for (cap, allowed) in [
        (Caps::ACCOUNT_ADMIN, true),
        (Caps::FILE_UPLOAD, false),
        (Caps::AUDIT_READ, true),
        (Caps::SWARM_ADVERTISE, true),
        (Caps::DOOR_RUN, false),
        (Caps::CONFIG_ADMIN, false),
        (Caps::USER_BAN, false),
        (Caps::MODERATE, false),
    ] {
        assert_eq!(caps & cap.0 != 0, allowed, "{cap:?}");
    }
    legacy.read_until(transaction::DISCONNECT_MSG).await;
    legacy.expect_closed().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while matches!(native.next_push().await, Ok(Some(_))) {}
    })
    .await
    .expect("native old subject is disconnected");
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), telnet.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&tail).contains("account access or password changed"));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if AuditRepo(&b.shared.pool)
                .recent(100)
                .await
                .unwrap()
                .iter()
                .any(|row| {
                    row.actor == "operator"
                        && row.action == "account-set"
                        && row.detail.contains("password/access")
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    operator.close().await;
    b.shutdown().await;
}

#[tokio::test]
async fn invalid_or_unrepresentable_access_never_partially_writes_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .auth
        .create_account("operator", PASS, Role::Admin)
        .await
        .unwrap();
    let alice = b
        .shared
        .auth
        .create_account("alice", PASS, Role::User)
        .await
        .unwrap();
    let user = b
        .shared
        .auth
        .login_password("alice", PASS, None)
        .await
        .unwrap();
    let token = user.token.unwrap().encode();
    let mut operator = connect(&b, "operator").await;
    let mut unknown = AccessMask::NONE;
    unknown.set_bit(63, true);
    let invalid = [
        vec![0; 7],
        vec![0; 9],
        [Privilege::CreateUsers]
            .into_iter()
            .collect::<AccessMask>()
            .to_bytes()
            .to_vec(),
        [Privilege::DeleteFiles]
            .into_iter()
            .collect::<AccessMask>()
            .to_bytes()
            .to_vec(),
        [Privilege::NewsDeleteArticle]
            .into_iter()
            .collect::<AccessMask>()
            .to_bytes()
            .to_vec(),
        [Privilege::NoAgreement]
            .into_iter()
            .collect::<AccessMask>()
            .to_bytes()
            .to_vec(),
        unknown.to_bytes().to_vec(),
    ];
    for (n, bytes) in invalid.into_iter().enumerate() {
        assert_ne!(
            set(&mut operator, "alice", bytes.clone(), NEXT_PASS)
                .await
                .header
                .error,
            0
        );
        assert_eq!(
            AccountsRepo(&b.shared.pool)
                .by_id(alice.id)
                .await
                .unwrap()
                .unwrap(),
            alice
        );
        assert!(b.shared.auth.login_resume(&token).await.is_ok());
        let name = format!("invalid-{n}");
        let reply = operator
            .roundtrip(
                transaction::NEW_USER,
                vec![
                    Field::credential(field::USER_LOGIN, &name),
                    Field::credential(field::USER_PASSWORD, NEXT_PASS),
                    Field::new(field::USER_ACCESS, bytes),
                ],
            )
            .await;
        assert_ne!(reply.header.error, 0);
        assert!(AccountsRepo(&b.shared.pool)
            .by_login(&name)
            .await
            .unwrap()
            .is_none());
    }
    operator.close().await;
    b.shutdown().await;
}

#[tokio::test]
async fn current_authority_role_ordering_and_superuser_protection_apply_to_access_edits() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    for (login, role) in [
        ("operator", Role::Admin),
        ("peer", Role::Admin),
        ("keeper", Role::Superuser),
        ("alice", Role::User),
    ] {
        b.shared
            .auth
            .create_account(login, PASS, role)
            .await
            .unwrap();
    }
    let mut operator = connect(&b, "operator").await;
    for target in ["operator", "peer", "keeper"] {
        let before = AccountsRepo(&b.shared.pool)
            .by_login(target)
            .await
            .unwrap()
            .unwrap();
        let original = get(&mut operator, target).await;
        assert_eq!(
            set(&mut operator, target, original.to_bytes().to_vec(), "")
                .await
                .header
                .error,
            0,
            "safe readback no-op"
        );
        let mut changed = original;
        changed.revoke(Privilege::UploadFiles);
        assert_ne!(
            set(
                &mut operator,
                target,
                changed.to_bytes().to_vec(),
                NEXT_PASS
            )
            .await
            .header
            .error,
            0
        );
        assert_eq!(
            AccountsRepo(&b.shared.pool)
                .by_id(before.id)
                .await
                .unwrap()
                .unwrap(),
            before
        );
    }
    // Even a superuser cannot promise an effective revoke for another
    // superuser, whose shared permission evaluator intentionally bypasses it.
    let mut keeper = connect(&b, "keeper").await;
    b.shared
        .auth
        .create_account("other-keeper", PASS, Role::Superuser)
        .await
        .unwrap();
    let mut impossible = get(&mut keeper, "other-keeper").await;
    impossible.revoke(Privilege::DownloadFiles);
    assert_ne!(
        set(
            &mut keeper,
            "other-keeper",
            impossible.to_bytes().to_vec(),
            NEXT_PASS
        )
        .await
        .header
        .error,
        0
    );

    // A custom lower-role account administrator cannot assign an admin role
    // or a new capability it does not itself hold.
    let class = ClassesRepo(&b.shared.pool)
        .upsert("limited", Caps::ACCOUNT_ADMIN.0)
        .await
        .unwrap();
    AccountsRepo(&b.shared.pool)
        .admin_set(
            "operator",
            Some(Role::Moderator as u8),
            Some(Some(class)),
            None,
        )
        .await
        .unwrap();
    let full_admin = get(&mut keeper, "peer").await;
    assert_ne!(
        set(
            &mut operator,
            "alice",
            full_admin.to_bytes().to_vec(),
            NEXT_PASS
        )
        .await
        .header
        .error,
        0
    );
    let create = operator
        .roundtrip(
            transaction::NEW_USER,
            vec![
                Field::credential(field::USER_LOGIN, "too-powerful"),
                Field::credential(field::USER_PASSWORD, PASS),
                Field::new(field::USER_ACCESS, full_admin.to_bytes().to_vec()),
            ],
        )
        .await;
    assert_ne!(create.header.error, 0);
    assert!(AccountsRepo(&b.shared.pool)
        .by_login("too-powerful")
        .await
        .unwrap()
        .is_none());

    let mut more = get(&mut keeper, "alice").await;
    more.grant(Privilege::Broadcast);
    assert_ne!(
        set(&mut operator, "alice", more.to_bytes().to_vec(), NEXT_PASS)
            .await
            .header
            .error,
        0,
        "assignable role still cannot receive a capability the actor lacks"
    );

    ClassesRepo(&b.shared.pool)
        .set_mask("limited", 0)
        .await
        .unwrap();
    let mask = get(&mut keeper, "alice").await;
    assert_ne!(
        set(&mut operator, "alice", mask.to_bytes().to_vec(), NEXT_PASS)
            .await
            .header
            .error,
        0,
        "old socket loses class grant"
    );
    let alice = AccountsRepo(&b.shared.pool)
        .by_login("alice")
        .await
        .unwrap()
        .unwrap();
    assert!(b
        .shared
        .auth
        .login_password("alice", PASS, None)
        .await
        .is_ok());
    assert_eq!(alice.role, Role::User as u8);
    keeper.close().await;
    operator.close().await;
    b.shutdown().await;
}
