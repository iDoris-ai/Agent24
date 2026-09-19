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
    child: Child,
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
        let child = command.spawn()?;
        let leader = i32::try_from(child.id()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "child pid does not fit POSIX pid_t",
            )
        })?;
        Ok(Self {
            child,
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
            return self
                .status
                .clone()
                .ok_or(StopError::Unconfirmed {
                    operation: "reap",
                    source: io::Error::other("reaped generation has no exit status"),
                });
        }
        if !matches!(self.phase, Phase::ForceKillRequested) {
            return Err(StopError::InvalidState("generation has not been force-killed"));
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
        self.status = Some(status.clone());
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
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
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
            let _ = killpg(self.group, Signal::SIGKILL);
        }
    }
}

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
}
