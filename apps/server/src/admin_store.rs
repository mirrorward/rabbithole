//! Store-facing helpers for the admin family.

use rabbithole_proto::admin as padm;
use rabbithole_store_server::repo::{AccountsRepo, ClassesRepo};
use rabbithole_store_server::SqlitePool;

use crate::Shared;

pub async fn list_accounts(
    pool: &SqlitePool,
    offset: i64,
    limit: i64,
) -> anyhow::Result<(Vec<padm::AccountEntry>, u64)> {
    let class_names: std::collections::HashMap<i64, String> = ClassesRepo(pool)
        .all()
        .await?
        .into_iter()
        .map(|c| (c.id, c.name))
        .collect();
    let total = AccountsRepo(pool).count().await? as u64;
    let rows = AccountsRepo(pool)
        .list(offset, limit)
        .await?
        .into_iter()
        .map(|a| {
            let class = a.class_id.and_then(|id| class_names.get(&id).cloned());
            padm::AccountEntry::new(a.id, a.login, a.role, class, a.disabled)
        })
        .collect();
    Ok((rows, total))
}

/// Returns whether the login existed.
pub async fn account_set(
    shared: &Shared,
    login: &str,
    role: Option<u8>,
    class: Option<&str>,
    disabled: Option<bool>,
) -> anyhow::Result<bool> {
    let class_id = match class {
        Some("") => Some(None), // explicit clear
        Some(name) => match shared.classes.id_by_name(name) {
            Some(id) => Some(Some(id)),
            None => return Ok(false),
        },
        None => None,
    };
    account_update(
        shared,
        login,
        None,
        role,
        class_id,
        disabled,
        "account authorization changed",
    )
    .await
}

/// Atomically apply authentication/authorization changes and revoke all
/// durable bearers, then invalidate every live protocol connection.
pub async fn account_update(
    shared: &Shared,
    login: &str,
    phc: Option<&str>,
    role: Option<u8>,
    class_id: Option<Option<i64>>,
    disabled: Option<bool>,
    reason: &str,
) -> anyhow::Result<bool> {
    let _gate = shared.auth_gate.write().await;
    let Some(account_id) = AccountsRepo(&shared.pool)
        .admin_update_and_revoke_sessions(login, phc, role, class_id, disabled)
        .await?
    else {
        return Ok(false);
    };
    shared.invalidate_account_sessions(account_id, reason);
    Ok(true)
}

/// Persist a class mask and invalidate every current member. The write gate
/// ensures a connection either activates before this change (and registers a
/// cancellation lease) or authenticates afterward under the new mask.
pub async fn class_set(shared: &Shared, name: &str, base_mask: u64) -> anyhow::Result<()> {
    let _gate = shared.auth_gate.write().await;
    let (class_id, account_ids) = ClassesRepo(&shared.pool)
        .set_mask_and_revoke_sessions(name, base_mask)
        .await?;
    shared.classes.install(class_id, name, base_mask);
    for account_id in account_ids {
        shared.invalidate_account_sessions(account_id, "permission class changed");
    }
    Ok(())
}
