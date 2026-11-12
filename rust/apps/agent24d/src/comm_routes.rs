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
    CommState, ContractExpectation, HyphaeDaemonSupervisor, HyphaeRunner, KeyringPasswordStore,
    MemoryPasswordStore, PasswordStore, ProbeArm, VerifiedBinary, current_platform,
};

use crate::hyphae_install::{HyphaeLock, InstallError, Verifier};
use agent24_components::{DownloadError, VerifyMode, install::InstallError as SignError};
use axum::{
    Json, Router,
    body::Body,
    extract::{MatchedPath, NestedPath, OriginalUri, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tokio_util::sync::CancellationToken;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Lock `contract` plus the production health period. `build_ready_state`
/// arms every supervisor with this before autostart.
fn hyphae_probe_arm(lock: &HyphaeLock) -> ProbeArm {
    let contract = &lock.component().contract;
    ProbeArm::production(ContractExpectation {
        name: contract.name.clone(),
        major: contract.major,
        min_minor: contract.min_minor,
        tested: contract.tested.clone(),
    })
    .with_failures_before_degraded(health_failure_threshold(
        lock.component().health_failures_before_degraded,
        "hyphae",
    ))
}

/// Bounds for the lock's `health_failures_before_degraded`. One failed sample
/// is already a lossy-link false alarm (ADR-038 §7.3), and more than ten samples
/// at 45 s would take over seven minutes to notice a dead relay.
const HEALTH_FAILURES_MIN: u32 = 1;
const HEALTH_FAILURES_MAX: u32 = 10;

/// Lock value, clamped to `[1, 10]`. No lock value keeps the ADR default (3).
fn health_failure_threshold(lock_value: Option<u32>, component: &str) -> u32 {
    let Some(requested) = lock_value else {
        return agent24_comm::HEALTH_FAILURES_BEFORE_DEGRADED;
    };
    let clamped = requested.clamp(HEALTH_FAILURES_MIN, HEALTH_FAILURES_MAX);
    if clamped != requested {
        tracing::warn!(
            component,
            requested,
            used = clamped,
            "component health_failures_before_degraded is out of range; clamped"
        );
    }
    clamped
}

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
pub struct CommDaemonSlot(
    Mutex<CommDaemonSlotState>,
    CancellationToken,
    Mutex<BuildNote>,
    /// Stop grace actually given to the Hyphae supervisor. The lock's
    /// `stop_grace_ms`, or the daemon's global budget when that is shorter.
    Mutex<Duration>,
    /// Times this slot has locked [`BuildNote`]. Unit tests assert one GET
    /// takes the lock once for both marks. Kept in release builds: the
    /// route-catalog source scan cannot skip `#[cfg(test)]` on a tuple field.
    std::sync::atomic::AtomicUsize,
    /// CLI the inbound poller uses. Set when a verified binary is ready.
    /// Cleared when the next build finds Hyphae unconfigured or fails.
    /// The poller clones this at the start of every round.
    Mutex<Option<Arc<dyn crate::inbound::HyphaeCli>>>,
);

/// What the last comm-state build left for `/api/v1/components`.
/// A D2a verdict and a generic start failure are different signals:
/// `rejection` is also `GET /api/v1/comm/daemon`'s `data.rejection`
/// (ADR-038 §7.11), and that field stays null when the binary was not refused.
#[derive(Debug, Default)]
struct BuildNote {
    rejection: Option<Rejection>,
    /// The build failed, but not with a binary verdict (missing file, I/O).
    /// Host view: `{state: "failed", reason: "start_failed"}`.
    start_failed: bool,
}

/// Why the last build of the comm state (daemon start, or the rebuild after a host
/// install) refused the binary it found. A stable, path-free code:
/// `GET /api/v1/components` (host view) shows it as the Hyphae item's `reason` with
/// state `failed`, and `GET /api/v1/comm/daemon` as `data.rejection`. Set and cleared
/// only by those builds: the next one that does not refuse a binary (a successful
/// start, or nothing installed) clears it. Fixing the file on disk is therefore seen at
/// the next start or install, not before; an install that fails before its rebuild
/// leaves it as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    /// The bytes matched the lock but D2a refused them (codesign requirement, Team ID,
    /// Hardened Runtime, or the checker could not run).
    SignatureRejected,
    /// The binary's SHA-256 is not the one the lock pins.
    HashMismatch,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignatureRejected => "signature_rejected",
            Self::HashMismatch => "hash_mismatch",
        }
    }
}

/// One read of `BuildNote`. The two marks are written together, so a GET
/// has to observe them together. Two locks can both miss if a new note is
/// written between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildMark {
    Rejected(Rejection),
    StartFailed,
}

/// A failed build of the comm state: the (log-only) message, and the stable
/// [`Rejection`] when the failure was the binary itself being refused.
#[derive(Debug)]
struct BuildFailure {
    reason: String,
    rejection: Option<Rejection>,
}

impl BuildFailure {
    fn rejected(rejection: Rejection, reason: String) -> Self {
        Self {
            reason,
            rejection: Some(rejection),
        }
    }
}

impl From<String> for BuildFailure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            rejection: None,
        }
    }
}

