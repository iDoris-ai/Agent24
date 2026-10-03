//! Windows lifecycle ownership backed by a processkit Job Object.

use std::io::{self, PipeReader, PipeWriter};

use crate::target::TreeObservation;
use processkit::{ErrorReason, IsolatedPipedChild, IsolatedPipedCommand, ProcessGroup};

fn processkit_error(error: processkit::Error) -> io::Error {
    let kind = match error.reason() {
        ErrorReason::Io(source) | ErrorReason::Spawn { source, .. } => source.kind(),
        ErrorReason::NotFound { .. } => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationId(u64);

impl GenerationId {
    pub fn new(value: u64) -> io::Result<Self> {
        (value != 0)
            .then_some(Self(value))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "generation is zero"))
    }
}

#[derive(Debug)]
pub struct GenerationOwner {
    generation: GenerationId,
    group: ProcessGroup,
}

impl GenerationOwner {
    pub fn new(generation: GenerationId) -> io::Result<Self> {
        ProcessGroup::new()
            .map(|group| Self { generation, group })
            .map_err(processkit_error)
    }

    pub fn spawn(self, command: IsolatedPipedCommand) -> io::Result<OwnedProcess> {
        let generation = self.generation;
        let child = self
            .group
            .spawn_isolated_piped(command)
            .map_err(processkit_error)?;
        Ok(OwnedProcess { generation, child })
    }
}

/// The only handles through which the actor may communicate with its child.
#[derive(Debug)]
pub struct OwnedPipes {
    pub stdin: PipeWriter,
    pub stdout: PipeReader,
    pub stderr: PipeReader,
}

#[derive(Debug)]
pub struct OwnedProcess {
    generation: GenerationId,
    child: IsolatedPipedChild,
}

