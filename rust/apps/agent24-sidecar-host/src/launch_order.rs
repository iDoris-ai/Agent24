use agent24_sidecar_host_protocol::{
    ErrorCode, Event, PROTOCOL_VERSION, Reply, Request, encode_reply,
};

use crate::{
    actor::{Deadlines, Phase},
    cleanup::CleanupStepError,
    launch::OwnedLaunch,
    outbox::{DriveStep, Outbox},
    output_io::{OutputWriteError, OutputWriter, PutFrameError, WriteStep},
    ready_io::ReadyGate,
    target::{ExitObservation, TreeObservation},
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
    fn put(&mut self, frame: Vec<u8>, now: Instant) -> Result<(), PutFrameError>;
    fn step(&mut self, now: Instant) -> Result<WriteStep, OutputWriteError>;
}

impl<W: Write> FrameSink for OutputWriter<W> {
    fn put(&mut self, frame: Vec<u8>, _now: Instant) -> Result<(), PutFrameError> {
        OutputWriter::put(self, frame)
    }

    fn step(&mut self, _now: Instant) -> Result<WriteStep, OutputWriteError> {
        OutputWriter::write_step(self)
    }
}

impl FrameSink for crate::output_worker::OutputWorker {
    fn put(&mut self, frame: Vec<u8>, now: Instant) -> Result<(), PutFrameError> {
        crate::output_worker::OutputWorker::put(self, frame, now)
    }

    fn step(&mut self, now: Instant) -> Result<WriteStep, OutputWriteError> {
        crate::output_worker::OutputWorker::step(self, now)
    }
}

impl FrameSink for &mut crate::output_worker::OutputWorker {
    fn put(&mut self, frame: Vec<u8>, now: Instant) -> Result<(), PutFrameError> {
        crate::output_worker::OutputWorker::put(self, frame, now)
    }

    fn step(&mut self, now: Instant) -> Result<WriteStep, OutputWriteError> {
        crate::output_worker::OutputWorker::step(self, now)
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
    pre_owned_output: bool,
}

impl<L: LaunchIdentity, S: FrameSink> LaunchOrder<L, S> {
    pub(crate) fn new(launch: L, sink: S) -> Self {
        Self {
            launch,
            sink,
            gate: ReadyGate::new(),
            held_ready: None,
            stage: LaunchOrderStage::Contained,
            pre_owned_output: false,
        }
    }

    pub(crate) fn queue_owned(&mut self, now: Instant) -> Result<(), LaunchOrderStage> {
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
        if self.sink.put(frame, now).is_err() {
            return self.fail();
        }
        self.stage = LaunchOrderStage::OwnedPending;
        Ok(())
    }

    pub(crate) fn output_step(&mut self, now: Instant) -> Result<WriteStep, LaunchOrderStage> {
        if self.stage == LaunchOrderStage::Contained {
            if !self.pre_owned_output {
                return self.fail();
            }
            return match self.sink.step(now) {
                Ok(WriteStep::Complete) => {
                    self.pre_owned_output = false;
                    Ok(WriteStep::Complete)
                }
                Ok(WriteStep::Pending) => Ok(WriteStep::Pending),
                Ok(WriteStep::Idle) | Err(_) => self.fail(),
            };
        }
        if matches!(
            self.stage,
            LaunchOrderStage::AwaitReady | LaunchOrderStage::Ready
        ) {
            return self
                .sink
                .step(now)
                .map_err(|_| LaunchOrderStage::CleanupRequired);
        }
        if self.stage != LaunchOrderStage::OwnedPending {
            return self.fail();
        }
        match self.sink.step(now) {
            Ok(WriteStep::Pending) => Ok(WriteStep::Pending),
            Ok(WriteStep::Complete) => {
                self.stage = LaunchOrderStage::AwaitReady;
                Ok(WriteStep::Complete)
            }
            // An output deadline flows to ActorLaunchOrder::fail_force(now).
            // Do not discard an already parsed READY while that transition
            // retains the same launch owner for cleanup.
            Err(OutputWriteError::Io(io::ErrorKind::TimedOut)) => self.fail_preserving_ready(),
            Ok(WriteStep::Idle) | Err(_) => self.fail(),
        }
    }