impl From<&str> for BuildFailure {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

/// `BinaryError::HashMismatch` is the lock refusing the bytes; anything else
/// (missing file, I/O) is not a verdict on the binary.
fn binary_failure(e: agent24_comm::BinaryError) -> BuildFailure {
    let reason = format!("Hyphae binary verification failed: {e}");
    match e {
        agent24_comm::BinaryError::HashMismatch { .. } => {
            BuildFailure::rejected(Rejection::HashMismatch, reason)
        }
        _ => reason.into(),
    }
}

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
            Mutex::new(BuildNote::default()),
            Mutex::new(Duration::ZERO),
            std::sync::atomic::AtomicUsize::new(0),
            Mutex::new(None),
        )
    }

    /// Replace the CLI handle the inbound poller reads each round.
    pub fn set_hyphae_cli(&self, cli: Option<Arc<dyn crate::inbound::HyphaeCli>>) {
        *self
            .5
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cli;
    }

    /// The CLI from the last successful comm build, if Hyphae is configured.
    #[must_use]
    pub fn hyphae_cli(&self) -> Option<Arc<dyn crate::inbound::HyphaeCli>> {
        self.5
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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
                // A verified binary now owns the slot: no earlier refusal applies.
                self.note(None, false);
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

    /// Why the last build refused the binary, if it did (see [`Rejection`]).
    pub fn rejection(&self) -> Option<Rejection> {
        self.note_lock().rejection
    }

    /// The last build failed without refusing the binary (see [`BuildNote`]).
    #[cfg(test)]
    pub fn start_failed(&self) -> bool {
        self.note_lock().start_failed
    }

    /// Rejection and start-failure from the same [`BuildNote`] lock.
    pub(crate) fn build_mark(&self) -> Option<BuildMark> {
        let note = self.note_lock();
        if let Some(rejection) = note.rejection {
            Some(BuildMark::Rejected(rejection))
        } else if note.start_failed {
            Some(BuildMark::StartFailed)
        } else {
            None
        }
    }

    /// Grace the Hyphae supervisor was given. `Duration::ZERO` until [`build`].
    /// Test-only: production reads the same value from `CommRuntime.grace`.
    #[cfg(test)]
    fn stop_grace(&self) -> Duration {
        *self
            .3
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn note_lock(&self) -> std::sync::MutexGuard<'_, BuildNote> {
        self.4.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.2
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn build_note_locks(&self) -> usize {
        self.4.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn note(&self, rejection: Option<Rejection>, start_failed: bool) {
        *self.note_lock() = BuildNote {
            rejection,
            start_failed,
        };
    }

    fn set_stop_grace(&self, grace: Duration) {
        *self
            .3
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = grace;
    }

    /// The registered supervisor, if any (read-only; status for `/api/v1/components`).
    pub fn current(&self) -> Option<Arc<HyphaeDaemonSupervisor>> {
        match &*self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            CommDaemonSlotState::Registered(handle) => Some(handle.clone()),
            _ => None,
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
    lock: &HyphaeLock,
    platform: &str,
) -> Result<Option<PathBuf>, BuildFailure> {
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
        &lock.installed_path(state_dir),
        sibling,
        lock,
        platform,
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
) -> Result<Option<PathBuf>, BuildFailure> {
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
                return Err(BuildFailure::rejected(
                    Rejection::HashMismatch,
                    "installed Hyphae component does not match the locked SHA-256".into(),
                ));
            }
            Ok(Some(component.to_path_buf()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(sibling.filter(|p| p.is_file())),
        Err(e) => Err(e.to_string().into()),
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

/// What a binary is checked against: the lock, the D2a verifier, the lock platform.
struct Verification<'a> {
    lock: &'a HyphaeLock,
    verifier: &'a Verifier,
    platform: &'a str,
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
    check: &Verification<'_>,
) -> Result<CommState, BuildFailure> {
    let Verification {
        lock,
        verifier,
        platform,
    } = *check;
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
    let expected = lock
        .expected_for(platform)
        .map_err(|e| format!("no verified Hyphae binary for platform {platform}: {e}"))?;
    let bin = VerifiedBinary::install(&source, expected, install_dir)
        .await
        .map_err(binary_failure)?;
    // ADR-038 D2a `AtRest`, on the exact copy that will run: offline (no Gatekeeper
    // round-trip), so an installed Hyphae still starts without a network. Notarization
    // was established by the install-time check. Linux: sha256 above is the whole check.
    crate::hyphae_install::verify_signature(
        lock,
        platform,
        bin.path(),
        verifier,
        VerifyMode::AtRest,
    )
    .await
    .map_err(|e| {
        BuildFailure::rejected(
            Rejection::SignatureRejected,
            format!("Hyphae signature verification failed: {e}"),
        )
    })?;
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
    // D3/D5: handshake before the first start (including autostart below),
    // then health while the daemon is running. Expectation comes from the
    // schema-2 lock, not a hardcoded constant.
    daemon.arm_probe(hyphae_probe_arm(lock));
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
    slot.set_hyphae_cli(Some(Arc::new(LiveHyphae {
        runner: Arc::clone(&runner),
        password_store: Arc::clone(&password_store),
        home: home.to_path_buf(),
    })));
    Ok(CommState::ready(runner, password_store, home.to_path_buf()).with_daemon(daemon))
}

/// Read-only Hyphae CLI for the inbound poller. Same runner and password
/// store as the comm routes. Does not change `agent24-comm`.
struct LiveHyphae {
    runner: Arc<HyphaeRunner>,
    password_store: Arc<dyn PasswordStore>,
    home: PathBuf,
}

#[async_trait::async_trait]
impl crate::inbound::HyphaeCli for LiveHyphae {
    async fn run(
        &self,
        args: &[String],
        with_password: bool,
    ) -> Result<Value, crate::inbound::HyphaeCliError> {
        let password = if with_password {
            Some(self.keystore_password().await?)
        } else {
            None
        };
        let invocation = agent24_comm::Invocation {
            args: args.iter().map(std::ffi::OsString::from).collect(),
            password,
            timeout: Some(DEFAULT_TIMEOUT),
        };
        match self.runner.run(invocation).await {
            Ok(agent24_comm::Envelope::Ok { data }) => Ok(data),
            Ok(agent24_comm::Envelope::Failed {
                exit: agent24_comm::ExitClass::AuthError,
                ..
            }) => Err(crate::inbound::HyphaeCliError::Locked),
            Ok(agent24_comm::Envelope::Failed { error, message, .. }) => Err(
                crate::inbound::HyphaeCliError::Failed(cli_failure(&error, &message)),
            ),
            Err(agent24_comm::RunnerError::Spawn(_)) => {
                Err(crate::inbound::HyphaeCliError::NotInstalled)
            }
            Err(err) => Err(crate::inbound::HyphaeCliError::Failed(one_line(
                &err.to_string(),
            ))),
        }
    }
}

impl LiveHyphae {
    /// Keystore password for `groupchat`. A missing file, salt, or keyring
    /// entry is [`crate::inbound::HyphaeCliError::Locked`]. The password is
    /// not logged.
    async fn keystore_password(
        &self,
    ) -> Result<agent24_comm::Password, crate::inbound::HyphaeCliError> {
        let salt = inbound_keystore_salt(&self.home).await?;
        let account = agent24_comm::Account::from_salt(&salt);
        self.password_store
            .get(&account)
            .await
            .map_err(|_| crate::inbound::HyphaeCliError::Locked)
    }
}

/// `keystore.json`'s `salt` only. Same read as comm's identity routes, copied
/// here because that helper is `pub(crate)` inside `agent24-comm`.
async fn inbound_keystore_salt(home: &Path) -> Result<String, crate::inbound::HyphaeCliError> {
    let path = home.join(".hyphae").join("keystore.json");
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| crate::inbound::HyphaeCliError::Locked)?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| crate::inbound::HyphaeCliError::Locked)?;
    value
        .get("salt")
        .and_then(Value::as_str)
        .filter(|salt| !salt.is_empty())
        .map(str::to_owned)
        .ok_or(crate::inbound::HyphaeCliError::Locked)
}

fn cli_failure(error: &str, message: &str) -> String {
    one_line(&format!("{error}: {message}"))
}

fn one_line(text: &str) -> String {
    let mut out: String = text
        .chars()
        .filter(|ch| *ch != '\n' && *ch != '\r')
        .take(160)
        .collect();
    if text.chars().filter(|ch| *ch != '\n' && *ch != '\r').count() > 160 {
        out.push('…');
    }
    out
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
    let lock = match HyphaeLock::embedded() {
        Ok(lock) => lock,
        Err(e) => {
            return unconfigured_router(CommState::binary_rejected(e.to_string()));
        }
    };
    // ADR-038 D5: the lock's stop grace, unless the daemon's shutdown budget
    // (the configured global grace, already capped by `shutdown_bounds::GRACE_MAX_MS`)
    // is shorter. A longer per-component wait would be cut off at `deadlines.modules`.
    let grace = component_stop_grace(lock.component().stop_grace_ms, grace, "hyphae");
    slot.set_stop_grace(grace);
    let runtime = Arc::new(CommRuntime {
        state_dir: state_dir.to_path_buf(),
        grace,
        slot: slot.clone(),
        lock,
        verifier: Arc::new(agent24_components::sign::Codesign),
        platform: current_platform(),
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
    lock: HyphaeLock,
    /// D2a verifier (codesign/spctl); only consulted for macOS assets.
    verifier: Verifier,
    /// The lock platform this daemon runs as (`current_platform()` in production).
    platform: String,
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

    /// Build the comm state and record what `/api/v1/components` should show.
    /// A binary verdict keeps its stable code. Any other failure is D5 `failed`
    /// with `start_failed` (no path). A build that succeeds, or that finds
    /// nothing installed, clears both.
    async fn try_build_state(&self) -> Result<CommState, String> {
        match self.build_state_or_failure().await {
            Ok(state) => {
                self.slot.note(None, false);
                Ok(state)
            }
            Err(failure) => {
                // No verdict: do not leave the slot blank. A blank slot is
                // `absent` ("not installed"), and this build did fail.
                // Drop any CLI from a previous successful build.
                self.slot.set_hyphae_cli(None);
                self.slot
                    .note(failure.rejection, failure.rejection.is_none());
                Err(failure.reason)
            }
        }
    }

    async fn build_state_or_failure(&self) -> Result<CommState, BuildFailure> {
        let password_store = match &self.password_store {
            Ok(store) => store.clone(),
            Err(reason) => return Err(reason.clone().into()),
        };
        let Some(source) = resolve_source_path(&self.state_dir, &self.lock, &self.platform).await?
        else {
            self.slot.set_hyphae_cli(None);
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
            &Verification {
                lock: &self.lock,
                verifier: &self.verifier,
                platform: &self.platform,
            },
        )
        .await
    }

    async fn installed_version(&self) -> Option<String> {
        // Revalidate on every status read, including after an external disk edit.
        resolve_candidates(
            None,
            None,
            &self.lock.installed_path(&self.state_dir),
            None,
            &self.lock,
            &self.platform,
        )
        .await
        .ok()
        .flatten()
        .map(|_| self.lock.version().to_owned())
    }

    async fn install(&self) -> Result<Value, (StatusCode, Json<Value>)> {
        let _install = self.installation.lock().await;
        let path = crate::hyphae_install::install(
            &self.lock,
            &self.platform,
            &self.state_dir,
            &self.verifier,
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
            "installed_version": self.lock.version(), "path": path,
            "restart_required": restart_required, "reason": reason,
        }}))
    }
}

/// Only reachable when the embedded Hyphae lock cannot be parsed (see [`build`]), i.e. when
/// there is nothing pinned to install. `POST /comm/install` is therefore deliberately absent
/// here: the installer is a method of [`CommRuntime`], which needs that lock. Every other comm
/// route answers with the `binary_rejected` state instead. (S1-1 / plan B6: expected, not drift.)
fn unconfigured_router(state: CommState) -> Router {
    Router::new().nest("/api/v1/comm", agent24_comm::router(state))
}

/// Lock `stop_grace_ms`, clamped to the daemon shutdown budget.
///
/// The budget is the configured global stop grace. `Params::from_env` already
/// caps that at [`agent24_protocol::shutdown_bounds::GRACE_MAX_MS`], which is
/// what `WORST_CASE_EXIT_MS` assumes. `deadlines.modules` is
/// `drain + that grace + CONFIRM`, and `kill_group_gracefully` stops waiting
/// at that instant. A lock value above the budget would be cut there; use the
/// budget and say so. A lock value within the budget replaces the global one,
/// including when the global knob was turned up. No lock value keeps the budget.
/// Does not change `shutdown_bounds` (Hyphae's 15s is under the 30s cap).
fn component_stop_grace(lock_ms: Option<u64>, budget: Duration, component: &str) -> Duration {
    let Some(requested) = lock_ms else {
        return budget;
    };
    let budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX);
    let cap_ms = budget_ms.min(agent24_protocol::shutdown_bounds::GRACE_MAX_MS);
    if requested > cap_ms {
        tracing::warn!(
            component,
            requested_ms = requested,
            budget_ms = cap_ms,
            "component stop_grace_ms exceeds the daemon shutdown budget; using the budget"
        );
        return Duration::from_millis(cap_ms);
    }
    Duration::from_millis(requested)
}

