//! COMM-1b: `KeystoreWriteLock` — serializes every Hyphae invocation that
//! writes `keystore.json` (COMM-HYPHAE.md §3, H3/G8).
//!
//! Hyphae's `SaveKeyStore` (`internal/identity/keystore.go`) is a read →
//! mutate-in-memory → write-temp-file → rename, with **no locking** of its
//! own: two concurrent `hyphae identity create` processes each load the
//! keystore, each add their own new identity to their own in-memory copy,
//! and then race to write it back. Whichever process's rename lands last
//! wins outright — the loser's newly-created identity (and its private key)
//! is silently gone, because the final file holds the last writer's
//! snapshot, not a merge of the two. This can't be fixed from inside this
//! crate's own process space once two child processes are already racing;
//! the fix is to never let two keystore-writing child processes run
//! concurrently in the first place. [`KeystoreWriteLock`] is exactly that:
//! an async mutex, held for the full lifetime of one keystore-writing
//! invocation (spawn through exit, not just around the spawn call), so a
//! second writer always starts only after the first one's `SaveKeyStore`
//! has already completed and been flushed to disk.
//!
//! This only serializes writers that go through *this* crate (comm's own
//! REST routes and CLI, all funneled through one `HyphaeRunner`). It does
//! not protect against some other, unrelated process writing the same
//! `keystore.json` — but nothing else is supposed to ever touch comm's
//! dedicated Hyphae HOME (COMM-HYPHAE.md G8).

use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard};

/// A single process-wide write lock, cheap to clone (an `Arc` underneath) so
/// every caller that needs to serialize keystore writes — comm's REST
/// routes, the `import` flow, test code — can share one lock through a
/// [`crate::runner::HyphaeRunner`] without threading a reference through
/// every call site.
#[derive(Clone, Default)]
pub struct KeystoreWriteLock(Arc<Mutex<()>>);

impl KeystoreWriteLock {
    /// Waits for exclusive access to the keystore. Hold the returned guard
    /// for as long as the keystore-writing invocation is in flight; dropping
    /// it (e.g. at the end of the enclosing `async fn`) releases the lock
    /// for the next writer.
    pub async fn acquire(&self) -> KeystoreGuard<'_> {
        KeystoreGuard(self.0.lock().await)
    }
}

/// RAII guard returned by [`KeystoreWriteLock::acquire`]. Holds no data of
/// its own; the lock is released when this is dropped.
pub struct KeystoreGuard<'a>(#[allow(dead_code)] MutexGuard<'a, ()>);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn serializes_concurrent_acquires() {
        let lock = KeystoreWriteLock::default();
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let lock = lock.clone();
            let concurrent = concurrent.clone();
            let max_concurrent = max_concurrent.clone();
            tasks.push(tokio::spawn(async move {
                let _guard = lock.acquire().await;
                let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn clone_shares_the_same_underlying_lock() {
        let lock = KeystoreWriteLock::default();
        let other = lock.clone();
        let guard = lock.acquire().await;
        // The clone must see the lock as held: `try_lock` on the same
        // underlying mutex must fail while `guard` is alive.
        assert!(other.0.try_lock().is_err());
        drop(guard);
        assert!(other.0.try_lock().is_ok());
    }
}
