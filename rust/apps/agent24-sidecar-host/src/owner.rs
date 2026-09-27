//! Windows lifecycle ownership backed by a processkit Job Object.

use std::{io, process::Stdio};

use crate::target::TreeObservation;
use processkit::ProcessGroup;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

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
            .map_err(io::Error::other)
    }

    pub fn spawn(self, mut command: Command) -> io::Result<OwnedProcess> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let generation = self.generation;
        let child = self.group.spawn(command).map_err(io::Error::other)?;
        Ok(OwnedProcess {
            generation,
            child,
            owner: self,
        })
    }
}

/// The only handles through which the actor may communicate with its child.
#[derive(Debug)]
pub struct OwnedPipes {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

#[derive(Debug)]
pub struct OwnedProcess {
    generation: GenerationId,
    child: Child,
    owner: GenerationOwner,
}

impl OwnedProcess {
    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Transfers all three child pipes exactly once.
    pub fn take_pipes(&mut self) -> io::Result<OwnedPipes> {
        match (
            self.child.stdin.take(),
            self.child.stdout.take(),
            self.child.stderr.take(),
        ) {
            (Some(stdin), Some(stdout), Some(stderr)) => Ok(OwnedPipes {
                stdin,
                stdout,
                stderr,
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child pipes have already been taken",
            )),
        }
    }

    /// Observes exit without consuming this process or releasing its Job.
    pub fn observe_exit(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Uses the Job Object's bounded counter, never a PID snapshot.
    pub fn tree_is_empty(&self) -> io::Result<bool> {
        self.owner
            .group
            .stats()
            .map(|stats| stats.active_process_count == 0)
            .map_err(io::Error::other)
    }

    pub(crate) fn reap_step(&mut self) -> io::Result<TreeObservation> {
        self.reap_step_with(|child| child.try_wait(), |process| process.tree_is_empty())
    }

    fn reap_step_with<W, P>(&mut self, wait: W, probe: P) -> io::Result<TreeObservation>
    where
        W: FnOnce(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
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
        self.owner.group.kill_all().map_err(io::Error::other)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn powershell(script: &str) -> Command {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ]);
        command
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

    fn process_is_alive(pid: u32) -> io::Result<bool> {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "tasklist failed with status {}",
                output.status
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
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
            .spawn(Command::new(
                "agent24-sidecar-program-that-does-not-exist.exe",
            ))
            .expect_err("missing executable must fail before returning a child");
        assert!(
            error
                .get_ref()
                .is_some_and(|source| source.is::<processkit::Error>()),
            "the processkit error must remain available as the source"
        );
    }

    #[tokio::test]
    async fn processkit_spawn_returns_a_generation_owned_process() {
        let generation = GenerationId::new(1).expect("non-zero generation");
        let owner = GenerationOwner::new(generation).expect("Job Object");
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "exit", "0"]);
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
        let OwnedPipes {
            mut stdin,
            mut stdout,
            mut stderr,
        } = pipes;
        let (stdout_text, stderr_text, status) =
            tokio::time::timeout(Duration::from_secs(10), async {
                stdin.write_all(b"hello\n").await.expect("write stdin");
                drop(stdin);
                let mut stdout_text = String::new();
                let mut stderr_text = String::new();
                stdout
                    .read_to_string(&mut stdout_text)
                    .await
                    .expect("read stdout");
                stderr
                    .read_to_string(&mut stderr_text)
                    .await
                    .expect("read stderr");
                let status = wait_until_exit(&mut process).await;
                (stdout_text, stderr_text, status)
            })
            .await
            .expect("pipe roundtrip deadline");
        assert_eq!(
            process.observe_exit().expect("repeat observe"),
            Some(status)
        );
        assert_eq!(stdout_text, "out:hello");
        assert_eq!(stderr_text, "err:hello");
        assert!(process.tree_is_empty().expect("Job stats"));
    }

    #[tokio::test]
    async fn exited_leader_does_not_hide_descendant_from_force_or_empty() {
        let marker = marker_path("exit-descendant");
        let marker_text = marker.display().to_string().replace('\'', "''");
        let script = format!(
            "$child = Start-Process powershell.exe -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -PassThru; Set-Content -LiteralPath '{marker_text}' -Value $child.Id"
        );
        let owner =
            GenerationOwner::new(GenerationId::new(5).expect("generation")).expect("Job Object");
        let mut process = owner
            .spawn(powershell(&script))
            .expect("spawn process tree");
        let descendant = read_pid(&marker).expect("child must publish a readable pid");
        assert!(wait_until_exit(&mut process).await.success());
        assert_eq!(
            process.reap_step().expect("leader-exit reap step"),
            TreeObservation::Present,
            "leader exit must not be mistaken for an empty Job tree"
        );
        process.force_kill().expect("force Job tree");
        process.force_kill().expect("repeat force Job tree");
        wait_until_confirmed_empty(&mut process).await;
        assert_eq!(
            process
                .reap_step()
                .expect("repeat confirmed-empty reap step"),
            TreeObservation::ConfirmedEmpty
        );
        wait_until_gone(descendant).expect("descendant teardown");
        let _ = std::fs::remove_file(marker);
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
        let script = format!(
            "$child = Start-Process powershell.exe -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -PassThru; Set-Content -LiteralPath '{marker_text}' -Value $child.Id; Wait-Process -Id $child.Id"
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
