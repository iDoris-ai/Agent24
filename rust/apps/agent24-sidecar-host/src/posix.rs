//! POSIX lifecycle ownership primitives.

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::os::unix::process::CommandExt;
use std::{
    ffi::OsString,
    io,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

/// Polling bounds are deliberately finite: stop must report an unconfirmed
/// generation rather than make its caller hang forever on a stuck process.
const LEADER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const GROUP_EMPTY_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL: Duration = Duration::from_millis(10);

/// Inputs for one helper generation. The owner, not the caller, chooses the
/// process group: the child becomes the group leader before it can exec.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    executable: PathBuf,
    cwd: PathBuf,
    argv: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl LaunchSpec {
    pub fn new(executable: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            cwd: cwd.into(),
            argv: Vec::new(),
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, value: impl Into<OsString>) -> Self {
        self.argv.push(value.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    TerminationRequested,
    ForceKillRequested,
    LeaderReaped,
    Reaped,
}

/// A stop could not establish ownership-safe completion within its bound.
#[derive(Debug)]
pub enum StopError {
    /// The requested operation is not valid for this lifecycle phase.
    InvalidState(&'static str),
    /// The caller may retry while this generation remains owned.
    Retryable {
        operation: &'static str,
        source: io::Error,
    },
    /// The leader was handled, but the group was not confirmed empty.
    Unconfirmed {
        operation: &'static str,
        source: io::Error,
    },
}

impl std::fmt::Display for StopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidState(message) => f.write_str(message),
            Self::Retryable { operation, source } => write!(f, "{operation}: {source}"),
            Self::Unconfirmed { operation, source } => write!(f, "{operation}: {source}"),
        }
    }
}

impl std::error::Error for StopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Retryable { source, .. } | Self::Unconfirmed { source, .. } => Some(source),
            Self::InvalidState(_) => None,
        }
    }
}

/// The sole owner of one helper generation and its process group.
///
/// `Child` stays held, and therefore unreaped, until [`Self::reap_after_stop`]
/// is called. This keeps the leader's PID/PGID reserved while a stop sequence
/// is deciding whether graceful termination was sufficient.
#[derive(Debug)]
pub struct OwnedGeneration {
    child: Option<Child>,
    group: Pid,
    phase: Phase,
    status: Option<ExitStatus>,
    permit: Option<GenerationPermit>,
}

