//! POSIX lifecycle ownership primitives.

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::os::unix::process::CommandExt;
use std::{
    collections::VecDeque,
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
const REAPER_QUEUE_CAPACITY: usize = 64;

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
        reaper.start()?;
        let child = command.spawn()?;
        let leader = i32::try_from(child.id()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "child pid does not fit POSIX pid_t",
            )
        })?;
        Ok(Self {
            child: Some(child),
            group: Pid::from_raw(leader),
            phase: Phase::Running,
            status: None,
        })
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        match self.phase {
            Phase::Running => self.signal(Signal::SIGTERM, Phase::TerminationRequested),
            Phase::TerminationRequested | Phase::ForceKillRequested => Ok(()),
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
            Phase::ForceKillRequested => Ok(()),
            Phase::Reaped => Err(invalid_state("generation has been reaped")),
        }
    }

    /// Reap only after a force-kill attempt, so descendants cannot outlive the
    /// generation merely because its leader handled SIGTERM and exited.
    pub fn reap_after_stop(&mut self) -> Result<ExitStatus, StopError> {
        if matches!(self.phase, Phase::Reaped) {
            self.confirm_group_empty(GROUP_EMPTY_TIMEOUT)?;
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
        self.phase = Phase::Reaped;
        self.confirm_group_empty(GROUP_EMPTY_TIMEOUT)?;
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
            let _ = self.signal(Signal::SIGKILL, Phase::ForceKillRequested);
            if let Some(child) = self.child.take() {
                global_reaper().enqueue(child);
            }
        }
    }
}

struct ReapJob {
    child: Option<Child>,
}

impl ReapJob {
    fn reap_once(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait();
            self.child = None;
        }
    }
}

struct ReaperState {
    queue: VecDeque<ReapJob>,
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
                queue: VecDeque::with_capacity(REAPER_QUEUE_CAPACITY),
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

    fn enqueue(self: &Arc<Self>, child: Child) {
        // The queue is deliberately bounded. Waiting for one slot is
        // backpressure, but never drops the exact Child handle on overflow.
        let mut state = recover_lock(self.state.lock());
        let mut child = Some(child);
        while state.queue.len() >= REAPER_QUEUE_CAPACITY {
            state = recover_lock(self.available.wait(state));
        }
        if let Some(child) = child.take() {
            state.queue.push_back(ReapJob { child: Some(child) });
            self.available.notify_one();
        }
    }

    fn run(self: Arc<Self>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_loop()));
        let mut state = recover_lock(self.state.lock());
        state.worker_started = false;
        self.available.notify_all();
    }

    fn run_loop(&self) {
        loop {
            let mut job = {
                let mut state = recover_lock(self.state.lock());
                while state.queue.is_empty() {
                    state = recover_lock(self.available.wait(state));
                }
                let job = state.queue.pop_front();
                self.available.notify_all();
                match job {
                    Some(job) => job,
                    None => continue,
                }
            };
            // Keep the Child in `job` across a panic, then retry. The worker
            // itself is detached and remains alive for the host lifetime.
            loop {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.reap_once()))
                    .is_ok()
                {
                    break;
                }
            }
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