// Preserve every MatchedPath for CORE-2 authorization while requests use the
// current comm instance. A fallback would erase those fixed route templates.
// The placeholder match also captures path parameters; [`forward_comm`] drops
// those before the live router matches the same path again.
fn runtime_router(runtime: Arc<CommRuntime>) -> Router {
    let routes = agent24_comm::router(CommState::unconfigured("initializing"))
        .route_layer(middleware::from_fn_with_state(
            runtime.clone(),
            forward_comm,
        ))
        .route("/install", post(install_component).with_state(runtime));
    Router::new().nest("/api/v1/comm", routes)
}

/// Outer `MatchedPath`, held aside so the live router's match does not
/// concatenate onto it. Restored by [`restore_forwarded_matched_path`].
#[derive(Clone)]
struct ForwardedMatchedPath(MatchedPath);

/// Drop path parameters captured by the placeholder comm router.
///
/// `runtime_router` matches each comm route so CORE-2's auth layer sees a
/// fixed `MatchedPath`. Axum records the captured parameters in a private
/// `UrlParams` extension and appends on a second match, so the live router's
/// `Path` extractor returns 500 (`Expected N but got 2N`). That type is not
/// public, so the parameters cannot be removed by name. The extensions map
/// is rebuilt from the values that must survive:
///
/// - `Authorization`, inserted by the parent auth layer (it runs after the
///   outer match and before this middleware). Kernel handlers read it with
///   `Extension<Authorization>`.
/// - `OriginalUri` and `NestedPath`, set by axum before this middleware. The
///   live router will not replace an `OriginalUri` that is already present,
///   so the client's full URI stays intact.
/// - `MatchedPath` is stashed as [`ForwardedMatchedPath`], not left as
///   `MatchedPath`. The live match treats an existing `MatchedPath` as a nest
///   prefix and concatenates (`append_nested_matched_path`), which would
///   double `/api/v1/comm/outbox/{event_id}/retry`. Auth has already copied
///   the outer template; the stash puts that same value back after the live
///   match.
///
/// When an outer layer starts inserting another extension (a `DefaultBodyLimit`
/// marker, or `OnUpgrade` for a WebSocket upgrade), add it to this whitelist
/// in the same change. `extensions.clear()` drops anything not copied back.
fn request_without_outer_path_params(req: Request) -> Request {
    let (mut parts, body) = req.into_parts();
    let matched = parts.extensions.remove::<MatchedPath>();
    let authorization = parts
        .extensions
        .remove::<crate::capabilities::Authorization>();
    let original_uri = parts.extensions.remove::<OriginalUri>();
    let nested_path = parts.extensions.remove::<NestedPath>();
    parts.extensions.clear();
    if let Some(matched) = matched {
        parts.extensions.insert(ForwardedMatchedPath(matched));
    }
    if let Some(authorization) = authorization {
        parts.extensions.insert(authorization);
    }
    if let Some(original_uri) = original_uri {
        parts.extensions.insert(original_uri);
    }
    if let Some(nested_path) = nested_path {
        parts.extensions.insert(nested_path);
    }
    Request::from_parts(parts, body)
}

