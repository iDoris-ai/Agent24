//! Offline gate for CLI commands that open `agent24.db` (CORE-5 D1).
//! Lock, then dock markers, then open. An existing file is never migrated.

use std::path::Path;

const BUSY: &str = "busy: daemon.singleton.lock is held; database not opened";

/// The returned file is the singleton lock. Keep it alive until the query ends.
///
/// A missing `agent24.db` is initialized with [`agent24_store::Store::open`]
/// (there is no old schema to upgrade — `os install`/`uninstall` must record
/// a revocation before any daemon has created the file). An existing file
/// uses [`agent24_store::Store::open_without_migration`].
pub(crate) async fn open_offline_store(
    data_dir: &Path,
) -> Result<(agent24_store::Store, std::fs::File), String> {
    let lock = acquire(data_dir)?;
    let path = data_dir.join("agent24.db");
    let opened = if path.is_file() {
        agent24_store::Store::open_without_migration(&path).await
    } else if path.exists() {
        return Err(format!(
            "could not open {}: not a database file",
            path.display()
        ));
    } else {
        agent24_store::Store::open(&path).await
    };
    Ok((opened.map_err(|err| err.to_string())?, lock))
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
