//! COMM-4a: `agent24d` supervising the long-running `hyphae daemon` child
//! (COMM-HYPHAE.md §6.1-§6.3).
//!
//! Scope, deliberately narrow (the full three-state model, manual relay
//! probe and `catch_up` log scraping are COMM-4b):
//! - start preconditions (default identity, `relay source==config`, a
//!   retrievable password) → `not_configured`/`locked`, no retry;
//! - exit-code-only classification: 3 → `locked` (no retry), 1 → `gave_up`
//!   (no retry), anything else → [`restart_policy::RestartPolicy`];
//! - a config-change restart (relay/default identity changed via the 2a
//!   routes) that bumps `generation` without touching the restart policy's
//!   failure count;
//! - a pid file carrying the process's start time, so a future `agent24d`
//!   can tell "my own orphaned daemon" from "an unrelated process that
//!   happens to reuse this pid" and never kills by name;
//! - a `shutdown()` entry point for `agent24d`'s own SHUT-1b stop sequence.
//!
//! This module depends on nothing but [`crate::runner`], [`crate::router`]
//! (for `resolve_account`) and [`crate::password_store`] — no run-related
//! code, per COMM-HYPHAE.md §7's zero-run boundary.

use std::ffi::OsString;
use std::fs::{OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::process::{
    Pid, Signal, kill_process_group, test_kill_process, test_kill_process_group,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::password_store::{Account, PasswordStore};
use crate::restart_policy::{Decision, RestartPolicy};
use crate::runner::{Envelope, HyphaeRunner, Invocation, filtered_env};

/// `logs/hyphae-daemon.log` is truncated at the start of a spawn once it
/// grows past this (COMM-HYPHAE.md §6.3; no rotation, M12).
const LOG_TRUNCATE_CAP: u64 = 5 * 1024 * 1024;

/// How long a freshly spawned daemon must stay up before it is promoted from
/// `starting` to `running` (COMM-HYPHAE.md §6.1 "就绪判定": "存活满 3 s 记为
/// running"). [`Ctx::ready_after`] lets a test shrink this; production
/// callers (`comm_routes.rs`) use this constant.
pub const READY_AFTER_DEFAULT: Duration = Duration::from_secs(3);

/// How long `kill_group_gracefully` waits after SIGTERM before SIGKILL, as a
/// **fallback default only** — `agent24d` passes the already-validated
/// `agent24d::lifecycle::Params::stop_grace` into [`Ctx::grace`] instead of
/// calling this.
///
/// Kept only for callers outside `agent24d` (and as the orphan-reap grace
/// before a [`Ctx`] exists at all): it used to be the ONE grace this whole
/// crate read, via its own raw, unvalidated parse of
/// `A24_MODULE_STOP_GRACE_MS` with a 5s default — while `agent24d`'s own
/// `lifecycle::Params` reads the exact same variable with a 500ms default
/// and a 100ms..=5000ms clamp. Two different numbers under one variable name
/// meant a running daemon's actual stop grace silently disagreed with the
/// SHUT-1b deadline computed from the OTHER number (PR #626 review, High
/// #2): `agent24d`'s shutdown could reach its watchdog before Hyphae had
/// even been sent SIGKILL.
#[must_use]
pub fn stop_grace() -> Duration {
    std::env::var("A24_MODULE_STOP_GRACE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(5))
}

// ---------------------------------------------------------------------
// pid file + orphan liveness
// ---------------------------------------------------------------------

/// `hyphae-daemon.pid` (COMM-HYPHAE.md §2): enough to tell a live process
/// that is genuinely the daemon we spawned apart from an unrelated one that
/// happens to reuse the same pid.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonPidFile {
    pid: u32,
    pgid: u32,
    /// `ps -o lstart=` for `pid`, captured right after spawn. Not a syscall
    /// (`proc_pidinfo`/`/proc/<pid>/stat`, as COMM-HYPHAE.md §6.1 names them)
    /// because this workspace forbids `unsafe_code` crate-wide and this
    /// gives the same judgement — a live pid whose start time no longer
    /// matches is a different process — without it.
    start_marker: String,
    generation: u64,
    bin_sha256: String,
}

async fn write_pid_file(path: &Path, f: &DaemonPidFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec(f).map_err(io::Error::other)?;
    let tmp = path.with_extension("pid.tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::set_permissions(&tmp, Permissions::from_mode(0o600)).await?;
    tokio::fs::rename(&tmp, path).await
}

async fn remove_pid_file(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

// ---------------------------------------------------------------------
// autostart persistence (COMM-HYPHAE.md §6.2)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct AutostartFile {
    autostart: bool,
}

/// Persists `daemon.autostart` (COMM-HYPHAE.md §6.2: true after the first
/// successful manual start, false after a manual stop), atomically — same
/// write-to-tmp-then-rename shape as [`write_pid_file`]. Best effort: a
/// failure here only means the NEXT `agent24d` start will not auto-start
/// (or will, if it should not have); it must never fail the start/stop
/// request that triggered it.
async fn write_autostart(path: &Path, value: bool) {
    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        tracing::warn!(error = %e, "comm: could not create the daemon-autostart directory");
        return;
    }
    let bytes = match serde_json::to_vec(&AutostartFile { autostart: value }) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "comm: could not serialize daemon-autostart state");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = tokio::fs::write(&tmp, &bytes).await {
        tracing::warn!(error = %e, "comm: could not write daemon-autostart state");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        tracing::warn!(error = %e, "comm: could not persist daemon-autostart state");
    }
}

/// Reads `daemon.autostart`; `false` on anything short of a clean `true` —
/// missing file, unreadable, not valid JSON. A daemon that was never told to
/// autostart, or whose record is unreadable, must not start on its own.
pub async fn read_autostart(path: &Path) -> bool {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return false;
    };
    serde_json::from_slice::<AutostartFile>(&bytes)
        .map(|f| f.autostart)
        .unwrap_or(false)
}

