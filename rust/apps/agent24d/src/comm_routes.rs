//! COMM-2a: wires `agent24_comm::router` under `/api/v1/comm/*`
//! (COMM-HYPHAE.md D1, §2, §3).
//!
//! This is the ONLY place agent24d resolves which Hyphae binary to run and
//! builds the current [`agent24_comm::CommState`] every comm route shares.
//! Installation replaces it after draining requests and stopping its supervisor.
//! `crate::server::serve` calls [`build`] once and merges the result into
//! `module_routes` right before [`crate::server::build_router_with_modules`]
//! — comm is kernel code, mounted the same way `/api/v1/os` or
//! `/api/v1/attached` are, NOT domain-OS machinery (`crate::domain`).
//!
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent24_comm::{
    CommState, HyphaeDaemonSupervisor, HyphaeLock, HyphaeRunner, KeyringPasswordStore,
    MemoryPasswordStore, PasswordStore, VerifiedBinary, current_platform,
};

use crate::hyphae_install::InstallError;
use agent24_comm::release::HyphaeRelease;
use agent24_components::DownloadError;
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tokio_util::sync::CancellationToken;

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
pub struct CommDaemonSlot(Mutex<CommDaemonSlotState>, CancellationToken);

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
        Self(
            Mutex::new(CommDaemonSlotState::Empty),
            CancellationToken::new(),
        )
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

    /// Keep the old handle registered until its replacement is published, so
    /// shutdown can always find it while installation awaits old.shutdown().
    fn supervisor_for_rebuild(&self) -> Result<Option<Arc<HyphaeDaemonSupervisor>>, String> {
        let guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*guard {
            CommDaemonSlotState::Closed => Err("agent24d is shutting down".into()),
            CommDaemonSlotState::Registered(handle) => Ok(Some(handle.clone())),
            CommDaemonSlotState::Empty => Ok(None),
        }
    }

    /// Mark the slot closed, cancel downloads, and return the current handle.
    pub fn close(&self) -> Option<Arc<HyphaeDaemonSupervisor>> {
        self.1.cancel();
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

/// Developer overrides precede the verified component, then the sibling binary.
async fn resolve_source_path(
    state_dir: &Path,
    release: &HyphaeRelease,
    lock: &HyphaeLock,
) -> Result<Option<PathBuf>, String> {
    let primary = std::env::var_os("A24_HYPHAE_BIN").map(PathBuf::from);
    let deprecated = std::env::var_os("A24_SPEAKER_BIN").map(PathBuf::from);
    if primary.is_none() && deprecated.is_some() {
        tracing::warn!("A24_SPEAKER_BIN is deprecated; set A24_HYPHAE_BIN instead");
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("hyphae")));
    resolve_candidates(
        primary,
        deprecated,
        &release.installed_path(state_dir),
        sibling,
        lock,
        &current_platform(),
    )
    .await
}

