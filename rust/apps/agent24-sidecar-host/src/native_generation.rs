//! Native assembly for one already-owned sidecar generation.
//!
//! This remains dormant: host stdio bootstrap and `run()` wiring are later
//! slices. The purpose here is to establish one concrete ownership boundary
//! before that wiring exists.

use std::time::Instant;

use crate::{
    actor::{Deadlines, Phase},
    control_worker::ControlWorker,
    generation_driver::GenerationDriver,
    launch::OwnedLaunch,
    launch_order::{ActorLaunchOrder, ActorLaunchOrderError, ScheduleState},
    output_worker::OutputWorker,
    pre_owned_cleanup::{IntoCleanupOwner, PreOwnedCleanup},
    ready_read_worker::ReadyReadWorker,
    stderr_drain_worker::{StderrDrainSnapshot, StderrDrainWorker},
    worker_slots::{WorkerSlotError, WorkerSlots},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeGenerationBuildErrorKind {
    MissingStdout,
    MissingStderr,
    Worker(WorkerSlotError),
    LaunchDeadlineExpired,
}

/// A failed assembly still owns the authoritative process target.
///
/// Pipe handles already moved into failed worker constructors may close. Consume this error into cleanup; do not retry assembly.
pub(crate) struct NativeGenerationBuildError {
    kind: NativeGenerationBuildErrorKind,
    launch: Box<OwnedLaunch>,
}

impl NativeGenerationBuildError {
    pub(crate) const fn kind(&self) -> NativeGenerationBuildErrorKind {
        self.kind
    }

    pub(crate) fn into_pre_owned_cleanup(self, phase: Phase, limits: Deadlines) -> PreOwnedCleanup {
        PreOwnedCleanup::new((*self.launch).into_cleanup_owner(), phase, limits, false)
    }
}

impl std::fmt::Debug for NativeGenerationBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeGenerationBuildError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// One real process owner plus its generation-local stdout/stderr workers.
///
/// Control input and host output stay borrowed from host-lifetime workers.
/// Stdout and stderr move exactly once out of `OwnedLaunch`; stdin stays with
/// the launch so graceful stop still closes the authoritative target's stdin.
pub(crate) struct NativeGeneration<'host> {
    driver: GenerationDriver<
        OwnedLaunch,
        &'host mut OutputWorker,
        &'host mut ControlWorker,
        ReadyReadWorker,
    >,
    stderr: StderrDrainWorker,
}

impl<'host> NativeGeneration<'host> {
    pub(crate) fn assemble(
        launch: OwnedLaunch,
        output: &'host mut OutputWorker,
        control: &'host mut ControlWorker,
        limits: Deadlines,
        launch_deadline: Instant,
        clock: impl FnOnce() -> Instant,
    ) -> Result<Self, NativeGenerationBuildError> {
        Self::assemble_in(
            WorkerSlots::host(),
            launch,
            output,
            control,
            limits,
            launch_deadline,
            clock,
        )
    }

    pub(crate) fn assemble_in(
        slots: &'static WorkerSlots,
        mut launch: OwnedLaunch,
        output: &'host mut OutputWorker,
        control: &'host mut ControlWorker,
        limits: Deadlines,
        launch_deadline: Instant,
        clock: impl FnOnce() -> Instant,
    ) -> Result<Self, NativeGenerationBuildError> {
        let stdout = match launch.pipes_mut().take_stdout() {
            Some(stdout) => stdout,
            None => {
                return Err(build_error(
                    NativeGenerationBuildErrorKind::MissingStdout,
                    launch,
                ));
            }
        };
        let stderr = match launch.pipes_mut().take_stderr() {
            Some(stderr) => stderr,
            None => {
                return Err(build_error(
                    NativeGenerationBuildErrorKind::MissingStderr,
                    launch,
                ));
            }
        };

        let ready = match ReadyReadWorker::from_native_stdout_in(slots, stdout) {
            Ok(ready) => ready,
            Err(error) => {
                return Err(build_error(
                    NativeGenerationBuildErrorKind::Worker(error),
                    launch,
                ));
            }
        };
        let stderr = match StderrDrainWorker::from_native_stderr_in(slots, stderr) {
            Ok(stderr) => stderr,
            Err(error) => {
                return Err(build_error(
                    NativeGenerationBuildErrorKind::Worker(error),
                    launch,
                ));
            }
        };
        let now = clock();
        if now >= launch_deadline {
            return Err(build_error(
                NativeGenerationBuildErrorKind::LaunchDeadlineExpired,
                launch,
            ));
        }
        let actor =
            ActorLaunchOrder::new(launch, output, Phase::Launching(launch_deadline), limits);
        Ok(Self {
            driver: GenerationDriver::new(actor, control, ready, now),
            stderr,
        })
    }

