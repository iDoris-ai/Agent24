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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent24_comm::{
    CommState, HyphaeDaemonSupervisor, HyphaeLock, HyphaeRunner, KeyringPasswordStore,
    MemoryPasswordStore, PasswordStore, VerifiedBinary, current_platform,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Where `agent24d`'s own SHUT-1b stop sequence finds the Hyphae daemon
/// supervisor `build()` may create — and the ONLY thing that makes "a
/// shutdown landing while `build()` is still running" safe (PR #626 review,
/// High #1).
///
/// Before this existed, the supervisor's handle was registered into a bare
/// `OnceLock` only AFTER `build_ready_state` had already attempted autostart
/// — so a shutdown that began early enough to find the cell still empty
/// finished its whole stop sequence believing comm had no process, while
/// `build()` went on to actually spawn the Hyphae daemon and only
/// afterward handed over a handle nobody was ever going to read again. The
/// process this created outlives `agent24d`'s own exit, undetected until
/// the NEXT `agent24d`'s orphan reap — exactly the SIGTERM-during-startup
/// race COMM-HYPHAE.md §6.1 relies on orphan reap to catch, except here it
/// was reachable on every ordinary shutdown, not just a crash.
///
/// The fix is a single lock shared between [`Self::register`] (called by
/// `build_ready_state` right after the supervisor actor is spawned, BEFORE
/// any autostart attempt) and [`Self::close`] (called once, by the
/// shutdown task): whichever gets there first decides what the other sees.
/// A `close` that lands first leaves a `register` that comes after it
/// refusing to authorize autostart; a `register` that lands first leaves
/// the handle somewhere `close` will always find it, no matter how much
/// later the shutdown itself begins. Racing a `start()` against a
/// `shutdown()` AFTER both have found each other through this lock is
/// still possible but is safe either way: both end up as `Cmd`s in the same
/// actor's single mailbox, so the actor either stops what it just started
/// or its mailbox is already closed and `start()` fails cleanly without
/// spawning anything (`agent24-comm`'s own `daemon.rs` doc comments cover
/// that part).
pub struct CommDaemonSlot(Mutex<CommDaemonSlotState>);

enum CommDaemonSlotState {
    Empty,
    Registered(Arc<HyphaeDaemonSupervisor>),
    /// A shutdown already ran its `close()` and found nothing — any
    /// `register` from here on must refuse to authorize an autostart.
    Closed,
}

impl CommDaemonSlot {
    #[must_use]
    pub fn new() -> Self {
        Self(Mutex::new(CommDaemonSlotState::Empty))
    }

    /// Called once by `build_ready_state`, right after the supervisor actor
    /// is spawned and before any autostart attempt. `true`: autostart may
    /// proceed. `false`: a shutdown already closed this slot — the caller
    /// must not autostart, and should call `handle.shutdown()` itself to
    /// tear down the (still process-less) actor it just spawned.
    fn register(&self, handle: Arc<HyphaeDaemonSupervisor>) -> bool {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *guard {
            CommDaemonSlotState::Closed => false,
            _ => {
                *guard = CommDaemonSlotState::Registered(handle);
                true
            }
        }
    }

    /// Called once by `agent24d`'s own SHUT-1b stop sequence. Marks the
    /// slot closed — so a `register` still in flight (or yet to run) is
    /// refused — and returns whatever handle was already registered.
    pub fn close(&self) -> Option<Arc<HyphaeDaemonSupervisor>> {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match std::mem::replace(&mut *guard, CommDaemonSlotState::Closed) {
            CommDaemonSlotState::Registered(handle) => Some(handle),
            _ => None,
        }
    }
}

impl Default for CommDaemonSlot {
    fn default() -> Self {
        Self::new()
    }
}

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

/// Every on-disk path `build_ready_state` needs, bundled so the function
/// itself stays under clippy's argument-count lint.
struct CommPaths<'a> {
    home: &'a Path,
    install_dir: &'a Path,
    pid_path: &'a Path,
    log_path: &'a Path,
    autostart_path: &'a Path,
}