/// Absolute path to `ps`, resolved once. Pinning it to an absolute path is
/// half of what makes [`ps_lstart`]'s marker caller-independent — a
/// `PATH`-relative lookup would otherwise let a different `agent24d`
/// environment (launchd's minimal `PATH` vs. an interactive shell's) resolve
/// to a different `ps` binary entirely.
fn ps_binary() -> &'static str {
    static PATH: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        ["/bin/ps", "/usr/bin/ps"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .unwrap_or("ps")
    })
}

/// `ps -o lstart= -p <pid>`, trimmed. `None` if `ps` could not find the pid
/// (already gone) or failed to run at all.
///
/// Run with a cleared, pinned environment (`LC_ALL=C`, `TZ=UTC`) and an
/// absolute `ps` path, so the marker for a given pid never depends on
/// whatever locale/timezone/`PATH` the calling `agent24d` process happens to
/// be running under. Without this, the same pid produces a different marker
/// string depending on the caller's environment (confirmed: `ps -o lstart=`
/// under `LC_ALL=zh_CN.UTF-8` or a different `TZ` renders a different
/// string for the exact same process) — and since `agent24d` can restart
/// under launchd's minimal environment or an interactive shell's at
/// different times, [`reap_orphan`]'s exact-string comparison across a
/// restart would otherwise silently stop matching, the previous instance's
/// orphan would never be killed, and the next spawn would overwrite the pid
/// file on top of it: two live `hyphae daemon` processes against the same
/// keystore (PR #626 review, blocking Medium).
///
/// `caller_env` exists only so `tests` can prove the normalization works
/// without mutating this process's own environment, which `forbid(unsafe_code)`
/// rules out (`std::env::set_var` is `unsafe` since Rust 2024). It is applied
/// first and then unconditionally overridden by `LC_ALL`/`TZ` below, so it
/// can never actually influence the result — exactly the property under
/// test.
async fn ps_lstart_with_caller_env(pid: u32, caller_env: &[(&str, &str)]) -> Option<String> {
    let mut cmd = Command::new(ps_binary());
    cmd.env_clear();
    for (key, value) in caller_env {
        cmd.env(key, value);
    }
    cmd.env("LC_ALL", "C");
    cmd.env("TZ", "UTC");
    cmd.args(["-o", "lstart=", "-p", &pid.to_string()]);
    let out = cmd.output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

async fn ps_lstart(pid: u32) -> Option<String> {
    ps_lstart_with_caller_env(pid, &[]).await
}

fn pid_alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|p| test_kill_process(p).is_ok())
}

/// How a killed process group's leader ended, for the shutdown/orphan
/// callers that need to tell `agent24d`'s own SHUT-1b record about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownLeader {
    GoneBeforeTerm,
    ExitedInGrace,
    KilledAfterGrace,
}

