//! Daemon discovery state file (`~/.agent24/daemon.json`).
//!
//! Written by agent24d after the ready line, removed on graceful shutdown.
//! The CLI's attached mode reads it to find a running daemon. Legacy state
//! contains the bearer token; capability-mode state deliberately does not.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The authentication authority advertised by a daemon discovery record.
///
/// This is intentionally an enum rather than a boolean. Adding another mode
/// must be an explicit protocol change, and serde rejects unknown values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// The migration-era, broad daemon bearer stored in `daemon.json`.
    #[default]
    LegacySingleToken,
    /// Discovery contains endpoint metadata only; host authority stays in
    /// the trusted desktop process memory.
    Capabilities,
}

impl AuthMode {
    pub fn is_capabilities(self) -> bool {
        matches!(self, Self::Capabilities)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonState {
    pub port: u16,
    /// Absent in capability mode. `default` keeps old daemon.json files
    /// readable while `skip_serializing_if` prevents a capability record from
    /// accidentally growing an empty bearer field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    pub pid: u32,
    pub version: String,
    /// A daemon incarnation identifier. Older discovery files did not have
    /// this field, so an empty value is accepted only for legacy records.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub generation: String,
    /// Missing means the pre-capability legacy format.
    #[serde(default)]
    pub auth_mode: AuthMode,
}

impl DaemonState {
    /// Construct and validate a legacy discovery record.
    pub fn new_legacy(
        port: u16,
        token: impl Into<String>,
        pid: u32,
        version: impl Into<String>,
        generation: impl Into<String>,
    ) -> Result<Self, String> {
        let state = Self {
            port,
            token: token.into(),
            pid,
            version: version.into(),
            generation: generation.into(),
            auth_mode: AuthMode::LegacySingleToken,
        };
        state.validate().map(|()| state)
    }

    /// Construct and validate a capability discovery record. No bearer is
    /// accepted by this constructor, making the safe shape easy to use.
    pub fn new_capabilities(
        port: u16,
        pid: u32,
        version: impl Into<String>,
        generation: impl Into<String>,
    ) -> Result<Self, String> {
        let state = Self {
            port,
            token: String::new(),
            pid,
            version: version.into(),
            generation: generation.into(),
            auth_mode: AuthMode::Capabilities,
        };
        state.validate().map(|()| state)
    }

    /// Check the mutually-exclusive credential contract.
    pub fn validate(&self) -> Result<(), String> {
        match self.auth_mode {
            AuthMode::LegacySingleToken => {
                if self.token.is_empty() {
                    return Err(
                        "legacy_single_token auth mode requires a non-empty token".to_owned()
                    );
                }
            }
            AuthMode::Capabilities => {
                if !self.token.is_empty() {
                    return Err("capabilities auth mode must not contain a token".to_owned());
                }
                if self.generation.is_empty() {
                    return Err("capabilities auth mode requires a daemon generation".to_owned());
                }
            }
        }
        Ok(())
    }

    /// Return the bearer usable by a host-authority caller. Capability mode
    /// intentionally has no such value in discovery and fails closed.
    pub fn bearer_token(&self) -> Result<&str, &'static str> {
        self.validate()
            .map_err(|_| "invalid daemon discovery state")?;
        match self.auth_mode {
            AuthMode::LegacySingleToken if self.token.is_empty() => {
                Err("legacy daemon discovery token missing")
            }
            AuthMode::LegacySingleToken => Ok(&self.token),
            AuthMode::Capabilities => Err("host authority unavailable"),
        }
    }
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
    state.validate().map_err(std::io::Error::other)?;
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
    state.validate().ok()?;
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
        && state.validate().is_ok()
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

    #[test]
    fn old_json_defaults_to_legacy_mode() {
        let state: DaemonState =
            serde_json::from_str(r#"{"port":1234,"token":"old-token","pid":42,"version":"0.3.0"}"#)
                .unwrap();
        assert_eq!(state.auth_mode, AuthMode::LegacySingleToken);
        assert_eq!(state.generation, "");
        assert_eq!(state.bearer_token(), Ok("old-token"));
    }

    #[test]
    fn capability_state_omits_token_and_requires_generation() {
        let state = DaemonState::new_capabilities(1234, 42, "0.3.0", "gen-1").unwrap();
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["auth_mode"], "capabilities");
        assert_eq!(json["generation"], "gen-1");
        assert!(json.get("token").is_none());
        assert_eq!(state.bearer_token(), Err("host authority unavailable"));
    }

    #[test]
    fn auth_modes_reject_mutually_incompatible_credentials() {
        let with_token = DaemonState {
            port: 1,
            token: "secret".into(),
            pid: 1,
            version: "v".into(),
            generation: "gen".into(),
            auth_mode: AuthMode::Capabilities,
        };
        assert!(with_token.validate().is_err());
        let without_token = DaemonState {
            port: 1,
            token: String::new(),
            pid: 1,
            version: "v".into(),
            generation: "gen".into(),
            auth_mode: AuthMode::LegacySingleToken,
        };
        assert!(without_token.validate().is_err());
    }
}
