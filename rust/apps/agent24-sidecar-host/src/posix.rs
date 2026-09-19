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
};

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
        })
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        self.signal(Signal::SIGTERM, Phase::TerminationRequested)
    }

    pub fn force_kill(&mut self) -> io::Result<()> {
        self.signal(Signal::SIGKILL, Phase::ForceKillRequested)
    }

    /// Reap only after a force-kill attempt, so descendants cannot outlive the
    /// generation merely because its leader handled SIGTERM and exited.
    pub fn reap_after_stop(&mut self) -> io::Result<ExitStatus> {
        if !matches!(self.phase, Phase::ForceKillRequested) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "generation has not been stopped",
            ));
        }
        let status = self.child.wait()?;
        self.phase = Phase::Reaped;
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
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        }
        self.phase = next;
        Ok(())
    }
}

impl Drop for OwnedGeneration {
    fn drop(&mut self) {
        if self.phase != Phase::Reaped {
            let _ = killpg(self.group, Signal::SIGKILL);
            let _ = self.child.wait();
        }
    }
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