impl OwnedProcess {
    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Transfers all three child pipes exactly once.
    pub fn take_pipes(&mut self) -> io::Result<OwnedPipes> {
        match self.child.take_pipes() {
            Some(pipes) => Ok(OwnedPipes {
                stdin: pipes.stdin,
                stdout: pipes.stdout,
                stderr: pipes.stderr,
            }),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child pipes have already been taken",
            )),
        }
    }

    /// Observes exit without consuming this process or releasing its Job.
    pub fn observe_exit(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait().map_err(processkit_error)
    }

    /// Uses the Job Object's bounded counter, never a PID snapshot.
    pub fn tree_is_empty(&self) -> io::Result<bool> {
        self.child.tree_is_empty().map_err(processkit_error)
    }

    pub(crate) fn reap_step(&mut self) -> io::Result<TreeObservation> {
        self.reap_step_with(
            |child| child.try_wait().map_err(processkit_error),
            |process| process.tree_is_empty(),
        )
    }

    fn reap_step_with<W, P>(&mut self, wait: W, probe: P) -> io::Result<TreeObservation>
    where
        W: FnOnce(&mut IsolatedPipedChild) -> io::Result<Option<std::process::ExitStatus>>,
        P: FnOnce(&OwnedProcess) -> io::Result<bool>,
    {
        match wait(&mut self.child)? {
            None => Ok(TreeObservation::Present),
            Some(_) => Ok(if probe(self)? {
                TreeObservation::ConfirmedEmpty
            } else {
                TreeObservation::Present
            }),
        }
    }

    /// Force-kills exactly this owned Job tree. Repeated calls are safe.
    pub fn force_kill(&mut self) -> io::Result<()> {
        self.child.kill_all().map_err(processkit_error)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::windows_test_io::{
        NativePipes, powershell_executable, process_is_alive, read_pair_then_cleanup,
        windows_executable,
    };
    use std::io::Write;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn powershell(script: &str) -> IsolatedPipedCommand {
        IsolatedPipedCommand::new(powershell_executable()).args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
    }

    fn powershell_literal() -> String {
        powershell_executable()
            .display()
            .to_string()
            .replace('\'', "''")
    }

    fn marker_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "agent24-sidecar-{name}-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the Unix epoch")
                .as_nanos()
        ))
    }

    fn read_pid(path: &Path) -> io::Result<u32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = None;
        while Instant::now() < deadline {
            match std::fs::read_to_string(path).and_then(|raw| {
                raw.trim().parse().map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid child pid: {error}"),
                    )
                })
            }) {
                Ok(pid) => return Ok(pid),
                Err(error) => last_error = Some(error),
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "child pid marker was not readable")
        }))
    }

    fn wait_until_gone(pid: u32) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !process_is_alive(pid)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if process_is_alive(pid)? {
            return Err(io::Error::other(format!(
                "process {pid} survived Job teardown"
            )));
        }
        Ok(())
    }

    async fn wait_until_exit(process: &mut OwnedProcess) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match process.observe_exit() {
                Ok(Some(status)) => return status,
                Ok(None) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Ok(None) => panic!("child did not exit before deadline"),
                Err(error) => panic!("observe child exit: {error}"),
            }
        }
    }

    async fn wait_until_confirmed_empty(process: &mut OwnedProcess) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match process.reap_step() {
                Ok(TreeObservation::ConfirmedEmpty) => return,
                Ok(TreeObservation::Present) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Ok(TreeObservation::Present) => {
                    panic!("Job tree did not become confirmed empty before deadline")
                }
                Ok(TreeObservation::Unconfirmed) => {
                    panic!("Windows reap does not produce unconfirmed observations")
                }
                Err(error) => panic!("reap Job tree: {error}"),
            }
        }
    }

    #[test]
    fn generation_zero_is_rejected() {
        let error = GenerationId::new(0).expect_err("zero must not own a process");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn spawn_failure_preserves_processkit_error_without_a_child() {
        let generation = GenerationId::new(1).expect("non-zero generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let error = owner
            .spawn(IsolatedPipedCommand::new(
                std::env::temp_dir().join("agent24-sidecar-program-that-does-not-exist.exe"),
            ))
            .expect_err("missing executable must fail before returning a child");
        assert!(
            error
                .get_ref()
                .is_some_and(|source| source.is::<processkit::Error>()),
            "the processkit error must remain available as the source"
        );
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn processkit_spawn_returns_a_generation_owned_process() {
        let generation = GenerationId::new(1).expect("non-zero generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let command =
            IsolatedPipedCommand::new(windows_executable("cmd.exe")).args(["/C", "exit", "0"]);
        let mut process = owner
            .spawn(command)
            .expect("suspended spawn and assignment");
        assert_eq!(process.generation(), generation);
        assert!(wait_until_exit(&mut process).await.success());
        assert!(process.tree_is_empty().expect("Job stats"));
    }

    #[tokio::test]
    async fn pipes_transfer_once_and_exit_observation_keeps_job_owned() {
        let generation = GenerationId::new(4).expect("non-zero generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let script = "$line=[Console]::In.ReadLine(); [Console]::Out.Write(\"out:$line\"); [Console]::Error.Write(\"err:$line\")";
        let mut process = owner
            .spawn(powershell(script))
            .expect("suspended spawn and assignment");
        let pipes = process.take_pipes().expect("owned pipes");
        assert!(process.take_pipes().is_err());
        let NativePipes {
            mut stdin,
            stdout,
            stderr,
        } = NativePipes::try_from(pipes).expect("convert unpolled owned pipes");
        stdin.write_all(b"hello\n").expect("write stdin");
        drop(stdin);
        let (stdout, stderr) =
            read_pair_then_cleanup(stdout, stderr, 64, 64, Duration::from_secs(10), || {
                process.force_kill()
            })
            .expect("bounded pipe roundtrip");
        let status = wait_until_exit(&mut process).await;
        wait_until_confirmed_empty(&mut process).await;
        let observed = process.observe_exit().expect("repeat observe");
        assert_eq!(observed, Some(status));
        assert_eq!(
            String::from_utf8(stdout).expect("stdout UTF-8"),
            "out:hello"
        );
        assert_eq!(
            String::from_utf8(stderr).expect("stderr UTF-8"),
            "err:hello"
        );
        assert!(process.tree_is_empty().expect("Job stats"));
    }

    #[tokio::test]
    async fn exited_leader_does_not_hide_descendant_from_force_or_empty() {
        let marker = marker_path("exit-descendant");
        let marker_text = marker.display().to_string().replace('\'', "''");
        let powershell_path = powershell_literal();
        let script = format!(
            "$child = Start-Process '{powershell_path}' -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -PassThru; Set-Content -LiteralPath '{marker_text}' -Value $child.Id"
        );
        let owner =
            GenerationOwner::new(GenerationId::new(5).expect("generation")).expect("Job Object");
        let mut process = owner
            .spawn(powershell(&script))
            .expect("spawn process tree");
        let descendant = read_pid(&marker).expect("child must publish a readable pid");
        let status = wait_until_exit(&mut process).await;
        let leader_reap = process.reap_step().expect("leader-exit reap step");
        process.force_kill().expect("force Job tree");
        process.force_kill().expect("repeat force Job tree");
        wait_until_confirmed_empty(&mut process).await;
        let empty_reap = process
            .reap_step()
            .expect("repeat confirmed-empty reap step");
        wait_until_gone(descendant).expect("descendant teardown");
        let _ = std::fs::remove_file(marker);
        assert!(status.success());
        assert_eq!(
            leader_reap,
            TreeObservation::Present,
            "leader exit must not be mistaken for an empty Job tree"
        );
        assert_eq!(empty_reap, TreeObservation::ConfirmedEmpty);
    }

    #[test]
    fn reap_step_waits_at_most_once_and_does_not_probe_before_leader_exit() {
        use std::cell::Cell;

        let generation = GenerationId::new(6).expect("generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let mut process = owner
            .spawn(powershell("Start-Sleep -Seconds 30"))
            .expect("spawn process");
        let waits = Cell::new(0);
        let probes = Cell::new(0);
        assert_eq!(
            process
                .reap_step_with(
                    |_| {
                        waits.set(waits.get() + 1);
                        Ok(None)
                    },
                    |_| {
                        probes.set(probes.get() + 1);
                        Ok(true)
                    },
                )
                .expect("reap step"),
            TreeObservation::Present
        );
        assert_eq!(waits.get(), 1);
        assert_eq!(probes.get(), 0);
        assert!(
            process
                .reap_step_with(
                    |_| Err(io::Error::other("try_wait failed")),
                    |_| panic!("probe must not run after try_wait error"),
                )
                .is_err()
        );
        assert_eq!(probes.get(), 0);
        process.force_kill().expect("force Job tree");
    }

    #[test]
    fn reap_step_exited_leader_uses_job_probe_and_preserves_errors() {
        let owner =
            GenerationOwner::new(GenerationId::new(7).expect("generation")).expect("Job Object");
        let mut process = owner.spawn(powershell("exit 0")).expect("spawn process");
        let probes = std::cell::Cell::new(0);
        let nonempty = process
            .reap_step_with(
                |_| Ok(Some(std::process::ExitStatus::default())),
                |_| {
                    probes.set(probes.get() + 1);
                    Ok(false)
                },
            )
            .expect("nonempty probe");
        assert_eq!(nonempty, TreeObservation::Present);
        assert_eq!(probes.get(), 1);

        let error = process.reap_step_with(
            |_| Ok(Some(std::process::ExitStatus::default())),
            |_| {
                probes.set(probes.get() + 1);
                Err(io::Error::other("stats failed"))
            },
        );
        assert!(error.is_err());
        assert_eq!(probes.get(), 2);

        assert_eq!(
            process
                .reap_step_with(
                    |_| Ok(Some(std::process::ExitStatus::default())),
                    |_| {
                        probes.set(probes.get() + 1);
                        Ok(true)
                    },
                )
                .expect("empty probe"),
            TreeObservation::ConfirmedEmpty
        );
        assert_eq!(probes.get(), 3);
        assert_eq!(
            process
                .reap_step_with(
                    |_| Ok(Some(std::process::ExitStatus::default())),
                    |_| Ok(true),
                )
                .expect("repeat empty probe"),
            TreeObservation::ConfirmedEmpty
        );
        process.force_kill().expect("force Job tree");
    }

    #[tokio::test]
    async fn dropping_owned_process_kills_the_job_tree() {
        let marker = marker_path("drop-tree");
        let marker_text = marker.display().to_string().replace('\'', "''");
        let powershell_path = powershell_literal();
        let script = format!(
            "$child = Start-Process '{powershell_path}' -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -PassThru; Set-Content -LiteralPath '{marker_text}' -Value $child.Id; Wait-Process -Id $child.Id"
        );
        let generation = GenerationId::new(2).expect("non-zero generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let process = owner
            .spawn(powershell(&script))
            .expect("spawn process tree");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let descendant = read_pid(&marker).expect("child must publish a readable pid");
        drop(process);
        wait_until_gone(descendant).expect("tasklist must confirm descendant teardown");
        let _ = std::fs::remove_file(marker);
    }

    #[tokio::test]
    async fn dropping_owned_process_kills_it() {
        let marker = marker_path("cancel-wait");
        let marker_text = marker.display().to_string().replace('\'', "''");
        let script = format!(
            "Set-Content -LiteralPath '{marker_text}' -Value $PID; Start-Sleep -Seconds 120"
        );
        let generation = GenerationId::new(3).expect("non-zero generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let process = owner
            .spawn(powershell(&script))
            .expect("spawn long-running process");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let child = read_pid(&marker).expect("child must publish a readable pid");
        drop(process);
        wait_until_gone(child).expect("tasklist must confirm owned process teardown");
        let _ = std::fs::remove_file(marker);
    }
}