    pub(crate) fn step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        self.driver.step(now)
    }

    pub(crate) fn schedule_state(&self) -> ScheduleState {
        self.driver.schedule_state()
    }

    pub(crate) fn stderr_snapshot(&mut self) -> StderrDrainSnapshot {
        self.stderr.snapshot()
    }

    pub(crate) fn into_pre_owned_cleanup(self) -> PreOwnedCleanup {
        self.driver.into_pre_owned_cleanup()
    }
}

fn build_error(
    kind: NativeGenerationBuildErrorKind,
    launch: OwnedLaunch,
) -> NativeGenerationBuildError {
    NativeGenerationBuildError {
        kind,
        launch: Box::new(launch),
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        launch::LaunchIntent, stderr_drain_worker::StderrDrainStatus, target::TreeObservation,
        worker_slots::WorkerRole,
    };
    use agent24_sidecar_host_protocol::{PROTOCOL_VERSION, Reply, Request, decode_reply};
    use std::{
        collections::BTreeMap,
        io::{self, Read, Write},
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    const LIMITS: Deadlines = Deadlines {
        launch: Duration::from_secs(2),
        ready: Duration::from_secs(2),
        graceful: Duration::from_secs(1),
        force: Duration::from_secs(1),
        drain: Duration::from_secs(1),
    };

    struct PendingRead;

    impl Read for PendingRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }
    }

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(input);
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct BrokenWriter;

    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn launch(id: u64) -> OwnedLaunch {
        let ready = r#"{"type":"ready","protocol":1,"port":1,"token":"tttttttttttttttttttttttttttttttt","version":"v"}"#;
        let request = Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: id,
            executable: "/bin/sh".into(),
            cwd: "/".into(),
            argv: vec![
                "-c".into(),
                format!("printf '%s\\n' '{ready}'; printf 'diag' >&2; exec sleep 30"),
            ],
            env: BTreeMap::new(),
        };
        OwnedLaunch::start(LaunchIntent::from_request(request).unwrap()).unwrap()
    }

    #[test]
    fn native_assembly_moves_generation_pipes_and_reaches_ready() {
        let _test_guard = crate::posix::tests::test_lock();
        let slots = WorkerSlots::isolated();
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut output = OutputWorker::new_in(
            slots,
            SharedWriter(Arc::clone(&written)),
            Duration::from_secs(2),
        )
        .unwrap();
        let mut control = ControlWorker::new_in(slots, PendingRead, None).unwrap();
        let now = Instant::now();
        let mut generation = NativeGeneration::assemble_in(
            slots,
            launch(81),
            &mut output,
            &mut control,
            LIMITS,
            now + LIMITS.launch,
            || now,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(generation.schedule_state().phase, Phase::Running) {
            generation.step(Instant::now()).unwrap();
            if Instant::now() >= deadline {
                panic!("native generation did not reach Ready");
            }
            thread::yield_now();
        }

        let stderr_deadline = Instant::now() + Duration::from_secs(2);
        while generation.stderr_snapshot().bytes_drained < 4 {
            if Instant::now() >= stderr_deadline {
                panic!("stderr drainer did not retain progress");
            }
            thread::yield_now();
        }
        assert_eq!(
            generation.stderr_snapshot().status,
            StderrDrainStatus::Running
        );
        drop(generation);

        // Reaching Running means the driver observed the Owned reply flush.
        // The worker may already be back in Idle by the time this test polls it.
        let frames = written.lock().unwrap().clone();
        let first = frames
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap();
        assert!(matches!(
            decode_reply(first),
            Ok(Reply::Owned { request_id: 81, .. })
        ));
        crate::posix::tests::wait_for_reaper_idle();
    }

    #[test]
    fn worker_admission_failure_returns_the_same_cleanup_authority_before_owned() {
        let _test_guard = crate::posix::tests::test_lock();
        let slots = WorkerSlots::isolated();
        let ready_permit = slots.reserve(WorkerRole::ReadyRead).unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut output = OutputWorker::new_in(
            slots,
            SharedWriter(Arc::clone(&written)),
            Duration::from_secs(2),
        )
        .unwrap();
        let mut control = ControlWorker::new_in(slots, PendingRead, None).unwrap();
        let now = Instant::now();
        let error = match NativeGeneration::assemble_in(
            slots,
            launch(82),
            &mut output,
            &mut control,
            LIMITS,
            now + LIMITS.launch,
            || now,
        ) {
            Ok(_) => panic!("busy ReadyRead slot unexpectedly assembled"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            NativeGenerationBuildErrorKind::Worker(WorkerSlotError::Busy(WorkerRole::ReadyRead))
        );
        assert!(
            written.lock().unwrap().is_empty(),
            "Owned escaped failed assembly"
        );

        let now = Instant::now();
        let mut cleanup =
            error.into_pre_owned_cleanup(Phase::ForceStopping(now + LIMITS.force), LIMITS);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match cleanup.step(Instant::now()).unwrap() {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present | TreeObservation::Unconfirmed
                    if Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                observation => panic!("cleanup authority did not converge: {observation:?}"),
            }
        }
        drop(ready_permit);
        crate::posix::tests::wait_for_reaper_idle();
    }

    #[test]
    fn missing_stdout_build_error_retains_cleanup_owner() {
        let _test_guard = crate::posix::tests::test_lock();
        let slots = WorkerSlots::isolated();
        let mut launch = launch(84);
        drop(launch.pipes_mut().take_stdout());
        let mut output = OutputWorker::new_in(
            slots,
            SharedWriter(Arc::new(Mutex::new(Vec::new()))),
            Duration::from_secs(2),
        )
        .unwrap();
        let mut control = ControlWorker::new_in(slots, PendingRead, None).unwrap();
        let now = Instant::now();
        let error = match NativeGeneration::assemble_in(
            slots,
            launch,
            &mut output,
            &mut control,
            LIMITS,
            now + LIMITS.launch,
            || now,
        ) {
            Ok(_) => panic!("missing stdout unexpectedly assembled"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), NativeGenerationBuildErrorKind::MissingStdout);
        let mut cleanup =
            error.into_pre_owned_cleanup(Phase::Launching(now + LIMITS.launch), LIMITS);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match cleanup.step(Instant::now()).unwrap() {
                crate::target::TreeObservation::ConfirmedEmpty => break,
                crate::target::TreeObservation::Present if Instant::now() < deadline => {
                    thread::yield_now()
                }
                tree => panic!("missing-pipe cleanup did not converge: {tree:?}"),
            }
        }
        crate::posix::tests::wait_for_reaper_idle();
    }

    #[test]
    fn assembled_generation_failure_is_consumed_by_pre_owned_cleanup() {
        let _test_guard = crate::posix::tests::test_lock();
        let slots = WorkerSlots::isolated();
        let mut output = OutputWorker::new_in(slots, BrokenWriter, Duration::from_secs(2)).unwrap();
        let mut control = ControlWorker::new_in(slots, PendingRead, None).unwrap();
        let now = Instant::now();
        let mut generation = NativeGeneration::assemble_in(
            slots,
            launch(83),
            &mut output,
            &mut control,
            LIMITS,
            now + LIMITS.launch,
            || now,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !generation.schedule_state().terminal {
            let _ = generation.step(Instant::now());
            assert!(Instant::now() < deadline, "transport failure did not latch");
            thread::yield_now();
        }
        let mut cleanup = generation.into_pre_owned_cleanup();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match cleanup.step(Instant::now()).unwrap() {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present if Instant::now() < deadline => thread::yield_now(),
                observation => panic!("generation owner did not clean up: {observation:?}"),
            }
        }
        crate::posix::tests::wait_for_reaper_idle();
    }
}
