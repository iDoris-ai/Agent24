//! Deterministic, non-blocking turn driver for one owned generation.
//! This is dormant: it owns no stdio handles and is not wired from `run`.

use agent24_sidecar_host_protocol::Request;
use std::time::Instant;

use crate::{
    control_io::IngressStep,
    control_worker::{ControlPermitError, ControlStep, ControlWorker, ControlWorkerError},
    launch_order::{
        ActorLaunchOrder, ActorLaunchOrderError, FrameSink, LaunchControl, LaunchIdentity,
        ScheduleState,
    },
    output_worker::OutputWorker,
    ready_read_worker::{
        ReadyRead, ReadyReadError, ReadyReadPermitError, ReadyReadStep, ReadyReadWorker,
    },
};

/// Only the detached worker is admitted: never synchronous `OutputWriter`.
trait GenerationSink: FrameSink {}
impl GenerationSink for OutputWorker {}

/// Private channel-only adapter for the host-lifetime control worker.
trait ControlPort {
    fn step(&mut self, now: Instant) -> Result<ControlStep, ControlWorkerError>;
    fn permit(&mut self, now: Instant) -> Result<(), ControlPermitError>;
}
impl ControlPort for ControlWorker {
    fn step(&mut self, now: Instant) -> Result<ControlStep, ControlWorkerError> {
        Self::step(self, now)
    }
    fn permit(&mut self, now: Instant) -> Result<(), ControlPermitError> {
        Self::permit(self, now)
    }
}

