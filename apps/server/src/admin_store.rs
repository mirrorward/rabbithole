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
    Ok(AccountsRepo(&shared.pool)
        .admin_set(login, role, class_id, disabled)
        .await?)
}
