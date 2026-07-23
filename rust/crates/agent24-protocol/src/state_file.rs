//! Daemon discovery state file (`~/.agent24/daemon.json`).
//!
//! Written by agent24d after the ready line, removed on graceful shutdown.
//! The CLI's attached mode reads it to find a running daemon. Contains the
//! bearer token → created with 0600 permissions on unix.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonState {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
}

pub fn state_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".agent24"))
}

pub fn state_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join("daemon.json"))
}

/// Exclusive advisory lock guarding every write / read-check-delete sequence
/// on daemon.json — without it, an exiting old daemon could race a starting
/// new daemon and delete the newcomer's freshly-written state (TOCTOU).
/// The lock file itself is permanent and content-free.
fn hold_lock(dir: &std::path::Path) -> std::io::Result<std::fs::File> {
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("daemon.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock) // unlocks on drop
}

pub fn write(state: &DaemonState) -> std::io::Result<()> {
    let Some(dir) = state_dir() else {
        return Err(std::io::Error::other("HOME not set"));
    };
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _lock = hold_lock(&dir)?;
    let path = dir.join("daemon.json");
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    // Never a world-readable window: create the temp file 0600 from the start,
    // write, then atomically rename over the destination.
    let tmp = dir.join(format!("daemon.json.tmp.{}", state.pid));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Lifetime singleton lock for non-ephemeral daemons: acquired (non-blocking)
/// at startup and held until process exit. A second daemon fails fast instead
/// of racing — this closes the concurrent-`daemon start` double-spawn leak
/// (two CLIs both deciding "nothing running" and both spawning).
/// Returns Ok(None) when another daemon already holds the lock.
pub fn try_acquire_singleton() -> std::io::Result<Option<std::fs::File>> {
    use fs2::FileExt;
    let Some(dir) = state_dir() else {
        return Err(std::io::Error::other("HOME not set"));
    };
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("daemon.singleton.lock"))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(_) => Ok(None),
    }
}

/// Read the state file. Returns None if absent/unreadable/stale (pid dead).
/// pid liveness is ONLY a stale-file heuristic — never a kill target; daemon
/// termination goes through the authenticated /api/v1/shutdown endpoint.
pub fn read_live() -> Option<DaemonState> {
    let path = state_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let state: DaemonState = serde_json::from_str(&raw).ok()?;
    if pid_alive(state.pid) {
        Some(state)
    } else {
        None
    }
}

/// Remove the state file only if it belongs to `pid`. The read-check-delete
/// sequence runs under the same exclusive lock as `write()` — an exiting old
/// daemon can never delete a newer daemon's freshly-written state.
pub fn remove_if_owner(pid: u32) {
    let Some(dir) = state_dir() else { return };
    let Ok(_lock) = hold_lock(&dir) else { return };
    let path = dir.join("daemon.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    if let Ok(state) = serde_json::from_str::<DaemonState>(&raw)
        && state.pid == pid
    {
        let _ = std::fs::remove_file(&path);
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0: existence probe without sending anything
        unsafe_free_kill(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true // conservatively assume alive on non-unix (Windows lands later)
    }
}

#[cfg(unix)]
fn unsafe_free_kill(pid: u32) -> bool {
    // std has no direct kill(0); probe via /proc on linux or `ps` fallback.
    if std::path::Path::new(&format!("/proc/{pid}")).exists() {
        return true;
    }
    // macOS has no /proc — use ps (cheap, and only on CLI/daemon boundary paths)
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn current_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn bogus_pid_is_dead() {
        // PID_MAX on macOS is 99998; 4194304 is safely out of range on linux defaults too
        assert!(!pid_alive(4_194_303));
    }
}