/// Has `child`'s leader exited, asked **without reaping it** (`waitid`
/// `WNOWAIT`), so its pid still keeps the process-group id until every real
/// signal this stop sends has gone out?
///
/// PR #626 review, blocking High #1: the previous version called
/// `child.try_wait()` on every poll of the grace loop below — which DOES
/// reap, immediately, the moment the leader exits. If the leader was the
/// group's only remaining member at that instant, its pid was free for the
/// kernel to hand to an unrelated process before the loop ever reached the
/// unconditional `SIGKILL` that used to follow — which would then land on
/// whatever reused that number, not on this group (exactly the hazard
/// `agent24-os-proto`'s `supervise.rs` names at length on `ModuleProcess`:
/// "the leader is NOT reaped until the group is dealt with").
///
/// `None` if `child.id()` is already `None` — it has been reaped by someone
/// else, which here only ever means a previous call of this same loop
/// already did the one real reap at the end of [`kill_group_gracefully`];
/// treated as "exited" since that is always true by then.
fn peek_child_exited(child: &mut Child) -> bool {
    let Some(raw_pid) = child.id() else {
        return true;
    };
    let Some(pid) = i32::try_from(raw_pid).ok().and_then(Pid::from_raw) else {
        return true;
    };
    use rustix::process::{WaitId, WaitidOptions, waitid};
    matches!(
        waitid(
            WaitId::Pid(pid),
            WaitidOptions::EXITED | WaitidOptions::NOHANG | WaitidOptions::NOWAIT,
        ),
        Ok(Some(_))
    )
}

/// SIGTERM the group, poll for it to empty for `grace`, then SIGKILL; reap
/// the leader only once every signal this call will ever send has gone out.
/// `pgid` is the process group id, which — since every spawn here uses
/// `process_group(0)` — is the leader's own pid.
///
/// `leader` is our own `Child` handle when we spawned the group (`None` for
/// an orphan from a previous `agent24d`, or a leader whose exit was already
/// observed without reaping it — see the call sites in `run_actor`). Its
/// exit is peeked (never reaped) throughout the grace loop
/// ([`peek_child_exited`]); an orphan's exit is probed by plain liveness
/// (`pid_alive`) instead, since it was never ours to reap either way, and
/// there is no reap-before-last-signal race to create for a pid we never
/// hold a `Child` for.
async fn kill_group_gracefully(
    pgid: u32,
    grace: Duration,
    mut leader: Option<&mut Child>,
) -> ShutdownLeader {
    let Some(pid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return ShutdownLeader::GoneBeforeTerm;
    };
    if kill_process_group(pid, Signal::Term).is_err() {
        // ESRCH (or any other failure): nothing to wait for.
        return ShutdownLeader::GoneBeforeTerm;
    }
    let deadline = Instant::now() + grace;
    let exited_in_grace = loop {
        let leader_gone = match leader.as_deref_mut() {
            Some(child) => peek_child_exited(child),
            None => !pid_alive(pgid),
        };
        if leader_gone && test_kill_process_group(pid).is_err() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if !exited_in_grace {
        let _ = kill_process_group(pid, Signal::Kill);
    }
    // The one real reap, now that TERM (and, if needed, KILL) have both
    // already gone out — never earlier (see `peek_child_exited`).
    if let Some(child) = leader {
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }
    if exited_in_grace {
        ShutdownLeader::ExitedInGrace
    } else {
        ShutdownLeader::KilledAfterGrace
    }
}

/// What was found and done about a possibly-orphaned pid file, for logging
/// and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanOutcome {
    /// No pid file at all.
    NoPidFile,
    /// A pid file existed but named a pid that is not alive, or could not
    /// be parsed; removed.
    Stale,
    /// The pid is alive, but its current start time does not match the
    /// recorded one — a different process entirely. **Never killed.**
    LeftAlone,
    /// The pid is alive and its start time matches: our own orphaned
    /// daemon from a previous `agent24d`. Killed.
    Killed,
}

/// Run once, early, before `agent24d` ever spawns a Hyphae daemon of its
/// own (COMM-HYPHAE.md §6.1 "孤儿识别"). Never matches by process name —
/// only (pid alive) AND (start time unchanged).
pub async fn reap_orphan(pid_path: &Path, grace: Duration) -> OrphanOutcome {
    let Ok(bytes) = tokio::fs::read(pid_path).await else {
        return OrphanOutcome::NoPidFile;
    };
    let Ok(recorded) = serde_json::from_slice::<DaemonPidFile>(&bytes) else {
        tracing::warn!("comm: hyphae-daemon.pid is not valid JSON; removing it");
        remove_pid_file(pid_path).await;
        return OrphanOutcome::Stale;
    };
    if !pid_alive(recorded.pid) {
        remove_pid_file(pid_path).await;
        return OrphanOutcome::Stale;
    }
    let current_marker = ps_lstart(recorded.pid).await;
    if current_marker.as_deref() != Some(recorded.start_marker.as_str()) {
        tracing::warn!(
            pid = recorded.pid,
            recorded_marker = recorded.start_marker.as_str(),
            current_marker = current_marker.as_deref().unwrap_or("<none>"),
            "comm: a pid file names a live pid whose start time no longer matches \
             the recorded one; leaving it alone (not treated as our own orphan)"
        );
        return OrphanOutcome::LeftAlone;
    }
    tracing::warn!(
        pid = recorded.pid,
        pgid = recorded.pgid,
        "comm: killing an orphaned hyphae daemon left by a previous agent24d"
    );
    kill_group_gracefully(recorded.pgid, grace, None).await;
    remove_pid_file(pid_path).await;
    OrphanOutcome::Killed
}