/// Put the outer `MatchedPath` back after the live router has matched.
///
/// The live match runs before this layer and has already written a fresh
/// `UrlParams` (one copy) plus its own `MatchedPath`. Replacing that
/// `MatchedPath` restores the template CORE-2 used.
async fn restore_forwarded_matched_path(mut req: Request, next: Next) -> Response {
    if let Some(ForwardedMatchedPath(matched)) =
        req.extensions_mut().remove::<ForwardedMatchedPath>()
    {
        req.extensions_mut().insert(matched);
    }
    next.run(req).await
}

async fn dispatch_inner(routes: Router, req: Request) -> Response {
    use tower::ServiceExt;
    let routes = routes.layer(middleware::from_fn(restore_forwarded_matched_path));
    let req = request_without_outer_path_params(req);
    match routes.oneshot(req).await {
        Ok(response) => response,
        Err(error) => match error {},
    }
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
    // Hold the read lock for the whole forward so an install's write lock
    // waits until this request has finished on the router it cloned.
    let routes = runtime.routes.read().await;
    dispatch_inner(routes.clone(), req).await
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
    value["data"]["installable"] = runtime.lock.expected_for(&runtime.platform).is_ok().into();
    // Stable code for a refused binary (`signature_rejected` | `hash_mismatch`), or null.
    value["data"]["rejection"] = json!(runtime.slot.rejection());
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

/// Notarization (`spctl --type install`) is an online Gatekeeper lookup for a bare,
/// unstapled Mach-O: a "not notarized" verdict on an offline machine is the usual cause.
const NEEDS_NETWORK: &str = "Hyphae's notarization could not be confirmed: verifying \
     notarization needs internet access. Connect to the internet and install again.";

/// `codesign` (or `spctl`) never produced a verdict: it could not be started, or the
/// check died before Gatekeeper was asked. Connectivity is not the problem here.
const CHECKER_UNAVAILABLE: &str = "Hyphae was not installed: the system signature checker \
     could not be run. See the agent24d log.";

/// Map an installation failure to a stable code and a fixed, path-free message.
///
/// `POST /comm/install` is host-only (ProductHost / legacy token; FirstPartyShell
/// tokens cannot elevate it and Creative is excluded), but the host UI still gets no
/// raw tool output: codesign stderr and I/O errors can carry absolute paths with the
/// user name. The full error goes to the daemon log only.
fn install_error(error: InstallError) -> (StatusCode, Json<Value>) {
    use std::borrow::Cow;
    let (status, code, message): (StatusCode, &str, Cow<'static, str>) = match &error {
        InstallError::Download(DownloadError::UnsupportedPlatform) => (
            StatusCode::CONFLICT,
            "hyphae_unsupported_platform",
            "Hyphae has no pinned release for this platform".into(),
        ),
        InstallError::Download(DownloadError::HashMismatch)
        | InstallError::Binary(agent24_comm::BinaryError::HashMismatch { .. }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "hyphae_hash_mismatch",
            "the Hyphae binary does not match the pinned SHA-256".into(),
        ),
        // D2a: the bytes matched the lock, but Gatekeeper's verdict was negative or its
        // online lookup stalled past the deadline; offline is the common cause of both.
        InstallError::Signature(SignError::NotNotarized) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "hyphae_signature_rejected",
            NEEDS_NETWORK.into(),
        ),
        InstallError::Signature(SignError::NotarizationCheckFailed(e))
            if e.kind() == std::io::ErrorKind::TimedOut =>
        {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "hyphae_signature_rejected",
                NEEDS_NETWORK.into(),
            )
        }
        // No verdict at all: spctl could not be started, or `codesign` could not be run /
        // finish (`Io`), so Gatekeeper never ran and "needs internet" would be a guess.
        InstallError::Signature(SignError::NotarizationCheckFailed(_) | SignError::Io(_)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "hyphae_signature_rejected",
            CHECKER_UNAVAILABLE.into(),
        ),
        InstallError::Signature(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "hyphae_signature_rejected",
            "the Hyphae binary's code signature does not match the pinned Developer ID; it was \
             not installed"
                .into(),
        ),
        // Static, path-free reasons are kept; everything else gets a fixed text.
        InstallError::Download(DownloadError::Network(why)) => (
            StatusCode::BAD_GATEWAY,
            "hyphae_download_failed",
            format!("Hyphae download failed: {why}").into(),
        ),
        InstallError::Download(DownloadError::Cancelled) => (
            StatusCode::BAD_GATEWAY,
            "hyphae_download_failed",
            "Hyphae download was cancelled".into(),
        ),
        InstallError::Download(DownloadError::Redirect(_)) => (
            StatusCode::BAD_GATEWAY,
            "hyphae_download_failed",
            "Hyphae download was redirected outside the allowed release hosts".into(),
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "hyphae_download_failed",
            "Hyphae download or installation failed; see the agent24d log".into(),
        ),
    };
    tracing::warn!(code, error = %error, "comm: Hyphae installation failed");
    (
        status,
        Json(json!({"ok": false, "error": code, "message": message})),
    )
}

#[cfg(test)]
mod install_error_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn map(error: InstallError) -> (StatusCode, String, String) {
        let (status, Json(body)) = install_error(error);
        (
            status,
            body["error"].as_str().unwrap().to_owned(),
            body["message"].as_str().unwrap().to_owned(),
        )
    }

    #[test]
    fn signature_failures_have_their_own_code_and_offline_says_so() {
        for offline in [
            SignError::NotNotarized,
            SignError::NotarizationCheckFailed(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "/usr/sbin/spctl did not finish within 60s",
            )),
        ] {
            let (status, code, message) = map(InstallError::Signature(offline));
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(code, "hyphae_signature_rejected");
            assert!(message.contains("needs internet access"), "{message}");
        }
        // #1551 L2: Gatekeeper never ran (codesign could not be spawned or timed out, or
        // spctl could not be started): same code, but not blamed on the network.
        for no_verdict in [
            SignError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "/usr/bin/codesign did not finish within 60s",
            )),
            SignError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No such file or directory",
            )),
            SignError::NotarizationCheckFailed(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No such file or directory",
            )),
        ] {
            let (status, code, message) = map(InstallError::Signature(no_verdict));
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(code, "hyphae_signature_rejected");
            assert_eq!(message, CHECKER_UNAVAILABLE);
            assert!(!message.contains("internet"), "{message}");
        }
        for rejected in [
            SignError::SignatureMismatch,
            SignError::HardenedRuntimeMissing,
            SignError::ManifestViolation("notarization not required".into()),
        ] {
            let (status, code, message) = map(InstallError::Signature(rejected));
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(code, "hyphae_signature_rejected");
            assert!(!message.contains("internet"), "{message}");
        }
        // A real digest mismatch keeps its own code.
        let (_, code, _) = map(InstallError::Download(DownloadError::HashMismatch));
        assert_eq!(code, "hyphae_hash_mismatch");
    }

    #[test]
    fn messages_never_carry_tool_output_or_paths() {
        let leaky = "/Users/alice/Library/Agent24/components/hyphae/v0.30.0/hyphae-0123: \
                     code object is not signed at all";
        let cases = [
            InstallError::Signature(SignError::SignatureCheckFailed(leaky.into())),
            InstallError::Binary(agent24_comm::BinaryError::Missing(
                "/Users/alice/secret/hyphae".into(),
            )),
            InstallError::Io(std::io::Error::other(leaky)),
            InstallError::Download(DownloadError::Io(std::io::Error::other(leaky))),
            InstallError::Download(DownloadError::Redirect(
                "https://evil.invalid/Users/alice".into(),
            )),
        ];
        for error in cases {
            let (_, code, message) = map(error);
            assert!(
                !message.contains("/Users") && !message.contains("alice"),
                "{code}: {message}"
            );
        }
    }
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

