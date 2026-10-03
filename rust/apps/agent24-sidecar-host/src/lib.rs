//! Rust-owned lifecycle boundary for the desktop sidecar helper.
//!
//! The wire protocol is deliberately kept in `agent24-sidecar-host-protocol`.
//! Platform lifecycle code lives here so the executable remains a thin entry
//! point and cannot grow a second protocol implementation.

use std::time::{Duration, Instant};

#[allow(dead_code)]
pub(crate) mod actor;
#[allow(dead_code)]
mod cleanup;
#[allow(dead_code)]
mod control_io;
#[allow(dead_code)]
mod control_worker;
#[allow(dead_code)]
mod first_launch_dispatch;
#[allow(dead_code)]
mod first_launch_ingress;
#[allow(dead_code)]
mod generation_driver;
#[allow(dead_code)]
mod generation_harness;
mod host_ports;
mod host_session;
mod host_stdio;
#[allow(dead_code)]
mod launch;
#[allow(dead_code)]
mod launch_order;
#[allow(dead_code)]
mod native_generation;
#[allow(dead_code)]
mod outbox;
#[allow(dead_code)]
mod output_io;
#[allow(dead_code)]
mod output_worker;
#[allow(dead_code)]
mod pipe_access;
#[allow(dead_code)]
mod pre_owned_cleanup;
#[allow(dead_code)]
mod ready_io;
#[allow(dead_code)]
mod ready_read_worker;
#[allow(dead_code)]
mod stderr_drain_worker;
#[allow(dead_code)]
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

#[allow(dead_code)]
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
