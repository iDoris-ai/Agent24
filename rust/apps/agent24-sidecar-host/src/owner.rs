use std::io;

use processkit::ProcessGroup;
use tokio::process::{Child, Command};

/// The opaque identity of one sidecar generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationId(u64);

impl GenerationId {
    /// Reject zero so a missing generation cannot accidentally own a process.
    pub fn new(value: u64) -> io::Result<Self> {
        (value != 0)
            .then_some(Self(value))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "generation is zero"))
    }
}

/// The sole owner of one sidecar generation's process tree.
#[derive(Debug)]
pub struct GenerationOwner {
    generation: GenerationId,
    group: ProcessGroup,
}

impl GenerationOwner {
    /// Create an empty kill-on-close Job Object for `generation`.
    pub fn new(generation: GenerationId) -> io::Result<Self> {
        ProcessGroup::new()
            .map(|group| Self { generation, group })
            .map_err(|error| io::Error::other(error.to_string()))
    }

    /// Spawn exactly one process into this generation and transfer ownership
    /// to the returned handle. The command is consumed so it cannot be reused
    /// after processkit installs its suspended-spawn setup.
    pub fn spawn(self, command: Command) -> io::Result<OwnedProcess> {
        let generation = self.generation;
        let child = self
            .group
            .spawn(command)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(OwnedProcess {
            generation,
            child,
            owner: self,
        })
    }
}

/// A running process whose Job Object owner is tied to the same generation.
#[derive(Debug)]
pub struct OwnedProcess {
    generation: GenerationId,
    child: Child,
    owner: GenerationOwner,
}

impl OwnedProcess {
    /// Return the generation that owns this process.
    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Wait for the process while retaining its owner until the wait ends.
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

    #[test]
    fn generation_zero_is_not_accepted() {
        assert!(GenerationId::new(0).is_err());
    }

    #[test]
    fn generation_identity_is_not_a_pid_lookup() {
        let id = GenerationId::new(7).expect("non-zero generation");
        assert_eq!(id, GenerationId::new(7).expect("same generation"));
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
}
