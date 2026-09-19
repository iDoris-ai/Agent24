//! Rust-owned lifecycle boundary for the desktop sidecar helper.
//!
//! The wire protocol is deliberately kept in `agent24-sidecar-host-protocol`.
//! Platform lifecycle code lives here so the executable remains a thin entry
//! point and cannot grow a second protocol implementation.

#[cfg(unix)]
mod posix;

/// Run the host process.
pub fn run() -> std::io::Result<()> {
    // The stdio actor and protocol dispatch are introduced in a later slice.
    // Keeping this entry point inert makes the package independently buildable
    // while the ownership primitive is reviewed in isolation.
    Ok(())
}