async fn resolve_candidates(
    primary: Option<PathBuf>,
    deprecated: Option<PathBuf>,
    component: &Path,
    sibling: Option<PathBuf>,
    lock: &HyphaeLock,
    platform: &str,
) -> Result<Option<PathBuf>, String> {
    if let Some(path) = primary.or(deprecated) {
        return Ok(Some(path));
    }
    match tokio::fs::symlink_metadata(component).await {
        Ok(meta) => {
            if !meta.is_file() {
                return Err("Hyphae component must be a regular file".into());
            }
            let bytes = tokio::fs::read(component)
                .await
                .map_err(|e| e.to_string())?;
            let expected = lock.expected_for(platform).map_err(|e| e.to_string())?;
            if agent24_comm::binary::sha256_of(&bytes) != expected {
                return Err("installed Hyphae component does not match the locked SHA-256".into());
            }
            Ok(Some(component.to_path_buf()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(sibling.filter(|p| p.is_file())),
        Err(e) => Err(e.to_string()),
    }
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
    lock: &HyphaeLock,
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
    let expected = lock
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
        return Err("agent24d is shutting down".into());
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

/// Build a comm router that remains available when Hyphae is absent or rejected.
/// The shutdown slot owns the current supervisor across hot replacements.
pub async fn build(state_dir: &Path, grace: Duration, slot: &Arc<CommDaemonSlot>) -> Router {
    let release = match HyphaeRelease::embedded() {
        Ok(release) => release,
        Err(e) => {
            return unconfigured_router(CommState::unconfigured(e.to_string()));
        }
    };
    let lock = match HyphaeLock::embedded() {
        Ok(lock) => lock,
        Err(e) => {
            return unconfigured_router(CommState::binary_rejected(e.to_string()));
        }
    };
    let runtime = Arc::new(CommRuntime {
        state_dir: state_dir.to_path_buf(),
        grace,
        slot: slot.clone(),
        release,
        lock,
        password_store: select_password_store(),
        routes: RwLock::new(Router::new()),
        installation: AsyncMutex::new(()),
    });
    *runtime.routes.write().await = agent24_comm::router(runtime.build_state().await);
    runtime_router(runtime)
}

struct CommRuntime {
    state_dir: PathBuf,
    grace: Duration,
    slot: Arc<CommDaemonSlot>,
    release: HyphaeRelease,
    lock: HyphaeLock,
    password_store: Result<Arc<dyn PasswordStore>, String>,
    routes: RwLock<Router>,
    installation: AsyncMutex<()>,
}

impl CommRuntime {
    async fn build_state(&self) -> CommState {
        if let Err(reason) = &self.password_store {
            return CommState::unconfigured(reason.clone());
        }
        match self.try_build_state().await {
            Ok(state) => state,
            Err(reason) => {
                tracing::warn!("comm: {reason}");
                CommState::binary_rejected(reason)
            }
        }
    }

    async fn try_build_state(&self) -> Result<CommState, String> {
        let password_store = match &self.password_store {
            Ok(store) => store.clone(),
            Err(reason) => return Err(reason.clone()),
        };
        let Some(source) = resolve_source_path(&self.state_dir, &self.release, &self.lock).await?
        else {
            return Ok(CommState::unconfigured(
                "no Hyphae binary is configured; download and set up Hyphae",
            ));
        };
        let dir = self.state_dir.join("comm");
        let paths = CommPaths {
            home: &dir.join("hyphae-home"),
            install_dir: &dir.join("bin"),
            pid_path: &dir.join("hyphae-daemon.pid"),
            log_path: &dir.join("logs/hyphae-daemon.log"),
            autostart_path: &dir.join("daemon-autostart.json"),
        };
        build_ready_state(
            &source,
            &paths,
            password_store,
            self.grace,
            &self.slot,
            &self.lock,
        )
        .await
    }

    async fn installed_version(&self) -> Option<String> {
        // Revalidate on every status read, including after an external disk edit.
        resolve_candidates(
            None,
            None,
            &self.release.installed_path(&self.state_dir),
            None,
            &self.lock,
            &current_platform(),
        )
        .await
        .ok()
        .flatten()
        .map(|_| self.release.tag.clone())
    }

    async fn install(&self) -> Result<Value, (StatusCode, Json<Value>)> {
        let _install = self.installation.lock().await;
        let path = crate::hyphae_install::install(
            &self.release,
            &self.lock,
            &current_platform(),
            &self.state_dir,
            &self.slot.1,
        )
        .await
        .map_err(install_error)?;
        // Drain in-flight requests before shutting down the old actor. Holding
        // this guard through publication prevents requests reaching a retired state.
        let mut routes = self.routes.write().await;
        let old = self
            .slot
            .supervisor_for_rebuild()
            .map_err(install_failure)?;
        if let Some(old) = old {
            old.shutdown(tokio::time::Instant::now() + self.grace).await;
        }
        let (state, restart_required, reason) = match self.try_build_state().await {
            Ok(state) => (state, false, None),
            Err(reason) => (
                CommState::binary_rejected(reason.clone()),
                true,
                Some(reason),
            ),
        };
        *routes = agent24_comm::router(state);
        Ok(json!({"ok": true, "data": {
            "installed_version": self.release.tag, "path": path,
            "restart_required": restart_required, "reason": reason,
        }}))
    }
}

fn unconfigured_router(state: CommState) -> Router {
    Router::new().nest("/api/v1/comm", agent24_comm::router(state))
}

// Preserve every MatchedPath for CORE-2 authorization while requests use the
// current comm instance. A fallback would erase those fixed route templates.
fn runtime_router(runtime: Arc<CommRuntime>) -> Router {
    let routes = agent24_comm::router(CommState::unconfigured("initializing"))
        .route_layer(middleware::from_fn_with_state(
            runtime.clone(),
            forward_comm,
        ))
        .route("/install", post(install_component).with_state(runtime));
    Router::new().nest("/api/v1/comm", routes)
}

async fn forward_comm(
    State(runtime): State<Arc<CommRuntime>>,
    req: Request,
    _next: Next,
) -> Response {
    if matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && req.uri().path() == "/daemon"
    {
        return component_status(State(runtime)).await;
    }
    let routes = runtime.routes.read().await;
    use tower::ServiceExt;
    match routes.clone().oneshot(req).await {
        Ok(res) => res,
        Err(e) => match e {},
    }
}

async fn component_status(State(runtime): State<Arc<CommRuntime>>) -> Response {
    let routes = runtime.routes.read().await;
    use tower::ServiceExt;
    let mut request = Request::new(Body::empty());
    *request.uri_mut() = axum::http::Uri::from_static("/daemon");
    let response = match routes.clone().oneshot(request).await {
        Ok(res) => res,
        Err(e) => match e {},
    };
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let mut value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        value = json!({"ok": true, "data": {
            "process": {"state": "not_configured", "generation": 0, "consecutive_failures": 0,
                "reason": value["error"]},
            "relay_probe": null, "catch_up": {"state": "unknown", "last_incomplete_at_ms": null},
        }});
    }
    value["data"]["installable"] = runtime
        .release
        .asset_for(&runtime.lock, &current_platform())
        .is_ok()
        .into();
    value["data"]["installed_version"] = json!(runtime.installed_version().await);
    Json(value).into_response()
}

async fn install_component(State(runtime): State<Arc<CommRuntime>>) -> Response {
    // Once authorized, finish publication even if the HTTP client disconnects.
    match tokio::spawn(async move { runtime.install().await }).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => install_failure("component installation task failed".into()).into_response(),
    }
}

fn install_failure(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"ok": false, "error": "hyphae_download_failed", "message": message})),
    )
}

