use agent24_sidecar_host_protocol::{Event, PROTOCOL_VERSION, Reply, encode_reply};

use crate::{
    actor::{Deadlines, Phase},
    cleanup::CleanupStepError,
    launch::OwnedLaunch,
    output_io::{OutputWriteError, OutputWriter, PutFrameError, WriteStep},
    ready_io::ReadyGate,
    target::TreeObservation,
};
use std::{
    io::{self, Write},
    time::Instant,
};

pub(crate) trait LaunchIdentity {
    fn request_id(&self) -> u64;
}

#[cfg(any(unix, windows))]
impl LaunchIdentity for OwnedLaunch {
    fn request_id(&self) -> u64 {
        self.request_id()
    }
}

pub(crate) trait FrameSink {
    fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError>;
    fn step(&mut self) -> Result<WriteStep, OutputWriteError>;
}

impl<W: Write> FrameSink for OutputWriter<W> {
    fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError> {
        OutputWriter::put(self, frame)
    }

    fn step(&mut self) -> Result<WriteStep, OutputWriteError> {
        OutputWriter::write_step(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchOrderStage {
    Contained,
    OwnedPending,
    AwaitReady,
    Ready,
    CleanupRequired,
}

pub(crate) struct LaunchOrder<L, S> {
    launch: L,
    sink: S,
    gate: ReadyGate,
    held_ready: Option<Event>,
    stage: LaunchOrderStage,
}

impl<L: LaunchIdentity, S: FrameSink> LaunchOrder<L, S> {
    pub(crate) fn new(launch: L, sink: S) -> Self {
        Self {
            launch,
            sink,
            gate: ReadyGate::new(),
            held_ready: None,
            stage: LaunchOrderStage::Contained,
        }
    }

    pub(crate) fn queue_owned(&mut self) -> Result<(), LaunchOrderStage> {
        if self.stage != LaunchOrderStage::Contained {
            return self.fail();
        }
        let reply = Reply::Owned {
            version: PROTOCOL_VERSION,
            request_id: self.launch.request_id(),
        };
        let frame = match encode_reply(&reply) {
            Ok(frame) => frame,
            Err(_) => return self.fail(),
        };
        if self.sink.put(frame).is_err() {
            return self.fail();
        }
        self.stage = LaunchOrderStage::OwnedPending;
        Ok(())
    }

    pub(crate) fn output_step(&mut self) -> Result<WriteStep, LaunchOrderStage> {
        if self.stage != LaunchOrderStage::OwnedPending {
            return self.fail();
        }
        match self.sink.step() {
            Ok(WriteStep::Pending) => Ok(WriteStep::Pending),
            Ok(WriteStep::Complete) => {
                self.stage = LaunchOrderStage::AwaitReady;
                Ok(WriteStep::Complete)
            }
            Ok(WriteStep::Idle) | Err(_) => self.fail(),
        }
    }

    pub(crate) fn ready(&mut self, input: &[u8]) -> Result<Option<Event>, LaunchOrderStage> {
        if self.stage == LaunchOrderStage::CleanupRequired {
            return Err(self.stage);
        }
        if self.stage == LaunchOrderStage::Contained {
            return self.fail();
        }
        let event = match self.gate.push(input) {
            Ok(Some(event)) => event,
            Ok(None) => return Ok(None),
            Err(_) => return self.fail(),
        };
        match self.stage {
            LaunchOrderStage::OwnedPending => {
                self.held_ready = Some(event);
                Ok(None)
            }
            LaunchOrderStage::AwaitReady => {
                self.stage = LaunchOrderStage::Ready;
                Ok(Some(event))
            }
            _ => self.fail(),
        }
    }

    pub(crate) fn take_ready(&mut self) -> Option<Event> {
        let event = (self.stage == LaunchOrderStage::AwaitReady)
            .then(|| self.held_ready.take())
            .flatten()?;
        self.stage = LaunchOrderStage::Ready;
        Some(event)
    }

    pub(crate) fn ready_eof(&mut self) -> Result<(), LaunchOrderStage> {
        if self.stage == LaunchOrderStage::CleanupRequired {
            return Err(self.stage);
        }
        match self.gate.finish() {
            Ok(()) => Ok(()),
            Err(_) => self.fail(),
        }
    }

    fn fail<T>(&mut self) -> Result<T, LaunchOrderStage> {
        self.stage = LaunchOrderStage::CleanupRequired;
        self.held_ready = None;
        Err(LaunchOrderStage::CleanupRequired)
    }
}

pub(crate) trait LaunchControl {
    fn stop(&mut self, force: bool) -> io::Result<()>;
    fn cleanup(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError>;
}

#[cfg(any(unix, windows))]
impl LaunchControl for OwnedLaunch {
    fn stop(&mut self, force: bool) -> io::Result<()> {
        if force {
            self.target_mut().request_stop(true)
        } else {
            self.parts_mut().1.close_stdin();
            self.target_mut().request_stop(false)
        }
    }

    fn cleanup(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError> {
        crate::cleanup::cleanup_step(phase, self.target_mut())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActorLaunchOrderError {
    CleanupRequired,
    InvalidTransition,
    Stop(io::ErrorKind),
    Reap(io::ErrorKind),
}

pub(crate) struct ActorLaunchOrder<L, S> {
    order: LaunchOrder<L, S>,
    phase: Phase,
    limits: Deadlines,
    released_ready: Option<Event>,
    terminal: Option<ActorLaunchOrderError>,
    force_ok: bool,
}

impl<L: LaunchIdentity + LaunchControl, S: FrameSink> ActorLaunchOrder<L, S> {
    pub(crate) fn new(launch: L, sink: S, phase: Phase, limits: Deadlines) -> Self {
        Self {
            order: LaunchOrder::new(launch, sink),
            phase,
            limits,
            released_ready: None,
            terminal: None,
            force_ok: false,
        }
    }

    pub(crate) fn queue_owned(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        self.check()?;
        self.advance(now);
        if self.stopping() {
            return Err(self.fail_force(now));
        }
        self.phase = match self.phase.owned(now, self.limits) {
            Ok(next @ Phase::AwaitReady(_)) => next,
            _ => return Err(self.fail_force(now)),
        };
        self.order.queue_owned().map_err(|_| self.fail_force(now))
    }

    pub(crate) fn output_step(&mut self, now: Instant) -> Result<WriteStep, ActorLaunchOrderError> {
        self.check()?;
        self.advance(now);
        if self.stopping() {
            return Err(self.fail_force(now));
        }
        let step = self.order.output_step().map_err(|_| self.fail_force(now))?;
        if step == WriteStep::Complete
            && let Some(event) = self.order.take_ready()
        {
            self.phase_ready(now)?;
            self.released_ready = Some(event);
        }
        Ok(step)
    }

    pub(crate) fn ready(
        &mut self,
        now: Instant,
        input: &[u8],
    ) -> Result<Option<Event>, ActorLaunchOrderError> {
        self.check()?;
        self.advance(now);
        if self.stopping() {
            return Err(self.fail_force(now));
        }
        let event = self.order.ready(input).map_err(|_| self.fail_force(now))?;
        let Some(event) = event else { return Ok(None) };
        self.phase_ready(now)?;
        Ok(Some(event))
    }

    fn phase_ready(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        if let Ok(Phase::Running) = self.phase.ready(now, self.limits) {
            self.phase = Phase::Running;
            Ok(())
        } else {
            Err(self.fail_force(now))
        }
    }

    pub(crate) fn take_ready(&mut self) -> Option<Event> {
        if self.terminal.is_none() && self.phase == Phase::Running {
            self.released_ready.take()
        } else {
            None
        }
    }

    pub(crate) fn ready_eof(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        self.check()?;
        self.advance(now);
        if self.stopping() {
            return Err(self.fail_force(now));
        }
        self.order.ready_eof().map_err(|_| self.fail_force(now))
    }

    pub(crate) fn stop(&mut self, force: bool, now: Instant) -> Result<(), ActorLaunchOrderError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        self.advance(now);
        let force = force
            || matches!(
                self.phase,
                Phase::ForceStopping(_) | Phase::Draining(_) | Phase::Unconfirmed
            );
        self.phase = self
            .phase
            .stop(force, now, self.limits)
            .map_err(|_| self.latch(ActorLaunchOrderError::InvalidTransition))?;
        match self.order.launch.stop(force) {
            Ok(()) => {
                self.force_ok |= force;
                self.order.held_ready = None;
                self.released_ready = None;
                Ok(())
            }
            Err(error) => {
                if !force {
                    self.phase = Phase::ForceStopping(now + self.limits.force);
                    let _ = self.try_force();
                }
                let error = ActorLaunchOrderError::Stop(error.kind());
                Err(self.latch(error))
            }
        }
    }

    pub(crate) fn cleanup_tick(
        &mut self,
        now: Instant,
    ) -> Result<TreeObservation, ActorLaunchOrderError> {
        self.advance(now);
        if matches!(
            self.phase,
            Phase::ForceStopping(_) | Phase::Draining(_) | Phase::Unconfirmed
        ) {
            self.try_force()?;
        }
        self.order
            .launch
            .cleanup(&mut self.phase)
            .map_err(|error| match error {
                CleanupStepError::Phase(_) => ActorLaunchOrderError::InvalidTransition,
                CleanupStepError::Reap(error) => ActorLaunchOrderError::Reap(error.kind()),
            })
    }

    fn check(&mut self) -> Result<(), ActorLaunchOrderError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if self.order.stage == LaunchOrderStage::CleanupRequired || self.stopping() {
            return Err(self.latch(ActorLaunchOrderError::CleanupRequired));
        }
        Ok(())
    }

    fn stopping(&self) -> bool {
        matches!(
            self.phase,
            Phase::GracefulStopping(_)
                | Phase::ForceStopping(_)
                | Phase::Draining(_)
                | Phase::Unconfirmed
                | Phase::Empty
        )
    }

    fn advance(&mut self, now: Instant) {
        self.phase = self.phase.advance(now, self.limits);
    }

    fn fail_force(&mut self, now: Instant) -> ActorLaunchOrderError {
        self.phase = Phase::ForceStopping(now + self.limits.force);
        let _ = self.try_force();
        self.latch(ActorLaunchOrderError::CleanupRequired)
    }

    fn latch(&mut self, error: ActorLaunchOrderError) -> ActorLaunchOrderError {
        self.terminal = Some(error);
        self.order.stage = LaunchOrderStage::CleanupRequired;
        self.order.held_ready = None;
        self.released_ready = None;
        error
    }

    fn try_force(&mut self) -> Result<(), ActorLaunchOrderError> {
        if self.force_ok {
            return Ok(());
        }
        match self.order.launch.stop(true) {
            Ok(()) => {
                self.force_ok = true;
                Ok(())
            }
            Err(error) => Err(ActorLaunchOrderError::Stop(error.kind())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use agent24_sidecar_host_protocol::{Request, decode_reply};
    use std::collections::{BTreeMap, VecDeque};

    struct FakeLaunch(u64);

    impl LaunchIdentity for FakeLaunch {
        fn request_id(&self) -> u64 {
            self.0
        }
    }

    struct FakeSink {
        frame: Option<Vec<u8>>,
        steps: Vec<Result<WriteStep, OutputWriteError>>,
    }

    impl FrameSink for FakeSink {
        fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError> {
            self.frame = Some(frame);
            Ok(())
        }

        fn step(&mut self) -> Result<WriteStep, OutputWriteError> {
            self.steps.remove(0)
        }
    }

    fn order(steps: Vec<Result<WriteStep, OutputWriteError>>) -> LaunchOrder<FakeLaunch, FakeSink> {
        LaunchOrder::new(FakeLaunch(7), FakeSink { frame: None, steps })
    }

    fn ready() -> &'static [u8] {
        br#"{"type":"ready","protocol":1,"port":1,"token":"tttttttttttttttttttttttttttttttt","version":"v"}
"#
    }

    #[test]
    fn owned_and_ready_are_ordered_and_released_once() {
        let mut owned = order(vec![Ok(WriteStep::Pending), Ok(WriteStep::Complete)]);
        assert!(owned.sink.frame.is_none());
        owned.queue_owned().unwrap();
        assert!(matches!(
            decode_reply(owned.sink.frame.as_ref().unwrap()).unwrap(),
            Reply::Owned { request_id: 7, .. }
        ));
        assert_eq!(owned.ready(ready()), Ok(None));
        assert_eq!(owned.output_step(), Ok(WriteStep::Pending));
        assert_eq!(owned.output_step(), Ok(WriteStep::Complete));
        assert!(owned.take_ready().is_some());
        assert!(owned.take_ready().is_none());

        let mut late = order(vec![Ok(WriteStep::Complete)]);
        late.queue_owned().unwrap();
        late.output_step().unwrap();
        assert!(late.ready(ready()).unwrap().is_some());
    }

    #[test]
    fn malformed_order_and_ready_streams_latch_cleanup() {
        let mut before_queue = order(vec![]);
        assert_eq!(
            before_queue.output_step(),
            Err(LaunchOrderStage::CleanupRequired)
        );
        assert_eq!(
            before_queue.ready(&[]),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut pre_ready_eof = order(vec![Ok(WriteStep::Complete)]);
        pre_ready_eof.queue_owned().unwrap();
        assert_eq!(
            pre_ready_eof.ready_eof(),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut post_ready_eof = order(vec![Ok(WriteStep::Complete)]);
        post_ready_eof.queue_owned().unwrap();
        post_ready_eof.output_step().unwrap();
        assert!(post_ready_eof.ready(ready()).unwrap().is_some());
        assert_eq!(post_ready_eof.ready_eof(), Ok(()));

        let mut trailing = order(vec![Ok(WriteStep::Complete)]);
        trailing.queue_owned().unwrap();
        trailing.output_step().unwrap();
        trailing.ready(ready()).unwrap();
        let mut bytes = ready().to_vec();
        bytes.extend_from_slice(ready());
        assert_eq!(
            trailing.ready(&bytes),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut write_error = order(vec![Err(OutputWriteError::Closed)]);
        write_error.queue_owned().unwrap();
        assert_eq!(
            write_error.output_step(),
            Err(LaunchOrderStage::CleanupRequired)
        );
        assert_eq!(
            write_error.ready_eof(),
            Err(LaunchOrderStage::CleanupRequired)
        );
    }

    struct ScriptLaunch {
        id: u64,
        stops: VecDeque<io::Result<()>>,
        forces: Vec<bool>,
        reaps: VecDeque<io::Result<TreeObservation>>,
    }

    impl LaunchIdentity for ScriptLaunch {
        fn request_id(&self) -> u64 {
            self.id
        }
    }

    impl LaunchControl for ScriptLaunch {
        fn stop(&mut self, force: bool) -> io::Result<()> {
            self.forces.push(force);
            self.stops
                .pop_front()
                .unwrap_or_else(|| panic!("scripted stop"))
        }

        fn cleanup(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError> {
            let observation = self
                .reaps
                .pop_front()
                .unwrap_or(Ok(TreeObservation::ConfirmedEmpty))
                .map_err(CleanupStepError::Reap)?;
            if observation == TreeObservation::ConfirmedEmpty {
                *phase = Phase::Empty;
            }
            Ok(observation)
        }
    }

    const LIMITS: Deadlines = Deadlines {
        launch: std::time::Duration::from_secs(2),
        ready: std::time::Duration::from_secs(4),
        graceful: std::time::Duration::from_secs(1),
        force: std::time::Duration::from_secs(1),
        drain: std::time::Duration::from_secs(1),
    };

    fn actor(
        stops: impl IntoIterator<Item = io::Result<()>>,
        reaps: impl IntoIterator<Item = io::Result<TreeObservation>>,
        steps: Vec<Result<WriteStep, OutputWriteError>>,
    ) -> ActorLaunchOrder<ScriptLaunch, FakeSink> {
        ActorLaunchOrder::new(
            ScriptLaunch {
                id: 7,
                stops: stops.into_iter().collect(),
                forces: Vec::new(),
                reaps: reaps.into_iter().collect(),
            },
            FakeSink { frame: None, steps },
            Phase::Launching(Instant::now() + LIMITS.launch),
            LIMITS,
        )
    }

    fn cleanup_required<T>(result: Result<T, ActorLaunchOrderError>) {
        assert!(matches!(
            result,
            Err(ActorLaunchOrderError::CleanupRequired)
        ));
    }

    fn io_error<T>(kind: io::ErrorKind) -> io::Result<T> {
        Err(io::Error::from(kind))
    }

    fn queue_ready(actor: &mut ActorLaunchOrder<ScriptLaunch, FakeSink>, now: Instant) {
        actor.queue_owned(now).unwrap();
        assert_eq!(actor.ready(now, ready()), Ok(None));
    }

    #[test]
    fn actor_waits_for_owned_and_latches_first_failure() {
        let start = Instant::now();
        let mut pending = actor(
            [],
            [],
            vec![Ok(WriteStep::Pending), Ok(WriteStep::Complete)],
        );
        queue_ready(&mut pending, start);
        assert_eq!(pending.output_step(start), Ok(WriteStep::Pending));
        assert_eq!(
            pending.output_step(start + LIMITS.launch + std::time::Duration::from_nanos(1)),
            Ok(WriteStep::Complete)
        );
        assert!(pending.take_ready().is_some());
        assert_eq!(pending.phase, Phase::Running);

        let mut deadline = actor([Ok(())], [], vec![Ok(WriteStep::Complete)]);
        queue_ready(&mut deadline, start);
        cleanup_required(deadline.output_step(start + LIMITS.ready));
        cleanup_required(deadline.ready_eof(start + LIMITS.ready));

        let mut failed = actor([Ok(())], [], vec![Err(OutputWriteError::Closed)]);
        queue_ready(&mut failed, start);
        cleanup_required(failed.output_step(start));
        failed
            .order
            .launch
            .stops
            .push_back(io_error(io::ErrorKind::BrokenPipe));
        cleanup_required(failed.ready_eof(start));
    }

    #[test]
    fn actor_cleanup_retries_force_and_reap() {
        let mut actor = actor([], [], vec![]);
        actor
            .order
            .launch
            .stops
            .extend([io_error(io::ErrorKind::BrokenPipe), Ok(()), Ok(())]);
        actor.order.launch.reaps.extend([
            Ok(TreeObservation::Present),
            io_error(io::ErrorKind::Interrupted),
            Ok(TreeObservation::ConfirmedEmpty),
        ]);
        let now = Instant::now();
        actor.phase = Phase::ForceStopping(now + LIMITS.force + LIMITS.drain);
        assert_eq!(
            actor.stop(true, now),
            Err(ActorLaunchOrderError::Stop(io::ErrorKind::BrokenPipe))
        );
        assert_eq!(
            actor.phase,
            Phase::ForceStopping(now + LIMITS.force + LIMITS.drain)
        );
        assert_eq!(actor.cleanup_tick(now), Ok(TreeObservation::Present));
        assert_eq!(
            actor.cleanup_tick(now),
            Err(ActorLaunchOrderError::Reap(io::ErrorKind::Interrupted))
        );
        assert_eq!(actor.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
        assert_eq!(actor.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
    }

    #[test]
    fn actor_stop_is_idempotent_and_deadlines_force() {
        let now = Instant::now();
        let mut running = actor(
            [Ok(()), Ok(()), Ok(())],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Ok(WriteStep::Complete)],
        );
        queue_ready(&mut running, now);
        running.output_step(now).unwrap();
        assert!(running.released_ready.is_some());
        running.stop(false, now).unwrap();
        running.stop(false, now).unwrap();
        running.stop(true, now).unwrap();
        assert_eq!(running.order.launch.forces, vec![false, false, true]);
        running.cleanup_tick(now).unwrap();
        assert!(running.take_ready().is_none());

        let mut held = actor([Ok(())], [], vec![]);
        queue_ready(&mut held, now);
        held.stop(false, now).unwrap();
        assert!(held.order.held_ready.is_none());
        assert!(held.take_ready().is_none());

        let mut exact = actor([Ok(())], [], vec![]);
        exact.queue_owned(now).unwrap();
        exact.stop(false, now + LIMITS.ready).unwrap();
        assert_eq!(exact.order.launch.forces, vec![true]);

        let mut graceful = actor([Ok(()), Ok(())], [], vec![]);
        graceful.queue_owned(now).unwrap();
        graceful.stop(false, now).unwrap();
        graceful.stop(false, now + LIMITS.graceful).unwrap();
        assert_eq!(graceful.order.launch.forces, vec![false, true]);

        let mut draining = actor([io_error(io::ErrorKind::BrokenPipe)], [], vec![]);
        draining.phase = Phase::Draining(now + LIMITS.drain);
        assert_eq!(
            draining.stop(true, now),
            Err(ActorLaunchOrderError::Stop(io::ErrorKind::BrokenPipe))
        );
        assert_eq!(draining.phase, Phase::Draining(now + LIMITS.drain));

        for at in [
            now + LIMITS.launch,
            now + LIMITS.launch + std::time::Duration::from_nanos(1),
        ] {
            let mut launch = actor([Ok(())], [], vec![]);
            launch.phase = Phase::Launching(now + LIMITS.launch);
            cleanup_required(launch.queue_owned(at));
            assert!(launch.order.sink.frame.is_none());
        }

        let mut force_first = actor([Ok(())], [], vec![]);
        force_first.stop(true, now).unwrap();
        assert_eq!(force_first.order.launch.forces, vec![true]);
        assert!(matches!(force_first.phase, Phase::ForceStopping(_)));
    }

    fn force_and_reap(launch: &mut OwnedLaunch) -> io::Result<()> {
        use std::time::Duration;

        let force = LaunchControl::stop(launch, true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let reap = loop {
            match launch.target_mut().reap_step() {
                Ok(TreeObservation::ConfirmedEmpty) => break Ok(()),
                Ok(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Ok(_) => {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "reap deadline expired",
                    ));
                }
                Err(error) => break Err(error),
            }
        };
        force?;
        reap
    }

    #[cfg(unix)]
    #[test]
    fn owned_soft_stop_closes_stdin_twice_then_force_reaps_same_owner() {
        use std::{io::Read, os::fd::AsFd, time::Duration};

        let _test_guard = crate::posix::tests::test_lock();
        let request = Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: 71,
            executable: "/bin/sh".into(),
            cwd: "/".into(),
            argv: vec!["-c".into(), "trap '' TERM; printf 'ready!'; IFS= read -r line || :; printf 'out-eof!'; printf 'err-eof!' >&2; exec sleep 30".into()],
            env: BTreeMap::new(),
        };
        let mut launch =
            OwnedLaunch::start(crate::launch::LaunchIntent::from_request(request).unwrap())
                .unwrap();
        fn bounded_read<const N: usize>(pipe: &impl AsFd) -> io::Result<[u8; N]> {
            let mut reader = std::fs::File::from(pipe.as_fd().try_clone_to_owned()?);
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut bytes = [0; N];
                let result = reader.read_exact(&mut bytes).map(|()| bytes);
                let _ = send.send(result);
            });
            receive.recv_timeout(Duration::from_secs(3)).map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "child marker deadline expired")
            })?
        }
        let ready = bounded_read::<6>(launch.parts_mut().1.stdout_mut());
        let first_stop = LaunchControl::stop(&mut launch, false);
        let second_stop = LaunchControl::stop(&mut launch, false);
        let stdin_closed = launch.parts_mut().1.stdin_mut().is_none();
        let stdout = bounded_read::<8>(launch.parts_mut().1.stdout_mut());
        let stderr = bounded_read::<8>(launch.parts_mut().1.stderr_mut());
        let cleanup = force_and_reap(&mut launch);
        drop(launch);
        crate::posix::tests::wait_for_reaper_idle();
        cleanup.unwrap();
        first_stop.unwrap();
        second_stop.unwrap();
        assert_eq!(&ready.unwrap(), b"ready!");
        assert!(stdin_closed, "soft stop left stdin open");
        assert_eq!(&stdout.unwrap(), b"out-eof!");
        assert_eq!(&stderr.unwrap(), b"err-eof!");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn owned_soft_stop_closes_stdin_twice_then_force_reaps_same_owner() {
        use std::time::Duration;
        use tokio::io::AsyncReadExt;

        let request = Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: 72,
            executable: "powershell.exe".into(),
            cwd: std::env::temp_dir().display().to_string(),
            argv: vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "$null = [Console]::In.ReadToEnd(); [Console]::Out.Write('out-eof!'); [Console]::Out.Flush(); [Console]::Error.Write('err-eof!'); [Console]::Error.Flush(); Start-Sleep -Seconds 30".into()],
            env: BTreeMap::from([(String::from("SystemRoot"), std::env::var("SystemRoot").unwrap())]),
        };
        let mut launch =
            OwnedLaunch::start(crate::launch::LaunchIntent::from_request(request).unwrap())
                .unwrap();
        let first_stop = LaunchControl::stop(&mut launch, false);
        let second_stop = LaunchControl::stop(&mut launch, false);
        let stdin_closed = launch.parts_mut().1.stdin_mut().is_none();
        let mut out = [0; 8];
        let stdout = tokio::time::timeout(
            Duration::from_secs(3),
            launch.parts_mut().1.stdout_mut().read_exact(&mut out),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "child stdout deadline expired"))
        .and_then(|result| result.map(|_| out));
        let mut err = [0; 8];
        let stderr = tokio::time::timeout(
            Duration::from_secs(3),
            launch.parts_mut().1.stderr_mut().read_exact(&mut err),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "child stderr deadline expired"))
        .and_then(|result| result.map(|_| err));
        let cleanup = force_and_reap(&mut launch);
        cleanup.unwrap();
        first_stop.unwrap();
        second_stop.unwrap();
        assert!(stdin_closed, "soft stop left stdin open");
        assert_eq!(&stdout.unwrap(), b"out-eof!");
        assert_eq!(&stderr.unwrap(), b"err-eof!");
    }
}