// ---------------------------------------------------------------------
// starting a daemon child
// ---------------------------------------------------------------------

/// Why `try_start` could not hand back a running child.
#[derive(Debug, Clone)]
pub enum DaemonStartError {
    /// A start precondition failed (COMM-HYPHAE.md §6.1): no default
    /// identity, relay `source != "config"` or empty, or no keystore yet.
    NotConfigured(String),
    /// The keystore password could not be retrieved.
    Locked(String),
    /// Everything was configured, but the invocation itself failed (a
    /// runner error reading identity/relay, or the spawn itself).
    Failed(String),
}

impl std::fmt::Display for DaemonStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured(m) | Self::Locked(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

/// What a daemon supervisor needs to start, stop and identify its child.
/// Built once by `agent24d` (`comm_routes.rs`) and handed to
/// [`HyphaeDaemonSupervisor::spawn`].
pub struct Ctx {
    pub runner: Arc<HyphaeRunner>,
    pub password_store: Arc<dyn PasswordStore>,
    pub home: PathBuf,
    pub pid_path: PathBuf,
    pub log_path: PathBuf,
    /// Where `daemon.autostart` is persisted (COMM-HYPHAE.md §6.2).
    pub autostart_path: PathBuf,
    /// SIGTERM-to-SIGKILL grace for this daemon's own stop/shutdown.
    /// `agent24d` passes the SAME already-validated
    /// `lifecycle::Params::stop_grace` every other out-of-process module
    /// stop uses (PR #626 review, High #2) — this `Ctx` never reads
    /// `A24_MODULE_STOP_GRACE_MS` itself.
    pub grace: Duration,
    /// How long a freshly spawned daemon must stay up before `starting`
    /// becomes `running` (COMM-HYPHAE.md §6.1). [`READY_AFTER_DEFAULT`] in
    /// production; tests shrink it so they do not have to sleep 3s.
    pub ready_after: Duration,
}

async fn read_list(ctx: &Ctx, args: &[&str]) -> Result<Value, DaemonStartError> {
    let inv = Invocation {
        args: args.iter().map(|a| OsString::from(*a)).collect(),
        password: None,
        timeout: None,
    };
    match ctx.runner.run(inv).await {
        Ok(Envelope::Ok { data }) => Ok(data),
        Ok(Envelope::Failed { error, message, .. }) => Err(DaemonStartError::NotConfigured(
            format!("{error}: {message}"),
        )),
        Err(e) => Err(DaemonStartError::Failed(e.to_string())),
    }
}

/// Opens the daemon's log file for one spawn: truncated if it has grown
/// past [`LOG_TRUNCATE_CAP`], mode 0600, stdout and stderr both pointed at
/// it (COMM-HYPHAE.md §6.3).
fn open_log_pair(path: &Path) -> io::Result<(Stdio, Stdio)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let truncate = std::fs::metadata(path)
        .map(|m| m.len() > LOG_TRUNCATE_CAP)
        .unwrap_or(false);
    let mut opts = OpenOptions::new();
    opts.create(true).mode(0o600);
    if truncate {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    let file = opts.open(path)?;
    let file2 = file.try_clone()?;
    Ok((Stdio::from(file), Stdio::from(file2)))
}

struct Spawned {
    child: Child,
    pid: u32,
    pgid: u32,
}

/// Checks every start precondition, then spawns `hyphae daemon` exactly as
/// COMM-HYPHAE.md §6.1 and this task's own brief specify: independent
/// process group, `env_clear` + [`crate::runner::ENV_ALLOW`], `HOME`/cwd
/// pinned to the Hyphae home, password on stdin, stdout/stderr to the log
/// file.
async fn try_start(ctx: &Ctx) -> Result<Spawned, DaemonStartError> {
    let identities = read_list(ctx, &["identity", "list"]).await?;
    let nickname = identities
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("default").and_then(Value::as_bool) == Some(true))
        })
        .and_then(|row| row.get("nickname"))
        .and_then(Value::as_str)
        .ok_or_else(|| DaemonStartError::NotConfigured("no default identity".to_owned()))?
        .to_owned();

    let relay = read_list(ctx, &["relay", "list"]).await?;
    let source = relay
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let relays: Vec<String> = relay
        .get("relays")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if source != "config" || relays.is_empty() {
        return Err(DaemonStartError::NotConfigured(
            "relay is not configured (COMM-HYPHAE.md §9 R1)".to_owned(),
        ));
    }

    let account: Account = crate::router::resolve_account(&ctx.home)
        .await
        .map_err(|e| DaemonStartError::NotConfigured(e.to_string()))?
        .ok_or_else(|| DaemonStartError::NotConfigured("no keystore yet".to_owned()))?;
    let password = ctx
        .password_store
        .get(&account)
        .await
        .map_err(|e| DaemonStartError::Locked(e.to_string()))?;

    let mut args: Vec<OsString> = vec![
        "daemon".into(),
        "--identity".into(),
        OsString::from(nickname),
        "--password-stdin".into(),
        "--json".into(),
        "--notify=false".into(),
        "--auto-reply=false".into(),
    ];
    for relay in &relays {
        args.push("--relay".into());
        args.push(OsString::from(relay));
    }

    let mut cmd = Command::new(ctx.runner.bin_path());
    cmd.args(&args);
    cmd.env_clear();
    for (key, value) in filtered_env(std::env::vars()) {
        cmd.env(key, value);
    }
    cmd.env("HOME", &ctx.home);
    cmd.current_dir(&ctx.home);
    cmd.process_group(0);
    cmd.stdin(Stdio::piped());
    let (stdout, stderr) =
        open_log_pair(&ctx.log_path).map_err(|e| DaemonStartError::Failed(e.to_string()))?;
    cmd.stdout(stdout);
    cmd.stderr(stderr);

    let mut child = cmd
        .spawn()
        .map_err(|e| DaemonStartError::Failed(e.to_string()))?;
    let pid = child
        .id()
        .ok_or_else(|| DaemonStartError::Failed("spawned child has no pid".to_owned()))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(password.as_bytes()).await {
            let _ = child.start_kill();
            return Err(DaemonStartError::Failed(e.to_string()));
        }
        let _ = stdin.shutdown().await;
    }
    // `process_group(0)` makes the leader's own pid the group id.
    Ok(Spawned {
        child,
        pid,
        pgid: pid,
    })
}

