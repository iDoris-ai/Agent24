//! P1-02c-c5 (design D7): the daemon's background task that keeps
//! `memory.db`'s WAL short. Policy lives in
//! [`agent24_memory::wal_maintenance`]; this is only the loop: poll, tick,
//! stop on the daemon-wide cancellation token. Results are `tracing` only —
//! no user event, no route.

use std::time::Instant;

use agent24_memory::wal_maintenance::{WalMaintainer, WalMaintenanceConfig};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Start the idle WAL checkpoint task. Returns `None` (nothing spawned) for an
/// in-memory store, or when the maintainer cannot be set up — the WAL then
/// simply keeps SQLite's own auto-checkpoint, as before this slice.
pub(crate) async fn spawn(
    kv: &agent24_memory::KvStore,
    cfg: WalMaintenanceConfig,
    cancel: CancellationToken,
) -> Option<JoinHandle<()>> {
    let maintainer = match kv.wal_maintainer(cfg, Instant::now()).await {
        Ok(Some(m)) => m,
        Ok(None) => return None,
        Err(err) => {
            tracing::warn!(error = %err, "memory.db wal maintenance unavailable");
            return None;
        }
    };
    Some(tokio::spawn(run(maintainer, cancel)))
}

async fn run(mut maintainer: WalMaintainer, cancel: CancellationToken) {
    let poll = maintainer.config().poll_interval;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(poll) => {}
        }
        // A tick is short (busy_timeout = 0), but still race it with shutdown.
        tokio::select! {
            () = cancel.cancelled() => return,
            res = maintainer.tick(Instant::now()) => {
                if let Err(err) = res {
                    tracing::warn!(error = %err, "memory.db wal maintenance tick failed; will retry");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::time::Duration;

    fn fast() -> WalMaintenanceConfig {
        WalMaintenanceConfig {
            idle_after: Duration::from_millis(150),
            min_interval: Duration::from_millis(50),
            poll_interval: Duration::from_millis(30),
        }
    }

    #[tokio::test]
    async fn in_memory_store_spawns_nothing() {
        let kv = agent24_memory::KvStore::open_memory().await.unwrap();
        kv.put("t", "before", &1).await.unwrap();
        assert!(spawn(&kv, fast(), CancellationToken::new()).await.is_none());
        // Probing must not have destroyed the ephemeral database.
        kv.put("t", "after", &2).await.unwrap();
        assert_eq!(kv.fetch::<i32>("t", "before").await.unwrap(), Some(1));
        assert_eq!(kv.fetch::<i32>("t", "after").await.unwrap(), Some(2));
    }

    #[tokio::test]
    async fn runs_truncates_when_idle_then_stops_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&dir.path().join("memory.db"))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let task = spawn(&kv, fast(), cancel.clone()).await.unwrap();
        for i in 0..200 {
            kv.put("t", &format!("k{i}"), &vec![i; 64]).await.unwrap();
        }
        let wal = dir.path().join("memory.db-wal");
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        // Idle: the WAL is cut to zero well within a second.
        let mut cut = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if std::fs::metadata(&wal).map_or(0, |m| m.len()) == 0 {
                cut = true;
                break;
            }
        }
        assert!(cut, "idle task never truncated the WAL");

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("task must exit promptly on shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_is_prompt_even_with_a_long_poll() {
        let dir = tempfile::tempdir().unwrap();
        let kv = agent24_memory::KvStore::open(&dir.path().join("memory.db"))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let task = spawn(&kv, WalMaintenanceConfig::default(), cancel.clone())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("must not wait out the 5s poll")
            .unwrap();
    }
}
