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

/// How long `kill_group_gracefully` waits after SIGTERM before SIGKILL.
/// Named after the same env var the rest of `agent24d` already uses for
/// module stop grace (COMM-HYPHAE.md §6.2).
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

/// `ps -o lstart= -p <pid>`, trimmed. `None` if `ps` could not find the pid
/// (already gone) or failed to run at all.
async fn ps_lstart(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
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

/// SIGTERM the group, poll for it to empty for `grace`, then SIGKILL.
/// `pgid` is the process group id, which — since every spawn here uses
/// `process_group(0)` — is the leader's own pid.
async fn kill_group_gracefully(pgid: u32, grace: Duration) -> ShutdownLeader {
    let Some(pid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return ShutdownLeader::GoneBeforeTerm;
    };
    if kill_process_group(pid, Signal::Term).is_err() {
        // ESRCH (or any other failure): nothing to wait for.
        return ShutdownLeader::GoneBeforeTerm;
    }
    let deadline = Instant::now() + grace;
    loop {
        if test_kill_process_group(pid).is_err() {
            return ShutdownLeader::ExitedInGrace;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = kill_process_group(pid, Signal::Kill);
    ShutdownLeader::KilledAfterGrace
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
    kill_group_gracefully(recorded.pgid, grace).await;
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
    if let Phase::Running { child, pgid, .. } = std::mem::replace(phase, Phase::Idle) {
        drop(child);
        let leader = kill_group_gracefully(pgid, grace).await;
        remove_pid_file(pid_path).await;
        return Some(leader);
    }
    *phase = Phase::Idle;
    None
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
            if let Some(marker) = ps_lstart(spawned.pid).await {
                let _ = write_pid_file(
                    &ctx.pid_path,
                    &DaemonPidFile {
                        pid: spawned.pid,
                        pgid: spawned.pgid,
                        start_marker: marker,
                        generation: *generation,
                        bin_sha256: ctx.runner.bin_sha256_hex(),
                    },
                )
                .await;
            }
            set_status(status, "running", *generation, failures, None);
            Ok(Phase::Running {
                child: spawned.child,
                started_at: Instant::now(),
                pgid: spawned.pgid,
            })
        }
        Err(e) => {
            status_for_start_error(status, &e, *generation, failures);
            Err(e)
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
    let grace = stop_grace();

    loop {
        let backoff_deadline = match &phase {
            Phase::Backoff { deadline } => Some(*deadline),
            _ => None,
        };
        let running = matches!(phase, Phase::Running { .. });

        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    Cmd::Start(reply) => {
                        if running {
                            let _ = reply.send(Ok(()));
                        } else {
                            match spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures()).await {
                                Ok(p) => {
                                    phase = p;
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
                        let _ = reply.send(());
                    }
                    Cmd::ConfigChanged(reply) => {
                        if running || backoff_deadline.is_some() {
                            stop_phase(&mut phase, &ctx.pid_path, grace).await;
                            phase = spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures())
                                .await
                                .unwrap_or(Phase::Idle);
                        }
                        let _ = reply.send(());
                    }
                    Cmd::Shutdown(reply) => {
                        let had_process = running;
                        let leader = stop_phase(&mut phase, &ctx.pid_path, grace).await;
                        let _ = reply.send(DaemonShutdownOutcome { had_process, leader });
                        return;
                    }
                }
            }

            exit = async {
                match &mut phase {
                    Phase::Running { child, .. } => child.wait().await,
                    _ => std::future::pending().await,
                }
            }, if running => {
                let Phase::Running { started_at, .. } = std::mem::replace(&mut phase, Phase::Idle) else {
                    unreachable!("guarded by `running`")
                };
                remove_pid_file(&ctx.pid_path).await;
                policy.ran(started_at, Instant::now());
                let code = exit.ok().and_then(|s| s.code());
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