// ---------------------------------------------------------------------
// the supervisor actor
// ---------------------------------------------------------------------

/// `GET /comm/daemon`'s `process` object (COMM-HYPHAE.md §5.2, trimmed to
/// what COMM-4a tracks — `relay_probe`/`catch_up` are COMM-4b).
#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    /// `stopped` | `running` | `backoff` | `locked` | `gave_up` | `not_configured`.
    pub state: &'static str,
    pub generation: u64,
    pub consecutive_failures: u32,
    pub reason: Option<String>,
}

fn initial_status() -> DaemonStatus {
    DaemonStatus {
        state: "stopped",
        generation: 0,
        consecutive_failures: 0,
        reason: None,
    }
}

/// What [`HyphaeDaemonSupervisor::shutdown`] found, in terms neutral enough
/// that this crate never has to know about `agent24d`'s own
/// `agent24_os_proto::stop_record::StopRecord` (COMM-HYPHAE.md §7's
/// zero-run boundary — `agent24d`'s `server.rs` does that translation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonShutdownOutcome {
    pub had_process: bool,
    pub leader: Option<ShutdownLeader>,
}

enum Cmd {
    Start(oneshot::Sender<Result<(), DaemonStartError>>),
    Stop(oneshot::Sender<()>),
    ConfigChanged(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<DaemonShutdownOutcome>),
}

enum Phase {
    Idle,
    /// Spawned, but not yet confirmed alive long enough to be `running`
    /// (COMM-HYPHAE.md §6.1's "存活满 3s 记为 running"; PR #626 review,
    /// Medium #6). An exit from here is classified exactly like an exit
    /// from [`Phase::Running`] — only the status string differs while it
    /// lasts.
    Starting {
        child: Child,
        started_at: Instant,
        pgid: u32,
        ready_at: tokio::time::Instant,
    },
    Running {
        child: Child,
        started_at: Instant,
        pgid: u32,
    },
    Backoff {
        deadline: tokio::time::Instant,
    },
}

/// A handle to the background task that owns the Hyphae daemon child for
/// this `agent24d`'s lifetime. Cheap to clone; every clone talks to the
/// same actor.
#[derive(Clone)]
pub struct HyphaeDaemonSupervisor {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    status: Arc<Mutex<DaemonStatus>>,
}

fn set_status(
    status: &Mutex<DaemonStatus>,
    state: &'static str,
    generation: u64,
    failures: u32,
    reason: Option<String>,
) {
    if let Ok(mut s) = status.lock() {
        *s = DaemonStatus {
            state,
            generation,
            consecutive_failures: failures,
            reason,
        };
    }
}