impl OwnedGeneration {
    pub fn launch(spec: LaunchSpec) -> io::Result<Self> {
        if !spec.executable.is_absolute() || !spec.cwd.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "launch paths must be absolute",
            ));
        }
        let mut command = Command::new(spec.executable);
        command.current_dir(spec.cwd).args(spec.argv).envs(spec.env);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.process_group(0);
        let reaper = global_reaper();
        // Start the permanent worker and reserve the single generation slot
        // before creating a process. This makes the one-host/one-generation
        // rule a property of this type, rather than a promise to callers.
        reaper.start()?;
        let permit = reaper.reserve()?;
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                drop(permit);
                return Err(error);
            }
        };
        let leader = match i32::try_from(child.id()) {
            Ok(leader) => leader,
            Err(_) => {
                // This cannot happen on a conforming POSIX host, but retain
                // ownership if it does: terminate and reap before returning
                // the initialization error and releasing the permit.
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                drop(permit);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "child pid does not fit POSIX pid_t",
                ));
            }
        };
        Ok(Self {
            child: Some(child),
            group: Pid::from_raw(leader),
            phase: Phase::Running,
            status: None,
            permit: Some(permit),
        })
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        match self.phase {
            Phase::Running => self.signal(Signal::SIGTERM, Phase::TerminationRequested),
            Phase::TerminationRequested | Phase::ForceKillRequested | Phase::LeaderReaped => Ok(()),
            Phase::Reaped => Err(invalid_state("generation has been reaped")),
        }
    }

    pub fn force_kill(&mut self) -> io::Result<()> {
        match self.phase {
            Phase::Running | Phase::TerminationRequested => {
                self.signal(Signal::SIGKILL, Phase::ForceKillRequested)
            }
            // A force-kill request is idempotent. In particular, a later
            // terminate call cannot regress this phase or send SIGTERM.
            Phase::ForceKillRequested | Phase::LeaderReaped => Ok(()),
            Phase::Reaped => Err(invalid_state("generation has been reaped")),
        }
    }

    /// Reap only after a force-kill attempt, so descendants cannot outlive the
    /// generation merely because its leader handled SIGTERM and exited.
    pub fn reap_after_stop(&mut self) -> Result<ExitStatus, StopError> {
        if matches!(self.phase, Phase::Reaped) {
            self.confirm_group_empty(GROUP_EMPTY_TIMEOUT)?;
            self.permit.take();
            return self.status.ok_or(StopError::Unconfirmed {
                operation: "reap",
                source: io::Error::other("reaped generation has no exit status"),
            });
        }
        if matches!(self.phase, Phase::LeaderReaped) {
            self.confirm_group_empty(GROUP_EMPTY_TIMEOUT)?;
            self.phase = Phase::Reaped;
            self.permit.take();
            return self.status.ok_or(StopError::Unconfirmed {
                operation: "reap",
                source: io::Error::other("reaped generation has no exit status"),
            });
        }
        if !matches!(self.phase, Phase::ForceKillRequested) {
            return Err(StopError::InvalidState(
                "generation has not been force-killed",
            ));
        }

        // WNOWAIT pins the leader's PID/PGID while the mandatory group kill is
        // being settled. A timeout is returned to the owner for retry; it is
        // never converted into a successful stop.
        if !self.wait_for_leader_exit(LEADER_EXIT_TIMEOUT)? {
            return Err(StopError::Unconfirmed {
                operation: "leader exit",
                source: timeout_error("leader did not exit after SIGKILL"),
            });
        }
        let status = self.reap_bounded(LEADER_EXIT_TIMEOUT)?;
        self.status = Some(status);
        // Keep a distinct state while group confirmation is pending. If the
        // bounded confirmation fails, Drop must transfer the permit to a
        // group-only retry job rather than releasing it with descendants
        // still owned by this generation.
        self.phase = Phase::LeaderReaped;
        self.confirm_group_empty(GROUP_EMPTY_TIMEOUT)?;
        self.phase = Phase::Reaped;
        // The permit is released only after both exact-child reaping and
        // process-group emptiness have been confirmed.
        self.permit.take();
        Ok(status)
    }

    fn signal(&mut self, signal: Signal, next: Phase) -> io::Result<()> {
        if self.phase == Phase::Reaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "generation has been reaped",
            ));
        }
        match killpg(self.group, signal) {
            Ok(()) | Err(Errno::ESRCH) => {}
            // macOS reports EPERM for an exited leader whose whole group is
            // zombies. Accept that only after waitpid WNOWAIT confirms this
            // exact child has exited; EPERM with a live leader is real.
            Err(Errno::EPERM) if self.leader_exited()? => {}
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        }
        self.phase = next;
        Ok(())
    }

    fn leader_exited(&self) -> io::Result<bool> {
        self.leader_exited_wnowait()
    }

    fn leader_exited_wnowait(&self) -> io::Result<bool> {
        use rustix::process::{Pid as RustixPid, WaitId, WaitIdOptions, waitid};
        let pid = RustixPid::from_raw(self.group.as_raw())
            .ok_or_else(|| io::Error::other("invalid owned process-group id"))?;
        let flags = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        match waitid(WaitId::Pid(pid), flags) {
            Ok(Some(status)) if status.exited() || status.killed() || status.dumped() => Ok(true),
            Ok(_) => Ok(false),
            Err(error) if error == rustix::io::Errno::INTR => Ok(false),
            Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
        }
    }

    fn wait_for_leader_exit(&self, limit: Duration) -> Result<bool, StopError> {
        let deadline = Instant::now() + limit;
        loop {
            match self.leader_exited() {
                Ok(true) => return Ok(true),
                Ok(false) if Instant::now() < deadline => std::thread::sleep(EXIT_POLL),
                Ok(false) => return Ok(false),
                Err(source) => {
                    return Err(StopError::Retryable {
                        operation: "observe leader exit",
                        source,
                    });
                }
            }
        }
    }

    fn reap_bounded(&mut self, limit: Duration) -> Result<ExitStatus, StopError> {
        let deadline = Instant::now() + limit;
        loop {
            match self
                .child
                .as_mut()
                .ok_or_else(|| StopError::Retryable {
                    operation: "reap leader",
                    source: io::Error::other("owned child handle is missing"),
                })?
                .try_wait()
            {
                Ok(Some(status)) => {
                    self.child = None;
                    return Ok(status);
                }
                Ok(None) if Instant::now() < deadline => std::thread::sleep(EXIT_POLL),
                Ok(None) => {
                    return Err(StopError::Retryable {
                        operation: "reap leader",
                        source: timeout_error("leader exit was observed but reap did not complete"),
                    });
                }
                Err(source) => {
                    return Err(StopError::Retryable {
                        operation: "reap leader",
                        source,
                    });
                }
            }
        }
    }

    fn confirm_group_empty(&self, limit: Duration) -> Result<(), StopError> {
        let deadline = Instant::now() + limit;
        loop {
            match killpg(self.group, None) {
                Err(Errno::ESRCH) => return Ok(()),
                Ok(()) | Err(Errno::EPERM) if Instant::now() < deadline => {
                    std::thread::sleep(EXIT_POLL)
                }
                Ok(()) | Err(Errno::EPERM) => {
                    return Err(StopError::Unconfirmed {
                        operation: "confirm group empty",
                        source: timeout_error("process group still has members"),
                    });
                }
                Err(error) => {
                    return Err(StopError::Retryable {
                        operation: "probe group",
                        source: io::Error::from_raw_os_error(error as i32),
                    });
                }
            }
        }
    }
}