#[cfg(test)]
mod hyphae_probe_arm_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use agent24_comm::{CONTRACT_TIMEOUT, HEALTH_INTERVAL, HEALTH_TIMEOUT};

    use super::*;

    #[test]
    fn production_arm_reads_the_embedded_lock() {
        let lock = HyphaeLock::embedded().expect("embedded hyphae lock");
        let arm = hyphae_probe_arm(&lock);
        assert_eq!(arm.expectation.name, "hyphae-cli");
        assert_eq!(arm.expectation.major, 1);
        assert_eq!(arm.expectation.min_minor, 0);
        assert_eq!(arm.expectation.tested, "1.0");
        assert_eq!(arm.health_interval, HEALTH_INTERVAL);
        assert_eq!(arm.health_timeout, HEALTH_TIMEOUT);
        assert_eq!(arm.contract_timeout, CONTRACT_TIMEOUT);
    }

    /// The same `build_ready_state` that production runs must arm before
    /// autostart. An incompatible contract then refuses to spawn. Deleting
    /// `daemon.arm_probe(...)` leaves the contract `Unknown` and, because
    /// this fixture can satisfy identity/relay/password, starts the daemon.
    #[tokio::test]
    async fn build_ready_state_arms_the_probe_and_refuses_an_incompatible_contract() {
        const SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  contract)
    printf '%s\n' '{"ok":true,"data":{"name":"hyphae-cli","contract_version":"9.0"}}'
    exit 0
    ;;
  identity)
    printf '%s\n' '{"ok":true,"data":[{"nickname":"alice","npub":"npub1x","default":true,"encrypted":true}]}'
    exit 0
    ;;
  relay)
    printf '%s\n' '{"ok":true,"data":{"relays":["wss://relay.example"],"source":"config"}}'
    exit 0
    ;;
  daemon)
    cat > /dev/null
    touch "$HOME/daemon-started"
    exec sleep 9999
    ;;
