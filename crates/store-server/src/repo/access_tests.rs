use super::*;
use crate::repo2::PersonasRepo;

fn access() -> AccountAccess {
    AccountAccess {
        role: 1,
        class_id: None,
        grant_mask: 1 << 60,
        revoke_mask: 1 << 61,
    }
}

#[tokio::test]
async fn account_access_creation_is_atomic_and_respects_all_identity_claims() {
    let pool = crate::open_in_memory().await.unwrap();
    let repo = AccountsRepo(&pool);
    let alice = repo
        .create_with_access("Alice", "old-phc", access())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(AccountAccess::from(&alice), access());
    assert_eq!(
        PersonasRepo(&pool)
            .for_account(alice.id)
            .await
            .unwrap()
            .len(),
        1
    );
    PersonasRepo(&pool)
        .create(alice.id, "Rabbit", false)
        .await
        .unwrap();
    sqlx::query("INSERT INTO retired_names (name, was, at) VALUES ('Gone', 'Gone', 0)")
        .execute(&pool)
        .await
        .unwrap();
    for taken in ["ALICE", "rabbit", "gone"] {
        assert!(repo
            .create_with_access(taken, "new-phc", access())
            .await
            .unwrap()
            .is_none());
    }
    sqlx::query(
        "CREATE TRIGGER block_persona BEFORE INSERT ON personas
        WHEN NEW.screen_name = 'blocked' BEGIN SELECT RAISE(ABORT, 'fixture'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(repo
        .create_with_access("blocked", "new-phc", access())
        .await
        .is_err());
    assert!(
        repo.by_login("blocked").await.unwrap().is_none(),
        "persona failure rolls back credentials"
    );
}

#[tokio::test]
async fn credential_access_edit_compares_snapshot_and_revokes_tokens_atomically() {
    let pool = crate::open_in_memory().await.unwrap();
    let repo = AccountsRepo(&pool);
    let account = repo
        .create_with_access("alice", "old-phc", access())
        .await
        .unwrap()
        .unwrap();
    SessionsRepo(&pool)
        .insert(&[1; 32], account.id, 3600)
        .await
        .unwrap();
    let mut wanted = access();
    wanted.role = 2;
    wanted.grant_mask = 0;
    assert!(repo
        .update_credentials_and_access(&account, Some("new-phc"), wanted)
        .await
        .unwrap());
    let updated = repo.by_id(account.id).await.unwrap().unwrap();
    assert_eq!(updated.phc.as_deref(), Some("new-phc"));
    assert_eq!(AccountAccess::from(&updated), wanted);
    assert_eq!(
        SessionsRepo(&pool)
            .revoke_account(account.id)
            .await
            .unwrap(),
        0
    );

    SessionsRepo(&pool)
        .insert(&[2; 32], account.id, 3600)
        .await
        .unwrap();
    assert!(!repo
        .update_credentials_and_access(&account, Some("stale-phc"), access())
        .await
        .unwrap());
    assert_eq!(repo.by_id(account.id).await.unwrap().unwrap(), updated);
    assert_eq!(
        SessionsRepo(&pool)
            .revoke_account(account.id)
            .await
            .unwrap(),
        1,
        "failed edit keeps valid sessions"
    );

    repo.admin_set("alice", None, None, Some(true))
        .await
        .unwrap();
    assert!(!repo
        .update_credentials_and_access(&updated, Some("disabled-phc"), access())
        .await
        .unwrap());
    assert_eq!(
        repo.by_id(account.id)
            .await
            .unwrap()
            .unwrap()
            .phc
            .as_deref(),
        Some("new-phc")
    );
}