impl Drop for OwnedGeneration {
    fn drop(&mut self) {
        if self.phase != Phase::Reaped {
            // Drop is deliberately non-blocking. Child::wait here used to
            // hang the host forever on an uninterruptible process; ownership
            // is reported as unconfirmed by the next explicit stop attempt.
            // Reuse the ownership-safe signal path so macOS EPERM is only
            // accepted after WNOWAIT confirms this leader has exited.
            if self.phase != Phase::LeaderReaped {
                let _ = self.signal(Signal::SIGKILL, Phase::ForceKillRequested);
            }
            let child = self.child.take();
            let child_reaped = match (self.phase, child.is_some()) {
                (Phase::LeaderReaped, false) => true,
                (_, true) => false,
                // A live generation cannot lose its exact Child without
                // first becoming LeaderReaped; abort before that Child could
                // be silently discarded.
                _ => std::process::abort(),
            };
            let permit = match self.permit.take() {
                Some(permit) => permit,
                None => {
                    // This is an internal invariant violation: a live
                    // child can only exist while its generation permit is
                    // held. Abort before `Child` is dropped, so the host
                    // cannot silently leak an unreaped process.
                    std::process::abort();
                }
            };
            let job = ReapJob {
                child,
                group: self.group,
                _permit: permit,
                child_reaped,
            };
            if let Err(job) = global_reaper().enqueue(job) {
                // The permit makes this impossible in a valid state. Do not
                // drop the exact Child if corruption ever violates that
                // invariant: abort while it is still owned.
                let _ = job;
                std::process::abort();
            }
        }
    }
}

struct ReapJob {
    child: Option<Child>,
    group: Pid,
    _permit: GenerationPermit,
    child_reaped: bool,
}