    /// Admit one already-encoded non-ownership frame to the one output sink.
    /// Ownership itself is deliberately kept on `queue_owned`: it is the
    /// transition which starts the Ready deadline.
    pub(crate) fn put_frame(
        &mut self,
        frame: Vec<u8>,
        now: Instant,
    ) -> Result<(), LaunchOrderStage> {
        if self.stage == LaunchOrderStage::CleanupRequired {
            return self.fail();
        }
        let result = self
            .sink
            .put(frame, now)
            .map_err(|_| LaunchOrderStage::CleanupRequired);
        if result.is_ok() && self.stage == LaunchOrderStage::Contained {
            self.pre_owned_output = true;
        }
        result
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

    fn fail_preserving_ready<T>(&mut self) -> Result<T, LaunchOrderStage> {
        self.stage = LaunchOrderStage::CleanupRequired;
        Err(LaunchOrderStage::CleanupRequired)
    }
}

pub(crate) trait LaunchControl {
    fn stop(&mut self, force: bool) -> io::Result<()>;
    fn observe_exit(&mut self) -> io::Result<ExitObservation>;
    fn cleanup(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActorLaunchOrderError {
    CleanupRequired,
    InvalidTransition,
    Stop(io::ErrorKind),
    Observe(io::ErrorKind),
    Reap(io::ErrorKind),
}

pub(crate) struct ActorLaunchOrder<L, S> {
    order: LaunchOrder<L, S>,
    phase: Phase,
    limits: Deadlines,
    released_ready: Option<Event>,
    terminal: Option<ActorLaunchOrderError>,
    force_ok: bool,
    force_attempted: bool,
    force_attempted_this_turn: bool,
    pending_exit: Option<Event>,
    outbox: Outbox,
}

/// The scheduler gets facts, not the actor's owner, sink, or bounded outbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScheduleState {
    pub(crate) phase: Phase,
    pub(crate) terminal: bool,
    pub(crate) output_pending: bool,
    pub(crate) exit_retained: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchStep {
    Idle,
    Blocked,
    Replied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForceAttempt {
    Confirmed,
    Pending,
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
            force_attempted: false,
            force_attempted_this_turn: false,
            pending_exit: None,
            outbox: Outbox::default(),
        }
    }

    pub(crate) const fn phase(&self) -> Phase {
        self.phase
    }

    /// Reset the driver's per-turn force guard before it invokes any actor
    /// operation.  This prevents a maintenance force and an output failure
    /// from issuing two native force attempts in one scheduler turn.
    pub(crate) fn begin_turn(&mut self) {
        self.force_attempted_this_turn = false;
    }

    pub(crate) fn schedule_state(&self) -> ScheduleState {
        ScheduleState {
            phase: self.phase,
            terminal: self.terminal.is_some()
                || self.order.stage == LaunchOrderStage::CleanupRequired,
            output_pending: self.output_pending(),
            exit_retained: self.exit_retained(),
        }
    }

    /// Run exactly one lifecycle path.  A stopping actor never receives a
    /// second force attempt from `tick` and `cleanup_tick` in the same turn.
    pub(crate) fn maintenance(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        if self.schedule_state().terminal || self.stopping() {
            self.cleanup_tick(now).map(|_| ())
        } else {
            self.tick(now).map(|_| ())
        }
    }

    /// Consume one completed request only when its reply can be admitted
    /// without overtaking output or an observed Exit.  The caller owns the
    /// one-slot completed-request buffer; this actor never creates a queue.
    pub(crate) fn dispatch_completed(
        &mut self,
        completed: &mut Option<Request>,
        now: Instant,
    ) -> Result<DispatchStep, ActorLaunchOrderError> {
        if completed.is_none() {
            return Ok(DispatchStep::Idle);
        }
        self.advance(now);
        if !self.completed_request_dispatch_allowed() {
            return Ok(DispatchStep::Blocked);
        }

        let Some(request) = completed.take() else {
            return Ok(DispatchStep::Idle);
        };
        let reply = match request {
            Request::Launch { request_id, .. } => Reply::Error {
                version: PROTOCOL_VERSION,
                request_id,
                code: ErrorCode::InvalidRequest,
            },
            Request::IsEmpty { request_id, .. } => Reply::Empty {
                version: PROTOCOL_VERSION,
                request_id,
                empty: matches!(self.phase, Phase::Empty),
            },
            Request::Signal { request_id, .. } if matches!(self.phase, Phase::Unconfirmed) => {
                Reply::Error {
                    version: PROTOCOL_VERSION,
                    request_id,
                    code: ErrorCode::SignalFailed,
                }
            }
            Request::Signal {
                request_id, force, ..
            } => match self.stop(force, now) {
                Ok(()) => Reply::Result {
                    version: PROTOCOL_VERSION,
                    request_id,
                },
                // A non-blocking native signal can race its publication. The
                // actor retains containment and emits only this static code.
                Err(ActorLaunchOrderError::Stop(io::ErrorKind::WouldBlock))
                    if self.terminal.is_none() =>
                {
                    Reply::Error {
                        version: PROTOCOL_VERSION,
                        request_id,
                        code: ErrorCode::SignalFailed,
                    }
                }
                Err(error) => return Err(error),
            },
        };
        self.queue_reply(&reply)
            .map_err(|_| self.fail_transport(now))?;
        Ok(DispatchStep::Replied)
    }

    /// Advance deadlines and make one non-consuming leader observation.
    /// The caller owns scheduling; repeated idle ticks never renew deadlines.
    pub(crate) fn tick(&mut self, now: Instant) -> Result<Option<Event>, ActorLaunchOrderError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        self.advance(now);
        if self.pending_exit.is_some() {
            self.admit_pending_exit()?;
            return Ok(None);
        }
        match self.phase {
            Phase::GracefulStopping(_) => return Ok(None),
            Phase::Empty => return Ok(None),
            Phase::ForceStopping(_) | Phase::Draining(_) | Phase::Unconfirmed => {
                self.try_force()?;
                self.admit_pending_exit()?;
                return Ok(None);
            }
            _ => {}
        }
        if !matches!(self.phase, Phase::AwaitReady(_) | Phase::Running)
            || self.pending_exit.is_some()
        {
            return Ok(None);
        }
        match self.order.launch.observe_exit() {
            Ok(ExitObservation::Running) => Ok(None),
            Ok(ExitObservation::Exited { code }) => {
                self.pending_exit = Some(Event::Exit {
                    protocol: PROTOCOL_VERSION,
                    code,
                });
                self.phase = self
                    .phase
                    .stop(true, now, self.limits)
                    .map_err(|_| self.latch(ActorLaunchOrderError::InvalidTransition))?;
                self.admit_pending_exit()?;
                Ok(None)
            }
            Err(error) => Err(ActorLaunchOrderError::Observe(error.kind())),
        }
    }

    /// Latch a transport failure and retain the owner exclusively for cleanup.
    pub(crate) fn fail_transport(&mut self, now: Instant) -> ActorLaunchOrderError {
        self.fail_force(now)
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
        self.order
            .queue_owned(now)
            .map_err(|_| self.fail_force(now))
    }

    pub(crate) fn output_step(&mut self, now: Instant) -> Result<WriteStep, ActorLaunchOrderError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if self.outbox.has_work()
            && self.order.stage != LaunchOrderStage::OwnedPending
            && !(self.order.stage == LaunchOrderStage::Contained && self.order.pre_owned_output)
        {
            return match self
                .outbox
                .drive(&mut self.order.sink, now)
                .map_err(|_| self.fail_force(now))?
            {
                DriveStep::Idle | DriveStep::Complete(_) => Ok(WriteStep::Complete),
                DriveStep::Pending | DriveStep::Admitted(_) => Ok(WriteStep::Pending),
            };
        }
        let step = self
            .order
            .output_step(now)
            .map_err(|_| self.fail_force(now))?;
        if step == WriteStep::Complete
            && let Some(event) = self.order.take_ready()
            && !self.stopping()
        {
            self.phase_ready(now)?;
            self.outbox
                .ready(&event)
                .map_err(|_| self.fail_force(now))?;
            self.released_ready = Some(event);
        }
        Ok(step)
    }

    /// The runtime's only bounded output-admission seam after `Owned`.
    pub(crate) fn put_frame(
        &mut self,
        frame: Vec<u8>,
        _now: Instant,
    ) -> Result<(), ActorLaunchOrderError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        self.outbox
            .encoded_reply(frame)
            .map_err(|_| ActorLaunchOrderError::CleanupRequired)
    }

    /// Queue a completed control reply without displacing a prior reply.
    /// A reply from an already-issued request may still flush after Exit gates
    /// future permits and dispatch.
    pub(crate) fn queue_reply(&mut self, reply: &Reply) -> Result<(), ActorLaunchOrderError> {
        self.outbox
            .reply(reply)
            .map_err(|_| ActorLaunchOrderError::CleanupRequired)
    }

    /// Whether a runtime may grant another control read permit or dispatch a
    /// completed request.  Output and cleanup deliberately do not use this gate.
    pub(crate) fn control_permit_allowed(&self) -> bool {
        !self.exit_retained()
    }