async fn build_ready_state(
    source: &Path,
    paths: &CommPaths<'_>,
    password_store: Arc<dyn PasswordStore>,
    // PR #626 review, High #2: the SAME already-validated grace every other
    // out-of-process module's stop uses (`lifecycle::Params::stop_grace`,
    // read ONCE in `server::serve`) — never this function's own read of the
    // raw env var, which used to disagree with SHUT-1b's own clamped
    // default (500ms vs. this crate's unvalidated 5s).
    grace: Duration,
    slot: &CommDaemonSlot,
) -> Result<CommState, String> {
    let CommPaths {
        home,
        install_dir,
        pid_path,
        log_path,
        autostart_path,
    } = *paths;
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
    agent24_comm::reap_orphan(pid_path, grace).await;

    let daemon = Arc::new(agent24_comm::HyphaeDaemonSupervisor::spawn(
        agent24_comm::DaemonCtx {
            runner: runner.clone(),
            password_store: password_store.clone(),
            home: home.to_path_buf(),
            pid_path: pid_path.to_path_buf(),
            log_path: log_path.to_path_buf(),
            autostart_path: autostart_path.to_path_buf(),
            grace,
            ready_after: agent24_comm::READY_AFTER_DEFAULT,
        },
    ));
    // PR #626 review, High #1: registered BEFORE any autostart attempt, and
    // synchronized (via `slot`'s own lock) with agent24d's own SHUT-1b
    // closing action — see `CommDaemonSlot`'s doc comment. A shutdown that
    // landed before this call already closed the slot; this `register`
    // then refuses to authorize autostart for the (still process-less)
    // actor just spawned above, which is shut down immediately instead.
    if !slot.register(daemon.clone()) {
        tracing::warn!(
            "comm: agent24d is already shutting down; the hyphae daemon supervisor just \
             created for this build will not be autostarted"
        );
        let _ = daemon.shutdown(tokio::time::Instant::now()).await;
        return Ok(CommState::ready(runner, password_store, home.to_path_buf()).with_daemon(daemon));
    }
    // COMM-HYPHAE.md §6.2: "agent24d 启动时，autostart=true 就自动拉起" — a
    // best-effort attempt; a failure here (e.g. the keystore password is
    // not currently retrievable) is logged, not fatal, same as every other
    // comm degrade-gracefully path in this file.
    if agent24_comm::read_autostart(autostart_path).await
        && let Err(e) = daemon.start().await
    {
        tracing::warn!("comm: autostart failed to start the hyphae daemon: {e}");
    }
    Ok(CommState::ready(runner, password_store, home.to_path_buf()).with_daemon(daemon))
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
    // PR #626 review, High #2: `server::serve`'s own already-validated
    // `lifecycle::Params::stop_grace` — see `build_ready_state`'s doc
    // comment on the parameter of the same name.
    grace: Duration,
    // PR #626 review, High #1: owned by `server::serve`, which also hands
    // it to its own shutdown task — see `CommDaemonSlot`'s doc comment.
    // The supervisor handle is no longer returned from this function at
    // all: registering it into `slot` (inside `build_ready_state`, before
    // any autostart) is what makes it discoverable, race-free, by a
    // shutdown that began before `build` even returns.
    slot: &CommDaemonSlot,
) -> axum::Router {
    let comm_dir = state_dir.join("comm");
    let home = comm_dir.join("hyphae-home");
    let install_dir = comm_dir.join("bin");
    let pid_path = comm_dir.join("hyphae-daemon.pid");
    let log_path = comm_dir.join("logs").join("hyphae-daemon.log");
    let autostart_path = comm_dir.join("daemon-autostart.json");
    let state = match select_password_store() {
        Err(reason) => {
            tracing::warn!("comm: {reason}");
            CommState::unconfigured(reason)
        }
        Ok(password_store) => match resolve_source_path() {
            None => CommState::unconfigured(
                "no Hyphae binary is configured; set A24_HYPHAE_BIN (COMM-HYPHAE.md §3)".to_owned(),
            ),
            Some(source) => {
                let paths = CommPaths {
                    home: &home,
                    install_dir: &install_dir,
                    pid_path: &pid_path,
                    log_path: &log_path,
                    autostart_path: &autostart_path,
                };
                match build_ready_state(&source, &paths, password_store, grace, slot).await {
                    Ok(state) => state,
                    Err(reason) => {
                        tracing::warn!("comm: {reason}");
                        CommState::binary_rejected(reason)
                    }
                }
            }
        },
    };
    // `agent24_comm::router` returns routes relative to wherever it is
    // mounted (`/identity`, `/relay`, …) — `nest` is what actually puts
    // them at `/api/v1/comm/*`, not `merge` (which would leave them at
    // their bare, un-prefixed paths and collide with nothing — silently).
    axum::Router::new().nest("/api/v1/comm", agent24_comm::router(state))
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

/// PR #626 review, High #1: `CommDaemonSlot`'s whole job is to make the
/// outcome of a `register`/`close` race depend on ORDER, never on which one
/// happened to look first and guess wrong. Proven directly and
/// deterministically here, independent of `build()`'s own env-var-driven
/// binary/password-store resolution (which this test module cannot safely
/// exercise: `select_password_store`/`resolve_source_path` read PROCESS-wide
/// env vars that every other test in this same binary shares, and the
/// embedded `hyphae.lock.json` this crate verifies real binaries against
/// has no fake-script equivalent a unit test could satisfy) — the actual
/// "autostart" decision in `build_ready_state` is a single `if
/// !slot.register(..) { ... }`, so proving the slot's two orderings here is
/// what proves that decision correct.
#[cfg(test)]
mod comm_daemon_slot_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A `DaemonCtx` good enough to spawn the supervisor ACTOR (an async
    /// task that does nothing until told to `start`/`shutdown`) — never
    /// good enough to actually run anything, which these tests never ask it
    /// to.
    async fn test_ctx(dir: &std::path::Path) -> agent24_comm::DaemonCtx {
        let script = dir.join("fake-hyphae");
        tokio::fs::write(&script, b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        let bytes = tokio::fs::read(&script).await.unwrap();
        let expected = agent24_comm::binary::sha256_of(&bytes);
        let bin = VerifiedBinary::install(&script, expected, &dir.join("bin"))
            .await
            .unwrap();
        let home = dir.join("home");
        agent24_comm::DaemonCtx {
            runner: Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5))),
            password_store: Arc::new(MemoryPasswordStore::new()),
            home,
            pid_path: dir.join("hyphae-daemon.pid"),
            log_path: dir.join("logs").join("hyphae-daemon.log"),
            autostart_path: dir.join("daemon-autostart.json"),
            grace: Duration::from_millis(500),
            ready_after: Duration::from_millis(50),
        }
    }

    /// Case 1 — `register` wins (the ordinary case: no shutdown is
    /// underway). A `close` running afterward (standing in for agent24d's
    /// own SHUT-1b stop sequence) must find the handle.
    #[tokio::test]
    async fn register_before_close_hands_the_handle_to_close() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CommDaemonSlot::new();
        let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(test_ctx(tmp.path()).await));

        assert!(
            slot.register(daemon.clone()),
            "the first register, with nothing having closed the slot yet, must be allowed to \
             autostart"
        );
        assert!(
            slot.close().is_some(),
            "a shutdown that runs AFTER register must find the handle that was registered"
        );
    }

    /// Case 2 — `close` wins (the SIGTERM-during-startup scenario: a
    /// shutdown lands before `build()` has even created the supervisor). The
    /// `register` that runs afterward must be refused — this `false` is the
    /// ONE thing `build_ready_state` reads to skip autostart, which is what
    /// keeps this scenario from orphaning a Hyphae process nothing will
    /// ever be told to stop.
    #[tokio::test]
    async fn close_before_register_forbids_the_late_register_from_authorizing_autostart() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CommDaemonSlot::new();

        assert!(
            slot.close().is_none(),
            "nothing was registered yet, so a shutdown this early has nothing to stop"
        );

        let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(test_ctx(tmp.path()).await));
        assert!(
            !slot.register(daemon.clone()),
            "a register that runs AFTER a close must be refused"
        );
        // `build_ready_state`'s own response to that `false`: shut the
        // (still process-less) actor down immediately rather than ever
        // calling `start()` on it.
        let outcome = daemon.shutdown(tokio::time::Instant::now()).await;
        assert!(
            !outcome.had_process,
            "nothing was ever started, so there is nothing to report as a process"
        );
    }
}
