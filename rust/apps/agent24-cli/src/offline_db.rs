//! Offline gate for CLI commands that open `agent24.db` (CORE-5 D1).
//! Lock, then dock markers, then open. An existing file is never migrated.
//! A missing file is not created: there is nothing to revoke, export, or delete.

use std::path::Path;

const BUSY: &str = "busy: daemon.singleton.lock is held; database not opened";

/// A database the caller may use, or proof that no database file exists.
///
/// The file is the singleton lock. Keep the value alive until the query ends.
pub(crate) enum OfflineDb {
    /// `agent24.db` is absent. No consent or decision row can exist.
    Absent(std::fs::File),
    Ready(agent24_store::Store, std::fs::File),
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

fn acquire(data_dir: &Path) -> Result<std::fs::File, String> {
    let lock = agent24_protocol::state_file::try_acquire_singleton_in(data_dir)
        .map_err(|err| format!("could not acquire daemon.singleton.lock: {err}"))?;
    let Some(lock) = lock else {
        return Err(BUSY.to_owned());
    };
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