impl ReapJob {
    /// Return `true` only after the exact leader has been reaped and its
    /// process group is confirmed empty. Errors retain the Child for retry.
    fn reap_once(&mut self) -> bool {
        if !self.child_reaped {
            match self.child.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(_status)) => self.child_reaped = true,
                    Ok(None) | Err(_) => return false,
                },
                None => return false,
            }
        }
        group_is_empty(self.group)
    }
}

struct GenerationPermit {
    reaper: Arc<Reaper>,
}

impl std::fmt::Debug for GenerationPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationPermit").finish_non_exhaustive()
    }
}

impl Drop for GenerationPermit {
    fn drop(&mut self) {
        let mut state = recover_lock(self.reaper.state.lock());
        // A permit is the authoritative active-generation bit. The slot may
        // already be empty because the worker owns the job, so only this bit
        // is cleared here.
        state.permit_held = false;
        self.reaper.available.notify_all();
    }
}

struct ReaperState {
    /// Exactly one generation may hold this permit. It remains held while a
    /// dropped child's job is owned by the worker.
    permit_held: bool,
    /// Permanent single-slot handoff. There is deliberately no queue: a
    /// second job cannot exist while the permit is held.
    slot: Option<ReapJob>,
    worker_started: bool,
}

struct Reaper {
    state: Mutex<ReaperState>,
    available: Condvar,
}

impl Reaper {
    fn new() -> Self {
        Self {
            state: Mutex::new(ReaperState {
                permit_held: false,
                slot: None,
                worker_started: false,
            }),
            available: Condvar::new(),
        }
    }

    fn start(self: &Arc<Self>) -> io::Result<()> {
        let mut state = recover_lock(self.state.lock());
        if state.worker_started {
            return Ok(());
        }
        // Keep the state lock through spawn so a concurrent launcher cannot
        // observe a worker that is merely starting and reserve a child before
        // thread creation has succeeded.
        state.worker_started = true;
        let worker = Arc::clone(self);
        if let Err(error) = thread::Builder::new()
            .name("agent24-sidecar-reaper".to_owned())
            .spawn(move || worker.run())
        {
            state.worker_started = false;
            return Err(error);
        }
        Ok(())
    }

    fn reserve(self: &Arc<Self>) -> io::Result<GenerationPermit> {
        let mut state = recover_lock(self.state.lock());
        if state.permit_held || state.slot.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another sidecar generation is still owned",
            ));
        }
        state.permit_held = true;
        Ok(GenerationPermit {
            reaper: Arc::clone(self),
        })
    }

    fn enqueue(self: &Arc<Self>, job: ReapJob) -> Result<(), ReapJob> {
        let mut state = recover_lock(self.state.lock());
        if state.slot.is_some() || !state.permit_held {
            return Err(job);
        }
        state.slot = Some(job);
        self.available.notify_one();
        Ok(())
    }

    fn run(self: Arc<Self>) {
        self.run_loop();
    }

    fn run_loop(&self) {
        loop {
            let mut job = {
                let mut state = recover_lock(self.state.lock());
                while state.slot.is_none() {
                    state = recover_lock(self.available.wait(state));
                }
                // The mutex is released before any wait/retry operation on
                // the child, so launches and the worker handoff never block
                // behind a stuck process.
                match state.slot.take() {
                    Some(job) => job,
                    None => continue,
                }
            };
            while !job.reap_once() {
                thread::sleep(EXIT_POLL);
            }
            // Dropping the job releases the permit only after exact-child
            // reaping and group-empty confirmation have both succeeded.
        }
    }
}

