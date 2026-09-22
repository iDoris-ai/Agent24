//! Rust-owned lifecycle boundary for the desktop sidecar helper.
//!
//! The wire protocol is deliberately kept in `agent24-sidecar-host-protocol`.
//! Platform lifecycle code lives here so the executable remains a thin entry
//! point and cannot grow a second protocol implementation.

#[allow(dead_code)]
pub(crate) mod actor;
#[allow(dead_code)]
mod control_io;

#[cfg(unix)]
mod posix;
#[cfg(unix)]
pub use posix::{LaunchSpec, OwnedGeneration, StopError};

#[cfg(windows)]
mod owner;
#[cfg(windows)]
pub use owner::{GenerationId, GenerationOwner, OwnedProcess};

/// Run the host process.
pub fn run() -> std::io::Result<()> {
    // The stdio actor and protocol dispatch are introduced in a later slice.
    // Keeping this entry point inert makes the package independently buildable
    // while the ownership primitive is reviewed in isolation.
    Ok(())
}
