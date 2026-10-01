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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::process::{
    Pid, Signal, kill_process_group, test_kill_process, test_kill_process_group,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

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

/// What [`kill_group_gracefully`] found, split into the two questions it
/// used to conflate (PR #642 review round 3, Medium #1): how the LEADER
/// itself ended (TERM vs KILL) says nothing about whether the final
/// post-KILL probe actually observed the whole group gone (`ESRCH`) or gave
/// up after the probe deadline with it still reporting `Ok`/`EPERM` — a
/// helper that outlives the leader's own SIGKILL (stuck in an
/// uninterruptible state, or simply not yet reaped by its new parent) used
/// to be indistinguishable from a group that was genuinely confirmed empty,
/// as long as the leader itself had exited within its own grace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillOutcome {
    pub leader: ShutdownLeader,
    /// `true` only if the final post-KILL probe actually observed `ESRCH`.
    /// `false` means it gave up at its own deadline with the group still
    /// reporting `Ok` (alive) or `EPERM` (alive under another uid) — the
    /// stop was NOT confirmed, regardless of what the leader did.
    pub group_confirmed_gone: bool,
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
///
/// `shutdown_deadline` is `agent24d`'s own absolute SHUT-1b deadline, set
/// (if at all) by [`HyphaeDaemonSupervisor::shutdown`] — re-read on every
/// poll of the grace loop, not just once at entry, so a shutdown that lands
/// WHILE this call is already waiting (a config-change restart's own stop
/// of the old generation, say) shrinks the effective deadline on the very
/// next poll instead of only on the next call (PR #626 review, High #2).
/// `None` (nobody has called `shutdown` yet, or this is the pre-startup
/// orphan reap, which never gets a `Ctx` to share one from) behaves exactly
/// as before: the full `grace`.
async fn kill_group_gracefully(
    pgid: u32,
    grace: Duration,
    mut leader: Option<&mut Child>,
    shutdown_deadline: &OnceLock<Instant>,
) -> KillOutcome {
    let Some(pid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return KillOutcome {
            leader: ShutdownLeader::GoneBeforeTerm,
            group_confirmed_gone: true,
        };
    };
    if kill_process_group(pid, Signal::Term).is_err() {
        // ESRCH (or any other failure): nothing to wait for.
        return KillOutcome {
            leader: ShutdownLeader::GoneBeforeTerm,
            group_confirmed_gone: true,
        };
    }
    let mut deadline = Instant::now() + grace;
    // PR #626 review, Medium #1: ONLY the leader's own exit decides
    // `ExitedInGrace` vs `KilledAfterGrace`. The group-probe used to gate
    // this too, but the leader is deliberately kept unreaped through this
    // whole loop (see `peek_child_exited`'s doc comment) — and on Linux an
    // unreaped zombie still answers a process-group signal-0 probe as a
    // live member, so a leader that exits instantly on TERM would
    // otherwise still exhaust the FULL grace waiting for a probe that
    // cannot succeed until the leader itself is reaped (which only happens
    // below, after this loop) — misreporting a clean, instant natural exit
    // as `KilledAfterGrace` (-> `KillAttempted` -> a shutdown summary that
    // reads `degraded` for nothing).
    let exited_in_grace = loop {
        if let Some(&d) = shutdown_deadline.get() {
            deadline = deadline.min(d);
        }
        let leader_gone = match leader.as_deref_mut() {
            Some(child) => peek_child_exited(child),
            None => !pid_alive(pgid),
        };
        if leader_gone {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // PR #626 review round 2 fix-of-a-fix: sent UNCONDITIONALLY, not only
    // when `!exited_in_grace` — the leader exiting on TERM is not the claim
    // "the tree is gone" (a helper the leader forked and left behind, which
    // ignores TERM, is still a live member of this same process group and
    // would otherwise survive both `stop` and `shutdown` indefinitely; same
    // reasoning as `agent24-os-proto`'s `ModuleProcess::terminate`, which
    // sends this second signal "once more even if the leader went on
    // TERM"). Harmless when there is truly nothing left: `ESRCH`.
    let _ = kill_process_group(pid, Signal::Kill);
    // The one real reap, now that TERM and KILL have both gone out — never
    // earlier (see `peek_child_exited`).
    if let Some(child) = leader {
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }
    // PR #626 review, Medium #1: the group-emptiness check moves HERE, after
    // the reap, where a signal-0 probe can actually tell "nothing left"
    // (`ESRCH`) apart from "something is still alive, or launchd has not
    // reaped a zombie helper yet, or a member runs as another user"
    // (`Ok`/`EPERM`) — the same split `agent24-os-proto`'s `supervise.rs`
    // (`ModuleProcess::group_empties_within`) uses. Bounded, not open-ended:
    // a member stuck in an uninterruptible state must not hang this call.
    //
    // PR #642 review round 3, Medium #1: this probe's own result (confirmed
    // `ESRCH` vs. gave-up-at-`Ok`/`EPERM`) is now carried all the way out as
    // `group_confirmed_gone`, instead of being folded into `exited_in_grace`
    // (which only ever answered "how did the LEADER end", not "is the group
    // actually gone").
    let group_confirmed_gone = confirm_group_gone(
        || test_kill_process_group(pid),
        Instant::now() + Duration::from_millis(500),
    )
    .await;
    KillOutcome {
        leader: if exited_in_grace {
            ShutdownLeader::ExitedInGrace
        } else {
            ShutdownLeader::KilledAfterGrace
        },
        group_confirmed_gone,
    }
}

/// Polls `probe` (a process/group signal-0 test) until it reports `ESRCH`
/// (confirmed gone: `true`) or `deadline` passes with it still reporting
/// `Ok`/any other `Errno` (not confirmed: `false`) — the exact decision
/// [`kill_group_gracefully`]'s and [`reap_orphan_group_gracefully`]'s own
/// post-signal probes make, pulled out so a test can drive it with a fake
/// `probe` that always answers `EPERM`/`Ok` (PR #642 review round 3, Medium
/// #1's "注入 EPERM/一直存在的测试" — a real member stuck in that state
/// forever cannot be constructed in a test; the decision this makes from
/// such a sequence can).
async fn confirm_group_gone(
    mut probe: impl FnMut() -> Result<(), rustix::io::Errno>,
    deadline: Instant,
) -> bool {
    loop {
        match probe() {
            Err(rustix::io::Errno::SRCH) => return true,
            _ if Instant::now() >= deadline => return false,
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
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

/// Orphan-specific twin of [`kill_group_gracefully`] (PR #642 review round
/// 3, High #2 — a regression `reap_orphan` itself introduced in the
/// previous round). Unlike a child this `agent24d` spawned and still holds
/// a `Child` handle for (never reaped until every real signal has gone out
/// — [`peek_child_exited`]'s doc comment explains why that is safe), an
/// ORPHAN's leader can be reaped by `init`/launchd at any moment this
/// process is not watching, which frees its pid for the kernel to hand to a
/// brand-new, unrelated process. Every spawn in this crate uses
/// `process_group(0)`, so that pid IS the group's pgid too: an
/// unconditional SIGKILL to that same numeric pgid, sent without
/// re-checking anything, could land on whatever reused it.
///
/// Two differences from [`kill_group_gracefully`] close that:
/// - the grace loop polls the GROUP itself (via [`confirm_group_gone`]), and
///   returns the moment it observes `ESRCH` — confirmed empty is the signal
///   to STOP, never the signal to send one more signal. A KILL is never
///   sent once the group is confirmed gone.
/// - if the grace elapses with the group still reporting `Ok`/`EPERM`
///   (still alive, or alive under another uid), the pid's identity is
///   reverified against the ORIGINAL `ps -o lstart=` marker recorded at
///   spawn time, before any KILL goes out. A mismatch (the number may
///   already have been handed to an unrelated process) sends no signal at
///   all and logs a warning instead.
///
/// `on_kill_attempt` fires immediately before the one KILL this function may
/// send, so a test can observe whether a KILL actually went out — sending
/// KILL to an already-dead group is a silent no-op from the outside, so
/// process survival alone cannot prove this discipline held.
async fn reap_orphan_group_gracefully(
    pid: Pid,
    grace: Duration,
    recorded_pid: u32,
    recorded_start_marker: &str,
    mut on_kill_attempt: impl FnMut(),
) {
    if kill_process_group(pid, Signal::Term).is_err() {
        return; // ESRCH (or any other failure): nothing to wait for.
    }
    let group_gone_in_grace =
        confirm_group_gone(|| test_kill_process_group(pid), Instant::now() + grace).await;
    if group_gone_in_grace {
        return; // Confirmed empty on its own — never escalate to KILL.
    }
    // Still reporting alive (or EPERM) after the full grace: before sending
    // a signal that could hit an unrelated process group, make sure this is
    // still OUR orphan.
    let current_marker = ps_lstart(recorded_pid).await;
    if current_marker.as_deref() != Some(recorded_start_marker) {
        tracing::warn!(
            pid = recorded_pid,
            recorded_marker = recorded_start_marker,
            current_marker = current_marker.as_deref().unwrap_or("<none>"),
            "comm: an orphaned hyphae daemon's identity could not be reverified before \
             escalating to SIGKILL (its pid may already have been reused by an unrelated \
             process); leaving it alone rather than risk killing the wrong process group"
        );
        return;
    }
    on_kill_attempt();
    let _ = kill_process_group(pid, Signal::Kill);
    // Best-effort confirmation only; there is nothing more this call can do
    // either way once the identity-checked KILL has gone out.
    let _ = confirm_group_gone(
        || test_kill_process_group(pid),
        Instant::now() + Duration::from_millis(500),
    )
    .await;
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
    // PR #642 review round 3, High #2: an orphan is never cleaned up through
    // `kill_group_gracefully` any more — see `reap_orphan_group_gracefully`'s
    // doc comment for why an unconditional KILL is unsafe specifically for a
    // pid/pgid we do not hold a `Child` handle for.
    if let Some(pid) = i32::try_from(recorded.pgid).ok().and_then(Pid::from_raw) {
        reap_orphan_group_gracefully(pid, grace, recorded.pid, &recorded.start_marker, || {}).await;
    }
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
///
/// `shutdown_deadline` is rechecked right before the actual `cmd.spawn()`
/// below (PR #642 review round 3, High #1): every await above it —
/// `identity list`, `relay list`, `resolve_account`, the password store's
/// own `get` — can take long enough for `agent24d`'s shutdown to land in
/// between. This is the cheap half of the fix: it stops the common case
/// from ever spawning a process that would just have to be killed again a
/// moment later. It cannot close the window entirely (`shutdown` could still
/// land between this check and the real `spawn()`, which is why
/// [`spawn_and_record`] re-checks once more, AFTER the process actually
/// exists, and cleans it up there if so).
async fn try_start(
    ctx: &Ctx,
    shutdown_deadline: &OnceLock<Instant>,
) -> Result<Spawned, DaemonStartError> {
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

    // PR #642 review round 3, High #1: the last point before the real
    // `cmd.spawn()` below where giving up costs nothing — every await above
    // this line could have let a shutdown land in the meantime.
    if shutdown_deadline.get().is_some() {
        return Err(DaemonStartError::Failed(
            "agent24d is shutting down".to_owned(),
        ));
    }

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

// ---------------------------------------------------------------------
// COMM-4b: the three-state model's other two legs — manual relay probe
// and log-scraped catch_up (COMM-HYPHAE.md §5.2, §6.3's "日志").
// ---------------------------------------------------------------------

/// `GET /comm/daemon`'s `relay_probe` object: the result of the most recent
/// *manual* `POST /comm/relay/probe` — comm never probes on a timer (M12).
/// `None` (at the call site, via [`HyphaeDaemonSupervisor::relay_probe_status`])
/// until the first probe ever runs.
#[derive(Debug, Clone, Serialize)]
pub struct RelayProbeStatus {
    pub url: Option<String>,
    pub connected: bool,
    pub at_ms: u64,
    pub error: Option<String>,
}

/// `GET /comm/daemon`'s `catch_up` object. The baseline Hyphae daemon has no
/// structured catch-up progress (COMM-HYPHAE.md §5.2 G2) — this can only
/// ever report `"unknown"` or `"incomplete"`, **never** `"complete"` —
/// there is no log line this crate would accept as proof the inbox is
/// caught up.
#[derive(Debug, Clone, Serialize)]
pub struct CatchUpStatus {
    pub state: &'static str,
    pub last_incomplete_at_ms: Option<u64>,
}

/// Current wall-clock time in Unix milliseconds. `0` on a clock that reports
/// before the epoch (never happens outside a misconfigured test rig) rather
/// than a panic — this is a status timestamp, not a correctness-critical
/// value.
pub(crate) fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How many new bytes [`catch_up_from_log`] reads from the log file in one
/// `read()` call while draining everything appended since the previous
/// [`HyphaeDaemonSupervisor::catch_up`] call. Bounds the synchronous cost of
/// any single chunk no matter how much the log grew between two status
/// calls — [`catch_up_from_log`]'s outer loop still drains all of the new
/// data (so a caller never sees a stale `catch_up`), it just never holds
/// more than one chunk's worth of bytes in memory at a time, and the
/// already-scanned prefix is never re-read (Codex COMM-4b review, High: the
/// pre-fix implementation `tokio::fs::read`-ed the ENTIRE log, unbounded,
/// on every single `GET /comm/daemon`/`start`/`stop` call).
const CATCH_UP_SCAN_CHUNK_BYTES: usize = 64 * 1024;

/// Everything in Hyphae's own diagnostic line after the six `HH:MM:SS`
/// digits: `] ` + the warning emoji (U+26A0 U+FE0F) + two spaces +
/// `Inbox scan incomplete:` (`internal/daemon/daemon.go:178,191`,
/// byte-exact — verified against the checked-out Hyphae source). Written as
/// explicit `\u{...}` escapes rather than the literal glyph so the exact
/// codepoints survive any editor/encoding round-trip.
const MARKER_SUFFIX: &[u8] = "] \u{26A0}\u{FE0F}  Inbox scan incomplete:".as_bytes();

/// `"[HH:MM:SS]"` (9 bytes) + [`MARKER_SUFFIX`] — the full fixed length of
/// Hyphae's diagnostic line prefix that [`line_starts_with_incomplete_marker`]
/// checks.
const MARKER_LINE_LEN: usize = 9 + MARKER_SUFFIX.len();

/// True iff `from_here` (the rest of the scan buffer from some candidate
/// line-start position onward; may run well past the end of the actual log
/// line) begins with Hyphae's own diagnostic line format —
/// `[HH:MM:SS] ⚠️  Inbox scan incomplete:` — byte-for-byte.
///
/// Codex COMM-4b review, Medium #2: anchoring to this full format (and only
/// ever at a true line start — see [`catch_up_from_log`]), rather than a
/// loose `contains("Inbox scan incomplete")` substring search, is what
/// keeps an inbound message *body* that merely happens to mention this
/// phrase from flipping `catch_up` to `"incomplete"` — Hyphae only ever
/// emits this exact line at the start of a line it printed itself.
///
/// KNOWN LIMITATION (documented here, not fixed — needs Hyphae to change):
/// a message body containing a literal `\n` can still forge a fake line
/// start inside the log (this scraper has no way to tell a newline
/// Hyphae's own `fmt.Printf` wrote from one sitting inside a logged
/// message's content), so a sufficiently crafted body followed by a forged
/// `[HH:MM:SS] ⚠️  Inbox scan incomplete:` line could still trip a false
/// positive. The blast radius is bounded: this crate has no code path that
/// ever produces `"complete"` at all, so a forged line can only push the
/// reported state towards the more conservative `"incomplete"`, never
/// towards a false `"complete"`. Closing this gap for real needs Hyphae to
/// emit a structured, un-forgeable catch_up signal (COMM-HYPHAE.md §5.2
/// G2) — out of scope for this crate.
fn line_starts_with_incomplete_marker(from_here: &[u8]) -> bool {
    if from_here.len() < MARKER_LINE_LEN {
        return false;
    }
    let digit = |b: u8| b.is_ascii_digit();
    from_here[0] == b'['
        && digit(from_here[1])
        && digit(from_here[2])
        && from_here[3] == b':'
        && digit(from_here[4])
        && digit(from_here[5])
        && from_here[6] == b':'
        && digit(from_here[7])
        && digit(from_here[8])
        && &from_here[9..MARKER_LINE_LEN] == MARKER_SUFFIX
}

/// [`catch_up_from_log`]'s cache across calls (Codex COMM-4b review, High):
/// without this, every call re-read and re-scanned the entire log file from
/// byte 0, unbounded, no matter how little of it was new. Guarded by a
/// [`tokio::sync::Mutex`] (not [`std::sync::Mutex`]) because the scan holds
/// this lock across the file I/O's `.await` points, to serialize concurrent
/// `catch_up()` calls against the SAME offset/tail state rather than racing
/// two scans against it.
#[derive(Default)]
struct CatchUpScanState {
    /// `(st_dev, st_ino)` of the log file this cache's `offset`/`tail`
    /// apply to. `None` only before the very first scan ever runs.
    file_id: Option<(u64, u64)>,
    /// Byte offset already scanned; the next scan reads from here, not
    /// from the start of the file.
    offset: u64,
    /// The last `MARKER_LINE_LEN - 1` bytes scanned so far (or fewer, if
    /// fewer than that have ever been scanned), carried into the next scan
    /// so a marker line split across two scans' chunk boundary is never
    /// missed — a full match can never fit entirely inside `tail` alone
    /// (it is one byte short of `MARKER_LINE_LEN`), so it always needs at
    /// least one byte of newly read data to complete, which is exactly
    /// when it's checked.
    tail: Vec<u8>,
    /// Whether the byte immediately preceding `tail[0]` in the file is a
    /// newline (or `tail` is empty at file offset 0 — the start of the
    /// file counts as a line start too). [`line_starts_with_incomplete_marker`]
    /// only ever applies at a true line start, and a candidate match
    /// beginning at `tail[0]` needs to know this without re-reading
    /// anything already scanned.
    tail_preceded_by_newline: bool,
    /// Whether an incomplete-scan line has EVER been recognized for this
    /// file identity. Once true, `catch_up`'s `state` stays `"incomplete"`
    /// — nothing un-sets it short of the log being truncated or replaced
    /// (a new `file_id`), same as the pre-fix whole-file-contains check.
    found_incomplete: bool,
    /// The wall-clock time (not the log file's mtime — Codex COMM-4b
    /// review, Medium #3) `catch_up_from_log` was running in when it most
    /// recently scanned NEW bytes containing an incomplete-scan line.
    /// Unchanged by a scan whose new bytes contain no marker line, even if
    /// the file's mtime moved — appending ordinary log output must never
    /// push this forward.
    last_incomplete_at_ms: Option<u64>,
}

/// Scrapes `log_path` for Hyphae's own diagnostic line
/// (`internal/daemon/daemon.go:178,191`) using `scan`'s cached
/// offset/identity so only the bytes appended since the previous call are
/// ever read. A missing or unreadable log (e.g. before the daemon has ever
/// started) reports whatever `scan` already knows — `"unknown"` before the
/// first successful scan — never an error.
async fn catch_up_from_log(log_path: &Path, scan: &AsyncMutex<CatchUpScanState>) -> CatchUpStatus {
    fn status_from(guard: &CatchUpScanState) -> CatchUpStatus {
        CatchUpStatus {
            state: if guard.found_incomplete {
                "incomplete"
            } else {
                "unknown"
            },
            last_incomplete_at_ms: guard.last_incomplete_at_ms,
        }
    }

    let mut guard = scan.lock().await;

    let Ok(meta) = tokio::fs::metadata(log_path).await else {
        return status_from(&guard);
    };
    let file_id = (meta.dev(), meta.ino());
    let len = meta.len();

    // A different inode/device, or a file now SHORTER than what was
    // already scanned, means the log was truncated or replaced (COMM-4a's
    // own `LOG_TRUNCATE_CAP` truncation at spawn, or any future rotation)
    // — start over from byte 0 rather than mixing scan state across two
    // different underlying files that happen to share a path.
    if guard.file_id != Some(file_id) || len < guard.offset {
        *guard = CatchUpScanState {
            file_id: Some(file_id),
            tail_preceded_by_newline: true,
            ..Default::default()
        };
    }

    if len > guard.offset {
        let Ok(mut file) = tokio::fs::File::open(log_path).await else {
            return status_from(&guard);
        };
        if file
            .seek(std::io::SeekFrom::Start(guard.offset))
            .await
            .is_ok()
        {
            let mut buf = vec![0u8; CATCH_UP_SCAN_CHUNK_BYTES];
            let mut matched_new_bytes = false;
            loop {
                let remaining = len.saturating_sub(guard.offset);
                if remaining == 0 {
                    break;
                }
                let want = remaining.min(CATCH_UP_SCAN_CHUNK_BYTES as u64) as usize;
                let Ok(n) = file.read(&mut buf[..want]).await else {
                    break;
                };
                if n == 0 {
                    break;
                }

                let mut window = std::mem::take(&mut guard.tail);
                window.extend_from_slice(&buf[..n]);

                for i in 0..window.len() {
                    let is_line_start = if i == 0 {
                        guard.tail_preceded_by_newline
                    } else {
                        window[i - 1] == b'\n'
                    };
                    if is_line_start && line_starts_with_incomplete_marker(&window[i..]) {
                        matched_new_bytes = true;
                    }
                }

                let keep = (MARKER_LINE_LEN.saturating_sub(1)).min(window.len());
                let discarded = window.len() - keep;
                if discarded > 0 {
                    guard.tail_preceded_by_newline = window[discarded - 1] == b'\n';
                }
                // else: nothing dropped from the front, so the byte
                // preceding the (unchanged) tail start is still whatever
                // it was before this chunk — `tail_preceded_by_newline`
                // stays as-is.
                guard.tail = window[discarded..].to_vec();
                guard.offset += n as u64;
            }
            if matched_new_bytes {
                guard.found_incomplete = true;
                guard.last_incomplete_at_ms = Some(wall_clock_ms());
            }
        }
    }

    status_from(&guard)
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
    /// `None` iff `had_process` is `false` (nothing to confirm). Otherwise
    /// [`KillOutcome::group_confirmed_gone`] — PR #642 review round 3,
    /// Medium #1: kept separate from `leader` so `server.rs` can record
    /// `GroupEnd::Gone` only when this is actually confirmed, instead of
    /// inferring it from how the leader alone ended.
    pub group_confirmed_gone: Option<bool>,
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
    /// Set once, by [`Self::shutdown`], BEFORE its `Cmd::Shutdown` is even
    /// sent — so every branch the actor runs from then on, even one already
    /// queued ahead of that message, sees a shutdown is underway and must
    /// not spawn past it (PR #626 review, High #2). Also the "cancel
    /// in-flight stop" signal `kill_group_gracefully` re-reads on every
    /// poll.
    shutdown_deadline: Arc<OnceLock<Instant>>,
    /// COMM-4b: the daemon's own log file (`Ctx::log_path`) — [`Self::catch_up`]
    /// reads only what's new since the previous call (via `catch_up_scan`'s
    /// cached offset), not the whole file; no periodic tailing, same
    /// on-demand posture `relay_probe` has (M12 cut periodic probing).
    log_path: Arc<PathBuf>,
    /// COMM-4b: [`Self::catch_up`]'s incremental-scan cache (offset, file
    /// identity, carried tail bytes, and the sticky `found_incomplete`/
    /// `last_incomplete_at_ms` facts) — see [`CatchUpScanState`].
    catch_up_scan: Arc<AsyncMutex<CatchUpScanState>>,
    /// COMM-4b: the most recent manual `POST /comm/relay/probe` result, if
    /// any. Written by the `/relay/probe` route via
    /// [`Self::record_relay_probe`] — this actor never touches it.
    relay_probe: Arc<Mutex<Option<RelayProbeStatus>>>,
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
        let shutdown_deadline = Arc::new(OnceLock::new());
        let log_path = Arc::new(ctx.log_path.clone());
        let catch_up_scan = Arc::new(AsyncMutex::new(CatchUpScanState::default()));
        let relay_probe = Arc::new(Mutex::new(None));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_actor(
            ctx,
            cmd_rx,
            Arc::clone(&status),
            Arc::clone(&shutdown_deadline),
        ));
        Self {
            cmd_tx,
            status,
            shutdown_deadline,
            log_path,
            catch_up_scan,
            relay_probe,
        }
    }

    /// COMM-4b: the result of the most recent manual relay probe, or `None`
    /// if `POST /comm/relay/probe` has never run against this supervisor.
    #[must_use]
    pub fn relay_probe_status(&self) -> Option<RelayProbeStatus> {
        self.relay_probe.lock().ok().and_then(|g| g.clone())
    }

    /// COMM-4b: records a manual relay probe's result, for a later
    /// [`Self::relay_probe_status`] (and so `GET /comm/daemon`) to report.
    /// Called by the `/relay/probe` route — never by this actor, which knows
    /// nothing about relay probing.
    pub fn record_relay_probe(&self, probe: RelayProbeStatus) {
        if let Ok(mut guard) = self.relay_probe.lock() {
            *guard = Some(probe);
        }
    }

    /// COMM-4b: `catch_up` as of right now, scraped incrementally from the
    /// daemon's own log file (COMM-HYPHAE.md §5.2) — only the bytes
    /// appended since the previous call are ever read.
    pub async fn catch_up(&self) -> CatchUpStatus {
        catch_up_from_log(&self.log_path, &self.catch_up_scan).await
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
    ///
    /// `deadline` is the SAME absolute instant the caller's own
    /// `tokio::time::timeout_at` races this call against
    /// (`agent24d::server`'s `deadlines.modules`, PR #626 review, High #2) —
    /// recorded here, synchronously, before the `Cmd::Shutdown` is even
    /// sent, so it is visible to the actor regardless of what it is already
    /// doing: an in-flight config-change restart's own stop of the OLD
    /// generation picks it up on its very next grace-loop poll
    /// ([`kill_group_gracefully`]) and a generation that has not been
    /// spawned yet never will be (every `spawn_and_record` call site checks
    /// it first).
    pub async fn shutdown(&self, deadline: tokio::time::Instant) -> DaemonShutdownOutcome {
        let _ = self.shutdown_deadline.set(deadline.into_std());
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(Cmd::Shutdown(tx)).is_ok()
            && let Ok(outcome) = rx.await
        {
            return outcome;
        }
        DaemonShutdownOutcome {
            had_process: false,
            leader: None,
            group_confirmed_gone: None,
        }
    }
}

async fn stop_phase(
    phase: &mut Phase,
    pid_path: &Path,
    grace: Duration,
    shutdown_deadline: &OnceLock<Instant>,
) -> Option<KillOutcome> {
    match std::mem::replace(phase, Phase::Idle) {
        Phase::Running {
            mut child, pgid, ..
        }
        | Phase::Starting {
            mut child, pgid, ..
        } => {
            let outcome =
                kill_group_gracefully(pgid, grace, Some(&mut child), shutdown_deadline).await;
            drop(child);
            remove_pid_file(pid_path).await;
            Some(outcome)
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
    shutdown_deadline: &OnceLock<Instant>,
) -> Result<Phase, DaemonStartError> {
    match try_start(ctx, shutdown_deadline).await {
        Ok(mut spawned) => {
            // PR #642 review round 3, High #1: the three call sites below
            // only check `shutdown_deadline` BEFORE this function is even
            // called, and `try_start`'s own pre-spawn check above closes
            // most of the window but not all of it — `spawn()` itself and
            // the password-stdin write that follows it are real awaits too.
            // This is the one point that can still see a shutdown that
            // landed anywhere in between, now that the process genuinely
            // exists: discard it (bounded by the same shared deadline
            // `kill_group_gracefully` already re-reads on every poll)
            // instead of ever recording or promoting it.
            if shutdown_deadline.get().is_some() {
                tracing::warn!(
                    pid = spawned.pid,
                    "comm: agent24d began shutting down while this hyphae daemon spawn was \
                     still starting up; cleaning it up instead of recording it"
                );
                let _ = kill_group_gracefully(
                    spawned.pgid,
                    ctx.grace,
                    Some(&mut spawned.child),
                    shutdown_deadline,
                )
                .await;
                return Err(DaemonStartError::Failed(
                    "agent24d is shutting down (spawn aborted mid-start)".to_owned(),
                ));
            }
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

/// The leader's `Child`/metadata once its exit has been confirmed (never
/// reaped yet — see [`wait_leader_exit_unreaped`]/[`peek_child_exited`]),
/// bundled so [`classify_exit`] stays under clippy's argument-count lint.
struct ExitedLeader {
    child: Child,
    started_at: Instant,
    pgid: u32,
}

/// Shared tail of the `exit_result` branch and the ready-timer branch's
/// "actually already exited" case (PR #626 review, Medium #2): clean up
/// whatever the leader left in its process group, reap it, then classify
/// the exit code into `locked`/`gave_up`/a restart decision — exactly the
/// same path either way, so an exit caught early (before `ready_after`)
/// is handled identically to one caught after.
async fn classify_exit(
    ctx: &Ctx,
    generation: u64,
    status: &Mutex<DaemonStatus>,
    policy: &mut RestartPolicy,
    shutdown_deadline: &OnceLock<Instant>,
    mut exited: ExitedLeader,
) -> Phase {
    let _ = kill_group_gracefully(
        exited.pgid,
        ctx.grace,
        Some(&mut exited.child),
        shutdown_deadline,
    )
    .await;
    let code = exited
        .child
        .try_wait()
        .ok()
        .flatten()
        .and_then(|s| s.code());
    remove_pid_file(&ctx.pid_path).await;
    policy.ran(exited.started_at, Instant::now());
    match code {
        Some(3) => {
            set_status(
                status,
                "locked",
                generation,
                policy.consecutive_failures(),
                Some("password_rejected".to_owned()),
            );
            Phase::Idle
        }
        Some(1) => {
            set_status(
                status,
                "gave_up",
                generation,
                policy.consecutive_failures(),
                Some("misconfigured".to_owned()),
            );
            Phase::Idle
        }
        _ => match policy.failed(Instant::now()) {
            Decision::RestartAfter(d) => {
                set_status(
                    status,
                    "backoff",
                    generation,
                    policy.consecutive_failures(),
                    None,
                );
                Phase::Backoff {
                    deadline: tokio::time::Instant::now() + d,
                }
            }
            Decision::GiveUp { .. } => {
                set_status(
                    status,
                    "gave_up",
                    generation,
                    policy.consecutive_failures(),
                    Some("restart_backoff_exhausted".to_owned()),
                );
                Phase::Idle
            }
        },
    }
}

async fn run_actor(
    ctx: Ctx,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    status: Arc<Mutex<DaemonStatus>>,
    shutdown_deadline: Arc<OnceLock<Instant>>,
) {
    let mut policy = RestartPolicy::new();
    let mut generation: u64 = 0;
    let mut phase = Phase::Idle;
    // PR #626 review, High #2: the SAME already-validated grace every other
    // out-of-process module's stop uses, handed in by `agent24d` — never
    // this crate's own unvalidated `stop_grace()` read of the raw env var.
    let grace = ctx.grace;
    // PR #626 review, Medium #3: set the moment a MANUAL `Cmd::Start` spawns
    // a child (fresh or while already `has_child`), cleared the moment that
    // intent is either confirmed (promoted to `running`: persisted then) or
    // abandoned (the leader exits before reaching `running`, or a `stop`
    // lands first) — so `daemon.autostart` is never persisted `true` for a
    // start that turns out to be a wrong password or a bad config.
    let mut pending_autostart = false;

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
                            if matches!(phase, Phase::Running { .. }) {
                                write_autostart(&ctx.autostart_path, true).await;
                            } else {
                                // Still `starting`, not yet confirmed — defer
                                // exactly like the fresh-spawn branch below
                                // (PR #626 review, Medium #3).
                                pending_autostart = true;
                            }
                            let _ = reply.send(Ok(()));
                        } else if shutdown_deadline.get().is_some() {
                            // PR #626 review, High #2: "关机开始后禁止再
                            // spawn" — agent24d is already shutting down;
                            // nothing may be spawned on its behalf from now
                            // on.
                            let _ = reply.send(Err(DaemonStartError::Failed(
                                "agent24d is shutting down".to_owned(),
                            )));
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
                            match spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures(), &shutdown_deadline).await {
                                Ok(p) => {
                                    phase = p;
                                    // PR #626 review, Medium #3: NOT
                                    // persisted yet — only once this spawn
                                    // actually reaches `running` (the
                                    // ready-timer branch below). A start
                                    // that fails fast (wrong password,
                                    // misconfigured) must not leave
                                    // `autostart=true` for every future
                                    // agent24d to retry forever.
                                    pending_autostart = true;
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
                        // PR #626 review, Medium #3: a stop cancels whatever
                        // manual-start intent was still pending confirmation.
                        pending_autostart = false;
                        stop_phase(&mut phase, &ctx.pid_path, grace, &shutdown_deadline).await;
                        set_status(&status, "stopped", generation, policy.consecutive_failures(), None);
                        // COMM-HYPHAE.md §6.2: a manual stop persists
                        // autostart=false, unconditionally.
                        write_autostart(&ctx.autostart_path, false).await;
                        let _ = reply.send(());
                    }
                    Cmd::ConfigChanged(reply) => {
                        if has_child || backoff_deadline.is_some() {
                            pending_autostart = false;
                            stop_phase(&mut phase, &ctx.pid_path, grace, &shutdown_deadline).await;
                            // PR #626 review, High #2: re-checked AFTER the
                            // stop (which may itself have run concurrently
                            // with a `shutdown()` call that landed mid-wait,
                            // shrinking the grace it just used) — "关机开始
                            // 后禁止再 spawn". Skipping the respawn here is
                            // what keeps a shutdown from ever having to
                            // chase a SECOND generation this restart would
                            // otherwise have just spawned.
                            phase = if shutdown_deadline.get().is_none() {
                                spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures(), &shutdown_deadline)
                                    .await
                                    .unwrap_or(Phase::Idle)
                            } else {
                                Phase::Idle
                            };
                        }
                        let _ = reply.send(());
                    }
                    Cmd::Shutdown(reply) => {
                        let had_process = has_child;
                        let outcome = stop_phase(&mut phase, &ctx.pid_path, grace, &shutdown_deadline).await;
                        let _ = reply.send(DaemonShutdownOutcome {
                            had_process,
                            leader: outcome.map(|o| o.leader),
                            group_confirmed_gone: outcome.map(|o| o.group_confirmed_gone),
                        });
                        return;
                    }
                }
            }

            () = tokio::time::sleep_until(ready_deadline.unwrap_or_else(tokio::time::Instant::now)), if ready_deadline.is_some() => {
                if let Phase::Starting { mut child, started_at, pgid, .. } = std::mem::replace(&mut phase, Phase::Idle) {
                    // PR #626 review, Medium #2: a synchronous, non-reaping
                    // re-check right before promoting — the leader may have
                    // exited after the last poll, or its exit and this
                    // timer may have become ready in the very same
                    // `select!` poll (the chosen arm among several ready
                    // ones is unspecified), either of which would otherwise
                    // report a dead process as `running` until some later,
                    // unrelated poll finally noticed.
                    if peek_child_exited(&mut child) {
                        pending_autostart = false;
                        phase = classify_exit(
                            &ctx,
                            generation,
                            &status,
                            &mut policy,
                            &shutdown_deadline,
                            ExitedLeader { child, started_at, pgid },
                        ).await;
                    } else {
                        phase = Phase::Running { child, started_at, pgid };
                        set_status(&status, "running", generation, policy.consecutive_failures(), None);
                        // PR #626 review, Medium #3: the deferred manual
                        // start intent is confirmed — persist it now.
                        if pending_autostart {
                            pending_autostart = false;
                            write_autostart(&ctx.autostart_path, true).await;
                        }
                    }
                }
            }

            exit_result = async {
                match exit_target {
                    Some(pid) => wait_leader_exit_unreaped(pid).await,
                    None => std::future::pending().await,
                }
            }, if has_child => {
                let exited = match std::mem::replace(&mut phase, Phase::Idle) {
                    Phase::Running { child, started_at, pgid } => ExitedLeader { child, started_at, pgid },
                    Phase::Starting { child, started_at, pgid, .. } => ExitedLeader { child, started_at, pgid },
                    _ => unreachable!("guarded by `has_child`"),
                };
                if let Err(e) = exit_result {
                    tracing::warn!(
                        error = %e,
                        "comm: could not confirm the hyphae daemon leader's exit; cleaning \
                         up its process group regardless"
                    );
                }
                // PR #626 review, Medium #3 (original): whatever the leader
                // left running in its process group (it may have forked
                // helpers) is cleaned up here, through the exact same path
                // `stop`/`shutdown` use, BEFORE the generation is allowed
                // to restart — otherwise an unsupervised descendant
                // outlives the restart and collides with the new
                // generation. PR #626 review round 2, Medium #3: the exit
                // also cancels any still-pending manual-start intent.
                pending_autostart = false;
                phase = classify_exit(&ctx, generation, &status, &mut policy, &shutdown_deadline, exited).await;
            }

            () = tokio::time::sleep_until(backoff_deadline.unwrap_or_else(tokio::time::Instant::now)), if backoff_deadline.is_some() => {
                // PR #626 review, High #2: "关机开始后禁止再 spawn".
                phase = if shutdown_deadline.get().is_none() {
                    spawn_and_record(&ctx, &mut generation, &status, policy.consecutive_failures(), &shutdown_deadline)
                        .await
                        .unwrap_or(Phase::Idle)
                } else {
                    Phase::Idle
                };
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

    /// PR #626 review round 2, Medium #2: the ready-timer branch's
    /// promotion to `running` relies on [`peek_child_exited`] to see an
    /// exit WITHOUT reaping it — proven directly, deterministically, here
    /// rather than by racing real wall-clock timing against `ready_after`
    /// in `tests/daemon_supervise.rs` (see that file's own comment on why a
    /// black-box reproduction of the exact `select!` tie is not attempted:
    /// checking liveness again afterward is itself racy against a leader
    /// that is, in fact, correctly reported `running` and simply exits a
    /// moment later).
    #[tokio::test]
    async fn peek_child_exited_sees_an_exit_without_reaping_it() {
        let mut child = Command::new("true").spawn().expect("spawn `true`");
        // `true` exits essentially instantly; a comfortable margin either way.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !peek_child_exited(&mut child) {
            assert!(Instant::now() < deadline, "the child never exited");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Still unreaped: `child.id()` only becomes `None` once tokio's own
        // `wait()` has reaped it, and `try_wait()` (which DOES reap on
        // success) must still find a pending exit status to collect.
        assert!(
            child.id().is_some(),
            "peek_child_exited must not reap the child"
        );
        assert!(
            child.try_wait().ok().flatten().is_some(),
            "an exit status must still be collectible after peek_child_exited reported it"
        );
    }

    /// PR #642 review round 3, Medium #1: a fake `probe` that always
    /// answers `EPERM` (standing in for a group member that is alive but
    /// running under another uid — or, just as well, one simply stuck `Ok`
    /// forever) must make [`confirm_group_gone`] give up at its own
    /// deadline and report `false`, bounded, not hang. A real process that
    /// outlives SIGKILL forever cannot be constructed in a test; the
    /// decision this function makes from that sequence can.
    #[tokio::test]
    async fn confirm_group_gone_gives_up_bounded_on_persistent_eperm() {
        let began = Instant::now();
        let gone = confirm_group_gone(
            || Err(rustix::io::Errno::PERM),
            Instant::now() + Duration::from_millis(80),
        )
        .await;
        assert!(
            !gone,
            "a probe that never reports ESRCH must never be read as `group_confirmed_gone`"
        );
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "must give up at its own deadline, not hang: took {:?}",
            began.elapsed()
        );
    }

    /// Same fixture, a probe that always reports `Ok` (the group genuinely
    /// still alive) — the other half of M1's "组一直存在或 EPERM" wording.
    #[tokio::test]
    async fn confirm_group_gone_gives_up_bounded_on_persistent_alive() {
        let gone = confirm_group_gone(|| Ok(()), Instant::now() + Duration::from_millis(80)).await;
        assert!(!gone);
    }

    /// `ESRCH` must still be read as confirmed-gone, immediately — the
    /// positive counterpart the two tests above are contrasted against.
    #[tokio::test]
    async fn confirm_group_gone_reports_true_on_esrch() {
        let began = Instant::now();
        let gone = confirm_group_gone(
            || Err(rustix::io::Errno::SRCH),
            Instant::now() + Duration::from_secs(5),
        )
        .await;
        assert!(gone);
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "ESRCH must return immediately, not wait out the deadline"
        );
    }

    /// PR #642 review round 3, High #2 (a regression the previous round
    /// introduced): once an orphan's process group is confirmed gone, no
    /// SIGKILL may ever be sent — sending one unconditionally, as the
    /// previous version did, risks hitting an unrelated process that has
    /// since reused this exact pid/pgid number (the orphan's leader can be
    /// reaped by `init`/launchd at any moment this process is not
    /// watching).
    ///
    /// The existing black-box `tests/daemon_supervise.rs::reap_orphan_kills_a_live_pid_whose_start_time_matches`
    /// cannot catch this: its test process stays the real parent and never
    /// calls `wait()` until after `reap_orphan` returns, so the fixture
    /// stays a zombie — never actually `ESRCH` — for the whole call,
    /// masking exactly the window this test is for. Here, a background task
    /// reaps the fixture itself, the moment it exits, to simulate
    /// `init`/launchd having already done so — and a KILL actually going
    /// out is observed directly via `on_kill_attempt`, since sending KILL
    /// to an already-dead group is a silent no-op from the outside.
    #[tokio::test]
    async fn reap_orphan_group_gracefully_never_kills_once_the_group_is_confirmed_gone() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec sleep 9999")
            .process_group(0)
            .spawn()
            .expect("spawn the orphan fixture");
        let raw_pid = child.id().expect("freshly spawned child has a pid");
        let pid = i32::try_from(raw_pid)
            .ok()
            .and_then(Pid::from_raw)
            .expect("a real pid fits `Pid`");
        // Simulates `init`/launchd reaping an orphan the instant it exits —
        // this task is the one place in the test that ever calls `wait()`,
        // never `reap_orphan_group_gracefully` itself (an orphan's cleanup
        // never holds a `Child` for it either).
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        // Let the fixture actually start before signalling it.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let kill_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&kill_attempts);
        reap_orphan_group_gracefully(
            pid,
            Duration::from_secs(2),
            raw_pid,
            "never-checked-unless-escalating",
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .await;

        assert_eq!(
            kill_attempts.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the group was confirmed gone (ESRCH) within the grace — escalating to SIGKILL \
             after that point is exactly the pid-reuse hazard this fix closes"
        );
    }

    /// The identity-reverification half of the same fix: if the group is
    /// STILL alive after the full grace (so escalating is even considered),
    /// a `start_marker` that no longer matches must suppress the KILL
    /// entirely rather than risk hitting a process that has reused this
    /// pid/pgid number.
    #[tokio::test]
    async fn reap_orphan_group_gracefully_skips_kill_when_identity_no_longer_matches() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; exec sleep 9999")
            .process_group(0)
            .spawn()
            .expect("spawn a TERM-ignoring fixture");
        let raw_pid = child.id().expect("freshly spawned child has a pid");
        let pid = i32::try_from(raw_pid)
            .ok()
            .and_then(Pid::from_raw)
            .expect("a real pid fits `Pid`");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let kill_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&kill_attempts);
        reap_orphan_group_gracefully(
            pid,
            Duration::from_millis(100),
            raw_pid,
            "this will never match a real `ps -o lstart=` marker",
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .await;

        assert_eq!(
            kill_attempts.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a mismatched identity must suppress the KILL, not just warn and send it anyway"
        );

        let _ = child.kill().await;
    }

    // -------------------------------------------------------------------
    // COMM-4b: catch_up log scraping (COMM-HYPHAE.md §5.2, task table row
    // "日志中出现 incomplete 行后，状态变为 incomplete" / "任何输入都不会
    // 产生 complete 状态").
    // -------------------------------------------------------------------

    /// A fresh, empty scan cache — every `catch_up_from_log` call below
    /// wants its own, not one shared/left over from another test.
    fn fresh_scan() -> AsyncMutex<CatchUpScanState> {
        AsyncMutex::new(CatchUpScanState::default())
    }

    #[tokio::test]
    async fn catch_up_is_unknown_when_the_log_does_not_exist_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let status = catch_up_from_log(&tmp.path().join("no-such-log"), &fresh_scan()).await;
        assert_eq!(status.state, "unknown");
        assert_eq!(status.last_incomplete_at_ms, None);
    }

    #[tokio::test]
    async fn catch_up_becomes_incomplete_once_the_marker_line_appears() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hyphae-daemon.log");
        // The real line, verbatim from `internal/daemon/daemon.go`.
        tokio::fs::write(
            &log,
            "🚀 Starting daemon for 'alice'\n\
             [10:00:00] ⚠️  Inbox scan incomplete: dial tcp 127.0.0.1:4: connect: connection refused\n",
        )
        .await
        .unwrap();
        let status = catch_up_from_log(&log, &fresh_scan()).await;
        assert_eq!(status.state, "incomplete");
        assert!(
            status.last_incomplete_at_ms.is_some_and(|ms| ms > 0),
            "{status:?}"
        );
    }

    /// G2 / COMM-4b's third acceptance line: no input ever produces
    /// `"complete"`. Includes adversarial content that textually contains
    /// "complete" (every "incomplete" line does too, as a substring) to
    /// prove the match is on the exact marker, not a loose "complete"/
    /// "incomplete" keyword search that could flip either way.
    #[tokio::test]
    async fn catch_up_state_is_never_literally_complete_for_any_log_content() {
        let samples: &[&[u8]] = &[
            b"",
            b"Inbox scan incomplete: timeout",
            b"[10:00:00] \xe2\x9a\xa0\xef\xb8\x8f  Inbox scan incomplete: dial tcp refused\n",
            b"Catch-up complete\n",
            b"inbox scan complete, 0 incomplete\n", // different case + "complete" present
            b"complete complete complete",
            b"\x00\x01\xffrandom non-utf8 noise\xfe",
        ];
        for sample in samples {
            let tmp = tempfile::tempdir().unwrap();
            let log = tmp.path().join("hyphae-daemon.log");
            tokio::fs::write(&log, sample).await.unwrap();
            let status = catch_up_from_log(&log, &fresh_scan()).await;
            assert_ne!(
                status.state, "complete",
                "sample {sample:?} must never produce \"complete\""
            );
            assert!(
                matches!(status.state, "unknown" | "incomplete"),
                "sample {sample:?} -> unexpected catch_up state {:?}",
                status.state
            );
        }
    }

    /// Codex COMM-4b review, Medium #2: a plain `contains("Inbox scan
    /// incomplete")` substring search (the pre-fix implementation) is
    /// forgeable by any inbound message whose *body* happens to mention
    /// that exact phrase — comm's own daemon log interleaves Hyphae's own
    /// diagnostic lines with whatever a peer sent. The match must be
    /// anchored to Hyphae's full diagnostic line format (`[HH:MM:SS] ⚠️
    /// Inbox scan incomplete:` at the start of a line,
    /// `internal/daemon/daemon.go:178,191`), not merely the phrase
    /// anywhere in the file.
    #[tokio::test]
    async fn catch_up_ignores_the_marker_phrase_inside_an_ordinary_message_body_line() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hyphae-daemon.log");
        tokio::fs::write(
            &log,
            "🚀 Starting daemon for 'alice'\n\
             📨 bob → alice: don't worry about that 'Inbox scan incomplete' thing, it's fine\n",
        )
        .await
        .unwrap();
        let status = catch_up_from_log(&log, &fresh_scan()).await;
        assert_eq!(
            status.state, "unknown",
            "a message body merely mentioning the phrase must never be mistaken for \
             Hyphae's own diagnostic line: {status:?}"
        );
    }

    /// Codex COMM-4b review, High: a log file many times larger than
    /// [`CATCH_UP_SCAN_CHUNK_BYTES`], with the marker line only at the very
    /// end, must still be detected correctly by the chunked scan (not just
    /// "doesn't crash") — proving the chunk loop actually drains every
    /// chunk up to the current length rather than stopping after the
    /// first one.
    #[tokio::test]
    async fn catch_up_detects_a_marker_line_at_the_end_of_a_log_far_larger_than_one_scan_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hyphae-daemon.log");
        let mut content =
            "👋 padding\n".repeat((CATCH_UP_SCAN_CHUNK_BYTES * 5) / "👋 padding\n".len() + 1);
        content.push_str("[23:59:59] ⚠️  Inbox scan incomplete: dial tcp refused\n");
        assert!(
            content.len() > CATCH_UP_SCAN_CHUNK_BYTES * 4,
            "test setup: content must actually exceed several scan chunks"
        );
        tokio::fs::write(&log, &content).await.unwrap();

        let scan = fresh_scan();
        let status = catch_up_from_log(&log, &scan).await;
        assert_eq!(status.state, "incomplete", "{status:?}");
    }

    /// Codex COMM-4b review, High: a marker line split exactly across two
    /// [`CATCH_UP_SCAN_CHUNK_BYTES`]-sized chunks of the SAME scan must
    /// still be recognized — proving the in-scan `tail` carry (not just
    /// the cross-CALL carry the other tests exercise) actually works.
    #[tokio::test]
    async fn catch_up_detects_a_marker_line_split_exactly_across_two_scan_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hyphae-daemon.log");
        let marker_line = "[08:30:00] \u{26A0}\u{FE0F}  Inbox scan incomplete: timeout\n";
        // Padding sized so the marker line's START lands a few bytes before
        // the first chunk boundary, forcing the split.
        let pad_len = CATCH_UP_SCAN_CHUNK_BYTES - 5;
        let mut content = "a".repeat(pad_len);
        content.push('\n');
        content.push_str(marker_line);
        tokio::fs::write(&log, &content).await.unwrap();

        let scan = fresh_scan();
        let status = catch_up_from_log(&log, &scan).await;
        assert_eq!(
            status.state, "incomplete",
            "a marker line split across two scan chunks must still be found: {status:?}"
        );
    }

    /// Codex COMM-4b review, High: incremental re-scans (cached offset)
    /// must behave identically to a from-scratch scan — writing the log in
    /// several separate pieces, with a `catch_up()` call after each piece,
    /// must end up in the same state as writing it all at once.
    #[tokio::test]
    async fn catch_up_incremental_scans_across_several_calls_agree_with_one_full_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = idle_ctx(tmp.path()).await;
        let log_path = ctx.log_path.clone();
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        let sup = HyphaeDaemonSupervisor::spawn(ctx);

        tokio::fs::write(&log_path, "🚀 Starting daemon for 'alice'\n")
            .await
            .unwrap();
        assert_eq!(sup.catch_up().await.state, "unknown");

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .await
            .unwrap();
        file.write_all(b"retry ticker: processed outbox\n")
            .await
            .unwrap();
        file.flush().await.unwrap();
        assert_eq!(sup.catch_up().await.state, "unknown");

        file.write_all(b"[12:00:00] \xe2\x9a\xa0\xef\xb8\x8f  Inbox scan incomplete: timeout\n")
            .await
            .unwrap();
        file.flush().await.unwrap();
        assert_eq!(sup.catch_up().await.state, "incomplete");

        // Further plain appends after the marker must stay "incomplete",
        // never fall back to "unknown".
        file.write_all(b"cleanup ticker fired\n").await.unwrap();
        file.flush().await.unwrap();
        assert_eq!(sup.catch_up().await.state, "incomplete");
    }

    /// Codex COMM-4b review, High: truncating/replacing the log mid-stream
    /// (exactly what COMM-4a's own spawn-time truncation, or a future
    /// rotation, does) must reset the scan from byte 0 rather than
    /// computing nonsense against a now-shorter or now-different file.
    #[tokio::test]
    async fn catch_up_rescans_from_scratch_when_the_log_is_truncated_and_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hyphae-daemon.log");
        tokio::fs::write(
            &log,
            "🚀 a very long preamble that is definitely not a marker line\n",
        )
        .await
        .unwrap();
        let scan = fresh_scan();
        assert_eq!(catch_up_from_log(&log, &scan).await.state, "unknown");

        // Truncate and replace with fresh, shorter content whose own
        // marker line sits right at the new file's start.
        tokio::fs::write(
            &log,
            b"[00:00:01] \xe2\x9a\xa0\xef\xb8\x8f  Inbox scan incomplete: reset\n",
        )
        .await
        .unwrap();
        let status = catch_up_from_log(&log, &scan).await;
        assert_eq!(
            status.state, "incomplete",
            "a truncated-and-replaced log must be rescanned from byte 0, not skipped \
             because its new length is shorter than the old offset: {status:?}"
        );
    }

    /// Codex COMM-4b review, High: non-UTF-8 bytes immediately adjacent to
    /// (not just far away from) a real marker line must neither panic nor
    /// corrupt the byte-level match — this crate does pure `[u8]`
    /// comparison, never a `String::from_utf8_lossy` decode, specifically
    /// to avoid lossy-decoding artifacts shifting byte offsets near a
    /// match.
    #[tokio::test]
    async fn catch_up_matches_byte_exact_even_with_invalid_utf8_immediately_around_the_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hyphae-daemon.log");
        let mut content: Vec<u8> = b"\xff\xfe garbage before \xc0\xc1\n".to_vec();
        content.extend_from_slice(
            b"[01:02:03] \xe2\x9a\xa0\xef\xb8\x8f  Inbox scan incomplete: \xfe\xff more garbage\n",
        );
        tokio::fs::write(&log, &content).await.unwrap();

        let scan = fresh_scan();
        let status = catch_up_from_log(&log, &scan).await;
        assert_eq!(status.state, "incomplete", "{status:?}");
    }

    /// Codex COMM-4b review, High: concurrent `catch_up()` calls against
    /// the SAME supervisor (as real concurrent `GET /comm/daemon`/`start`/
    /// `stop` requests would produce) must never panic or deadlock, and
    /// must all agree on the final state once the writer is done — proving
    /// the `tokio::sync::Mutex` around the scan cache actually serializes
    /// concurrent scans against the same offset/tail state.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_catch_up_calls_never_panic_and_agree_on_the_final_state() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = idle_ctx(tmp.path()).await;
        let log_path = ctx.log_path.clone();
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(&log_path, b"").await.unwrap();
        let sup = Arc::new(HyphaeDaemonSupervisor::spawn(ctx));

        let writer_log = log_path.clone();
        let writer = tokio::spawn(async move {
            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&writer_log)
                .await
                .unwrap();
            for i in 0..200u32 {
                file.write_all(format!("padding line {i}\n").as_bytes())
                    .await
                    .unwrap();
                file.flush().await.unwrap();
            }
            file.write_all(b"[11:22:33] \xe2\x9a\xa0\xef\xb8\x8f  Inbox scan incomplete: done\n")
                .await
                .unwrap();
            file.flush().await.unwrap();
        });

        let mut readers = Vec::new();
        for _ in 0..16 {
            let sup = Arc::clone(&sup);
            readers.push(tokio::spawn(async move { sup.catch_up().await }));
        }

        writer.await.unwrap();
        for r in readers {
            // Any intermediate state ("unknown" before the marker line was
            // written, "incomplete" after) is fine; a panic or a hang is not.
            let status = r.await.unwrap();
            assert!(
                matches!(status.state, "unknown" | "incomplete"),
                "{status:?}"
            );
        }

        let final_status = sup.catch_up().await;
        assert_eq!(final_status.state, "incomplete", "{final_status:?}");
    }

    // -------------------------------------------------------------------
    // COMM-4b: manual relay probe result storage.
    // -------------------------------------------------------------------

    /// A minimal [`Ctx`] whose binary is never actually invoked — the
    /// supervisor starts `Idle` and spawns nothing until `start()` is
    /// called, which this test never does.
    async fn idle_ctx(dir: &Path) -> Ctx {
        let source = dir.join("hyphae-fake.sh");
        tokio::fs::write(&source, "#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        let bytes = tokio::fs::read(&source).await.unwrap();
        let expected = crate::binary::sha256_of(&bytes);
        let bin = crate::binary::VerifiedBinary::install(&source, expected, &dir.join("bin"))
            .await
            .unwrap();
        let home = dir.join("home");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let runner = Arc::new(crate::runner::HyphaeRunner::new(
            bin,
            home.clone(),
            Duration::from_secs(5),
        ));
        Ctx {
            runner,
            password_store: Arc::new(crate::password_store::MemoryPasswordStore::new()),
            home,
            pid_path: dir.join("hyphae-daemon.pid"),
            log_path: dir.join("logs").join("hyphae-daemon.log"),
        }
    }

    /// Codex COMM-4b review, Medium #3: `last_incomplete_at_ms` must only
    /// move when the incremental scan NEWLY recognizes an incomplete line
    /// — not every time `catch_up()` happens to run after the log file's
    /// mtime changed for some unrelated reason (e.g. the daemon appending
    /// ordinary, non-marker output). The pre-fix implementation read the
    /// log FILE's mtime on every call, so any later write — marker or
    /// not — pushed the timestamp forward.
    #[tokio::test]
    async fn catch_up_last_incomplete_at_ms_does_not_move_when_only_plain_lines_are_appended_afterward()
     {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = idle_ctx(tmp.path()).await;
        let log_path = ctx.log_path.clone();
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        let sup = HyphaeDaemonSupervisor::spawn(ctx);

        tokio::fs::write(
            &log_path,
            "[10:00:00] ⚠️  Inbox scan incomplete: dial tcp refused\n",
        )
        .await
        .unwrap();
        let first = sup.catch_up().await;
        assert_eq!(first.state, "incomplete", "{first:?}");
        let first_ms = first
            .last_incomplete_at_ms
            .expect("a just-recognized incomplete line must carry a timestamp");

        // Give the filesystem clock room to tick forward, then append a
        // perfectly ordinary line (no marker) — this used to bump the
        // log's mtime and, with it, the reported `last_incomplete_at_ms`.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .await
            .unwrap();
        file.write_all(b"retry ticker: processed outbox, nothing pending\n")
            .await
            .unwrap();
        file.flush().await.unwrap();
        drop(file);

        let second = sup.catch_up().await;
        assert_eq!(second.state, "incomplete", "{second:?}");
        assert_eq!(
            second.last_incomplete_at_ms,
            Some(first_ms),
            "appending a plain, non-marker line must never move the fault timestamp: \
             first={first:?} second={second:?}"
        );
    }

    #[tokio::test]
    async fn relay_probe_status_round_trips_through_record() {
        let tmp = tempfile::tempdir().unwrap();
        let sup = HyphaeDaemonSupervisor::spawn(idle_ctx(tmp.path()).await);
        assert!(sup.relay_probe_status().is_none(), "no probe has run yet");

        sup.record_relay_probe(RelayProbeStatus {
            url: Some("wss://relay.example".to_owned()),
            connected: true,
            at_ms: 123,
            error: None,
        });
        let probe = sup.relay_probe_status().expect("just recorded");
        assert!(probe.connected);
        assert_eq!(probe.url.as_deref(), Some("wss://relay.example"));
        assert_eq!(probe.error, None);

        sup.record_relay_probe(RelayProbeStatus {
            url: Some("wss://relay.example".to_owned()),
            connected: false,
            at_ms: 456,
            error: Some("dial tcp: connection refused".to_owned()),
        });
        let probe = sup.relay_probe_status().expect("just recorded");
        assert!(
            !probe.connected,
            "the latest probe replaces the previous one"
        );
    }

    /// pre-pr-check C2: `relay_probe` is a new concurrency primitive
    /// (`Arc<Mutex<Option<RelayProbeStatus>>>`), written by `record_relay_probe`
    /// unconditionally (it is a plain replace, not a check-then-act on the
    /// PREVIOUS value, so there is no TOCTOU window C1 would ask about) and
    /// read by `relay_probe_status`. The property that actually needs a real
    /// concurrent test is that the `Mutex` serializes the whole struct
    /// replace: a reader must never observe a torn mix of one writer's `url`
    /// with another writer's `at_ms`. Each of 64 concurrent writers records a
    /// probe whose three fields are all derived from the same index, so any
    /// mismatch among them in the end state proves a torn write.
    ///
    /// Codex COMM-4b review, Low: the earlier version of this test ran on
    /// the DEFAULT (single-threaded) `#[tokio::test]` runtime, so its 64
    /// "concurrent" writer tasks only ever interleaved cooperatively on one
    /// OS thread, never truly in parallel; and it joined every writer
    /// BEFORE reading even once, so no reader ever actually ran while a
    /// writer was still in flight. This version runs on a multi-thread
    /// runtime, uses a [`tokio::sync::Barrier`] so every writer and reader
    /// starts its critical section at the same instant, and keeps reader
    /// tasks spinning on `relay_probe_status()` for as long as writers are
    /// still running, so a torn struct — if the `Mutex` ever let one
    /// through — has an actual chance to be observed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_relay_probe_writes_never_tear_the_recorded_struct() {
        const WRITERS: usize = 64;
        const READERS: usize = 8;

        let tmp = tempfile::tempdir().unwrap();
        let sup = Arc::new(HyphaeDaemonSupervisor::spawn(idle_ctx(tmp.path()).await));
        let barrier = Arc::new(tokio::sync::Barrier::new(WRITERS + READERS));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut writer_tasks = Vec::with_capacity(WRITERS);
        for i in 0..WRITERS as u64 {
            let sup = Arc::clone(&sup);
            let barrier = Arc::clone(&barrier);
            writer_tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                sup.record_relay_probe(RelayProbeStatus {
                    url: Some(format!("wss://relay-{i}.example")),
                    connected: i.is_multiple_of(2),
                    at_ms: i,
                    error: if i.is_multiple_of(2) {
                        None
                    } else {
                        Some(format!("down-{i}"))
                    },
                });
            }));
        }

        let mut reader_tasks = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            let sup = Arc::clone(&sup);
            let barrier = Arc::clone(&barrier);
            let done = Arc::clone(&done);
            reader_tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let mut reads = 0u64;
                // Keep reading, concurrently with the writers above (real
                // OS-thread parallelism under the multi_thread flavor),
                // until every writer has been joined. `yield_now` matters
                // here: without an `.await` point, this loop would never
                // hand its worker thread back to the scheduler, and could
                // starve the very writer tasks it's supposed to race
                // against.
                while !done.load(std::sync::atomic::Ordering::Acquire) {
                    if let Some(probe) = sup.relay_probe_status() {
                        let i = probe.at_ms;
                        assert_eq!(
                            probe.url.as_deref(),
                            Some(format!("wss://relay-{i}.example").as_str()),
                            "torn write: {probe:?}"
                        );
                        assert_eq!(
                            probe.connected,
                            i.is_multiple_of(2),
                            "torn write: {probe:?}"
                        );
                        let expected_error = if i.is_multiple_of(2) {
                            None
                        } else {
                            Some(format!("down-{i}"))
                        };
                        assert_eq!(probe.error, expected_error, "torn write: {probe:?}");
                    }
                    reads += 1;
                    tokio::task::yield_now().await;
                }
                reads
            }));
        }

        for w in writer_tasks {
            w.await.unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Release);

        let mut total_reads = 0u64;
        for r in reader_tasks {
            total_reads += r.await.unwrap();
        }
        assert!(
            total_reads > 0,
            "readers never got a chance to run concurrently with writers"
        );

        let probe = sup
            .relay_probe_status()
            .expect("64 writers ran; something must be recorded");
        // Every field must come from the SAME writer `i`, not a mix.
        let i = probe.at_ms;
        assert_eq!(
            probe.url.as_deref(),
            Some(format!("wss://relay-{i}.example").as_str())
        );
        assert_eq!(
            probe.connected,
            i.is_multiple_of(2),
            "torn write: {probe:?}"
        );
        let expected_error = if i.is_multiple_of(2) {
            None
        } else {
            Some(format!("down-{i}"))
        };
        assert_eq!(probe.error, expected_error, "torn write: {probe:?}");
    }
}
