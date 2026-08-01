//! Direct, non-lossy cancellation registry for authenticated connections.
//!
//! The broadcast bus is for product events; authorization revocation cannot
//! depend on a laggable channel. Each live protocol surface owns a lease and
//! selects its cancellation future alongside socket input.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

#[derive(Default)]
pub struct AccountSessionRegistry {
    inner: Mutex<HashMap<i64, HashMap<u64, Arc<Cancellation>>>>,
}

#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl AccountSessionRegistry {
    pub fn register(self: &Arc<Self>, account_id: i64, session_id: u64) -> AccountSessionLease {
        let cancellation = Arc::new(Cancellation::default());
        self.inner
            .lock()
            .entry(account_id)
            .or_default()
            .insert(session_id, cancellation.clone());
        AccountSessionLease {
            registry: self.clone(),
            account_id,
            session_id,
            cancellation,
        }
    }

    pub fn invalidate(&self, account_id: i64) -> usize {
        let sessions = self.inner.lock().remove(&account_id).unwrap_or_default();
        let count = sessions.len();
        for cancellation in sessions.into_values() {
            cancellation.cancel();
        }
        count
    }

    fn unregister(&self, account_id: i64, session_id: u64, expected: &Arc<Cancellation>) {
        let mut inner = self.inner.lock();
        let Some(sessions) = inner.get_mut(&account_id) else {
            return;
        };
        if sessions
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            sessions.remove(&session_id);
        }
        if sessions.is_empty() {
            inner.remove(&account_id);
        }
    }
}

pub struct AccountSessionLease {
    registry: Arc<AccountSessionRegistry>,
    account_id: i64,
    session_id: u64,
    cancellation: Arc<Cancellation>,
}

impl AccountSessionLease {
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }
}

impl Drop for AccountSessionLease {
    fn drop(&mut self) {
        self.registry
            .unregister(self.account_id, self.session_id, &self.cancellation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalidate_is_direct_and_account_scoped() {
        let registry = Arc::new(AccountSessionRegistry::default());
        let a = registry.register(10, 1);
        let b = registry.register(10, 2);
        let other = registry.register(11, 3);
        assert_eq!(registry.invalidate(10), 2);
        a.cancelled().await;
        b.cancelled().await;
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
        assert!(!other.is_cancelled());
    }
}
