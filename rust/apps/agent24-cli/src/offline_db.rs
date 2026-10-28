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