    pub(crate) fn completed_request_dispatch_allowed(&self) -> bool {
        let state = self.schedule_state();
        !state.terminal
            && !state.exit_retained
            && !state.output_pending
            && self.owned_acknowledged()
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
        self.outbox
            .ready(&event)
            .map_err(|_| self.fail_force(now))?;
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
        if self.terminal.is_none() {
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
        if force
            && self.force_attempted
            && matches!(
                self.phase,
                Phase::ForceStopping(_) | Phase::Draining(_) | Phase::Unconfirmed
            )
        {
            return if self.force_ok {
                Ok(())
            } else {
                Err(ActorLaunchOrderError::Stop(io::ErrorKind::WouldBlock))
            };
        }
        self.force_attempted |= force;
        self.force_attempted_this_turn |= force;
        match self.order.launch.stop(force) {
            Ok(()) => {
                self.force_ok |= force;
                self.order.held_ready = None;
                Ok(())
            }
            // Darwin can report a transient WouldBlock while SIGKILL races
            // the kernel's publication of the leader exit. Keep ownership
            // and the original force deadline; cleanup will issue one retry.
            Err(error) if force && error.kind() == io::ErrorKind::WouldBlock => {
                Err(ActorLaunchOrderError::Stop(io::ErrorKind::WouldBlock))
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
        if matches!(self.phase, Phase::GracefulStopping(_)) {
            match self.order.launch.observe_exit() {
                Ok(ExitObservation::Running) => return Ok(TreeObservation::Present),
                Ok(ExitObservation::Exited { code }) => {
                    self.pending_exit = Some(Event::Exit {
                        protocol: PROTOCOL_VERSION,
                        code,
                    });
                    self.phase = self
                        .phase
                        .stop(true, now, self.limits)
                        .map_err(|_| ActorLaunchOrderError::InvalidTransition)?;
                }
                Err(error) => return Err(ActorLaunchOrderError::Observe(error.kind())),
            }
        }
        if matches!(
            self.phase,
            Phase::ForceStopping(_) | Phase::Draining(_) | Phase::Unconfirmed
        ) && self.try_force()? == ForceAttempt::Pending
        {
            return Ok(TreeObservation::Present);
        }
        let result = self
            .order
            .launch
            .cleanup(&mut self.phase)
            .map_err(|error| match error {
                CleanupStepError::Phase(_) => ActorLaunchOrderError::InvalidTransition,
                CleanupStepError::Reap(error) => ActorLaunchOrderError::Reap(error.kind()),
            })?;
        self.admit_pending_exit()?;
        Ok(result)
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
        if !matches!(self.phase, Phase::Empty)
            && !matches!(
                self.phase,
                Phase::ForceStopping(_) | Phase::Draining(_) | Phase::Unconfirmed
            )
        {
            self.phase = Phase::ForceStopping(now + self.limits.force);
        }
        if !self.force_attempted_this_turn && !matches!(self.phase, Phase::Empty) {
            let _ = self.try_force();
        }
        // The output deadline is a control-plane failure, not a transfer of
        // process ownership. Keep a captured READY event available to cleanup
        // diagnostics while permanently latching the actor.
        self.terminal = Some(ActorLaunchOrderError::CleanupRequired);
        self.order.stage = LaunchOrderStage::CleanupRequired;
        ActorLaunchOrderError::CleanupRequired
    }

    fn latch(&mut self, error: ActorLaunchOrderError) -> ActorLaunchOrderError {
        self.terminal = Some(error);
        self.order.stage = LaunchOrderStage::CleanupRequired;
        self.order.held_ready = None;
        error
    }

    fn try_force(&mut self) -> Result<ForceAttempt, ActorLaunchOrderError> {
        if self.force_ok {
            return Ok(ForceAttempt::Confirmed);
        }
        self.force_attempted_this_turn = true;
        self.force_attempted = true;
        match self.order.launch.stop(true) {
            Ok(()) => {
                self.force_ok = true;
                Ok(ForceAttempt::Confirmed)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(ForceAttempt::Pending),
            Err(error) => Err(ActorLaunchOrderError::Stop(error.kind())),
        }
    }

    /// Keep the observed Exit in actor state until both containment and the
    /// bounded outbox accept it.  In particular, neither an occupied slot nor
    /// a transient/real stop error transfers the event to a caller.
    fn admit_pending_exit(&mut self) -> Result<(), ActorLaunchOrderError> {
        let Some(event) = self.pending_exit.clone() else {
            return Ok(());
        };
        if !self.force_ok && self.try_force()? != ForceAttempt::Confirmed {
            return Ok(());
        }
        if self.outbox.exit(&event).is_ok() {
            self.pending_exit = None;
        }
        Ok(())
    }

    fn exit_retained(&self) -> bool {
        self.pending_exit.is_some() || self.outbox.exit_retained()
    }

    fn output_pending(&self) -> bool {
        self.outbox.has_work()
            || self.order.pre_owned_output
            || self.order.stage == LaunchOrderStage::OwnedPending
    }

    fn owned_acknowledged(&self) -> bool {
        matches!(
            self.order.stage,
            LaunchOrderStage::AwaitReady | LaunchOrderStage::Ready
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use agent24_sidecar_host_protocol::{ErrorCode, Request, decode_event, decode_reply};
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Condvar, Mutex},
    };

    struct FakeLaunch(u64);

    impl LaunchIdentity for FakeLaunch {
        fn request_id(&self) -> u64 {
            self.0
        }
    }

    struct FakeSink {
        frame: Option<Vec<u8>>,
        frames: Vec<Vec<u8>>,
        steps: Vec<Result<WriteStep, OutputWriteError>>,
        put_now: Vec<Instant>,
        step_now: Vec<Instant>,
    }

    impl FrameSink for FakeSink {
        fn put(&mut self, frame: Vec<u8>, now: Instant) -> Result<(), PutFrameError> {
            self.frames.push(frame.clone());
            self.frame = Some(frame);
            self.put_now.push(now);
            Ok(())
        }

        fn step(&mut self, now: Instant) -> Result<WriteStep, OutputWriteError> {
            self.step_now.push(now);
            self.steps.remove(0)
        }
    }

    fn order(steps: Vec<Result<WriteStep, OutputWriteError>>) -> LaunchOrder<FakeLaunch, FakeSink> {
        LaunchOrder::new(
            FakeLaunch(7),
            FakeSink {
                frame: None,
                frames: Vec::new(),
                steps,
                put_now: Vec::new(),
                step_now: Vec::new(),
            },
        )
    }

    fn ready() -> &'static [u8] {
        br#"{"type":"ready","protocol":1,"port":1,"token":"tttttttttttttttttttttttttttttttt","version":"v"}
"#
    }

    #[test]
    fn owned_and_ready_are_ordered_and_released_once() {
        let now = Instant::now();
        let mut owned = order(vec![Ok(WriteStep::Pending), Ok(WriteStep::Complete)]);
        assert!(owned.sink.frame.is_none());
        owned.queue_owned(now).unwrap();
        assert!(matches!(
            decode_reply(owned.sink.frame.as_ref().unwrap()).unwrap(),
            Reply::Owned { request_id: 7, .. }
        ));
        assert_eq!(owned.ready(ready()), Ok(None));
        assert_eq!(owned.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(owned.output_step(now), Ok(WriteStep::Complete));
        assert!(owned.take_ready().is_some());
        assert!(owned.take_ready().is_none());

        let mut late = order(vec![Ok(WriteStep::Complete)]);
        late.queue_owned(now).unwrap();
        late.output_step(now).unwrap();
        assert!(late.ready(ready()).unwrap().is_some());
    }

    #[test]
    fn launch_order_forwards_admission_and_poll_timestamps() {
        let now = Instant::now();
        let poll = now + std::time::Duration::from_millis(1);
        let mut order = order(vec![Ok(WriteStep::Pending)]);

        order.queue_owned(now).unwrap();
        assert_eq!(order.output_step(poll), Ok(WriteStep::Pending));
        assert_eq!(order.sink.put_now, vec![now]);
        assert_eq!(order.sink.step_now, vec![poll]);
    }

    #[test]
    fn malformed_order_and_ready_streams_latch_cleanup() {
        let now = Instant::now();
        let mut before_queue = order(vec![]);
        assert_eq!(
            before_queue.output_step(now),
            Err(LaunchOrderStage::CleanupRequired)
        );
        assert_eq!(
            before_queue.ready(&[]),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut pre_ready_eof = order(vec![Ok(WriteStep::Complete)]);
        pre_ready_eof.queue_owned(now).unwrap();
        assert_eq!(
            pre_ready_eof.ready_eof(),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut post_ready_eof = order(vec![Ok(WriteStep::Complete)]);
        post_ready_eof.queue_owned(now).unwrap();
        post_ready_eof.output_step(now).unwrap();
        assert!(post_ready_eof.ready(ready()).unwrap().is_some());
        assert_eq!(post_ready_eof.ready_eof(), Ok(()));

        let mut trailing = order(vec![Ok(WriteStep::Complete)]);
        trailing.queue_owned(now).unwrap();
        trailing.output_step(now).unwrap();
        trailing.ready(ready()).unwrap();
        let mut bytes = ready().to_vec();
        bytes.extend_from_slice(ready());
        assert_eq!(
            trailing.ready(&bytes),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut write_error = order(vec![Err(OutputWriteError::Closed)]);
        write_error.queue_owned(now).unwrap();
        assert_eq!(
            write_error.output_step(now),
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
        observations: VecDeque<io::Result<ExitObservation>>,
        observed: usize,
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

        fn observe_exit(&mut self) -> io::Result<ExitObservation> {
            self.observed += 1;
            self.observations
                .pop_front()
                .unwrap_or(Ok(ExitObservation::Running))
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
                observations: VecDeque::new(),
                observed: 0,
                reaps: reaps.into_iter().collect(),
            },
            FakeSink {
                frame: None,
                frames: Vec::new(),
                steps,
                put_now: Vec::new(),
                step_now: Vec::new(),
            },
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

    fn dispatcher(
        stops: impl IntoIterator<Item = io::Result<()>>,
        steps: Vec<Result<WriteStep, OutputWriteError>>,
    ) -> ActorLaunchOrder<ScriptLaunch, FakeSink> {
        let mut actor = actor(stops, [], steps);
        actor.phase = Phase::Running;
        actor.order.stage = LaunchOrderStage::Ready;
        actor
    }

    fn flush_reply(actor: &mut ActorLaunchOrder<ScriptLaunch, FakeSink>, now: Instant) {
        assert_eq!(actor.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(actor.output_step(now), Ok(WriteStep::Complete));
    }

    fn signal(request_id: u64, force: bool) -> Request {
        Request::Signal {
            version: PROTOCOL_VERSION,
            request_id,
            force,
        }
    }

    fn is_empty(request_id: u64) -> Request {
        Request::IsEmpty {
            version: PROTOCOL_VERSION,
            request_id,
        }
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
    fn pending_force_does_not_latch_or_reset_deadline() {
        let now = Instant::now();
        let deadline = now + LIMITS.force;
        let mut actor = actor(
            [
                io_error(io::ErrorKind::WouldBlock),
                io_error(io::ErrorKind::WouldBlock),
                io_error(io::ErrorKind::WouldBlock),
                io_error(io::ErrorKind::WouldBlock),
                io_error(io::ErrorKind::WouldBlock),
            ],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![],
        );
        actor.phase = Phase::ForceStopping(deadline);

        assert_eq!(
            actor.stop(true, now),
            Err(ActorLaunchOrderError::Stop(io::ErrorKind::WouldBlock))
        );
        assert_eq!(actor.phase, Phase::ForceStopping(deadline));
        assert!(actor.terminal.is_none());
        assert!(!actor.force_ok);
        assert_eq!(actor.order.launch.forces, vec![true]);

        assert_eq!(actor.cleanup_tick(now), Ok(TreeObservation::Present));
        assert_eq!(actor.phase, Phase::ForceStopping(deadline));
        assert!(actor.terminal.is_none());
        assert!(!actor.force_ok);
        assert_eq!(actor.order.launch.forces, vec![true, true]);
        assert_eq!(
            actor.order.launch.reaps.len(),
            1,
            "pending force must not reap"
        );

        assert_eq!(
            actor.cleanup_tick(now + std::time::Duration::from_millis(1)),
            Ok(TreeObservation::Present)
        );
        assert_eq!(actor.phase, Phase::ForceStopping(deadline));
        assert!(actor.terminal.is_none());
        assert!(!actor.force_ok);
        assert_eq!(actor.order.launch.forces, vec![true, true, true]);
        assert_eq!(actor.order.launch.reaps.len(), 1);

        assert_eq!(actor.cleanup_tick(deadline), Ok(TreeObservation::Present));
        assert_eq!(actor.phase, Phase::Draining(deadline + LIMITS.drain));
        assert!(actor.terminal.is_none());
        assert!(!actor.force_ok);
        assert_eq!(actor.order.launch.forces, vec![true, true, true, true]);

        assert_eq!(
            actor.cleanup_tick(deadline + LIMITS.drain),
            Ok(TreeObservation::Present)
        );
        assert_eq!(actor.phase, Phase::Unconfirmed);
        assert!(actor.terminal.is_none());
        assert!(!actor.force_ok);
        assert_eq!(
            actor.order.launch.forces,
            vec![true, true, true, true, true]
        );
        assert_eq!(actor.order.launch.reaps.len(), 1);
    }

    #[test]
    fn output_timeout_forces_once_and_preserves_owner_and_held_ready() {
        let now = Instant::now();
        let deadline = now + LIMITS.force;
        let mut actor = actor(
            [
                io_error(io::ErrorKind::WouldBlock),
                io_error(io::ErrorKind::WouldBlock),
                io_error(io::ErrorKind::WouldBlock),
            ],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Err(OutputWriteError::Io(io::ErrorKind::TimedOut))],
        );
        queue_ready(&mut actor, now);

        cleanup_required(actor.output_step(now));
        assert_eq!(actor.phase, Phase::ForceStopping(deadline));
        assert_eq!(actor.order.launch.forces, vec![true]);
        assert!(actor.order.held_ready.is_some());
        assert!(actor.terminal.is_some());

        assert_eq!(actor.cleanup_tick(now), Ok(TreeObservation::Present));
        assert_eq!(actor.phase, Phase::ForceStopping(deadline));
        assert_eq!(actor.order.launch.forces, vec![true, true]);
        assert_eq!(actor.order.launch.reaps.len(), 1);
        assert_eq!(
            actor.cleanup_tick(now + std::time::Duration::from_millis(1)),
            Ok(TreeObservation::Present)
        );
        assert_eq!(actor.phase, Phase::ForceStopping(deadline));
        assert_eq!(actor.order.launch.forces, vec![true, true, true]);
        assert_eq!(actor.order.launch.reaps.len(), 1);
    }

    #[test]
    fn blocked_output_worker_does_not_block_force_or_cleanup() {
        #[derive(Default)]
        struct State {
            entered: bool,
            released: bool,
            flushed: bool,
            writer_dropped: bool,
            bytes: Vec<u8>,
        }
        type GateState = Arc<(Mutex<State>, Condvar)>;
        struct Gate(GateState);
        impl Write for Gate {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let (lock, changed) = &*self.0;
                let mut state = lock.lock().unwrap();
                state.entered = true;
                changed.notify_all();
                while !state.released {
                    state = changed.wait(state).unwrap();
                }
                state.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                let (lock, changed) = &*self.0;
                lock.lock().unwrap().flushed = true;
                changed.notify_all();
                Ok(())
            }
        }
        impl Drop for Gate {
            fn drop(&mut self) {
                let (lock, changed) = &*self.0;
                lock.lock().unwrap().writer_dropped = true;
                changed.notify_all();
            }
        }
        struct Release(GateState);
        impl Release {
            fn now(&self) {
                let (lock, changed) = &*self.0;
                lock.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .released = true;
                changed.notify_all();
            }
        }
        impl Drop for Release {
            fn drop(&mut self) {
                self.now();
            }
        }

        let now = Instant::now();
        let gate = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let release = Release(gate.clone());
        let sink = crate::output_worker::OutputWorker::new(
            Gate(gate.clone()),
            std::time::Duration::from_secs(3),
        )
        .unwrap();
        let actor = ActorLaunchOrder::new(
            ScriptLaunch {
                id: 8,
                stops: [Ok(())].into(),
                forces: Vec::new(),
                observations: VecDeque::new(),
                observed: 0,
                reaps: VecDeque::new(),
            },
            sink,
            Phase::ForceStopping(now + LIMITS.force),
            LIMITS,
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut actor = actor;
            let put = actor.order.sink.put(b"held-frame\n".to_vec(), now);
            let step = actor.order.sink.step(now);
            let busy = actor.order.sink.put(b"second\n".to_vec(), now);
            let cleanup = actor.cleanup_tick(now);
            let forces = actor.order.launch.forces;
            let _ = tx.send((put, step, busy, cleanup, forces));
        });
        let (lock, changed) = &*gate;
        let state = lock.lock().unwrap();
        let entered = changed
            .wait_timeout_while(state, std::time::Duration::from_secs(3), |state| {
                !state.entered
            })
            .map(|(state, timeout)| state.entered && !timeout.timed_out())
            .unwrap_or(false);
        let early = rx.recv_timeout(std::time::Duration::from_secs(2)).ok();
        release.now();
        let flushed = changed
            .wait_timeout_while(
                lock.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                std::time::Duration::from_secs(3),
                |state| !state.flushed,
            )
            .map(|(state, timeout)| state.flushed && !timeout.timed_out())
            .unwrap_or(false);
        let worker_dropped = changed
            .wait_timeout_while(
                lock.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                std::time::Duration::from_secs(3),
                |state| !state.writer_dropped,
            )
            .map(|(state, timeout)| state.writer_dropped && !timeout.timed_out())
            .unwrap_or(false);
        drop(release);
        assert!(entered, "writer never reached gate");
        assert!(
            early.is_some(),
            "actor watchdog expired before gate release"
        );
        let (put, step, busy, cleanup, forces) = early.unwrap();
        assert!(put.is_ok());
        assert_eq!(
            step,
            Ok(WriteStep::Pending),
            "output polling blocked behind writer"
        );
        assert_eq!(busy, Err(PutFrameError::Busy(b"second\n".to_vec())));
        assert_eq!(cleanup, Ok(TreeObservation::ConfirmedEmpty));
        assert_eq!(forces, vec![true]);
        assert!(flushed, "released worker did not flush before deadline");
        assert!(
            worker_dropped,
            "detached output worker did not exit before deadline"
        );
        assert_eq!(lock.lock().unwrap().bytes, b"held-frame\n");
    }

    #[test]
    fn graceful_observation_running_waits_and_exit_forces_before_reap() {
        let now = Instant::now();
        let mut running = actor([], [], vec![]);
        running.phase = Phase::GracefulStopping(now + LIMITS.graceful);
        assert_eq!(running.cleanup_tick(now), Ok(TreeObservation::Present));
        assert_eq!(running.order.launch.observed, 1);
        assert!(running.order.launch.forces.is_empty());
        assert!(matches!(running.phase, Phase::GracefulStopping(_)));

        let mut exited = actor([Ok(())], [Ok(TreeObservation::ConfirmedEmpty)], vec![]);
        exited.phase = Phase::GracefulStopping(now + LIMITS.graceful);
        exited
            .order
            .launch
            .observations
            .push_back(Ok(ExitObservation::Exited { code: Some(0) }));
        assert_eq!(
            exited.cleanup_tick(now),
            Ok(TreeObservation::ConfirmedEmpty)
        );
        assert_eq!(exited.order.launch.observed, 1);
        assert_eq!(exited.order.launch.forces, vec![true]);
        assert_eq!(exited.phase, Phase::Empty);
    }

    #[test]
    fn graceful_deadline_and_observation_or_force_errors_retry_safely() {
        let now = Instant::now();
        let mut exact = actor([Ok(())], [Ok(TreeObservation::ConfirmedEmpty)], vec![]);
        exact.phase = Phase::GracefulStopping(now + LIMITS.graceful);
        assert_eq!(
            exact.cleanup_tick(now + LIMITS.graceful),
            Ok(TreeObservation::ConfirmedEmpty)
        );
        assert_eq!(exact.order.launch.observed, 0);
        assert_eq!(exact.order.launch.forces, vec![true]);

        let mut retry = actor(
            [
                io_error(io::ErrorKind::BrokenPipe),
                io_error(io::ErrorKind::WouldBlock),
                Ok(()),
            ],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Ok(WriteStep::Complete)],
        );
        retry.phase = Phase::GracefulStopping(now + LIMITS.graceful);
        retry
            .order
            .launch
            .observations
            .push_back(Ok(ExitObservation::Exited { code: Some(41) }));
        assert_eq!(
            retry.cleanup_tick(now),
            Err(ActorLaunchOrderError::Stop(io::ErrorKind::BrokenPipe))
        );
        assert!(matches!(retry.phase, Phase::ForceStopping(_)));
        assert!(!retry.force_ok);
        assert!(retry.pending_exit.is_some());
        assert_eq!(retry.cleanup_tick(now), Ok(TreeObservation::Present));
        assert_eq!(retry.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
        assert_eq!(retry.phase, Phase::Empty);
        assert_eq!(retry.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(retry.output_step(now), Ok(WriteStep::Complete));
        assert!(matches!(
            decode_event(&retry.order.sink.frames[0]),
            Ok(Event::Exit { code: Some(41), .. })
        ));
        assert_eq!(retry.order.sink.frames.len(), 1);
        assert_eq!(retry.order.launch.forces, vec![true, true, true]);
        assert_eq!(retry.order.launch.observed, 1);

        let mut observe_error = actor([Ok(())], [Ok(TreeObservation::ConfirmedEmpty)], vec![]);
        observe_error.phase = Phase::GracefulStopping(now + LIMITS.graceful);
        observe_error
            .order
            .launch
            .observations
            .push_back(io_error(io::ErrorKind::Interrupted));
        assert_eq!(
            observe_error.cleanup_tick(now),
            Err(ActorLaunchOrderError::Observe(io::ErrorKind::Interrupted))
        );
        assert!(matches!(observe_error.phase, Phase::GracefulStopping(_)));
        assert!(observe_error.order.launch.forces.is_empty());
        assert_eq!(
            observe_error.cleanup_tick(now + LIMITS.graceful),
            Ok(TreeObservation::ConfirmedEmpty)
        );
        assert_eq!(observe_error.order.launch.observed, 1);
        assert_eq!(observe_error.order.launch.forces, vec![true]);
    }

    #[test]
    fn empty_is_idempotent_and_unconfirmed_accepts_late_empty() {
        let now = Instant::now();
        let mut empty = actor([], [], vec![]);
        empty.phase = Phase::Empty;
        assert_eq!(empty.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
        assert_eq!(empty.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
        assert!(empty.order.launch.forces.is_empty());

        let mut late = actor(
            [Ok(())],
            [
                Ok(TreeObservation::Unconfirmed),
                Ok(TreeObservation::ConfirmedEmpty),
            ],
            vec![],
        );
        late.phase = Phase::Unconfirmed;
        assert_eq!(late.cleanup_tick(now), Ok(TreeObservation::Unconfirmed));
        assert_eq!(late.phase, Phase::Unconfirmed);
        assert_eq!(late.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
        assert_eq!(late.phase, Phase::Empty);
        assert_eq!(late.order.launch.forces, vec![true]);
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
        assert!(running.take_ready().is_some());

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

    #[test]
    fn runtime_tick_keeps_absolute_ready_deadline_and_reaps_one_exit() {
        let now = Instant::now();
        let deadline = now + LIMITS.ready;
        let mut late = actor([Ok(())], [], vec![]);
        late.phase = Phase::AwaitReady(deadline);
        assert_eq!(late.tick(deadline), Ok(None));
        assert_eq!(late.phase(), Phase::ForceStopping(deadline + LIMITS.force));
        assert_eq!(late.order.launch.forces, vec![true]);

        let grace_deadline = now + LIMITS.graceful;
        let mut graceful = actor([], [], vec![]);
        graceful.phase = Phase::GracefulStopping(grace_deadline);
        assert_eq!(graceful.tick(now), Ok(None));
        assert_eq!(graceful.phase(), Phase::GracefulStopping(grace_deadline));
        assert!(graceful.order.launch.forces.is_empty());
        let mut empty = actor([], [], vec![]);
        empty.phase = Phase::Empty;
        assert_eq!(empty.tick(now), Ok(None));
        assert_eq!(empty.phase(), Phase::Empty);

        let mut exited = actor(
            [Ok(())],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Ok(WriteStep::Complete)],
        );
        exited.phase = Phase::Running;
        exited
            .order
            .launch
            .observations
            .push_back(Ok(ExitObservation::Exited { code: Some(9) }));
        assert_eq!(exited.tick(now), Ok(None));
        assert!(!exited.control_permit_allowed());
        assert!(!exited.completed_request_dispatch_allowed());
        assert_eq!(exited.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(exited.output_step(now), Ok(WriteStep::Complete));
        assert!(exited.control_permit_allowed());
        assert_eq!(exited.order.launch.observed, 1);
        assert_eq!(
            exited.cleanup_tick(now),
            Ok(TreeObservation::ConfirmedEmpty)
        );
        assert_eq!(exited.order.launch.observed, 1);
    }

    #[test]
    fn runtime_tick_preserves_exit_across_force_retry() {
        let now = Instant::now();
        let mut exited = actor(
            [io_error(io::ErrorKind::BrokenPipe), Ok(())],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Ok(WriteStep::Complete)],
        );
        exited.phase = Phase::Running;
        exited
            .order
            .launch
            .observations
            .push_back(Ok(ExitObservation::Exited { code: Some(9) }));

        assert_eq!(
            exited.tick(now),
            Err(ActorLaunchOrderError::Stop(io::ErrorKind::BrokenPipe))
        );
        assert!(matches!(exited.phase(), Phase::ForceStopping(_)));
        assert_eq!(
            exited.cleanup_tick(now),
            Ok(TreeObservation::ConfirmedEmpty)
        );
        assert_eq!(exited.phase(), Phase::Empty);
        assert!(exited.outbox.exit_retained());
        assert_eq!(exited.tick(now), Ok(None));
        assert!(!exited.control_permit_allowed());
        assert_eq!(exited.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(exited.output_step(now), Ok(WriteStep::Complete));
        assert!(exited.control_permit_allowed());
        assert_eq!(exited.order.launch.observed, 1);
        assert_eq!(exited.order.launch.forces, vec![true, true]);
    }

    #[test]
    fn occupied_exit_retains_observation_until_empty_handoff_once() {
        let now = Instant::now();
        let mut actor = actor(
            [Ok(())],
            [],
            vec![Ok(WriteStep::Complete), Ok(WriteStep::Complete)],
        );
        actor.phase = Phase::Running;
        actor
            .order
            .launch
            .observations
            .push_back(Ok(ExitObservation::Exited { code: Some(9) }));
        actor
            .outbox
            .exit(&Event::Exit {
                protocol: 1,
                code: Some(8),
            })
            .unwrap();
        assert_eq!(actor.tick(now), Ok(None));
        assert!(actor.pending_exit.is_some());
        assert!(!actor.control_permit_allowed());
        assert_eq!(actor.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(actor.output_step(now), Ok(WriteStep::Complete));
        assert_eq!(actor.tick(now), Ok(None));
        assert_eq!(actor.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(actor.output_step(now), Ok(WriteStep::Complete));
        assert_eq!(actor.order.sink.put_now.len(), 2);
        assert!(actor.control_permit_allowed());
    }

    #[test]
    fn queued_ready_survives_exit_with_partial_reply_ready_and_closed_gates() {
        let now = Instant::now();
        let mut ready = actor(
            [Ok(())],
            [],
            vec![
                Ok(WriteStep::Complete),
                Ok(WriteStep::Pending),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Pending),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
            ],
        );
        queue_ready(&mut ready, now);
        ready
            .queue_reply(&Reply::Error {
                version: 1,
                request_id: 7,
                code: ErrorCode::LaunchFailed,
            })
            .unwrap();
        assert_eq!(ready.output_step(now), Ok(WriteStep::Complete));
        assert_eq!(ready.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(ready.output_step(now), Ok(WriteStep::Pending));
        ready
            .order
            .launch
            .observations
            .push_back(Ok(ExitObservation::Exited { code: Some(9) }));
        assert_eq!(ready.tick(now), Ok(None));
        assert!(!ready.control_permit_allowed() && !ready.completed_request_dispatch_allowed());
        assert_eq!(ready.output_step(now), Ok(WriteStep::Complete));
        assert_eq!(ready.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(ready.output_step(now), Ok(WriteStep::Pending));
        assert!(!ready.control_permit_allowed() && !ready.completed_request_dispatch_allowed());
        assert_eq!(ready.output_step(now), Ok(WriteStep::Complete));
        assert!(!ready.control_permit_allowed() && !ready.completed_request_dispatch_allowed());
        assert_eq!(ready.output_step(now), Ok(WriteStep::Pending));
        assert!(!ready.control_permit_allowed() && !ready.completed_request_dispatch_allowed());
        assert_eq!(ready.output_step(now), Ok(WriteStep::Complete));
        assert!(ready.control_permit_allowed());
        assert!(ready.completed_request_dispatch_allowed());
        assert!(matches!(
            decode_reply(&ready.order.sink.frames[1]),
            Ok(Reply::Error {
                code: ErrorCode::LaunchFailed,
                ..
            })
        ));
        assert!(matches!(
            decode_event(&ready.order.sink.frames[2]),
            Ok(Event::Ready { .. })
        ));
        assert!(matches!(
            decode_event(&ready.order.sink.frames[3]),
            Ok(Event::Exit { code: Some(9), .. })
        ));
    }

    #[test]
    fn held_ready_completed_during_shutdown_does_not_fail_output() {
        let now = Instant::now();
        let mut actor = actor(
            [Ok(())],
            [],
            vec![Ok(WriteStep::Pending), Ok(WriteStep::Complete)],
        );
        queue_ready(&mut actor, now);
        assert_eq!(actor.output_step(now), Ok(WriteStep::Pending));

        assert_eq!(actor.tick(now + LIMITS.ready), Ok(None));
        assert!(matches!(actor.phase(), Phase::ForceStopping(_)));
        assert_eq!(
            actor.output_step(now + LIMITS.ready),
            Ok(WriteStep::Complete)
        );
        assert!(actor.order.held_ready.is_none());
        assert!(actor.take_ready().is_none());
        assert!(actor.terminal.is_none());
    }

    #[test]
    fn runtime_transport_failure_forces_while_sink_is_pending() {
        let now = Instant::now();
        let mut actor = actor(
            [Ok(())],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Ok(WriteStep::Pending)],
        );
        actor.queue_owned(now).unwrap();
        assert_eq!(actor.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(
            actor.fail_transport(now),
            ActorLaunchOrderError::CleanupRequired
        );
        assert_eq!(actor.order.launch.forces, vec![true]);
        assert_eq!(actor.cleanup_tick(now), Ok(TreeObservation::ConfirmedEmpty));
    }

    #[test]
    fn output_health_is_independent_of_stopping_and_pre_owned_lifecycle() {
        let now = Instant::now();
        let mut failed_launch = actor([], [], vec![Ok(WriteStep::Complete)]);
        failed_launch
            .put_frame(b"launch-failed\n".to_vec(), now)
            .unwrap();
        assert_eq!(failed_launch.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(failed_launch.output_step(now), Ok(WriteStep::Complete));
        assert!(failed_launch.order.sink.frame.is_some());

        let mut stopped = actor([], [], vec![Ok(WriteStep::Complete)]);
        stopped.phase = Phase::Empty;
        stopped.order.stage = LaunchOrderStage::Ready;
        stopped.put_frame(b"exit\n".to_vec(), now).unwrap();
        assert_eq!(stopped.output_step(now), Ok(WriteStep::Pending));
        assert_eq!(stopped.output_step(now), Ok(WriteStep::Complete));
    }

    #[test]
    fn dispatcher_waits_for_owned_ack_before_consuming_a_completed_request() {
        let now = Instant::now();
        let mut actor = actor(
            [],
            [],
            vec![Ok(WriteStep::Complete), Ok(WriteStep::Complete)],
        );
        let mut request = Some(signal(1, true));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Blocked)
        );
        assert!(request.is_some());

        actor.queue_owned(now).unwrap();
        let mut empty = Some(is_empty(2));
        assert_eq!(
            actor.dispatch_completed(&mut empty, now),
            Ok(DispatchStep::Blocked)
        );
        assert!(empty.is_some());
        assert_eq!(actor.output_step(now), Ok(WriteStep::Complete));
        assert!(actor.completed_request_dispatch_allowed());
        assert_eq!(
            actor.dispatch_completed(&mut empty, now),
            Ok(DispatchStep::Replied)
        );
        flush_reply(&mut actor, now);
        assert!(matches!(
            decode_reply(&actor.order.sink.frames[0]),
            Ok(Reply::Owned { .. })
        ));
    }

    #[test]
    fn owned_dispatch_maps_signal_empty_and_launch_without_new_ownership() {
        let now = Instant::now();
        let mut actor = dispatcher(
            [Ok(())],
            vec![
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
                Ok(WriteStep::Complete),
            ],
        );

        let mut request = Some(signal(11, true));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        assert!(request.is_none());
        assert!(matches!(
            actor.schedule_state().phase,
            Phase::ForceStopping(_)
        ));
        flush_reply(&mut actor, now);
        assert!(matches!(
            decode_reply(&actor.order.sink.frames[0]),
            Ok(Reply::Result { request_id: 11, .. })
        ));

        let mut request = Some(is_empty(12));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        flush_reply(&mut actor, now);
        assert!(matches!(
            decode_reply(&actor.order.sink.frames[1]),
            Ok(Reply::Empty {
                request_id: 12,
                empty: false,
                ..
            })
        ));

        let mut request = Some(Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: 13,
            executable: "/ignored".into(),
            cwd: "/".into(),
            argv: vec![],
            env: BTreeMap::new(),
        });
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        flush_reply(&mut actor, now);
        assert!(matches!(
            decode_reply(&actor.order.sink.frames[2]),
            Ok(Reply::Error {
                request_id: 13,
                code: ErrorCode::InvalidRequest,
                ..
            })
        ));

        actor.phase = Phase::Empty;
        let mut request = Some(is_empty(14));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        flush_reply(&mut actor, now);
        assert!(matches!(
            decode_reply(&actor.order.sink.frames[3]),
            Ok(Reply::Empty {
                request_id: 14,
                empty: true,
                ..
            })
        ));
        assert_eq!(actor.order.launch.forces, vec![true]);
    }

    #[test]
    fn draining_deadline_signal_is_static_then_maintenance_can_confirm_empty() {
        let now = Instant::now();
        let mut actor = actor(
            [Ok(())],
            [Ok(TreeObservation::ConfirmedEmpty)],
            vec![Ok(WriteStep::Complete)],
        );
        actor.phase = Phase::Draining(now);
        actor.order.stage = LaunchOrderStage::Ready;
        let mut request = Some(signal(31, true));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        assert_eq!(actor.phase(), Phase::Unconfirmed);
        flush_reply(&mut actor, now);
        assert!(matches!(
            decode_reply(&actor.order.sink.frames[0]),
            Ok(Reply::Error {
                request_id: 31,
                code: ErrorCode::SignalFailed,
                ..
            })
        ));
        assert!(!actor.schedule_state().terminal);
        assert_eq!(actor.maintenance(now), Ok(()));
        assert_eq!(actor.phase(), Phase::Empty);
        assert_eq!(actor.order.launch.forces, vec![true]);
    }

    #[test]
    fn graceful_and_duplicate_successful_force_signals_are_nonblocking() {
        let now = Instant::now();
        let mut graceful = dispatcher([Ok(())], vec![Ok(WriteStep::Complete)]);
        let mut request = Some(signal(41, false));
        assert_eq!(
            graceful.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        assert!(matches!(graceful.phase(), Phase::GracefulStopping(_)));
        assert_eq!(graceful.order.launch.forces, vec![false]);

        let mut forced = dispatcher(
            [Ok(())],
            vec![Ok(WriteStep::Complete), Ok(WriteStep::Complete)],
        );
        let mut request = Some(signal(42, true));
        forced.dispatch_completed(&mut request, now).unwrap();
        flush_reply(&mut forced, now);
        let mut duplicate = Some(signal(43, true));
        assert_eq!(
            forced.dispatch_completed(&mut duplicate, now),
            Ok(DispatchStep::Replied)
        );
        assert_eq!(forced.order.launch.forces, vec![true]);
    }

    #[test]
    fn dispatcher_leaves_one_completed_request_outside_while_output_is_occupied() {
        let now = Instant::now();
        let mut actor = dispatcher([], vec![Ok(WriteStep::Complete), Ok(WriteStep::Complete)]);
        actor
            .queue_reply(&Reply::Result {
                version: PROTOCOL_VERSION,
                request_id: 1,
            })
            .unwrap();
        let mut request = Some(is_empty(2));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Blocked)
        );
        assert!(request.is_some());
        assert!(actor.schedule_state().output_pending);
        assert!(!actor.completed_request_dispatch_allowed());
        flush_reply(&mut actor, now);
        assert!(actor.completed_request_dispatch_allowed());
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        assert!(request.is_none());
    }

    #[test]
    fn dispatcher_gates_pending_and_inflight_exit_and_retains_exit_through_empty() {
        let now = Instant::now();
        let exit = Event::Exit {
            protocol: PROTOCOL_VERSION,
            code: Some(7),
        };
        let mut pending = dispatcher([], vec![]);
        pending.pending_exit = Some(exit.clone());
        let mut request = Some(is_empty(1));
        assert_eq!(
            pending.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Blocked)
        );
        assert!(request.is_some());

        let mut inflight = dispatcher([], vec![]);
        inflight.outbox.exit(&exit).unwrap();
        assert_eq!(inflight.output_step(now), Ok(WriteStep::Pending));
        let mut request = Some(is_empty(2));
        assert_eq!(
            inflight.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Blocked)
        );
        assert!(inflight.schedule_state().exit_retained);

        let mut empty = dispatcher([], vec![]);
        empty.phase = Phase::Empty;
        empty.force_ok = true;
        empty.pending_exit = Some(exit);
        assert_eq!(empty.maintenance(now), Ok(()));
        assert_eq!(empty.schedule_state().phase, Phase::Empty);
        assert!(empty.schedule_state().exit_retained);
    }

    #[test]
    fn maintenance_uses_one_force_path_and_duplicate_signal_does_not_repeat_force() {
        let now = Instant::now();
        let mut actor = dispatcher(
            [io_error(io::ErrorKind::WouldBlock)],
            vec![Ok(WriteStep::Complete)],
        );
        actor.phase = Phase::ForceStopping(now + LIMITS.force);
        assert_eq!(actor.maintenance(now), Ok(()));
        assert_eq!(actor.order.launch.forces, vec![true]);
        assert_eq!(actor.order.launch.reaps.len(), 0);

        let mut request = Some(signal(3, true));
        assert_eq!(
            actor.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        assert!(request.is_none());
        assert_eq!(actor.order.launch.forces, vec![true]);
    }

    #[test]
    fn recoverable_signal_failure_is_static_and_fatal_failure_never_replies() {
        let now = Instant::now();
        let mut recoverable = dispatcher(
            [io_error(io::ErrorKind::WouldBlock)],
            vec![Ok(WriteStep::Complete)],
        );
        let mut request = Some(signal(41, true));
        assert_eq!(
            recoverable.dispatch_completed(&mut request, now),
            Ok(DispatchStep::Replied)
        );
        flush_reply(&mut recoverable, now);
        assert!(matches!(
            decode_reply(&recoverable.order.sink.frames[0]),
            Ok(Reply::Error {
                code: ErrorCode::SignalFailed,
                request_id: 41,
                ..
            })
        ));
        assert_eq!(recoverable.order.launch.forces, vec![true]);

        let mut fatal = dispatcher([io_error(io::ErrorKind::BrokenPipe), Ok(())], vec![]);
        let mut request = Some(signal(42, true));
        assert_eq!(
            fatal.dispatch_completed(&mut request, now),
            Err(ActorLaunchOrderError::Stop(io::ErrorKind::BrokenPipe))
        );
        assert!(fatal.schedule_state().terminal);
        assert!(fatal.order.sink.frames.is_empty());
        assert_eq!(fatal.maintenance(now), Ok(()));
        assert!(fatal.order.sink.frames.is_empty());
    }

    fn force_and_reap(launch: &mut OwnedLaunch) -> io::Result<()> {
        use std::time::Duration;

        let force = LaunchControl::stop(launch, true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut phase = Phase::ForceStopping(Instant::now() + Duration::from_secs(10));
        let reap = loop {
            match LaunchControl::cleanup(launch, &mut phase) {
                Ok(TreeObservation::ConfirmedEmpty) => break Ok(()),
                Ok(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Ok(_) => {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "reap deadline expired",
                    ));
                }
                Err(CleanupStepError::Reap(error)) => break Err(error),
                Err(CleanupStepError::Phase(_)) => break Err(io::Error::other("invalid phase")),
            }
        };
        force?;
        reap
    }

    #[cfg(unix)]
    fn start_bounded_read<const N: usize, R>(
        mut reader: R,
    ) -> std::sync::mpsc::Receiver<(io::Result<[u8; N]>, R)>
    where
        R: std::io::Read + Send + 'static,
    {
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = [0; N];
            let result = reader.read_exact(&mut bytes).map(|()| bytes);
            let _ = send.send((result, reader));
        });
        receive
    }

    #[cfg(unix)]
    fn finish_bounded_read<const N: usize, R>(
        receive: std::sync::mpsc::Receiver<(io::Result<[u8; N]>, R)>,
    ) -> io::Result<([u8; N], R)> {
        let (result, reader) = receive
            .recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "child marker deadline expired")
            })?;
        result.map(|bytes| (bytes, reader))
    }

    #[cfg(unix)]
    #[test]
    fn owned_soft_stop_closes_stdin_twice_then_force_reaps_same_owner() {
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
        let (stdout_pipe, stderr_pipe) = {
            let pipes = launch.pipes_mut();
            let Some(stdout) = pipes.take_stdout() else {
                panic!("stdout moves once");
            };
            let Some(stderr) = pipes.take_stderr() else {
                panic!("stderr moves once");
            };
            (stdout, stderr)
        };
        let ready = finish_bounded_read(start_bounded_read::<6, _>(stdout_pipe));
        let first_stop = LaunchControl::stop(&mut launch, false);
        let second_stop = LaunchControl::stop(&mut launch, false);
        let stdin_closed = launch.pipes_mut().stdin_mut().is_none();
        let (ready, stdout_pipe) = match ready {
            Ok((ready, stdout_pipe)) => (Ok(ready), Some(stdout_pipe)),
            Err(error) => (Err(error), None),
        };
        let stdout = stdout_pipe.map(|pipe| finish_bounded_read(start_bounded_read::<8, _>(pipe)));
        let stderr = finish_bounded_read(start_bounded_read::<8, _>(stderr_pipe));
        let cleanup = force_and_reap(&mut launch);
        drop(launch);
        crate::posix::tests::wait_for_reaper_idle();
        cleanup.unwrap();
        first_stop.unwrap();
        second_stop.unwrap();
        assert_eq!(&ready.unwrap(), b"ready!");
        assert!(stdin_closed, "soft stop left stdin open");
        assert_eq!(&stdout.unwrap().unwrap().0, b"out-eof!");
        assert_eq!(&stderr.unwrap().0, b"err-eof!");
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
        let (mut stdout_pipe, mut stderr_pipe) = {
            let pipes = launch.pipes_mut();
            let Some(stdout) = pipes.take_stdout() else {
                panic!("stdout moves once");
            };
            let Some(stderr) = pipes.take_stderr() else {
                panic!("stderr moves once");
            };
            (stdout, stderr)
        };
        let first_stop = LaunchControl::stop(&mut launch, false);
        let second_stop = LaunchControl::stop(&mut launch, false);
        let stdin_closed = launch.pipes_mut().stdin_mut().is_none();
        let mut out = [0; 8];
        let stdout = tokio::time::timeout(Duration::from_secs(3), stdout_pipe.read_exact(&mut out))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "child stdout deadline expired"))
            .and_then(|result| result.map(|_| out));
        let mut err = [0; 8];
        let stderr = tokio::time::timeout(Duration::from_secs(3), stderr_pipe.read_exact(&mut err))
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
