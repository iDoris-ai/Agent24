//! COMM-2a: wires `agent24_comm::router` under `/api/v1/comm/*`
//! (COMM-HYPHAE.md D1, §2, §3).
//!
//! This is the ONLY place agent24d resolves which Hyphae binary to run and
//! builds the one [`agent24_comm::CommState`] every comm route shares.
//! `crate::server::serve` calls [`build`] once and merges the result into
//! `module_routes` right before [`crate::server::build_router_with_modules`]
//! — comm is kernel code, mounted the same way `/api/v1/os` or
//! `/api/v1/attached` are, NOT domain-OS machinery (`crate::domain`).
//!
//! **Known gap**: unlike the kernel's own literal routes, `/api/v1/comm` is
//! not listed in `crate::domain::RESERVED_KERNEL_SEGMENTS` — that list is
//! pinned 1:1 against literal `"/api/v1/..."` strings found inside
//! `build_router_with_modules`'s own source
//! (`reserved_segments_match_the_kernel_routes_exactly`), and comm is merged
//! in from `serve` instead, one call site up, to keep this wiring in its own
//! file per COMM-2a's own brief ("只在 server 中加挂载点"). A domain-OS
//! package literally named `comm` would collide with this mount at merge
//! time (an axum panic at startup, not a silent takeover) — worth closing
//! when COMM-4a (daemon supervision) touches this file again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    CommState, HyphaeLock, HyphaeRunner, KeyringPasswordStore, MemoryPasswordStore, PasswordStore,
    VerifiedBinary, current_platform,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Resolves the Hyphae binary path per COMM-HYPHAE.md §3: `A24_HYPHAE_BIN`,
/// then the deprecated `A24_SPEAKER_BIN` (logged once), then a `hyphae`
/// binary next to this executable. `None` means comm was never configured
/// at all — a DIFFERENT fact from "a path was given and failed
/// verification" (see `agent24_comm::CommState`'s own doc comment), and
/// `build` keeps the two distinguishable in the state it builds.
fn resolve_source_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("A24_HYPHAE_BIN") {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("A24_SPEAKER_BIN") {
        tracing::warn!(
            "A24_SPEAKER_BIN is deprecated; set A24_HYPHAE_BIN instead (COMM-HYPHAE.md §3, D8)"
        );
        return Some(PathBuf::from(path));
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("hyphae")))
        .filter(|p| p.exists())
}

async fn build_ready_state(
    source: &Path,
    home: &Path,
    install_dir: &Path,
    password_store: Arc<dyn PasswordStore>,
    pid_path: &Path,
    log_path: &Path,
) -> Result<(CommState, Arc<agent24_comm::HyphaeDaemonSupervisor>), String> {
    let source = source
        .canonicalize()
        .map_err(|e| format!("Hyphae binary {source:?} is not readable: {e}"))?;
    tokio::fs::create_dir_all(home)
        .await
        .map_err(|e| format!("creating {}: {e}", home.display()))?;
    let platform = current_platform();
    let expected = HyphaeLock::embedded()
        .map_err(|e| format!("embedded hyphae.lock.json is invalid: {e}"))?
        .expected_for(&platform)
        .map_err(|e| format!("no verified Hyphae binary for platform {platform}: {e}"))?;
    let bin = VerifiedBinary::install(&source, expected, install_dir)
        .await
        .map_err(|e| format!("Hyphae binary verification failed: {e}"))?;
    let runner = Arc::new(HyphaeRunner::new(bin, home.to_path_buf(), DEFAULT_TIMEOUT));
    // COMM-4a §6.1 "孤儿识别": before this `agent24d` ever spawns a Hyphae
    // daemon of its own, clean up one left behind by a previous `agent24d`
    // that crashed or was SIGKILLed — pid alive AND start time unchanged,
    // never by name.
    agent24_comm::reap_orphan(pid_path, agent24_comm::stop_grace()).await;

    let daemon = Arc::new(agent24_comm::HyphaeDaemonSupervisor::spawn(
        agent24_comm::DaemonCtx {
            runner: runner.clone(),
            password_store: password_store.clone(),
            home: home.to_path_buf(),
            pid_path: pid_path.to_path_buf(),
            log_path: log_path.to_path_buf(),
        },
    ));
    let state =
        CommState::ready(runner, password_store, home.to_path_buf()).with_daemon(daemon.clone());
    Ok((state, daemon))
}

/// Which `PasswordStore` backend `A24_COMM_PASSWORD_STORE` selects
/// (COMM-HYPHAE.md §6.4). `Keyring` is the default — both when the variable
/// is unset and when it is set to `"keyring"` explicitly. An unrecognized
/// value is kept distinguishable from both real choices so `build` can
/// refuse it outright rather than silently falling back to either one.
#[derive(Debug, PartialEq, Eq)]
enum PasswordStoreChoice {
    Keyring,
    Memory,
    Invalid(String),
}