esac
exit 0
"#;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("hyphae-fake.sh");
        tokio::fs::write(&source, SCRIPT).await.unwrap();
        let bytes = tokio::fs::read(&source).await.unwrap();
        let lock =
            HyphaeLock::override_for_test("linux-x64", agent24_comm::binary::sha256_of(&bytes));
        let home = dir.path().join("hyphae-home");
        tokio::fs::create_dir_all(home.join(".hyphae"))
            .await
            .unwrap();
        tokio::fs::write(
            home.join(".hyphae").join("keystore.json"),
            br#"{"salt":"dGVzdHNhbHQ="}"#,
        )
        .await
        .unwrap();
        let autostart_path = dir.path().join("daemon-autostart.json");
        tokio::fs::write(&autostart_path, br#"{"autostart":true}"#)
            .await
            .unwrap();
        let install_dir = dir.path().join("bin");
        let pid_path = dir.path().join("hyphae-daemon.pid");
        let log_path = dir.path().join("logs").join("hyphae-daemon.log");
        let store = Arc::new(MemoryPasswordStore::new());
        store
            .put(
                &agent24_comm::Account::from_salt("dGVzdHNhbHQ="),
                &agent24_comm::Password::new(b"testpass".to_vec()).unwrap(),
            )
            .await
            .unwrap();
        let slot = CommDaemonSlot::new();
        let verifier: Verifier = Arc::new(agent24_components::sign::Unsigned);
        let state = build_ready_state(
            &source,
            &CommPaths {
                home: &home,
                install_dir: &install_dir,
                pid_path: &pid_path,
                log_path: &log_path,
                autostart_path: &autostart_path,
            },
            store,
            Duration::from_millis(200),
            &slot,
            &Verification {
                lock: &lock,
                verifier: &verifier,
                platform: "linux-x64",
            },
        )
        .await
        .expect("fixture binary matches the overridden lock");
        let daemon = slot
            .current()
            .expect("build_ready_state registers the supervisor");
        let snap = daemon.probe_snapshot();
        let status = daemon.status();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let spawned = home.join("daemon-started").exists();
        daemon.shutdown(tokio::time::Instant::now()).await;
        drop(state);
        assert_eq!(
            snap.contract,
            agent24_comm::ContractView::Mismatch,
            "the supervisor must be armed with the lock contract"
        );
        assert!(
            status
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("contract_mismatch")),
            "status {status:?}"
        );
        assert!(
            !spawned,
            "an incompatible contract must not start the daemon"
        );
        assert_ne!(status.state, "running");
        assert_ne!(status.state, "starting");
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
        // Fixture scripts carry no signature; the policy itself is tested in
        // hyphae_install.rs. This accepts every executable it is shown.
        runtime_with(
            dir,
            &current_platform(),
            Arc::new(agent24_components::sign::Unsigned),
        )
        .await
    }

    async fn runtime_with(dir: &Path, platform: &str, verifier: Verifier) -> Arc<CommRuntime> {
        let lock =
            HyphaeLock::override_for_test(platform, agent24_comm::binary::sha256_of(FIXTURE));
        let runtime = Arc::new(CommRuntime {
            state_dir: dir.to_path_buf(),
            grace: Duration::from_millis(50),
            slot: Arc::new(CommDaemonSlot::new()),
            lock,
            verifier,
            platform: platform.to_owned(),
            password_store: Ok(Arc::new(MemoryPasswordStore::new())),
            routes: RwLock::new(agent24_comm::router(CommState::unconfigured(
                "no Hyphae binary is configured",
            ))),
            installation: AsyncMutex::new(()),
        });
        let cache = dir.join("components/hyphae-cache");
        tokio::fs::create_dir_all(&cache).await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [cache.clone()] {
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

    /// B6: with no usable lock there is no installer, so `/comm/install` is not routed
    /// (404) while the rest of comm still is. `runtime_router` has it (positive control).
    #[tokio::test]
    async fn unconfigured_router_has_no_install_route_by_design() {
        async fn status_of(app: &Router, method: &str, path: &str) -> StatusCode {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
        let app = unconfigured_router(CommState::binary_rejected("bad lock".to_owned()));
        assert_eq!(
            status_of(&app, "POST", "/api/v1/comm/install").await,
            StatusCode::NOT_FOUND
        );
        assert_ne!(
            status_of(&app, "GET", "/api/v1/comm/identity").await,
            StatusCode::NOT_FOUND,
            "the rest of comm stays routed"
        );
        let dir = tempfile::tempdir().unwrap();
        let live = runtime_router(runtime(dir.path()).await);
        assert_ne!(
            status_of(&live, "POST", "/api/v1/comm/install").await,
            StatusCode::NOT_FOUND,
            "positive control: the configured router does route it"
        );
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
            assert_eq!(installed["data"]["installed_version"], "v0.30.1");
            assert_eq!(
                tokio::fs::read(runtime.lock.installed_path(dir.path()))
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
            assert_eq!(process["data"]["installed_version"], "v0.30.1");
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
        let install_dir = runtime.lock.install_dir(dir.path());
        assert!(!install_dir.join("hyphae.installing").exists());
        assert_eq!(
            tokio::fs::read(runtime.lock.installed_path(dir.path()))
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
        let installed = runtime.lock.installed_path(dir.path());
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
            .component_mut()
            .assets
            .remove(&current_platform());
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
        Arc::get_mut(&mut runtime)
            .unwrap()
            .lock
            .component_mut()
            .release_url_base = "https://untrusted.invalid/release".into();
        tokio::fs::remove_file(
            dir.path()
                .join("components/hyphae-cache/fixture-hyphae.verified"),
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
            Some(sibling.clone())
        );
        // A symlink to a matching binary is still not a regular file.
        #[cfg(unix)]
        {
            tokio::fs::symlink(&sibling, &component).await.unwrap();
            let err = resolve_candidates(None, None, &component, Some(sibling), &lock, &platform)
                .await
                .unwrap_err();
            assert!(err.reason.contains("regular file"), "{err:?}");
            assert_eq!(err.rejection, None, "not a verdict on the bytes");
        }
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

    /// A D2a verifier that refuses (as `codesign` would for a wrong Team ID) until told
    /// otherwise, so one runtime can go from rejected to recovered.
    #[derive(Default)]
    struct Switch {
        refuse: std::sync::atomic::AtomicBool,
    }
    impl agent24_components::SignatureVerifier for Switch {
        fn verify(
            &self,
            _: &Path,
            _: &agent24_components::SigningRequirement,
        ) -> Result<(), SignError> {
            if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
                Err(SignError::SignatureMismatch)
            } else {
                Ok(())
            }
        }
    }

    /// The startup path of [`build`]: build the state once and publish its routes.
    async fn start(runtime: &Arc<CommRuntime>) {
        let state = runtime.build_state().await;
        *runtime.routes.write().await = agent24_comm::router(state);
    }

    /// `GET /api/v1/components` through the real router, as the host and as a
    /// first-party (redacted) caller, sharing `runtime`'s slot.
    async fn components(runtime: &Arc<CommRuntime>) -> (Value, Value) {
        use crate::enroll_api::EnrollAuthority;
        let (mut state, host, _creative) = crate::server::tests::capability_state().await;
        state.comm_slot = Some(runtime.slot.clone());
        let first_party = state
            .first_party
            .as_ref()
            .unwrap()
            .issue(&crate::capabilities::first_party::test_peer("daemon-test"))
            .unwrap()
            .secret
            .to_string();
        let app = crate::server::build_router(state);
        let mut out = Vec::new();
        for token in [host, first_party] {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/components")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let hyphae = body["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == "hyphae")
                .cloned()
                .unwrap();
            out.push(hyphae);
        }
        (out.remove(0), out.remove(0))
    }

    async fn install_fixture(runtime: &CommRuntime, dir: &Path, bytes: &[u8]) {
        let installed = runtime.lock.installed_path(dir);
        tokio::fs::create_dir_all(installed.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&installed, bytes).await.unwrap();
    }

    async fn shutdown(runtime: &CommRuntime) {
        if let Some(handle) = runtime.slot.close() {
            handle.shutdown(tokio::time::Instant::now()).await;
        }
    }

    #[tokio::test]
    async fn rejected_signature_reads_failed_signature_rejected_and_recovers_after_install() {
        let dir = tempfile::tempdir().unwrap();
        let switch = Arc::new(Switch::default());
        switch
            .refuse
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // darwin-arm64 on every host, so the (fake) codesign check runs even on Linux CI.
        let runtime = runtime_with(dir.path(), "darwin-arm64", switch.clone()).await;
        install_fixture(&runtime, dir.path(), FIXTURE).await;
        start(&runtime).await;
        assert_eq!(runtime.slot.rejection(), Some(Rejection::SignatureRejected));

        let (host, redacted) = components(&runtime).await;
        assert_eq!(host["state"], "failed");
        assert_eq!(host["reason"], "signature_rejected");
        // Redacted keeps only {id, version, state, provides}: no reason.
        let keys: Vec<_> = redacted.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["id", "provides", "state", "version"]);
        assert_eq!(redacted["state"], "failed");

        let app = runtime_router(runtime.clone());
        let (status, daemon) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(daemon["data"]["rejection"], "signature_rejected");
        assert_eq!(daemon["data"]["process"]["state"], "not_configured");

        // The checker now accepts it: a host install rebuilds and clears the rejection.
        switch
            .refuse
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let (status, installed) = request(&app, "POST", "/api/v1/comm/install").await;
        assert_eq!(status, StatusCode::OK, "{installed}");
        assert_eq!(installed["data"]["restart_required"], false);
        assert_eq!(runtime.slot.rejection(), None);
        let (host, _) = components(&runtime).await;
        assert_ne!(host["state"], "failed", "{host}");
        assert_ne!(host["state"], "absent", "{host}");
        assert!(host.get("reason").is_none_or(|r| r != "signature_rejected"));
        let (_, daemon) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(daemon["data"]["rejection"], Value::Null);
        shutdown(&runtime).await;
    }

    #[tokio::test]
    async fn wrong_sha_reads_failed_hash_mismatch_and_recovers_on_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        install_fixture(&runtime, dir.path(), b"tampered").await;
        start(&runtime).await;

        let (host, redacted) = components(&runtime).await;
        assert_eq!(host["state"], "failed");
        assert_eq!(host["reason"], "hash_mismatch");
        assert_eq!(redacted["state"], "failed");
        assert!(redacted.get("reason").is_none());
        let app = runtime_router(runtime.clone());
        let (_, daemon) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(daemon["data"]["rejection"], "hash_mismatch");
        assert_eq!(daemon["data"]["installed_version"], Value::Null);

        // The right bytes are back: the next start clears it.
        install_fixture(&runtime, dir.path(), FIXTURE).await;
        start(&runtime).await;
        assert_eq!(runtime.slot.rejection(), None);
        let (host, _) = components(&runtime).await;
        assert_ne!(host["state"], "failed", "{host}");
        let (_, daemon) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(daemon["data"]["rejection"], Value::Null);
        shutdown(&runtime).await;
    }

    #[tokio::test]
    async fn non_rejection_start_failure_reads_failed_start_failed_and_absence_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        install_fixture(&runtime, dir.path(), FIXTURE).await;
        // `create_dir_all(comm/hyphae-home)` cannot make a directory through a file.
        // Same class as `A24_HYPHAE_BIN` pointing at a missing file: the build
        // fails, the binary was not refused, and there is no supervisor.
        tokio::fs::write(dir.path().join("comm"), b"not-a-directory")
            .await
            .unwrap();
        start(&runtime).await;
        assert_eq!(runtime.slot.rejection(), None);
        assert!(runtime.slot.start_failed());

        let (host, redacted) = components(&runtime).await;
        assert_eq!(host["state"], "failed");
        assert_eq!(host["reason"], "start_failed");
        assert_eq!(redacted["state"], "failed");
        assert!(redacted.get("reason").is_none(), "{redacted}");
        let app = runtime_router(runtime.clone());
        let (_, daemon) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(daemon["data"]["rejection"], Value::Null);
        let (_, identity) = request(&app, "GET", "/api/v1/comm/identity").await;
        assert_eq!(identity["error"], "binary_rejected");

        // Nothing installed any more: plain `absent`, not a leftover failure.
        tokio::fs::remove_file(dir.path().join("comm"))
            .await
            .unwrap();
        tokio::fs::remove_file(runtime.lock.installed_path(dir.path()))
            .await
            .unwrap();
        start(&runtime).await;
        assert!(!runtime.slot.start_failed());
        let (host, _) = components(&runtime).await;
        assert_eq!(host["state"], "absent", "{host}");
        assert!(host.get("reason").is_none(), "{host}");
    }

    #[tokio::test]
    async fn components_reads_both_build_marks_under_one_lock() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        install_fixture(&runtime, dir.path(), FIXTURE).await;
        tokio::fs::write(dir.path().join("comm"), b"not-a-directory")
            .await
            .unwrap();
        start(&runtime).await;
        assert_eq!(runtime.slot.rejection(), None);
        assert!(runtime.slot.start_failed());

        let before = runtime.slot.build_note_locks();
        let (host, redacted) = components(&runtime).await;
        let locks = runtime.slot.build_note_locks() - before;
        assert_eq!(host["state"], "failed");
        assert_eq!(host["reason"], "start_failed");
        assert_eq!(redacted["state"], "failed");
        // Host and redacted each call `hyphae_item` once. `rejection()` then
        // `start_failed()` would lock twice per call, so this would be 4.
        assert_eq!(
            locks, 2,
            "rejection and start_failed must share one BuildNote lock, got {locks}"
        );
    }

    #[test]
    fn component_stop_grace_uses_the_lock_until_the_shutdown_budget() {
        assert_eq!(
            component_stop_grace(Some(15_000), Duration::from_secs(30), "hyphae"),
            Duration::from_millis(15_000)
        );
        assert_eq!(
            component_stop_grace(Some(15_000), Duration::from_millis(100), "hyphae"),
            Duration::from_millis(100)
        );
        assert_eq!(
            component_stop_grace(None, Duration::from_millis(50), "hyphae"),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn health_failure_threshold_defaults_and_clamps() {
        assert_eq!(health_failure_threshold(None, "hyphae"), 3);
        assert_eq!(health_failure_threshold(Some(5), "hyphae"), 5);
        assert_eq!(health_failure_threshold(Some(0), "hyphae"), 1);
        assert_eq!(health_failure_threshold(Some(1), "hyphae"), 1);
        assert_eq!(health_failure_threshold(Some(10), "hyphae"), 10);
        assert_eq!(health_failure_threshold(Some(11), "hyphae"), 10);
        assert_eq!(health_failure_threshold(Some(u32::MAX), "hyphae"), 10);
    }

    #[test]
    fn probe_arm_reads_the_threshold_from_the_lock() {
        let mut lock = HyphaeLock::embedded().unwrap();
        assert_eq!(
            hyphae_probe_arm(&lock).failures_before_degraded,
            3,
            "the shipped lock does not set it"
        );
        lock.component_mut().health_failures_before_degraded = Some(5);
        assert_eq!(hyphae_probe_arm(&lock).failures_before_degraded, 5);
        lock.component_mut().health_failures_before_degraded = Some(99);
        assert_eq!(hyphae_probe_arm(&lock).failures_before_degraded, 10);
    }

    #[tokio::test]
    async fn hyphae_stop_grace_follows_the_lock_within_the_shutdown_budget() {
        let dir = tempfile::tempdir().unwrap();
        let wide = Arc::new(CommDaemonSlot::new());
        let _app = build(dir.path(), Duration::from_secs(30), &wide).await;
        assert_eq!(wide.stop_grace(), Duration::from_millis(15_000));

        let tight = Arc::new(CommDaemonSlot::new());
        let _app = build(dir.path(), Duration::from_millis(100), &tight).await;
        assert_eq!(tight.stop_grace(), Duration::from_millis(100));
    }

    #[tokio::test]
    async fn not_installed_stays_absent_and_a_later_absence_clears_a_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        start(&runtime).await;
        let (host, redacted) = components(&runtime).await;
        assert_eq!(host["state"], "absent");
        assert!(host.get("reason").is_none(), "{host}");
        assert_eq!(redacted["state"], "absent");
        let app = runtime_router(runtime.clone());
        let (_, daemon) = request(&app, "GET", "/api/v1/comm/daemon").await;
        assert_eq!(daemon["data"]["rejection"], Value::Null);

        // Rejected, then the bad file is deleted: back to plain `absent`, not `failed`.
        install_fixture(&runtime, dir.path(), b"tampered").await;
        start(&runtime).await;
        assert_eq!(runtime.slot.rejection(), Some(Rejection::HashMismatch));
        tokio::fs::remove_file(runtime.lock.installed_path(dir.path()))
            .await
            .unwrap();
        start(&runtime).await;
        let (host, _) = components(&runtime).await;
        assert_eq!(host["state"], "absent");
        assert!(host.get("reason").is_none(), "{host}");
    }

    fn braced_route_templates(source: &str, anchor: &str) -> std::collections::BTreeSet<String> {
        let start = source
            .find(anchor)
            .unwrap_or_else(|| panic!("{anchor} missing"));
        let body = &source[start..];
        let end = body
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{anchor} is not brace-terminated"));
        let body = &body[..end];
        body.match_indices(".route(")
            .filter_map(|(offset, _)| {
                let rest = &body[offset + ".route(".len()..];
                let rest = &rest[rest.find('"')? + 1..];
                let path = rest[..rest.find('"')?].to_owned();
                path.contains('{').then_some(path)
            })
            .collect()
    }

    /// Every `{param}` route merged into `agent24_comm::router`. The scanner
    /// fails this test when a new one appears and is not listed below.
    fn path_param_templates() -> std::collections::BTreeSet<String> {
        let mut routes = braced_route_templates(
            include_str!("../../../crates/agent24-comm/src/router.rs"),
            "pub fn router(",
        );
        routes.extend(braced_route_templates(
            include_str!("../../../crates/agent24-comm/src/groupchat.rs"),
            "pub(crate) fn routes(",
        ));
        routes
    }

    async fn raw_call(
        app: &Router,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(bearer) = bearer {
            builder = builder.header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {bearer}"),
            );
        }
        let body = match body {
            Some(body) => {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
                Body::from(body.to_owned())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (status, text)
    }

    #[tokio::test]
    async fn path_param_routes_through_runtime_router_are_not_path_extractor_500s() {
        const GID: &str = "0123456789abcdef0123456789abcdef";
        const LID: &str = "fedcba9876543210fedcba9876543210";
        // (method, template, concrete uri, json body)
        let hits = [
            (
                "POST",
                "/outbox/{event_id}/retry",
                "/api/v1/comm/outbox/evt-1/retry".to_string(),
                None,
            ),
            (
                "GET",
                "/groupchat/{group_id}/members",
                format!("/api/v1/comm/groupchat/{GID}/members"),
                None,
            ),
            (
                "GET",
                "/groupchat/{group_id}/invites",
                format!("/api/v1/comm/groupchat/{GID}/invites"),
                None,
            ),
            (
                "GET",
                "/groupchat/{group_id}/messages",
                format!("/api/v1/comm/groupchat/{GID}/messages"),
                None,
            ),
            (
                "POST",
                "/groupchat/{group_id}/messages",
                format!("/api/v1/comm/groupchat/{GID}/messages"),
                Some(r#"{"body":"hi"}"#),
            ),
            (
                "GET",
                "/groupchat/{group_id}/messages/{logical_id}",
                format!("/api/v1/comm/groupchat/{GID}/messages/{LID}"),
                None,
            ),
            (
                "POST",
                "/groupchat/{group_id}/messages/{logical_id}/retry",
                format!("/api/v1/comm/groupchat/{GID}/messages/{LID}/retry"),
                Some("{}"),
            ),
            (
                "POST",
                "/groupchat/invites/{invite_id}/accept",
                format!("/api/v1/comm/groupchat/invites/{GID}/accept"),
                Some("{}"),
            ),
        ];
        let tested: std::collections::BTreeSet<String> = hits
            .iter()
            .map(|(_, template, _, _)| (*template).to_owned())
            .collect();
        assert_eq!(
            tested,
            path_param_templates(),
            "every agent24_comm path-parameter route must be hit through runtime_router"
        );

        let dir = tempfile::tempdir().unwrap();
        let app = runtime_router(runtime(dir.path()).await);
        for (method, template, uri, body) in hits {
            let (status, text) = raw_call(&app, method, &uri, None, body).await;
            assert_ne!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{method} {template} returned the path-extractor 500: {text}"
            );
            assert!(
                !text.contains("Wrong number of path arguments"),
                "{method} {template}: {text}"
            );
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{method} {template} is not JSON ({e}): {text}"));
            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "{method} {template} body={value}"
            );
            assert_eq!(value["error"], "not_configured", "{method} {template}");
        }
    }

    /// The parent auth layer reads `MatchedPath` before `forward_comm`. A token
    /// that cannot `comm.write` must still be forbidden on the real template,
    /// and a host token must reach the handler (409), not the path-extractor 500.
    #[tokio::test]
    async fn path_param_route_still_rejects_a_token_without_comm_write() {
        let (state, host, creative) = crate::server::tests::capability_state().await;
        let dir = tempfile::tempdir().unwrap();
        let app = crate::server::build_router_with_modules(
            state,
            runtime_router(runtime(dir.path()).await),
        );
        let uri = "/api/v1/comm/outbox/evt-1/retry";

        let (status, text) = raw_call(&app, "POST", uri, Some(&creative), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        let denied: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(denied["error"]["code"], "forbidden");

        let (status, text) = raw_call(&app, "POST", uri, Some(&host), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        let allowed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(allowed["error"], "not_configured");
    }

    /// Same route, through `runtime_router`, for the two callers the creative/host
    /// case does not cover. A docked shell has no `comm.write`, so the outer
    /// match must still classify the template and forbid it. A legacy token
    /// must reach the handler (409), not the path-extractor 500.
    #[tokio::test]
    async fn path_param_route_auth_covers_docked_shell_and_legacy_token() {
        use crate::enroll_api::EnrollAuthority;

        let uri = "/api/v1/comm/outbox/evt-1/retry";
        let dir = tempfile::tempdir().unwrap();
        let routes = runtime_router(runtime(dir.path()).await);

        let (state, _host, _creative) = crate::server::tests::capability_state().await;
        let store = state.capabilities.clone().unwrap();
        let docked = crate::capabilities::first_party::FirstPartyAuthority::new_docked(store)
            .issue(&crate::capabilities::first_party::test_peer("daemon-test"))
            .unwrap()
            .secret
            .to_string();
        let app = crate::server::build_router_with_modules(state, routes.clone());
        let (status, text) = raw_call(&app, "POST", uri, Some(&docked), None).await;
        assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
        assert!(!text.contains("Wrong number of path arguments"), "{text}");
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        let denied: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(denied["error"]["code"], "forbidden");

        let legacy = crate::server::tests::state().await;
        let legacy_app = crate::server::build_router_with_modules(legacy, routes);
        let (status, text) = raw_call(&legacy_app, "POST", uri, Some("testtoken"), None).await;
        assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
        assert!(!text.contains("Wrong number of path arguments"), "{text}");
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        let allowed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(allowed["error"], "not_configured");
    }

    /// Same double-match as `runtime_router`, on a probe handler that reports
    /// what survived: one path parameter, the outer `MatchedPath`, and the
    /// `Authorization` extension inserted the way CORE-2 inserts it.
    #[tokio::test]
    async fn forward_keeps_matched_path_and_authorization() {
        use axum::extract::{Extension, MatchedPath, OriginalUri, Path};
        use axum::routing::get;

        async fn seen(
            Path(id): Path<String>,
            matched: MatchedPath,
            OriginalUri(original): OriginalUri,
            nested: axum::extract::NestedPath,
            Extension(auth): Extension<crate::capabilities::Authorization>,
        ) -> Json<Value> {
            Json(json!({
                "id": id,
                "matched": matched.as_str(),
                "original": original.path(),
                "nested": nested.as_str(),
                "capability_id": auth.claims.capability_id,
            }))
        }

        async fn insert_authorization(mut req: axum::extract::Request, next: Next) -> Response {
            req.extensions_mut()
                .insert(crate::capabilities::Authorization {
                    epoch: 7,
                    claims: crate::capabilities::CapabilityClaims {
                        first_party: None,
                        capability_id: "cap-forward".to_owned(),
                        audience: crate::capabilities::Audience::ProductHost,
                        component_id: None,
                        workspace_id: None,
                        attachment_id: None,
                        principal_id: None,
                        daemon_generation: "g".to_owned(),
                        host_generation: None,
                        sidecar_generation: None,
                        created_at: 0,
                        expires_at: 0,
                        deadline: None,
                        allowed_operations: std::collections::BTreeSet::new(),
                    },
                });
            next.run(req).await
        }

        async fn probe_forward(
            State(inner): State<Router>,
            req: axum::extract::Request,
            _next: Next,
        ) -> Response {
            dispatch_inner(inner, req).await
        }

        let inner = Router::new().route("/item/{id}", get(seen));
        let app = Router::new()
            .nest(
                "/api/v1/comm",
                Router::new()
                    .route("/item/{id}", get(|| async { StatusCode::NO_CONTENT }))
                    .route_layer(middleware::from_fn_with_state(inner, probe_forward)),
            )
            .layer(middleware::from_fn(insert_authorization));

        let (status, text) = raw_call(&app, "GET", "/api/v1/comm/item/abc", None, None).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["id"], "abc");
        assert_eq!(value["matched"], "/api/v1/comm/item/{id}");
        assert_eq!(value["original"], "/api/v1/comm/item/abc");
        assert_eq!(value["nested"], "/api/v1/comm");
        assert_eq!(value["capability_id"], "cap-forward");
    }
}
