//! Windows lifecycle ownership backed by a processkit Job Object.

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
