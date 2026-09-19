use std::io;

use processkit::ProcessGroup;
use tokio::process::{Child, Command};

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

    pub fn spawn(self, command: Command) -> io::Result<OwnedProcess> {
        let generation = self.generation;
        let child = self.group.spawn(command).map_err(io::Error::other)?;
        Ok(OwnedProcess {
            generation,
            child,
            owner: self,
        })
    }
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

    pub async fn wait(mut self) -> io::Result<std::process::ExitStatus> {
        let result = self.child.wait().await;
        drop(self.owner);
        result
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

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

    fn read_pid(path: &Path) -> u32 {
        std::fs::read_to_string(path)
            .expect("child wrote its pid")
            .trim()
            .parse()
            .expect("child pid is numeric")
    }

    fn process_is_alive(pid: u32) -> bool {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false)
    }

    fn wait_until_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !process_is_alive(pid),
            "process {pid} survived Job teardown"
        );
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
        let process = owner
            .spawn(command)
            .expect("suspended spawn and assignment");
        assert_eq!(process.generation(), generation);
        assert!(process.wait().await.expect("wait").success());
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
        let descendant = read_pid(&marker);
        drop(process);
        wait_until_gone(descendant);
        let _ = std::fs::remove_file(marker);
    }

    #[tokio::test]
    async fn cancelling_wait_drops_the_owned_process_and_kills_it() {
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
        let child = read_pid(&marker);
        let timed_out = tokio::time::timeout(Duration::from_millis(100), process.wait())
            .await
            .expect_err("the sleep must outlive the bounded wait");
        drop(timed_out);
        wait_until_gone(child);
        let _ = std::fs::remove_file(marker);
    }
}
