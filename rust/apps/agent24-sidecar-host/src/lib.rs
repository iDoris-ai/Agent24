//! Rust-owned lifecycle boundary for the desktop sidecar helper.
//!
//! **frozen (ADR-038 Q1=(b)).** Open Design is hosted by the TypeScript
//! `SidecarManager` (#1580, #1584). No product crate depends on this one.
//! Windows CI (`.github/workflows/sidecar-windows.yml`) still compiles and
//! tests it. The wire protocol stays in `agent24-sidecar-host-protocol`.
//!
//! The crate-level `dead_code` allow replaces the per-module allows. Items
//! here are reached from unit tests and from the Windows target, not from
//! agent24d, the CLI, or the desktop. Deleting them would drop that CI.

#![allow(
    dead_code,
    reason = "frozen (ADR-038 Q1=(b)): kept for Windows CI and unit tests; not on the product path"
)]

use std::time::{Duration, Instant};

pub(crate) mod actor;
mod cleanup;
mod control_io;
mod control_worker;
mod first_launch_dispatch;
mod first_launch_ingress;
mod generation_driver;
mod generation_harness;
mod host_ports;
mod host_session;
mod host_stdio;
mod launch;
mod launch_order;
mod native_generation;
mod outbox;
mod output_io;
mod output_worker;
mod pipe_access;
mod pre_owned_cleanup;
mod ready_io;
mod ready_read_worker;
mod stderr_drain_worker;
mod worker_slots;

#[cfg(unix)]
mod posix;
#[cfg(unix)]
pub use posix::{LaunchSpec, OwnedGeneration, OwnedPipes, StopError};

#[cfg(windows)]
mod owner;
#[cfg(all(windows, test))]
mod windows_test_io;
#[cfg(windows)]
pub use owner::{GenerationId, GenerationOwner, OwnedPipes, OwnedProcess};

pub(crate) mod target;

#[cfg(test)]
mod native_harness_tests;

const HOST_LAUNCH_BUDGET: Duration = Duration::from_secs(10);
const HOST_OUTPUT_BUDGET: Duration = Duration::from_secs(2);
const HOST_LIMITS: actor::Deadlines = actor::Deadlines {
    launch: HOST_LAUNCH_BUDGET,
    ready: Duration::from_secs(30),
    graceful: Duration::from_secs(5),
    force: Duration::from_secs(10),
    drain: Duration::from_secs(10),
};

/// Run one host session over independently owned copies of process stdio.
pub fn run() -> std::io::Result<()> {
    let stdio = host_stdio::acquire().map_err(std::io::Error::other)?;
    let mut ports = host_ports::HostPorts::new_in(
        worker_slots::WorkerSlots::host(),
        stdio.stdin,
        stdio.stdout,
        HOST_OUTPUT_BUDGET,
    )
    .map_err(std::io::Error::other)?;
    host_session::run_session(
        &mut ports,
        HOST_LAUNCH_BUDGET,
        HOST_LIMITS,
        Instant::now,
        std::thread::sleep,
        || false,
    )
    .map_err(std::io::Error::other)
}
