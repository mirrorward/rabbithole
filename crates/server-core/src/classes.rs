//! Live class-mask cache — the mechanism behind "edit a class, every
//! member changes instantly" (the KDX lesson).
//!
//! Sessions never snapshot a class mask: they rebuild their [`Subject`]
//! through this cache on every permission check, so an admin's `ClassSet`
//! takes effect on the very next request from every member.

use std::collections::HashMap;

use parking_lot::RwLock;
use rabbithole_store_server::repo::ClassesRepo;
use rabbithole_store_server::{SqlitePool, StoreError};

#[derive(Default)]
pub struct ClassCache {
    masks: RwLock<HashMap<i64, u64>>,
    ids: RwLock<HashMap<String, i64>>,
}

impl ClassCache {
    /// Load every class from the store.
    pub async fn load(pool: &SqlitePool) -> Result<ClassCache, StoreError> {
        let cache = ClassCache::default();
        cache.reload(pool).await?;
        Ok(cache)
    }

    pub async fn reload(&self, pool: &SqlitePool) -> Result<(), StoreError> {
        let mut masks = HashMap::new();
        let mut ids = HashMap::new();
        for class in ClassesRepo(pool).all().await? {
            masks.insert(class.id, class.base_mask);
            ids.insert(class.name, class.id);
        }
        *self.masks.write() = masks;
        *self.ids.write() = ids;
        Ok(())
    }

    pub fn mask(&self, class_id: Option<i64>) -> u64 {
        class_id
            .and_then(|id| self.masks.read().get(&id).copied())
            .unwrap_or(0)
    }

    pub fn id_by_name(&self, name: &str) -> Option<i64> {
        self.ids.read().get(name).copied()
    }

    /// Persist + apply a mask change (creating the class if new).
    pub async fn set(&self, pool: &SqlitePool, name: &str, mask: u64) -> Result<(), StoreError> {
        let id = ClassesRepo(pool).upsert(name, mask).await?;
        self.masks.write().insert(id, mask);
        self.ids.write().insert(name.to_string(), id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_store_server::open_in_memory;

    #[tokio::test]
    async fn load_set_and_live_read() {
        let pool = open_in_memory().await.unwrap();
        let cache = ClassCache::load(&pool).await.unwrap();
        let member = cache.id_by_name("member").unwrap();
        assert_eq!(cache.mask(Some(member)), 0);

        cache.set(&pool, "member", 0b1010).await.unwrap();
        assert_eq!(cache.mask(Some(member)), 0b1010);

        // New class created on the fly.
        cache.set(&pool, "vip", 0xFF).await.unwrap();
        let vip = cache.id_by_name("vip").unwrap();
        assert_eq!(cache.mask(Some(vip)), 0xFF);

        // Survives a reload from the store.
        cache.reload(&pool).await.unwrap();
        assert_eq!(cache.mask(Some(vip)), 0xFF);
        assert_eq!(cache.mask(None), 0);
    }
}