fn status_for_start_error(
    status: &Mutex<DaemonStatus>,
    e: &DaemonStartError,
    generation: u64,
    failures: u32,
) {
    let (state, reason) = match e {
        DaemonStartError::NotConfigured(r) => ("not_configured", r.clone()),
        DaemonStartError::Locked(r) => ("locked", r.clone()),
        DaemonStartError::Failed(r) => ("gave_up", r.clone()),
    };
    set_status(status, state, generation, failures, Some(reason));
}

impl HyphaeDaemonSupervisor {
    /// Starts the background actor (in `Stopped` state; it spawns nothing
    /// until [`Self::start`] is called).
    #[must_use]
    pub fn spawn(ctx: Ctx) -> Self {
        let status = Arc::new(Mutex::new(initial_status()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_actor(ctx, cmd_rx, Arc::clone(&status)));
        Self { cmd_tx, status }
    }

    #[must_use]
    pub fn status(&self) -> DaemonStatus {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| DaemonStatus {
                state: "gave_up",
                generation: 0,
                consecutive_failures: 0,
                reason: Some("status lock poisoned".to_owned()),
            })
    }

    /// Idempotent: a `start` while already running just confirms it.
    pub async fn start(&self) -> Result<(), DaemonStartError> {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::Start(tx)).is_err() {
            return Err(DaemonStartError::Failed(
                "daemon supervisor is gone".to_owned(),
            ));
        }
        rx.await.unwrap_or_else(|_| {
            Err(DaemonStartError::Failed(
                "daemon supervisor is gone".to_owned(),
            ))
        })
    }

    pub async fn stop(&self) {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::Stop(tx)).is_ok() {
            let _ = rx.await;
        }
    }

    /// Stop-then-restart after a relay/default-identity change (2a routes),
    /// a no-op unless the daemon is currently running or backing off
    /// (COMM-HYPHAE.md §6.3). Does not touch the restart policy's failure
    /// count; does bump `generation` on the restart.
    pub async fn on_config_changed(&self) {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::ConfigChanged(tx)).is_ok() {
            let _ = rx.await;
        }
    }

    /// `agent24d`'s own shutdown sequence (SHUT-1b): SIGTERM, grace, then
    /// SIGKILL the whole group, and end the actor.
    pub async fn shutdown(&self) -> DaemonShutdownOutcome {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::Shutdown(tx)).is_ok()
            && let Ok(outcome) = rx.await
        {
            return outcome;
        }
        DaemonShutdownOutcome {
            had_process: false,
            leader: None,
        }
    }
}

async fn stop_phase(phase: &mut Phase, pid_path: &Path, grace: Duration) -> Option<ShutdownLeader> {
    match std::mem::replace(phase, Phase::Idle) {
        Phase::Running {
            mut child, pgid, ..
        }
        | Phase::Starting {
            mut child, pgid, ..
        } => {
            let leader = kill_group_gracefully(pgid, grace, Some(&mut child)).await;
            drop(child);
            remove_pid_file(pid_path).await;
            Some(leader)
        }
        // Idle or Backoff: already replaced with Idle above (a stop cancels
        // a pending restart too).
        _ => None,
    }
}

async fn spawn_and_record(
    ctx: &Ctx,
    generation: &mut u64,
    status: &Mutex<DaemonStatus>,
    failures: u32,
) -> Result<Phase, DaemonStartError> {
    match try_start(ctx).await {
        Ok(spawned) => {
            *generation += 1;
            match ps_lstart(spawned.pid).await {
                Some(marker) => {
                    if let Err(e) = write_pid_file(
                        &ctx.pid_path,
                        &DaemonPidFile {
                            pid: spawned.pid,
                            pgid: spawned.pgid,
                            start_marker: marker,
                            generation: *generation,
                            bin_sha256: ctx.runner.bin_sha256_hex(),
                        },
                    )
                    .await
                    {
                        tracing::warn!(
                            pid = spawned.pid,
                            error = %e,
                            "comm: failed to write hyphae-daemon.pid; this spawn will not \
                             be reap-orphan-tracked by a future agent24d"
                        );
                    }
                }
                None => {
                    tracing::warn!(
                        pid = spawned.pid,
                        "comm: could not read the just-spawned daemon's start time via \
                         `ps`; skipping hyphae-daemon.pid — this spawn will not be \
                         reap-orphan-tracked by a future agent24d"
                    );
                }
            }
            set_status(status, "starting", *generation, failures, None);
            Ok(Phase::Starting {
                child: spawned.child,
                started_at: Instant::now(),
                pgid: spawned.pgid,
                ready_at: tokio::time::Instant::now() + ctx.ready_after,
            })
        }
        Err(e) => {
            status_for_start_error(status, &e, *generation, failures);
            Err(e)
        }
    }
}

