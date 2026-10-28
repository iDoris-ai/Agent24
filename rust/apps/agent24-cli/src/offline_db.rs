//! Offline gate for CLI commands that open `agent24.db` (CORE-5 D1).
//! Lock, then dock markers, then open. An existing file is never migrated.
//! A missing file is not created: there is nothing to revoke, export, or delete.

use std::path::Path;

const BUSY: &str = "busy: daemon.singleton.lock is held; database not opened";

/// A database the caller may use, or proof that no database file exists.
///
/// Keep the value alive until the query ends, then await [`Self::close`].
pub(crate) enum OfflineDb {
    /// `agent24.db` is absent. No consent or decision row can exist.
    Absent(OfflineLock),
    Ready(agent24_store::Store, OfflineLock),
}

pub(crate) struct OfflineLock(std::fs::File);

impl Drop for OfflineLock {
    fn drop(&mut self) {
        // A concurrent fork can retain the open file description until exec.
        // Closing only our descriptor would leave its flock held meanwhile.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl OfflineDb {
    pub(crate) async fn close(self) {
        match self {
            Self::Absent(lock) => drop(lock),
            Self::Ready(store, lock) => {
                store.close().await;
                drop(lock);
            }
        }
    }
}

/// Lock, refuse dock markers, then open an existing file without migrating it.
///
/// A missing `agent24.db` is [`OfflineDb::Absent`]. It is not created and no
/// migration runs. Callers treat that as nothing to do.
pub(crate) async fn open_offline_store(data_dir: &Path) -> Result<OfflineDb, String> {
    let lock = acquire(data_dir)?;
    let path = data_dir.join("agent24.db");
    if path.is_file() {
        let store = agent24_store::Store::open_without_migration(&path)
            .await
            .map_err(|err| err.to_string())?;
        #[cfg(test)]
        tests::observe_open(&store).await;
        Ok(OfflineDb::Ready(store, lock))
    } else if path.exists() {
        Err(format!(
            "could not open {}: not a database file",
            path.display()
        ))
    } else {
        Ok(OfflineDb::Absent(lock))
    }
}

fn acquire(data_dir: &Path) -> Result<OfflineLock, String> {
    let lock = agent24_protocol::state_file::try_acquire_singleton_in(data_dir)
        .map_err(|err| format!("could not acquire daemon.singleton.lock: {err}"))?;
    let Some(lock) = lock else {
        return Err(BUSY.to_owned());
    };
    let lock = OfflineLock(lock);
    marker_absent(data_dir, "migration_failed")?;
    marker_absent(data_dir, "migration-guard.json")?;
    Ok(lock)
}

fn marker_absent(data_dir: &Path, name: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(data_dir.join(name)) {
        Ok(_) => Err(format!("docked: {name} is present; database not opened")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("docked: could not read {name}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::Path;

    use agent24_store::{Store, test_hooks::pool};
    use sqlx::{Sqlite, SqlitePool, pool::PoolConnection};

    tokio::task_local! {
        // Keep a real connection checked out so close cannot finish until the
        // test returns it. Task scoping keeps parallel CLI tests independent.
        static OPENED: tokio::sync::mpsc::UnboundedSender<(SqlitePool, PoolConnection<Sqlite>)>;
    }

    pub(super) async fn observe_open(store: &Store) {
        if let Ok(sender) = OPENED.try_with(Clone::clone) {
            let pool = pool(store);
            let connection = pool.acquire().await.unwrap();
            sender.send((pool.clone(), connection)).unwrap();
        }
    }

    #[test]
    #[ignore = "spawned by close-order tests to probe the lock from another process"]
    fn offline_close_lock_probe() {
        let Ok(dir) = std::env::var("AGENT24_CLOSE_PROBE_DIR") else {
            return;
        };
        let dir = Path::new(&dir);
        let lock = agent24_protocol::state_file::try_acquire_singleton_in(dir).unwrap();
        let closing = std::env::var("AGENT24_CLOSE_PROBE_CLOSING").unwrap() == "true";
        assert_eq!(
            lock.is_none(),
            closing,
            "singleton lock must stay held until all store connections close"
        );
        if !closing {
            for name in ["agent24.db-wal", "agent24.db-shm"] {
                assert!(!dir.join(name).exists(), "residual SQLite sidecar: {name}");
            }
        }
    }

    fn probe_lock(dir: &Path, closing: bool) {
        // A child observes the actual interprocess lock, without relying on
        // same-process flock semantics or sleeps. output() also reaps it.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "offline_db::tests::offline_close_lock_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("AGENT24_CLOSE_PROBE_DIR", dir)
            .env("AGENT24_CLOSE_PROBE_CLOSING", closing.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "lock probe failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn assert_close_order(
        dir: &Path,
        operation: impl std::future::Future<Output = Result<(), String>>,
    ) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let (sender, mut opened) = tokio::sync::mpsc::unbounded_channel();
            let operation = OPENED.scope(sender, operation);
            tokio::pin!(operation);
            let (pool, connection) = tokio::select! {
                result = &mut operation => panic!("entry returned before opening: {result:?}"),
                observation = opened.recv() => observation.unwrap(),
            };
            tokio::select! {
                result = &mut operation => panic!("entry returned without awaiting Store::close: {result:?}"),
                () = pool.close_event() => {}
            }
            assert!(pool.size() > 0, "borrowed connection must delay close");
            probe_lock(dir, true);
            drop(connection);
            let result = operation.await;
            assert!(pool.is_closed(), "entry must explicitly close its pool");
            assert_eq!(pool.size(), 0, "close must await every connection");
            probe_lock(dir, false);

            let reopened = Store::open_without_migration(&dir.join("agent24.db"))
                .await
                .unwrap();
            reopened.export_decision_log(None, None).await.unwrap();
            reopened.close().await;
            result
        })
        .await
        .unwrap()
    }

    async fn database(dir: &Path, refuse_consent: bool) {
        let store = Store::open(&dir.join("agent24.db")).await.unwrap();
        if refuse_consent {
            sqlx::query(
                "CREATE TRIGGER refuse BEFORE INSERT ON module_consent_generations \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END",
            )
            .execute(pool(&store))
            .await
            .unwrap();
        }
        store.close().await;
    }

    #[tokio::test]
    async fn offline_db_close_waits_for_connections_before_unlocking() {
        let dir = tempfile::tempdir().unwrap();
        database(dir.path(), false).await;
        assert_close_order(dir.path(), async {
            super::open_offline_store(dir.path()).await?.close().await;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn consent_revocation_closes_before_unlocking_on_success_and_error() {
        for refuse in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            database(dir.path(), refuse).await;
            let result = assert_close_order(
                dir.path(),
                crate::invalidate_module_consents_in(Some(dir.path()), "consent-mod"),
            )
            .await;
            if refuse {
                assert!(result.unwrap_err().contains("refused"));
            } else {
                result.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn decide_closes_before_unlocking_on_success_and_error() {
        for action in [
            crate::DecideAction::Export {
                jsonl: true,
                point: None,
                since: None,
            },
            crate::DecideAction::Delete {
                action: crate::DecideDeleteAction::All { confirm: true },
            },
            crate::DecideAction::Delete {
                action: crate::DecideDeleteAction::All { confirm: false },
            },
        ] {
            let rejected = matches!(
                action,
                crate::DecideAction::Delete {
                    action: crate::DecideDeleteAction::All { confirm: false }
                }
            );
            let dir = tempfile::tempdir().unwrap();
            database(dir.path(), false).await;
            let result =
                assert_close_order(dir.path(), crate::cmd_decide_in(dir.path(), action)).await;
            if rejected {
                assert!(result.unwrap_err().contains("--confirm"));
            } else {
                result.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn releasing_offline_lock_unlocks_inherited_descriptors() {
        for existing in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            if existing {
                agent24_store::Store::open(&dir.path().join("agent24.db"))
                    .await
                    .unwrap()
                    .close()
                    .await;
            }
            let db = super::open_offline_store(dir.path()).await.unwrap();
            let inherited = match &db {
                super::OfflineDb::Absent(lock) | super::OfflineDb::Ready(_, lock) => {
                    // fork inherits the same open file description, just like dup.
                    lock.0.try_clone().unwrap()
                }
            };
            drop(db);
            drop(crate::tests::hold_singleton(dir.path()));
            drop(inherited);
        }
    }
}