/// Private channel-only adapter for the generation's one stdout reader.
trait ReadyPort {
    fn step(&mut self) -> Result<ReadyReadStep, ReadyReadError>;
    fn permit(&mut self) -> Result<(), ReadyReadPermitError>;
}
impl ReadyPort for ReadyReadWorker {
    fn step(&mut self) -> Result<ReadyReadStep, ReadyReadError> {
        Self::step(self)
    }
    fn permit(&mut self) -> Result<(), ReadyReadPermitError> {
        Self::permit(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureState {
    None,
    DeferredTransport,
    LatchedTransport,
}

/// One actor, one control port, and exactly one completed request slot.
pub(crate) struct GenerationDriver<L, S, C, R> {
    actor: ActorLaunchOrder<L, S>,
    control: C,
    ready: R,
    completed: Option<Request>,
    control_eof: bool,
    ready_eof: bool,
    failure: FailureState,
}

#[allow(private_bounds)]
impl<L, S, C, R> GenerationDriver<L, S, C, R>
where
    L: LaunchIdentity + LaunchControl,
    S: GenerationSink,
    C: ControlPort,
    R: ReadyPort,
{
    /// Admit `Owned` before the first turn while retaining all owners for cleanup.
    pub(crate) fn new(
        mut actor: ActorLaunchOrder<L, S>,
        control: C,
        ready: R,
        now: Instant,
    ) -> Self {
        actor.begin_turn();
        let failure = if actor.queue_owned(now).is_ok() {
            FailureState::None
        } else {
            // `queue_owned` forced containment; retain owners for retry/reap.
            FailureState::LatchedTransport
        };
        Self {
            actor,
            control,
            ready,
            completed: None,
            control_eof: false,
            ready_eof: false,
            failure,
        }
    }

    pub(crate) fn schedule_state(&self) -> ScheduleState {
        self.actor.schedule_state()
    }

    /// One turn: control poll/EOF; one lifecycle path; a Ready poll only for
    /// an active generation; output barrier; at most one dispatch; then at
    /// most one Ready credit followed by one control credit.
    pub(crate) fn step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        self.actor.begin_turn();
        if self.failure == FailureState::DeferredTransport {
            self.latch_transport(now);
            return Err(ActorLaunchOrderError::CleanupRequired);
        }
        if self.failure == FailureState::LatchedTransport || self.schedule_state().terminal {
            return self.actor.cleanup_tick(now).map(|_| ());
        }

        let mut eof_shutdown = false;
        if !self.control_eof {
            match self.control.step(now) {
                Ok(ControlStep::Idle | ControlStep::Pending)
                | Ok(ControlStep::Complete(IngressStep::Pending)) => {}
                Ok(ControlStep::Complete(IngressStep::Eof)) => {
                    self.control_eof = true;
                    eof_shutdown = true;
                }
                Ok(ControlStep::Complete(IngressStep::Request(request))) => {
                    if self.completed.is_some() {
                        self.control_eof = true;
                        self.latch_transport(now);
                        return Err(ActorLaunchOrderError::CleanupRequired);
                    }
                    self.completed = Some(request);
                }
                Err(_) => {
                    self.control_eof = true;
                    self.latch_transport(now);
                    if self.failure == FailureState::LatchedTransport {
                        return Err(ActorLaunchOrderError::CleanupRequired);
                    }
                }
            }
        }

        let maintenance = if eof_shutdown
            && matches!(
                self.schedule_state().phase,
                crate::actor::Phase::AwaitLaunch
                    | crate::actor::Phase::Launching(_)
                    | crate::actor::Phase::AwaitReady(_)
                    | crate::actor::Phase::Running
            ) {
            self.actor.stop(false, now)
        } else {
            self.actor.maintenance(now)
        };
        let state = self.schedule_state();
        if state.terminal {
            return maintenance.map(|_| ());
        }
        let exit_barrier = state.exit_retained;
        if !exit_barrier
            && !self.ready_eof
            && matches!(
                state.phase,
                crate::actor::Phase::AwaitReady(_) | crate::actor::Phase::Running
            )
        {
            match self.ready.step() {
                Ok(ReadyReadStep::Idle | ReadyReadStep::Pending)
                | Ok(ReadyReadStep::Complete(ReadyRead::Pending)) => {}
                Ok(ReadyReadStep::Complete(ReadyRead::Chunk { bytes, len })) => {
                    self.actor.ready(now, &bytes[..len])?;
                }
                Ok(ReadyReadStep::Complete(ReadyRead::Eof)) => {
                    self.ready_eof = true;
                    self.actor.ready_eof(now)?;
                }
                Err(_) => {
                    self.ready_eof = true;
                    self.latch_transport(now);
                    return Err(ActorLaunchOrderError::CleanupRequired);
                }
            }
        }
        let state = self.schedule_state();
        if state.terminal {
            return Ok(());
        }
        if state.output_pending {
            self.actor.output_step(now)?;
            maintenance?;
            if exit_barrier || self.schedule_state().exit_retained {
                return Ok(());
            }
            return self.permit_last(now);
        }
        if state.exit_retained {
            return maintenance;
        }
        maintenance?;
        self.actor.dispatch_completed(&mut self.completed, now)?;
        if self.schedule_state().terminal {
            return Ok(());
        }
        self.permit_last(now)
    }

    fn permit_last(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        let state = self.schedule_state();
        if !self.ready_eof
            && !state.terminal
            && !state.exit_retained
            && matches!(
                state.phase,
                crate::actor::Phase::AwaitReady(_) | crate::actor::Phase::Running
            )
        {
            match self.ready.permit() {
                Ok(()) | Err(ReadyReadPermitError::Busy) => {}
                Err(ReadyReadPermitError::Closed) => {
                    self.ready_eof = true;
                    self.failure = FailureState::DeferredTransport;
                    return Ok(());
                }
            }
        }
        if self.control_eof || self.completed.is_some() || !self.actor.control_permit_allowed() {
            return Ok(());
        }
        match self.control.permit(now) {
            Ok(()) | Err(ControlPermitError::Busy) => Ok(()),
            Err(ControlPermitError::Closed) => {
                self.control_eof = true;
                if !matches!(self.schedule_state().phase, crate::actor::Phase::Empty) {
                    self.failure = FailureState::DeferredTransport;
                }
                Ok(())
            }
        }
    }

    fn latch_transport(&mut self, now: Instant) {
        if self.failure == FailureState::LatchedTransport
            || matches!(self.schedule_state().phase, crate::actor::Phase::Empty)
        {
            return;
        }
        self.failure = FailureState::LatchedTransport;
        let _ = self.actor.fail_transport(now);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        actor::{Deadlines, Phase},
        cleanup::CleanupStepError,
        control_io::IngressError,
        output_io::{OutputWriteError, PutFrameError, WriteStep},
        target::{ExitObservation, TreeObservation},
    };
    use agent24_sidecar_host_protocol::{
        Event, PROTOCOL_VERSION, Reply, decode_event, decode_reply,
    };
    use std::{
        collections::VecDeque,
        io,
        sync::{Arc, Mutex},
        time::Duration,
    };

    const L: Deadlines = Deadlines {
        launch: Duration::from_secs(2),
        ready: Duration::from_secs(3),
        graceful: Duration::from_secs(1),
        force: Duration::from_secs(1),
        drain: Duration::from_secs(1),
    };
    #[derive(Default)]
    struct R {
        stops: Vec<bool>,
        stop_errors: VecDeque<io::ErrorKind>,
        cleanups: usize,
        reap_errors: VecDeque<io::ErrorKind>,
        trees: VecDeque<TreeObservation>,
        polls: usize,
        permits: usize,
        ready_polls: usize,
        ready_permits: usize,
        reject_puts: usize,
        frames: Vec<Vec<u8>>,
    }
    struct Launch(Arc<Mutex<R>>, VecDeque<io::Result<ExitObservation>>);
    impl LaunchIdentity for Launch {
        fn request_id(&self) -> u64 {
            7
        }
    }
    impl LaunchControl for Launch {
        fn stop(&mut self, force: bool) -> io::Result<()> {
            let mut r = self.0.lock().unwrap();
            r.stops.push(force);
            r.stop_errors
                .pop_front()
                .map_or(Ok(()), |k| Err(io::Error::from(k)))
        }
        fn observe_exit(&mut self) -> io::Result<ExitObservation> {
            self.1.pop_front().unwrap_or(Ok(ExitObservation::Running))
        }
        fn cleanup(&mut self, p: &mut Phase) -> Result<TreeObservation, CleanupStepError> {
            let mut r = self.0.lock().unwrap();
            r.cleanups += 1;
            if let Some(k) = r.reap_errors.pop_front() {
                return Err(CleanupStepError::Reap(io::Error::from(k)));
            }
            let tree = r
                .trees
                .pop_front()
                .unwrap_or(TreeObservation::ConfirmedEmpty);
            drop(r);
            if tree == TreeObservation::ConfirmedEmpty {
                *p = Phase::Empty;
            }
            Ok(tree)
        }
    }
    struct Sink(Arc<Mutex<R>>, VecDeque<Result<WriteStep, OutputWriteError>>);
    impl FrameSink for Sink {
        fn put(&mut self, f: Vec<u8>, _: Instant) -> Result<(), PutFrameError> {
            let mut r = self.0.lock().unwrap();
            if r.reject_puts > 0 {
                r.reject_puts -= 1;
                return Err(PutFrameError::Closed(f));
            }
            r.frames.push(f);
            Ok(())
        }
        fn step(&mut self, _: Instant) -> Result<WriteStep, OutputWriteError> {
            self.1.pop_front().unwrap_or(Ok(WriteStep::Complete))
        }
    }
    impl GenerationSink for Sink {}
    struct Control {
        r: Arc<Mutex<R>>,
        s: VecDeque<Result<ControlStep, ControlWorkerError>>,
        p: VecDeque<Result<(), ControlPermitError>>,
    }
    impl ControlPort for Control {
        fn step(&mut self, _: Instant) -> Result<ControlStep, ControlWorkerError> {
            self.r.lock().unwrap().polls += 1;
            self.s.pop_front().unwrap_or(Ok(ControlStep::Idle))
        }
        fn permit(&mut self, _: Instant) -> Result<(), ControlPermitError> {
            self.r.lock().unwrap().permits += 1;
            self.p.pop_front().unwrap_or(Ok(()))
        }
    }
    struct Ready {
        r: Arc<Mutex<R>>,
        s: VecDeque<Result<ReadyReadStep, ReadyReadError>>,
        p: VecDeque<Result<(), ReadyReadPermitError>>,
    }
    impl ReadyPort for Ready {
        fn step(&mut self) -> Result<ReadyReadStep, ReadyReadError> {
            self.r.lock().unwrap().ready_polls += 1;
            self.s.pop_front().unwrap_or(Ok(ReadyReadStep::Idle))
        }
        fn permit(&mut self) -> Result<(), ReadyReadPermitError> {
            self.r.lock().unwrap().ready_permits += 1;
            self.p.pop_front().unwrap_or(Ok(()))
        }
    }
    fn d(
        r: Arc<Mutex<R>>,
        s: impl IntoIterator<Item = Result<ControlStep, ControlWorkerError>>,
        p: impl IntoIterator<Item = Result<(), ControlPermitError>>,
        o: impl IntoIterator<Item = Result<WriteStep, OutputWriteError>>,
        e: impl IntoIterator<Item = io::Result<ExitObservation>>,
    ) -> GenerationDriver<Launch, Sink, Control, Ready> {
        d_at(r, s, p, o, e, Phase::Launching(Instant::now() + L.launch))
    }
    fn d_at(
        r: Arc<Mutex<R>>,
        s: impl IntoIterator<Item = Result<ControlStep, ControlWorkerError>>,
        p: impl IntoIterator<Item = Result<(), ControlPermitError>>,
        o: impl IntoIterator<Item = Result<WriteStep, OutputWriteError>>,
        e: impl IntoIterator<Item = io::Result<ExitObservation>>,
        phase: Phase,
    ) -> GenerationDriver<Launch, Sink, Control, Ready> {
        let actor = ActorLaunchOrder::new(
            Launch(r.clone(), e.into_iter().collect()),
            Sink(r.clone(), o.into_iter().collect()),
            phase,
            L,
        );
        let control = Control {
            r: r.clone(),
            s: s.into_iter().collect(),
            p: p.into_iter().collect(),
        };
        let ready = Ready {
            r,
            s: VecDeque::new(),
            p: VecDeque::new(),
        };
        if matches!(phase, Phase::Launching(_)) {
            GenerationDriver::new(actor, control, ready, Instant::now())
        } else {
            GenerationDriver {
                actor,
                control,
                ready,
                completed: None,
                control_eof: false,
                ready_eof: false,
                failure: FailureState::None,
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn d_ready(
        r: Arc<Mutex<R>>,
        control_steps: impl IntoIterator<Item = Result<ControlStep, ControlWorkerError>>,
        control_permits: impl IntoIterator<Item = Result<(), ControlPermitError>>,
        ready_steps: impl IntoIterator<Item = Result<ReadyReadStep, ReadyReadError>>,
        ready_permits: impl IntoIterator<Item = Result<(), ReadyReadPermitError>>,
        output: impl IntoIterator<Item = Result<WriteStep, OutputWriteError>>,
        exits: impl IntoIterator<Item = io::Result<ExitObservation>>,
        now: Instant,
    ) -> GenerationDriver<Launch, Sink, Control, Ready> {
        GenerationDriver::new(
            ActorLaunchOrder::new(
                Launch(r.clone(), exits.into_iter().collect()),
                Sink(r.clone(), output.into_iter().collect()),
                Phase::Launching(now + L.launch),
                L,
            ),
            Control {
                r: r.clone(),
                s: control_steps.into_iter().collect(),
                p: control_permits.into_iter().collect(),
            },
            Ready {
                r,
                s: ready_steps.into_iter().collect(),
                p: ready_permits.into_iter().collect(),
            },
            now,
        )
    }
    fn sig(id: u64) -> Request {
        Request::Signal {
            version: PROTOCOL_VERSION,
            request_id: id,
            force: false,
        }
    }
    fn ready() -> &'static [u8] {
        br#"{"type":"ready","protocol":1,"port":1,"token":"tttttttttttttttttttttttttttttttt","version":"v"}
"#
    }
    fn chunk(input: &[u8]) -> [u8; crate::ready_read_worker::READ_CHUNK_BYTES] {
        let mut bytes = [0; crate::ready_read_worker::READ_CHUNK_BYTES];
        bytes[..input.len()].copy_from_slice(input);
        bytes
    }
    fn prime(x: &mut GenerationDriver<Launch, Sink, Control, Ready>, n: Instant) {
        x.actor.output_step(n).unwrap();
    }
    fn empty(
        x: &mut GenerationDriver<Launch, Sink, Control, Ready>,
        r: &Arc<Mutex<R>>,
        n: Instant,
    ) {
        x.actor.stop(true, n).unwrap();
        x.actor.cleanup_tick(n).unwrap();
        r.lock().unwrap().stops.clear()
    }
    fn reply(x: &mut GenerationDriver<Launch, Sink, Control, Ready>, id: u64) {
        x.actor
            .queue_reply(&Reply::Result {
                version: 1,
                request_id: id,
            })
            .unwrap()
    }

    #[rustfmt::skip] #[test] fn owned_ready_exit_order(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[],[],[Ok(WriteStep::Complete);3],[Ok(ExitObservation::Running),Ok(ExitObservation::Exited{code:Some(9)})]);prime(&mut x,n);x.actor.ready(n,ready()).unwrap();for _ in 0..3{x.step(n).unwrap()}let f=&r.lock().unwrap().frames;assert!(matches!(decode_reply(&f[0]),Ok(Reply::Owned{..})));assert!(matches!(decode_event(&f[1]),Ok(Event::Ready{..})));assert!(matches!(decode_event(&f[2]),Ok(Event::Exit{code:Some(9),..})));}
    #[rustfmt::skip] #[test] fn output_barrier_retains_request_and_one_credit(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Ok(ControlStep::Complete(IngressStep::Request(sig(11))))],[],[Ok(WriteStep::Complete),Ok(WriteStep::Pending),Ok(WriteStep::Complete)],[]);prime(&mut x,n);reply(&mut x,10);x.step(n).unwrap();assert!(x.completed.is_some());for _ in 0..3{x.step(n).unwrap()}assert!(x.completed.is_none());x.step(n).unwrap();let f=&r.lock().unwrap().frames;assert!(matches!(decode_reply(&f[1]),Ok(Reply::Result{request_id:10,..})));assert!(matches!(decode_reply(&f[2]),Ok(Reply::Result{request_id:11,..})));let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[Ok(ControlStep::Idle)],[Ok(())],[Ok(WriteStep::Complete)],[]);prime(&mut x,n);x.step(n).unwrap();let s=r.lock().unwrap();assert_eq!((s.polls,s.permits),(1,1));}
    #[test]
    fn deadlines_and_force_are_once_per_turn() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let mut x = d_ready(r.clone(), [], [], [], [], [Ok(WriteStep::Pending)], [], now);
        x.step(now + L.ready).unwrap();
        assert!(matches!(x.schedule_state().phase, Phase::ForceStopping(_)));
        assert_eq!(r.lock().unwrap().stops, vec![true]);

        let r = Arc::new(Mutex::new(R::default()));
        r.lock()
            .unwrap()
            .stop_errors
            .push_back(io::ErrorKind::WouldBlock);
        let mut x = d_ready(
            r.clone(),
            [],
            [],
            [],
            [],
            [Err(OutputWriteError::Closed)],
            [],
            now,
        );
        assert_eq!(
            x.step(now + L.ready),
            Err(ActorLaunchOrderError::CleanupRequired)
        );
        assert_eq!(r.lock().unwrap().stops, vec![true]);
    }
    #[rustfmt::skip] #[test] fn eof_stops_running_and_closed_empty_drains(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Ok(ControlStep::Complete(IngressStep::Eof))],[],[Ok(WriteStep::Complete)],[Ok(ExitObservation::Running)]);prime(&mut x,n);x.actor.ready(n,ready()).unwrap();x.step(n).unwrap();assert_eq!(r.lock().unwrap().stops,vec![false]);let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[Err(ControlWorkerError::Closed)],[],[Ok(WriteStep::Complete)],[]);empty(&mut x,&r,n);reply(&mut x,8);x.step(n).unwrap();x.step(n).unwrap();let s=r.lock().unwrap();assert!(s.stops.is_empty());assert_eq!((s.polls,s.frames.len()),(1,2));}
    #[rustfmt::skip] #[test] fn maintenance_errors_and_transport_cleanup_retry_without_starvation(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Err(ControlWorkerError::Closed)],[],[],[]);assert_eq!(x.step(n),Err(ActorLaunchOrderError::CleanupRequired));r.lock().unwrap().reap_errors.push_back(io::ErrorKind::Interrupted);assert_eq!(x.step(n),Err(ActorLaunchOrderError::Reap(io::ErrorKind::Interrupted)));x.step(n).unwrap();let s=r.lock().unwrap();assert_eq!((s.cleanups,s.stops.clone()),(2,vec![true]));drop(s);let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[],[],[Ok(WriteStep::Complete)],[Err(io::Error::from(io::ErrorKind::Interrupted)),Err(io::Error::from(io::ErrorKind::Interrupted))]);prime(&mut x,n);reply(&mut x,9);for _ in 0..2{assert_eq!(x.step(n),Err(ActorLaunchOrderError::Observe(io::ErrorKind::Interrupted)))}assert_eq!(r.lock().unwrap().frames.len(),2);}
    #[rustfmt::skip] #[test] fn closed_permit_and_empty_output_failure_do_not_force_empty(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Ok(ControlStep::Idle)],[Err(ControlPermitError::Closed)],[],[]);x.step(n).unwrap();assert_eq!(x.step(n),Err(ActorLaunchOrderError::CleanupRequired));assert_eq!(r.lock().unwrap().stops,vec![true]);let r=Arc::new(Mutex::new(R::default()));let mut x=d_at(r.clone(),[],[],[Err(OutputWriteError::Closed)],[],Phase::Empty);reply(&mut x,10);x.step(n).unwrap();assert_eq!(x.step(n),Err(ActorLaunchOrderError::CleanupRequired));assert_eq!(x.schedule_state().phase,Phase::Empty);assert!(r.lock().unwrap().stops.is_empty());}

    #[test]
    fn eof_unconfirmed_continues_maintenance_without_a_new_stop() {
        let r = Arc::new(Mutex::new(R::default()));
        r.lock().unwrap().trees.extend([
            TreeObservation::Unconfirmed,
            TreeObservation::Unconfirmed,
            TreeObservation::Unconfirmed,
        ]);
        let n = Instant::now();
        let mut x = d(
            r.clone(),
            [Ok(ControlStep::Complete(IngressStep::Eof))],
            [],
            [],
            [],
        );
        x.actor.stop(true, n).unwrap();
        x.actor.cleanup_tick(n + L.force).unwrap();
        x.actor.cleanup_tick(n + L.force + L.drain).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Unconfirmed);
        r.lock().unwrap().stops.clear();
        x.step(n).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Unconfirmed);
        assert!(r.lock().unwrap().stops.is_empty());
    }

    #[test]
    fn deadline_eof_force_would_block_and_output_failure_force_once() {
        let r = Arc::new(Mutex::new(R::default()));
        r.lock()
            .unwrap()
            .stop_errors
            .push_back(io::ErrorKind::WouldBlock);
        let n = Instant::now();
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Complete(IngressStep::Eof))],
            [],
            [],
            [],
            [Err(OutputWriteError::Closed)],
            [],
            n,
        );
        assert_eq!(
            x.step(n + L.ready),
            Err(ActorLaunchOrderError::CleanupRequired)
        );
        assert_eq!(r.lock().unwrap().stops, vec![true]);
    }

    #[test]
    fn eof_matrix_gracefully_stops_active_once_and_never_reopens_ingress() {
        let n = Instant::now();
        for phase in [
            Phase::AwaitLaunch,
            Phase::Launching(n + L.launch),
            Phase::AwaitReady(n + L.ready),
            Phase::Running,
        ] {
            let r = Arc::new(Mutex::new(R::default()));
            let mut x = d_at(
                r.clone(),
                [Ok(ControlStep::Complete(IngressStep::Eof))],
                [],
                [],
                [Ok(ExitObservation::Running), Ok(ExitObservation::Running)],
                phase,
            );

            x.step(n).unwrap();
            assert!(matches!(
                x.schedule_state().phase,
                Phase::GracefulStopping(_)
            ));
            x.step(n).unwrap();

            let r = r.lock().unwrap();
            assert_eq!(r.stops, vec![false]);
            assert_eq!(r.polls, 1);
        }

        for phase in [
            Phase::GracefulStopping(n + L.graceful),
            Phase::ForceStopping(n + L.force),
            Phase::Draining(n + L.drain),
        ] {
            let r = Arc::new(Mutex::new(R::default()));
            r.lock().unwrap().trees.push_back(TreeObservation::Present);
            let mut x = d_at(
                r.clone(),
                [Ok(ControlStep::Complete(IngressStep::Eof))],
                [],
                [],
                [Ok(ExitObservation::Running)],
                phase,
            );

            x.step(n).unwrap();
            assert_eq!(x.schedule_state().phase, phase);
            x.step(n).unwrap();

            let r = r.lock().unwrap();
            assert!(r.stops.iter().all(|force| *force));
            assert_eq!(r.polls, 1);
        }
    }

    #[test]
    fn eof_unconfirmed_becomes_empty_only_on_confirmed_tree_evidence() {
        let r = Arc::new(Mutex::new(R::default()));
        r.lock().unwrap().trees.extend([
            TreeObservation::Unconfirmed,
            TreeObservation::ConfirmedEmpty,
        ]);
        let n = Instant::now();
        let mut x = d_at(
            r.clone(),
            [Ok(ControlStep::Complete(IngressStep::Eof))],
            [],
            [],
            [],
            Phase::Unconfirmed,
        );

        x.step(n).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Unconfirmed);
        x.step(n).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Empty);

        let r = r.lock().unwrap();
        assert!(r.stops.iter().all(|force| *force));
        assert_eq!((r.polls, r.cleanups), (1, 2));
    }

    #[test]
    fn completion_survives_retained_exit_and_exit_precedes_its_reply() {
        let r = Arc::new(Mutex::new(R::default()));
        let n = Instant::now();
        let mut x = d(
            r.clone(),
            [Ok(ControlStep::Complete(IngressStep::Request(
                Request::IsEmpty {
                    version: PROTOCOL_VERSION,
                    request_id: 41,
                },
            )))],
            [],
            [
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
            ],
            [Ok(ExitObservation::Exited { code: Some(17) })],
        );
        prime(&mut x, n);
        x.actor.ready(n, ready()).unwrap();

        x.step(n).unwrap();
        assert!(matches!(
            x.completed,
            Some(Request::IsEmpty { request_id: 41, .. })
        ));
        assert!(x.schedule_state().exit_retained);
        for _ in 0..5 {
            x.step(n).unwrap();
        }
        assert!(x.completed.is_none());

        let r = r.lock().unwrap();
        assert_eq!(r.stops, vec![true]);
        assert!(matches!(
            decode_reply(&r.frames[0]),
            Ok(Reply::Owned { .. })
        ));
        assert!(matches!(
            decode_event(&r.frames[1]),
            Ok(Event::Ready { .. })
        ));
        assert!(matches!(
            decode_event(&r.frames[2]),
            Ok(Event::Exit { code: Some(17), .. })
        ));
        assert!(matches!(
            decode_reply(&r.frames[3]),
            Ok(Reply::Empty {
                request_id: 41,
                empty: true,
                ..
            })
        ));
        assert_eq!(
            r.frames
                .iter()
                .filter(|frame| matches!(decode_event(frame), Ok(Event::Exit { .. })))
                .count(),
            1
        );
    }

    #[test]
    fn second_completion_fails_closed_without_replacing_the_first_slot() {
        let r = Arc::new(Mutex::new(R::default()));
        let n = Instant::now();
        let a = sig(101);
        let b = sig(102);
        let mut x = d(
            r.clone(),
            [
                Ok(ControlStep::Complete(IngressStep::Request(a.clone()))),
                Ok(ControlStep::Complete(IngressStep::Request(b))),
            ],
            [],
            [Ok(WriteStep::Complete)],
            [],
        );
        prime(&mut x, n);
        reply(&mut x, 9);

        x.step(n).unwrap();
        assert_eq!(x.completed, Some(a));
        assert_eq!(x.step(n), Err(ActorLaunchOrderError::CleanupRequired));
        assert_eq!(x.completed, Some(sig(101)));
        assert!(x.control_eof);
        x.step(n).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Empty);

        let r = r.lock().unwrap();
        assert_eq!((r.polls, r.stops.clone()), (2, vec![true]));
        assert_eq!(r.frames.len(), 2);
        assert!(matches!(
            decode_reply(&r.frames[1]),
            Ok(Reply::Result { request_id: 9, .. })
        ));
    }

    #[test]
    fn closed_permit_retries_cleanup_through_reap_and_tree_evidence() {
        let r = Arc::new(Mutex::new(R::default()));
        {
            let mut r = r.lock().unwrap();
            r.stop_errors.push_back(io::ErrorKind::WouldBlock);
            r.reap_errors.push_back(io::ErrorKind::Interrupted);
            r.trees.extend([
                TreeObservation::Unconfirmed,
                TreeObservation::Unconfirmed,
                TreeObservation::ConfirmedEmpty,
            ]);
        }
        let n = Instant::now();
        let mut x = d(
            r.clone(),
            [Ok(ControlStep::Idle)],
            [Err(ControlPermitError::Closed)],
            [],
            [],
        );

        x.step(n).unwrap();
        assert_eq!(x.failure, FailureState::DeferredTransport);
        assert_eq!(x.step(n), Err(ActorLaunchOrderError::CleanupRequired));
        assert_eq!(x.failure, FailureState::LatchedTransport);
        assert_eq!(
            x.step(n),
            Err(ActorLaunchOrderError::Reap(io::ErrorKind::Interrupted))
        );
        x.step(n + L.force).unwrap();
        x.step(n + L.force + L.drain).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Unconfirmed);
        x.step(n + L.force + L.drain + L.drain).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Empty);

        let r = r.lock().unwrap();
        assert_eq!((r.polls, r.permits), (1, 1));
        assert_eq!(r.stops, vec![true, true]);
        assert_eq!(r.cleanups, 4);
    }

    #[test]
    fn fatal_control_table_latches_pending_output_but_empty_still_drains_it() {
        let n = Instant::now();
        for error in [
            ControlWorkerError::TimedOut,
            ControlWorkerError::Ingress(IngressError::Io(io::ErrorKind::InvalidData)),
            ControlWorkerError::Closed,
        ] {
            let r = Arc::new(Mutex::new(R::default()));
            let mut x = d(r.clone(), [Err(error)], [], [Ok(WriteStep::Complete)], []);
            prime(&mut x, n);
            reply(&mut x, 55);

            assert_eq!(x.step(n), Err(ActorLaunchOrderError::CleanupRequired));
            assert!(x.schedule_state().terminal);
            assert!(x.schedule_state().output_pending);
            assert_eq!(r.lock().unwrap().stops, vec![true]);
            assert_eq!(r.lock().unwrap().frames.len(), 1);
        }

        let r = Arc::new(Mutex::new(R::default()));
        let mut x = d_at(
            r.clone(),
            [Err(ControlWorkerError::Closed)],
            [],
            [Ok(WriteStep::Complete)],
            [],
            Phase::Empty,
        );
        reply(&mut x, 56);

        x.step(n).unwrap();
        assert!(x.schedule_state().output_pending);
        x.step(n).unwrap();
        assert!(!x.schedule_state().output_pending);

        let r = r.lock().unwrap();
        assert!(r.stops.is_empty());
        assert_eq!(r.polls, 1);
        assert!(matches!(
            decode_reply(&r.frames[0]),
            Ok(Reply::Result { request_id: 56, .. })
        ));
    }

    #[test]
    fn fragmented_ready_waits_for_owned_flush_and_then_releases_in_order() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let bytes = ready();
        let split = bytes.len() / 2;
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Idle), Ok(ControlStep::Idle)],
            [Ok(()), Ok(())],
            [
                Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                    bytes: chunk(&bytes[..split]),
                    len: split,
                })),
                Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                    bytes: chunk(&bytes[split..]),
                    len: bytes.len() - split,
                })),
            ],
            [Ok(()), Ok(())],
            [Ok(WriteStep::Complete), Ok(WriteStep::Complete)],
            [Ok(ExitObservation::Running), Ok(ExitObservation::Running)],
            now,
        );

        x.step(now).unwrap();
        assert_eq!(r.lock().unwrap().frames.len(), 1);
        x.step(now).unwrap();

        let r = r.lock().unwrap();
        assert!(matches!(
            decode_reply(&r.frames[0]),
            Ok(Reply::Owned { .. })
        ));
        assert!(matches!(
            decode_event(&r.frames[1]),
            Ok(Event::Ready { .. })
        ));
        assert_eq!((r.ready_polls, r.ready_permits), (2, 2));
    }

    #[test]
    fn ready_deadline_at_the_exact_instant_wins_before_ready_poll() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Idle)],
            [],
            [Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                bytes: chunk(ready()),
                len: ready().len(),
            }))],
            [],
            [Ok(WriteStep::Pending)],
            [],
            now,
        );

        x.step(now + L.ready).unwrap();
        assert!(matches!(x.schedule_state().phase, Phase::ForceStopping(_)));
        let r = r.lock().unwrap();
        assert_eq!(r.ready_polls, 0);
        assert_eq!(r.stops, vec![true]);
    }

    #[test]
    fn exit_wins_over_simultaneous_control_and_ready() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Complete(IngressStep::Request(sig(88))))],
            [],
            [Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                bytes: chunk(ready()),
                len: ready().len(),
            }))],
            [],
            [Ok(WriteStep::Complete), Ok(WriteStep::Complete)],
            [Ok(ExitObservation::Exited { code: Some(7) })],
            now,
        );

        x.step(now).unwrap();
        x.step(now).unwrap();
        let r = r.lock().unwrap();
        assert_eq!(r.ready_polls, 0);
        assert!(matches!(
            decode_reply(&r.frames[0]),
            Ok(Reply::Owned { .. })
        ));
        assert!(matches!(
            decode_event(&r.frames[1]),
            Ok(Event::Exit { code: Some(7), .. })
        ));
    }

    #[test]
    fn ready_eof_after_ready_only_closes_stdout() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Idle), Ok(ControlStep::Idle)],
            [Ok(()), Ok(())],
            [
                Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                    bytes: chunk(ready()),
                    len: ready().len(),
                })),
                Ok(ReadyReadStep::Complete(ReadyRead::Eof)),
            ],
            [Ok(()), Ok(())],
            [Ok(WriteStep::Complete), Ok(WriteStep::Complete)],
            [Ok(ExitObservation::Running), Ok(ExitObservation::Running)],
            now,
        );

        x.step(now).unwrap();
        x.step(now).unwrap();
        assert!(x.ready_eof);
        assert_eq!(x.schedule_state().phase, Phase::Running);
        assert!(r.lock().unwrap().stops.is_empty());
    }

    #[test]
    fn rejected_owned_admission_retains_the_driver_for_force_and_reap_retry() {
        let r = Arc::new(Mutex::new(R::default()));
        {
            let mut state = r.lock().unwrap();
            state.reject_puts = 1;
            state.stop_errors.push_back(io::ErrorKind::WouldBlock);
        }
        let now = Instant::now();
        let mut x = GenerationDriver::new(
            ActorLaunchOrder::new(
                Launch(r.clone(), VecDeque::new()),
                Sink(r.clone(), VecDeque::new()),
                Phase::Launching(now + L.launch),
                L,
            ),
            Control {
                r: r.clone(),
                s: VecDeque::new(),
                p: VecDeque::new(),
            },
            Ready {
                r: r.clone(),
                s: VecDeque::new(),
                p: VecDeque::new(),
            },
            now,
        );

        assert_eq!(x.failure, FailureState::LatchedTransport);
        assert_eq!(r.lock().unwrap().stops, vec![true]);
        x.step(now).unwrap();
        assert_eq!(x.schedule_state().phase, Phase::Empty);
        let state = r.lock().unwrap();
        assert_eq!(state.stops, vec![true, true]);
        assert_eq!((state.polls, state.ready_polls), (0, 0));
    }

    #[test]
    fn retained_exit_output_never_grants_a_control_credit_in_its_final_turn() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let mut x = d_ready(
            r.clone(),
            [
                Ok(ControlStep::Idle),
                Ok(ControlStep::Idle),
                Ok(ControlStep::Idle),
            ],
            [Ok(()), Ok(()), Ok(())],
            [],
            [],
            [
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
            ],
            [Ok(ExitObservation::Exited { code: Some(3) })],
            now,
        );

        x.step(now).unwrap();
        x.step(now).unwrap();
        x.step(now).unwrap();
        assert!(!x.schedule_state().exit_retained && !x.schedule_state().output_pending);
        let state = r.lock().unwrap();
        assert_eq!(state.permits, 0);
        assert!(matches!(
            decode_event(&state.frames[1]),
            Ok(Event::Exit { code: Some(3), .. })
        ));
    }

    #[test]
    fn duplicate_ready_forces_once_and_ready_closed_defers_control() {
        let r = Arc::new(Mutex::new(R::default()));
        let now = Instant::now();
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Idle), Ok(ControlStep::Idle)],
            [Ok(()), Ok(())],
            [
                Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                    bytes: chunk(ready()),
                    len: ready().len(),
                })),
                Ok(ReadyReadStep::Complete(ReadyRead::Chunk {
                    bytes: chunk(b"pollution"),
                    len: 9,
                })),
            ],
            [Ok(()), Ok(())],
            [Ok(WriteStep::Complete)],
            [Ok(ExitObservation::Running), Ok(ExitObservation::Running)],
            now,
        );
        x.step(now).unwrap();
        assert_eq!(x.step(now), Err(ActorLaunchOrderError::CleanupRequired));
        x.step(now).unwrap();
        assert_eq!(r.lock().unwrap().stops, vec![true]);

        let r = Arc::new(Mutex::new(R::default()));
        let mut x = d_ready(
            r.clone(),
            [Ok(ControlStep::Idle)],
            [Ok(())],
            [Ok(ReadyReadStep::Idle)],
            [Err(ReadyReadPermitError::Closed)],
            [Ok(WriteStep::Complete)],
            [Ok(ExitObservation::Running)],
            now,
        );
        x.step(now).unwrap();
        assert_eq!(x.failure, FailureState::DeferredTransport);
        let counts = r.lock().unwrap();
        assert_eq!((counts.ready_permits, counts.permits), (1, 0));
        drop(counts);
        assert_eq!(x.step(now), Err(ActorLaunchOrderError::CleanupRequired));
    }
}