fn recover_lock<T>(result: std::sync::LockResult<T>) -> T {
    match result {
        Ok(value) => value,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn global_reaper() -> Arc<Reaper> {
    REAPER.get_or_init(|| Arc::new(Reaper::new())).clone()
}

static REAPER: OnceLock<Arc<Reaper>> = OnceLock::new();

fn invalid_state(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn timeout_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

fn group_is_empty(group: Pid) -> bool {
    matches!(killpg(group, None), Err(Errno::ESRCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleeping_generation() -> OwnedGeneration {
        match OwnedGeneration::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("sleep 30")) {
            Ok(generation) => generation,
            Err(error) => panic!("spawn /bin/sh: {error}"),
        }
    }

    fn exiting_generation() -> OwnedGeneration {
        match OwnedGeneration::launch(LaunchSpec::new("/bin/sh", "/").arg("-c").arg("exit 0")) {
            Ok(generation) => generation,
            Err(error) => panic!("spawn /bin/sh: {error}"),
        }
    }

    #[test]
    fn graceful_stop_keeps_leader_owned_until_reap() {
        let mut generation = sleeping_generation();
        assert!(generation.reap_after_stop().is_err());
        assert!(generation.terminate().is_ok());
        assert!(generation.reap_after_stop().is_err());
        assert!(generation.force_kill().is_ok());
        let status = match generation.reap_after_stop() {
            Ok(status) => status,
            Err(error) => panic!("reap leader: {error}"),
        };
        assert!(!status.success());
    }

    #[test]
    fn force_kill_is_limited_to_the_owned_group() {
        let mut generation = sleeping_generation();
        assert!(generation.force_kill().is_ok());
        let status = match generation.reap_after_stop() {
            Ok(status) => status,
            Err(error) => panic!("reap leader: {error}"),
        };
        assert!(!status.success());
        assert!(generation.force_kill().is_err());
    }

    #[test]
    fn phase_transitions_are_monotonic_and_idempotent() {
        let mut generation = sleeping_generation();
        assert!(generation.terminate().is_ok());
        assert!(generation.terminate().is_ok());
        assert!(generation.force_kill().is_ok());
        assert!(generation.force_kill().is_ok());
        // A force-kill request is terminal for signalling; terminate must not
        // regress it or send SIGTERM after SIGKILL.
        assert!(generation.terminate().is_ok());
        assert!(generation.reap_after_stop().is_ok());
        assert!(generation.terminate().is_err());
    }

    #[test]
    fn drop_does_not_wait_for_the_child() {
        let started = std::time::Instant::now();
        let generation = sleeping_generation();
        drop(generation);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "Drop unexpectedly waited for the child"
        );
    }

    #[test]
    fn drop_is_reaped_by_the_long_lived_global_worker() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

        let generation = sleeping_generation();
        let pid = match generation.child.as_ref() {
            Some(child) => child.id(),
            None => panic!("generation lost its child before drop"),
        };
        let started = Instant::now();
        drop(generation);
        assert!(started.elapsed() < Duration::from_millis(100));

        let pid = match i32::try_from(pid).ok().and_then(Pid::from_raw) {
            Some(pid) => pid,
            None => panic!("child pid does not fit POSIX pid_t"),
        };
        let flags = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match waitid(WaitId::Pid(pid), flags) {
                Err(error) if error == rustix::io::Errno::CHILD => break,
                Err(error) if error == rustix::io::Errno::INTR => {}
                Ok(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(_) => panic!("global worker did not reap child before deadline"),
                Err(error) => panic!("waitid: {error}"),
            }
        }
    }

    #[test]
    fn exited_leader_is_confirmed_before_group_kill_and_reap() {
        let mut generation = exiting_generation();
        assert!(matches!(
            generation.wait_for_leader_exit(std::time::Duration::from_secs(1)),
            Ok(true)
        ));
        assert!(generation.force_kill().is_ok());
        assert!(generation.reap_after_stop().is_ok());
    }

    /// This exercises the macOS all-zombie `killpg(SIGKILL) -> EPERM` case.
    /// Other platforms do not claim this regression because their kernel
    /// reports a different result for an exited, unreaped process group.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_exited_unreaped_group_eperm_is_retry_safe() {
        let mut generation = exiting_generation();
        assert!(matches!(
            generation.wait_for_leader_exit(std::time::Duration::from_secs(1)),
            Ok(true)
        ));
        assert!(generation.force_kill().is_ok());
        assert!(generation.reap_after_stop().is_ok());
    }
}