/// Pure classifier, independent of `std::env`, so a test can exercise every
/// branch without mutating process-global environment state (which would
/// race against every other `#[test]` in this binary).
fn classify_password_store_choice(raw: Option<&std::ffi::OsStr>) -> PasswordStoreChoice {
    let Some(raw) = raw else {
        return PasswordStoreChoice::Keyring;
    };
    match raw.to_str() {
        Some("keyring") => PasswordStoreChoice::Keyring,
        Some("memory") => PasswordStoreChoice::Memory,
        Some(other) => PasswordStoreChoice::Invalid(other.to_owned()),
        None => PasswordStoreChoice::Invalid(raw.to_string_lossy().into_owned()),
    }
}

/// Reads `A24_COMM_PASSWORD_STORE` and builds the matching store, or returns
/// the reason it couldn't (an unrecognized value — never silently keyring or
/// memory, COMM-HYPHAE.md §6.4).
fn select_password_store() -> Result<Arc<dyn PasswordStore>, String> {
    match classify_password_store_choice(std::env::var_os("A24_COMM_PASSWORD_STORE").as_deref()) {
        PasswordStoreChoice::Keyring => Ok(Arc::new(KeyringPasswordStore::new())),
        PasswordStoreChoice::Memory => {
            tracing::warn!(
                "comm: A24_COMM_PASSWORD_STORE=memory — the Hyphae keystore password is kept \
                 in memory only and will be lost on every daemon restart; this is for tests or \
                 ad-hoc joint debugging ONLY, never for production (COMM-HYPHAE.md §6.4)"
            );
            Ok(Arc::new(MemoryPasswordStore::new()))
        }
        PasswordStoreChoice::Invalid(value) => Err(format!(
            "A24_COMM_PASSWORD_STORE={value:?} is not a recognized password store backend \
             (expected \"keyring\" or \"memory\", COMM-HYPHAE.md §6.4)"
        )),
    }
}

/// Builds the comm router. Never fails and never stops `agent24d` from
/// starting: an unresolvable or unverifiable binary, or a misconfigured
/// `A24_COMM_PASSWORD_STORE`, degrades to a state whose every route reports
/// `not_configured`/`binary_rejected` instead (COMM-2a's own brief —
/// "不能让 daemon 起不来"). The second element is COMM-4a's daemon
/// supervisor handle, `None` on the degraded paths — kept by the caller
/// (`server.rs`) so its own shutdown sequence can stop the Hyphae daemon as
/// part of SHUT-1b.
pub async fn build(
    state_dir: &Path,
) -> (
    axum::Router,
    Option<Arc<agent24_comm::HyphaeDaemonSupervisor>>,
) {
    let comm_dir = state_dir.join("comm");
    let home = comm_dir.join("hyphae-home");
    let install_dir = comm_dir.join("bin");
    let pid_path = comm_dir.join("hyphae-daemon.pid");
    let log_path = comm_dir.join("logs").join("hyphae-daemon.log");
    let (state, daemon) = match select_password_store() {
        Err(reason) => {
            tracing::warn!("comm: {reason}");
            (CommState::unconfigured(reason), None)
        }
        Ok(password_store) => match resolve_source_path() {
            None => (
                CommState::unconfigured(
                    "no Hyphae binary is configured; set A24_HYPHAE_BIN (COMM-HYPHAE.md §3)"
                        .to_owned(),
                ),
                None,
            ),
            Some(source) => {
                match build_ready_state(
                    &source,
                    &home,
                    &install_dir,
                    password_store,
                    &pid_path,
                    &log_path,
                )
                .await
                {
                    Ok((state, daemon)) => (state, Some(daemon)),
                    Err(reason) => {
                        tracing::warn!("comm: {reason}");
                        (CommState::binary_rejected(reason), None)
                    }
                }
            }
        },
    };
    // `agent24_comm::router` returns routes relative to wherever it is
    // mounted (`/identity`, `/relay`, …) — `nest` is what actually puts
    // them at `/api/v1/comm/*`, not `merge` (which would leave them at
    // their bare, un-prefixed paths and collide with nothing — silently).
    (
        axum::Router::new().nest("/api/v1/comm", agent24_comm::router(state)),
        daemon,
    )
}

#[cfg(test)]
mod password_store_choice_tests {
    use super::*;

    #[test]
    fn unset_defaults_to_keyring() {
        assert_eq!(
            classify_password_store_choice(None),
            PasswordStoreChoice::Keyring
        );
    }

    #[test]
    fn explicit_keyring_selects_keyring() {
        assert_eq!(
            classify_password_store_choice(Some(std::ffi::OsStr::new("keyring"))),
            PasswordStoreChoice::Keyring
        );
    }

    #[test]
    fn memory_selects_memory() {
        assert_eq!(
            classify_password_store_choice(Some(std::ffi::OsStr::new("memory"))),
            PasswordStoreChoice::Memory
        );
    }

    #[test]
    fn unrecognized_value_is_invalid_not_a_silent_fallback() {
        assert_eq!(
            classify_password_store_choice(Some(std::ffi::OsStr::new("bogus"))),
            PasswordStoreChoice::Invalid("bogus".to_owned())
        );
    }
}