fn install_error(error: InstallError) -> (StatusCode, Json<Value>) {
    let (status, code) = match &error {
        InstallError::Download(DownloadError::UnsupportedPlatform) => {
            (StatusCode::CONFLICT, "hyphae_unsupported_platform")
        }
        InstallError::Download(DownloadError::HashMismatch)
        | InstallError::Binary(agent24_comm::BinaryError::HashMismatch { .. }) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "hyphae_hash_mismatch")
        }
        _ => (StatusCode::BAD_GATEWAY, "hyphae_download_failed"),
    };
    (
        status,
        Json(json!({"ok": false, "error": code, "message": error.to_string()})),
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

    #[tokio::test]
    async fn shutdown_can_still_find_old_supervisor_during_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CommDaemonSlot::new();
        let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(test_ctx(tmp.path()).await));
        assert!(slot.register(daemon.clone()));
        assert!(slot.supervisor_for_rebuild().unwrap().is_some());
        assert!(
            slot.close().is_some(),
            "shutdown must retain visibility while an install awaits old.shutdown()"
        );
        daemon.shutdown(tokio::time::Instant::now()).await;
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

#[cfg(test)]
mod hy1_regression_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn missing_binary_status_offers_install_without_restarting_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let slot = Arc::new(CommDaemonSlot::new());
        let app = build(dir.path(), Duration::from_millis(50), &slot).await;
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/comm/daemon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["data"]["installable"].is_boolean());
        assert!(value["data"]["installed_version"].is_null());
    }
    const FIXTURE: &[u8] = b"#!/bin/sh\necho '{\"ok\":true,\"data\":[]}'\n";

    async fn runtime(dir: &Path) -> Arc<CommRuntime> {
        let platform = current_platform();
        let lock =
            HyphaeLock::override_for_test(&platform, agent24_comm::binary::sha256_of(FIXTURE));
        let mut release = HyphaeRelease::embedded().unwrap();
        release
            .assets
            .insert(platform, Some("fixture-hyphae".into()));
        let runtime = Arc::new(CommRuntime {
            state_dir: dir.to_path_buf(),
            grace: Duration::from_millis(50),
            slot: Arc::new(CommDaemonSlot::new()),
            release,
            lock,
            password_store: Ok(Arc::new(MemoryPasswordStore::new())),
            routes: RwLock::new(agent24_comm::router(CommState::unconfigured(
                "no Hyphae binary is configured",
            ))),
            installation: AsyncMutex::new(()),
        });
        let cache = dir.join("idoris/component-cache/hyphae");
        tokio::fs::create_dir_all(&cache).await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                dir.join("idoris"),
                dir.join("idoris/component-cache"),
                cache.clone(),
            ] {
                tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .await
                    .unwrap();
            }
        }
        tokio::fs::write(cache.join("fixture-hyphae.verified"), FIXTURE)
            .await
            .unwrap();
        runtime
    }

    async fn request(app: &Router, method: &str, path: &str) -> (StatusCode, Value) {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn install_api_hot_rebuilds_not_configured_and_reinstall_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        let app = runtime_router(runtime.clone());
        let (_, before) = request(&app, "GET", "/api/v1/comm/identity").await;
        assert_eq!(before["error"], "not_configured");
        for _ in 0..2 {
            let (status, installed) = request(&app, "POST", "/api/v1/comm/install").await;
            assert_eq!(status, StatusCode::OK, "{installed}");
            assert_eq!(installed["data"]["restart_required"], false);
            assert_eq!(installed["data"]["installed_version"], "v0.27.2");
            assert_eq!(
                tokio::fs::read(runtime.release.installed_path(dir.path()))
                    .await
                    .unwrap(),
                FIXTURE
            );
            let (status, identities) = request(&app, "GET", "/api/v1/comm/identity").await;
            assert_eq!(status, StatusCode::OK, "{identities}");
            assert_eq!(identities, json!({"ok":true,"data":[]}));
            let (status, process) = request(&app, "GET", "/api/v1/comm/daemon").await;
            assert_eq!(status, StatusCode::OK);
            assert_ne!(process["data"]["process"]["state"], "not_configured");
            assert_eq!(process["data"]["installed_version"], "v0.27.2");
        }
        if let Some(handle) = runtime.slot.close() {
            handle.shutdown(tokio::time::Instant::now()).await;
        }
    }

    #[tokio::test]
    async fn concurrent_installs_serialize_and_both_publish_one_verified_binary() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        let app = runtime_router(runtime.clone());
        let (a, b) = tokio::join!(
            request(&app, "POST", "/api/v1/comm/install"),
            request(&app, "POST", "/api/v1/comm/install"),
        );
        for (status, body) in [a, b] {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["data"]["restart_required"], false);
        }
        let install_dir = runtime.release.install_dir(dir.path());
        assert!(!install_dir.join("hyphae.installing").exists());
        assert_eq!(
            tokio::fs::read(runtime.release.installed_path(dir.path()))
                .await
                .unwrap(),
            FIXTURE
        );
        let (status, identities) = request(&app, "GET", "/api/v1/comm/identity").await;
        assert_eq!(status, StatusCode::OK, "{identities}");
        if let Some(handle) = runtime.slot.close() {
            handle.shutdown(tokio::time::Instant::now()).await;
        }
    }

    #[tokio::test]
    async fn install_api_rejects_hash_mismatch_and_status_does_not_claim_installed() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        let installed = runtime.release.installed_path(dir.path());
        tokio::fs::create_dir_all(installed.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&installed, b"tampered").await.unwrap();
        let app = runtime_router(runtime);
        let (status, result) = request(&app, "POST", "/api/v1/comm/install").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(result["error"], "hyphae_hash_mismatch");
        let (_, result) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(result["data"]["installed_version"], Value::Null);
    }

    #[tokio::test]
    async fn install_api_null_lock_platform_is_unsupported_and_never_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = runtime(dir.path()).await;
        Arc::get_mut(&mut runtime)
            .unwrap()
            .lock
            .binaries
            .insert(current_platform(), None);
        let app = runtime_router(runtime);
        let (_, before) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(before["data"]["installable"], false);
        let (status, result) = request(&app, "POST", "/api/v1/comm/install").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(result["error"], "hyphae_unsupported_platform");
        assert!(!dir.path().join("components/hyphae").exists());
    }

    #[tokio::test]
    async fn install_api_download_failure_preserves_existing_comm_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = runtime(dir.path()).await;
        Arc::get_mut(&mut runtime).unwrap().release.release_url_base =
            "https://untrusted.invalid/release".into();
        tokio::fs::remove_file(
            dir.path()
                .join("idoris/component-cache/hyphae/fixture-hyphae.verified"),
        )
        .await
        .unwrap();
        let app = runtime_router(runtime);
        let (status, result) = request(&app, "POST", "/api/v1/comm/install").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(result["error"], "hyphae_download_failed");
        let (_, result) = request(&app, "GET", "/api/v1/comm/identity").await;
        assert_eq!(result["error"], "not_configured");
    }

    #[tokio::test]
    async fn resolution_honors_overrides_and_reverifies_components_before_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let component = dir.path().join("component");
        let sibling = dir.path().join("sibling");
        let primary = dir.path().join("primary");
        let deprecated = dir.path().join("deprecated");
        let platform = current_platform();
        let lock =
            HyphaeLock::override_for_test(&platform, agent24_comm::binary::sha256_of(FIXTURE));
        tokio::fs::write(&component, b"tampered").await.unwrap();
        tokio::fs::write(&sibling, FIXTURE).await.unwrap();
        assert_eq!(
            resolve_candidates(
                Some(primary.clone()),
                Some(deprecated.clone()),
                &component,
                Some(sibling.clone()),
                &lock,
                &platform
            )
            .await
            .unwrap(),
            Some(primary)
        );
        assert_eq!(
            resolve_candidates(
                None,
                Some(deprecated.clone()),
                &component,
                Some(sibling.clone()),
                &lock,
                &platform
            )
            .await
            .unwrap(),
            Some(deprecated)
        );
        assert!(
            resolve_candidates(
                None,
                None,
                &component,
                Some(sibling.clone()),
                &lock,
                &platform
            )
            .await
            .is_err()
        );
        tokio::fs::write(&component, FIXTURE).await.unwrap();
        assert_eq!(
            resolve_candidates(
                None,
                None,
                &component,
                Some(sibling.clone()),
                &lock,
                &platform
            )
            .await
            .unwrap(),
            Some(component.clone())
        );
        tokio::fs::remove_file(&component).await.unwrap();
        assert_eq!(
            resolve_candidates(
                None,
                None,
                &component,
                Some(sibling.clone()),
                &lock,
                &platform
            )
            .await
            .unwrap(),
            Some(sibling)
        );
    }

    #[tokio::test]
    async fn closed_slot_refuses_install_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        runtime.slot.close();
        let app = runtime_router(runtime);
        let (status, result) = request(&app, "POST", "/api/v1/comm/install").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(result["error"], "hyphae_download_failed");
    }
}