/// Waits for the leader at `pid` to exit **without reaping it** — the same
/// `waitid` `WNOWAIT` discipline as [`peek_child_exited`], for the same
/// reason: a natural exit (crash, or being killed by something other than
/// our own `stop`) must not free the pid for reuse before
/// `kill_group_gracefully` has had a chance to clean up whatever
/// process-group members the leader may have left running (PR #626 review,
/// Medium #3 — previously `child.wait().await` reaped the leader as the
/// very mechanism used to detect the exit, before any such cleanup ran, and
/// any descendant was simply left unsupervised).
///
/// # Errors
///
/// The OS failed to report the leader's state.
async fn wait_leader_exit_unreaped(pid: Pid) -> io::Result<()> {
    use rustix::process::{WaitId, WaitidOptions, waitid};
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitidOptions::EXITED | WaitidOptions::NOHANG | WaitidOptions::NOWAIT,
        ) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(e) => return Err(e.into()),
        }
    }
}

async fn run_actor(
    ctx: Ctx,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    status: Arc<Mutex<DaemonStatus>>,
) {
    let mut policy = RestartPolicy::new();
    let mut generation: u64 = 0;
    let mut phase = Phase::Idle;
    // PR #626 review, High #2: the SAME already-validated grace every other
    // out-of-process module's stop uses, handed in by `agent24d` — never
    // this crate's own unvalidated `stop_grace()` read of the raw env var.
    let grace = ctx.grace;

    loop {
        let backoff_deadline = match &phase {
            Phase::Backoff { deadline } => Some(*deadline),
            _ => None,
        };
        let ready_deadline = match &phase {
            Phase::Starting { ready_at, .. } => Some(*ready_at),
            _ => None,
        };
        // The leader's pid, while there is one to watch for an exit —
        // captured by value (not a borrow of `phase`) so the `exit_result`
        // branch below needs no access to `phase` until it actually fires.
        let exit_target: Option<Pid> = match &phase {
            Phase::Running { pgid, .. } | Phase::Starting { pgid, .. } => {
                i32::try_from(*pgid).ok().and_then(Pid::from_raw)
            }
            _ => None,
        };
        let has_child = exit_target.is_some();

        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    Cmd::Start(reply) => {
                        if has_child {
                            write_autostart(&ctx.autostart_path, true).await;
                            let _ = reply.send(Ok(()));
                        } else {
                            // PR #626 review, Medium #4: an explicit manual
                            // start is a fresh vote of confidence from
                            // whoever called it — it must not inherit a
                            // breaker tripped by an earlier, unrelated
                            // crash loop. Automatic restarts (the backoff
                            // timer, below) and config-change restarts keep
                            // the policy exactly as it was; only THIS path
                            // resets it.
                            policy = RestartPolicy::new();
                            match spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures()).await {
                                Ok(p) => {
                                    phase = p;
                                    // COMM-HYPHAE.md §6.2: a successful
                                    // manual start persists autostart=true.
                                    write_autostart(&ctx.autostart_path, true).await;
                                    let _ = reply.send(Ok(()));
                                }
                                Err(e) => {
                                    phase = Phase::Idle;
                                    let _ = reply.send(Err(e));
                                }
                            }
                        }
                    }
                    Cmd::Stop(reply) => {
                        stop_phase(&mut phase, &ctx.pid_path, grace).await;
                        set_status(&status, "stopped", generation, policy.consecutive_failures(), None);
                        // COMM-HYPHAE.md §6.2: a manual stop persists
                        // autostart=false, unconditionally.
                        write_autostart(&ctx.autostart_path, false).await;
                        let _ = reply.send(());
                    }
                    Cmd::ConfigChanged(reply) => {
                        if has_child || backoff_deadline.is_some() {
                            stop_phase(&mut phase, &ctx.pid_path, grace).await;
                            phase = spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures())
                                .await
                                .unwrap_or(Phase::Idle);
                        }
                        let _ = reply.send(());
                    }
                    Cmd::Shutdown(reply) => {
                        let had_process = has_child;
                        let leader = stop_phase(&mut phase, &ctx.pid_path, grace).await;
                        let _ = reply.send(DaemonShutdownOutcome { had_process, leader });
                        return;
                    }
                }
            }

            () = tokio::time::sleep_until(ready_deadline.unwrap_or_else(tokio::time::Instant::now)), if ready_deadline.is_some() => {
                if let Phase::Starting { child, started_at, pgid, .. } = std::mem::replace(&mut phase, Phase::Idle) {
                    phase = Phase::Running { child, started_at, pgid };
                    set_status(&status, "running", generation, policy.consecutive_failures(), None);
                }
            }

            exit_result = async {
                match exit_target {
                    Some(pid) => wait_leader_exit_unreaped(pid).await,
                    None => std::future::pending().await,
                }
            }, if has_child => {
                let (mut child, started_at, pgid) = match std::mem::replace(&mut phase, Phase::Idle) {
                    Phase::Running { child, started_at, pgid } => (child, started_at, pgid),
                    Phase::Starting { child, started_at, pgid, .. } => (child, started_at, pgid),
                    _ => unreachable!("guarded by `has_child`"),
                };
                if let Err(e) = exit_result {
                    tracing::warn!(
                        error = %e,
                        "comm: could not confirm the hyphae daemon leader's exit; cleaning \
                         up its process group regardless"
                    );
                }
                // PR #626 review, Medium #3: whatever the leader left
                // running in its process group (it may have forked
                // helpers) is cleaned up here, through the exact same path
                // `stop`/`shutdown` use, BEFORE the generation is allowed
                // to restart — otherwise an unsupervised descendant
                // outlives the restart and collides with the new
                // generation.
                let _ = kill_group_gracefully(pgid, grace, Some(&mut child)).await;
                let code = child.try_wait().ok().flatten().and_then(|s| s.code());
                remove_pid_file(&ctx.pid_path).await;
                policy.ran(started_at, Instant::now());
                match code {
                    Some(3) => set_status(&status, "locked", generation, policy.consecutive_failures(), Some("password_rejected".to_owned())),
                    Some(1) => set_status(&status, "gave_up", generation, policy.consecutive_failures(), Some("misconfigured".to_owned())),
                    _ => match policy.failed(Instant::now()) {
                        Decision::RestartAfter(d) => {
                            phase = Phase::Backoff { deadline: tokio::time::Instant::now() + d };
                            set_status(&status, "backoff", generation, policy.consecutive_failures(), None);
                        }
                        Decision::GiveUp { .. } => {
                            set_status(&status, "gave_up", generation, policy.consecutive_failures(), Some("restart_backoff_exhausted".to_owned()));
                        }
                    },
                }
            }

            () = tokio::time::sleep_until(backoff_deadline.unwrap_or_else(tokio::time::Instant::now)), if backoff_deadline.is_some() => {
                phase = spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures())
                    .await
                    .unwrap_or(Phase::Idle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// PR #626 review, blocking Medium: `ps_lstart`'s marker must not depend
    /// on whatever locale/timezone the *calling* `agent24d` process happens
    /// to have inherited — launchd's minimal environment and an
    /// interactive-shell-started one are both normal ways this desktop app
    /// gets launched, and genuinely differ. Before the fix, the same pid
    /// produced a different `ps -o lstart=` string under a different
    /// `LC_ALL`/`TZ` (verified by the review three separate times on a real
    /// machine), which made `reap_orphan`'s exact-string comparison across
    /// an `agent24d` restart silently stop matching.
    ///
    /// Can't mutate this process's own environment to prove it
    /// (`forbid(unsafe_code)` rules out the now-`unsafe`
    /// `std::env::set_var`), so the "different caller environment" is
    /// simulated via `ps_lstart_with_caller_env`'s explicit parameter —
    /// exactly the lever a real caller-environment difference would pull,
    /// which `reap_orphan`/`spawn_and_record` (both of which only ever call
    /// the zero-arg `ps_lstart`) never get to touch, which is the whole
    /// point of normalizing it internally.
    ///
    /// The existing `tests/daemon_supervise.rs` orphan tests all compute
    /// their comparison marker via a plain `ps` call made in the *same*
    /// process environment `reap_orphan` runs in, so they are structurally
    /// unable to catch this — this test closes that gap.
    #[tokio::test]
    async fn ps_lstart_marker_is_stable_across_different_caller_environments() {
        let mut child = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn a long-lived process to query");
        let pid = child.id();

        let marker_a = ps_lstart_with_caller_env(pid, &[("TZ", "UTC"), ("LC_ALL", "C")])
            .await
            .expect("ps should find the live pid");
        let marker_b =
            ps_lstart_with_caller_env(pid, &[("TZ", "Asia/Shanghai"), ("LC_ALL", "zh_CN.UTF-8")])
                .await
                .expect("ps should find the live pid");

        assert_eq!(
            marker_a, marker_b,
            "the same pid must produce the same start-time marker no matter what \
             locale/timezone the caller process happens to be running under, or an \
             orphan from a previous agent24d restart under a different environment \
             will never be recognized as our own"
        );

        let _ = child.kill();
        let _ = child.wait();
    }
}
