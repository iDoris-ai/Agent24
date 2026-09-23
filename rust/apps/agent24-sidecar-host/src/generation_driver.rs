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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureState {
    None,
    DeferredTransport,
    LatchedTransport,
}

/// One actor, one control port, and exactly one completed request slot.
pub(crate) struct GenerationDriver<L, S, C> {
    actor: ActorLaunchOrder<L, S>,
    control: C,
    completed: Option<Request>,
    control_eof: bool,
    failure: FailureState,
}

#[allow(private_bounds)]
impl<L, S, C> GenerationDriver<L, S, C>
where
    L: LaunchIdentity + LaunchControl,
    S: GenerationSink,
    C: ControlPort,
{
    pub(crate) fn new(actor: ActorLaunchOrder<L, S>, control: C) -> Self {
        Self {
            actor,
            control,
            completed: None,
            control_eof: false,
            failure: FailureState::None,
        }
    }

    pub(crate) fn schedule_state(&self) -> ScheduleState {
        self.actor.schedule_state()
    }

    /// One turn: failure cleanup; one poll; one lifecycle path; output
    /// barrier; at most one dispatch; then at most one final permit.
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
        if state.output_pending {
            self.actor.output_step(now)?;
            if exit_barrier || self.schedule_state().terminal {
                return maintenance.map(|_| ());
            }
            maintenance?;
            return self.permit_last(now);
        }
        if exit_barrier {
            return maintenance.map(|_| ());
        }
        maintenance?;
        self.actor.dispatch_completed(&mut self.completed, now)?;
        if self.schedule_state().terminal {
            return Ok(());
        }
        self.permit_last(now)
    }

    fn permit_last(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
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
            self.0.lock().unwrap().frames.push(f);
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
    fn d(
        r: Arc<Mutex<R>>,
        s: impl IntoIterator<Item = Result<ControlStep, ControlWorkerError>>,
        p: impl IntoIterator<Item = Result<(), ControlPermitError>>,
        o: impl IntoIterator<Item = Result<WriteStep, OutputWriteError>>,
        e: impl IntoIterator<Item = io::Result<ExitObservation>>,
    ) -> GenerationDriver<Launch, Sink, Control> {
        d_at(r, s, p, o, e, Phase::Launching(Instant::now() + L.launch))
    }
    fn d_at(
        r: Arc<Mutex<R>>,
        s: impl IntoIterator<Item = Result<ControlStep, ControlWorkerError>>,
        p: impl IntoIterator<Item = Result<(), ControlPermitError>>,
        o: impl IntoIterator<Item = Result<WriteStep, OutputWriteError>>,
        e: impl IntoIterator<Item = io::Result<ExitObservation>>,
        phase: Phase,
    ) -> GenerationDriver<Launch, Sink, Control> {
        GenerationDriver::new(
            ActorLaunchOrder::new(
                Launch(r.clone(), e.into_iter().collect()),
                Sink(r.clone(), o.into_iter().collect()),
                phase,
                L,
            ),
            Control {
                r,
                s: s.into_iter().collect(),
                p: p.into_iter().collect(),
            },
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
    fn prime(x: &mut GenerationDriver<Launch, Sink, Control>, n: Instant) {
        x.actor.queue_owned(n).unwrap();
        x.actor.output_step(n).unwrap();
    }
    fn empty(x: &mut GenerationDriver<Launch, Sink, Control>, r: &Arc<Mutex<R>>, n: Instant) {
        x.actor.stop(true, n).unwrap();
        x.actor.cleanup_tick(n).unwrap();
        r.lock().unwrap().stops.clear()
    }
    fn reply(x: &mut GenerationDriver<Launch, Sink, Control>, id: u64) {
        x.actor
            .queue_reply(&Reply::Result {
                version: 1,
                request_id: id,
            })
            .unwrap()
    }

    #[rustfmt::skip] #[test] fn owned_ready_exit_order(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[],[],[Ok(WriteStep::Complete);3],[Ok(ExitObservation::Running),Ok(ExitObservation::Exited{code:Some(9)})]);prime(&mut x,n);x.actor.ready(n,ready()).unwrap();for _ in 0..3{x.step(n).unwrap()}let f=&r.lock().unwrap().frames;assert!(matches!(decode_reply(&f[0]),Ok(Reply::Owned{..})));assert!(matches!(decode_event(&f[1]),Ok(Event::Ready{..})));assert!(matches!(decode_event(&f[2]),Ok(Event::Exit{code:Some(9),..})));}
    #[rustfmt::skip] #[test] fn output_barrier_retains_request_and_one_credit(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Ok(ControlStep::Complete(IngressStep::Request(sig(11))))],[],[Ok(WriteStep::Complete),Ok(WriteStep::Pending),Ok(WriteStep::Complete)],[]);prime(&mut x,n);reply(&mut x,10);x.step(n).unwrap();assert!(x.completed.is_some());for _ in 0..3{x.step(n).unwrap()}assert!(x.completed.is_none());x.step(n).unwrap();let f=&r.lock().unwrap().frames;assert!(matches!(decode_reply(&f[1]),Ok(Reply::Result{request_id:10,..})));assert!(matches!(decode_reply(&f[2]),Ok(Reply::Result{request_id:11,..})));let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[Ok(ControlStep::Idle)],[Ok(())],[Ok(WriteStep::Complete)],[]);prime(&mut x,n);x.step(n).unwrap();let s=r.lock().unwrap();assert_eq!((s.polls,s.permits),(1,1));}
    #[rustfmt::skip] #[test] fn deadlines_and_force_are_once_per_turn(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[],[],[Ok(WriteStep::Pending)],[]);x.actor.queue_owned(n).unwrap();x.step(n+L.ready).unwrap();assert!(matches!(x.schedule_state().phase,Phase::ForceStopping(_)));assert_eq!(r.lock().unwrap().stops,vec![true]);let r=Arc::new(Mutex::new(R::default()));r.lock().unwrap().stop_errors.push_back(io::ErrorKind::WouldBlock);let mut x=d(r.clone(),[],[],[Err(OutputWriteError::Closed)],[]);x.actor.queue_owned(n).unwrap();assert_eq!(x.step(n+L.ready),Err(ActorLaunchOrderError::CleanupRequired));assert_eq!(r.lock().unwrap().stops,vec![true]);}
    #[rustfmt::skip] #[test] fn eof_stops_running_and_closed_empty_drains(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Ok(ControlStep::Complete(IngressStep::Eof))],[],[Ok(WriteStep::Complete)],[Ok(ExitObservation::Running)]);prime(&mut x,n);x.actor.ready(n,ready()).unwrap();x.step(n).unwrap();assert_eq!(r.lock().unwrap().stops,vec![false]);let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[Err(ControlWorkerError::Closed)],[],[Ok(WriteStep::Complete)],[]);empty(&mut x,&r,n);reply(&mut x,8);x.step(n).unwrap();x.step(n).unwrap();let s=r.lock().unwrap();assert!(s.stops.is_empty());assert_eq!((s.polls,s.frames.len()),(1,1));}
    #[rustfmt::skip] #[test] fn maintenance_errors_and_transport_cleanup_retry_without_starvation(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Err(ControlWorkerError::Closed)],[],[],[]);assert_eq!(x.step(n),Err(ActorLaunchOrderError::CleanupRequired));r.lock().unwrap().reap_errors.push_back(io::ErrorKind::Interrupted);assert_eq!(x.step(n),Err(ActorLaunchOrderError::Reap(io::ErrorKind::Interrupted)));x.step(n).unwrap();let s=r.lock().unwrap();assert_eq!((s.cleanups,s.stops.clone()),(2,vec![true]));drop(s);let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[],[],[Ok(WriteStep::Complete)],[Err(io::Error::from(io::ErrorKind::Interrupted)),Err(io::Error::from(io::ErrorKind::Interrupted))]);prime(&mut x,n);reply(&mut x,9);for _ in 0..2{assert_eq!(x.step(n),Err(ActorLaunchOrderError::Observe(io::ErrorKind::Interrupted)))}assert_eq!(r.lock().unwrap().frames.len(),2);}
    #[rustfmt::skip] #[test] fn closed_permit_and_empty_output_failure_do_not_force_empty(){let r=Arc::new(Mutex::new(R::default()));let n=Instant::now();let mut x=d(r.clone(),[Ok(ControlStep::Idle)],[Err(ControlPermitError::Closed)],[],[]);x.step(n).unwrap();assert_eq!(x.step(n),Err(ActorLaunchOrderError::CleanupRequired));assert_eq!(r.lock().unwrap().stops,vec![true]);let r=Arc::new(Mutex::new(R::default()));let mut x=d(r.clone(),[],[],[Err(OutputWriteError::Closed)],[]);empty(&mut x,&r,n);reply(&mut x,10);x.step(n).unwrap();assert_eq!(x.step(n),Err(ActorLaunchOrderError::CleanupRequired));assert_eq!(x.schedule_state().phase,Phase::Empty);assert!(r.lock().unwrap().stops.is_empty());}

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
        let mut x = d(
            r.clone(),
            [Ok(ControlStep::Complete(IngressStep::Eof))],
            [],
            [Err(OutputWriteError::Closed)],
            [],
        );
        x.actor.queue_owned(n).unwrap();
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
        let mut x = d(
            r.clone(),
            [Err(ControlWorkerError::Closed)],
            [],
            [Ok(WriteStep::Complete)],
            [],
        );
        empty(&mut x, &r, n);
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
}
