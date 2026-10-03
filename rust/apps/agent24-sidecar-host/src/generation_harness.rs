//! Dormant turn boundary around one generation owner.
//! `Unconfirmed` means the owner must keep polling; `Empty` only means owner
//! cleanup is complete. A normal `Empty` tombstone still needs a `Continue`
//! step. A cancelled `Empty` generation uses N6's strict no-op force-cancel
//! step; deadlines are neither stored nor computed as `now + duration`.

use std::time::Instant;

use crate::{
    generation_driver::SessionEnd,
    launch_order::{ActorLaunchOrderError, ScheduleState},
    native_generation::NativeGeneration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnIntent {
    Continue,
    ForceCancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TurnReport {
    pub(crate) state: ScheduleState,
    pub(crate) session_end: Option<SessionEnd>,
    pub(crate) error: Option<ActorLaunchOrderError>,
    pub(crate) first_error: Option<ActorLaunchOrderError>,
}

trait GenerationTurn {
    fn step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError>;
    fn force_cancel_step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError>;
    fn schedule_state(&self) -> ScheduleState;
    fn session_end(&self) -> Option<SessionEnd>;
}

impl GenerationTurn for NativeGeneration<'_> {
    fn step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        NativeGeneration::step(self, now)
    }

    fn force_cancel_step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
        NativeGeneration::force_cancel_step(self, now)
    }

    fn schedule_state(&self) -> ScheduleState {
        NativeGeneration::schedule_state(self)
    }

    fn session_end(&self) -> Option<SessionEnd> {
        NativeGeneration::session_end(self)
    }
}

pub(crate) struct GenerationHarness<G> {
    generation: G,
    cancelled: bool,
    first_error: Option<ActorLaunchOrderError>,
}

#[allow(private_bounds)]
impl<G: GenerationTurn> GenerationHarness<G> {
    pub(crate) fn new(generation: G) -> Self {
        Self {
            generation,
            cancelled: false,
            first_error: None,
        }
    }

    pub(crate) fn generation(&self) -> &G {
        &self.generation
    }

    pub(crate) fn turn(&mut self, intent: TurnIntent, now: Instant) -> TurnReport {
        self.cancelled |= intent == TurnIntent::ForceCancel;
        let result = if self.cancelled {
            self.generation.force_cancel_step(now)
        } else {
            self.generation.step(now)
        };
        let error = result.err();
        if self.first_error.is_none() {
            self.first_error = error;
        }
        TurnReport {
            state: self.generation.schedule_state(),
            session_end: self.generation.session_end(),
            error,
            first_error: self.first_error,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::actor::Phase;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Calls {
        steps: Vec<Instant>,
        cancels: Vec<Instant>,
        drops: usize,
        identity: usize,
        state: Option<Phase>,
        errors: Vec<Option<ActorLaunchOrderError>>,
        session_end: Option<SessionEnd>,
    }

    struct Fake(Arc<Mutex<Calls>>);

    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.lock().unwrap().drops += 1;
        }
    }

    impl GenerationTurn for Fake {
        fn step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
            let mut calls = self.0.lock().unwrap();
            calls.steps.push(now);
            match calls.errors.pop().flatten() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn force_cancel_step(&mut self, now: Instant) -> Result<(), ActorLaunchOrderError> {
            let mut calls = self.0.lock().unwrap();
            calls.cancels.push(now);
            match calls.errors.pop().flatten() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn schedule_state(&self) -> ScheduleState {
            let calls = self.0.lock().unwrap();
            ScheduleState {
                phase: calls.state.unwrap_or(Phase::Running),
                terminal: false,
                output_pending: false,
                owned_acknowledged: false,
                exit_retained: false,
            }
        }

        fn session_end(&self) -> Option<SessionEnd> {
            self.0.lock().unwrap().session_end
        }
    }

    fn fake() -> (Fake, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls {
            identity: 17,
            ..Calls::default()
        }));
        (Fake(Arc::clone(&calls)), calls)
    }

    #[test]
    fn one_continue_turn_steps_once_and_keeps_owner() {
        let (fake, calls) = fake();
        let mut harness = GenerationHarness::new(fake);
        let identity = harness.generation().0.lock().unwrap().identity;
        let report = harness.turn(TurnIntent::Continue, Instant::now());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.steps.len(), 1);
        assert!(calls.cancels.is_empty());
        assert_eq!(calls.identity, identity);
        assert_eq!(calls.drops, 0);
        assert_eq!(report.error, None);
    }

    #[test]
    fn turn_report_forwards_session_end_fact() {
        let (fake, calls) = fake();
        calls.lock().unwrap().session_end = Some(SessionEnd::ParentEof);
        let mut harness = GenerationHarness::new(fake);
        let report = harness.turn(TurnIntent::Continue, Instant::now());
        assert_eq!(report.session_end, Some(SessionEnd::ParentEof));
    }

    #[test]
    fn cancellation_is_sticky_and_each_turn_calls_once() {
        let (fake, calls) = fake();
        let mut harness = GenerationHarness::new(fake);
        harness.turn(TurnIntent::ForceCancel, Instant::now());
        harness.turn(TurnIntent::Continue, Instant::now());
        let calls = calls.lock().unwrap();
        assert!(calls.steps.is_empty());
        assert_eq!(calls.cancels.len(), 2);
    }

    #[test]
    fn later_errors_do_not_replace_first_or_drop_owner() {
        let (fake, calls) = fake();
        calls.lock().unwrap().errors = vec![
            Some(ActorLaunchOrderError::Reap(std::io::ErrorKind::Interrupted)),
            Some(ActorLaunchOrderError::Stop(std::io::ErrorKind::BrokenPipe)),
        ];
        let mut harness = GenerationHarness::new(fake);
        let first = harness.turn(TurnIntent::Continue, Instant::now());
        let second = harness.turn(TurnIntent::Continue, Instant::now());
        assert_eq!(first.error, first.first_error);
        assert_eq!(
            second.error,
            Some(ActorLaunchOrderError::Reap(std::io::ErrorKind::Interrupted))
        );
        assert_eq!(second.first_error, first.first_error);
        assert_eq!(calls.lock().unwrap().drops, 0);
    }

    #[test]
    fn absolute_deadline_is_forwarded_unchanged() {
        let (fake, calls) = fake();
        let mut harness = GenerationHarness::new(fake);
        let now = Instant::now() + std::time::Duration::from_secs(9);
        harness.turn(TurnIntent::Continue, now);
        assert_eq!(calls.lock().unwrap().steps, vec![now]);
    }

    #[test]
    fn unconfirmed_to_empty_does_not_end_owner_turns_early() {
        let (fake, calls) = fake();
        calls.lock().unwrap().state = Some(Phase::Unconfirmed);
        let mut harness = GenerationHarness::new(fake);
        assert_eq!(
            harness
                .turn(TurnIntent::Continue, Instant::now())
                .state
                .phase,
            Phase::Unconfirmed
        );
        calls.lock().unwrap().state = Some(Phase::Empty);
        assert_eq!(
            harness
                .turn(TurnIntent::Continue, Instant::now())
                .state
                .phase,
            Phase::Empty
        );
        assert_eq!(calls.lock().unwrap().steps.len(), 2);
    }

    #[test]
    fn empty_continue_polls_and_cancelled_empty_uses_force_path() {
        let (fake, calls) = fake();
        calls.lock().unwrap().state = Some(Phase::Empty);
        let mut harness = GenerationHarness::new(fake);
        harness.turn(TurnIntent::Continue, Instant::now());
        harness.turn(TurnIntent::ForceCancel, Instant::now());
        harness.turn(TurnIntent::Continue, Instant::now());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.steps.len(), 1);
        assert_eq!(calls.cancels.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn native_generation_cancels_to_empty_through_harness() {
        use crate::{
            actor::Deadlines,
            control_worker::ControlWorker,
            launch::{LaunchIntent, OwnedLaunch},
            native_generation::NativeGeneration,
            output_worker::OutputWorker,
            worker_slots::WorkerSlots,
        };
        use agent24_sidecar_host_protocol::{PROTOCOL_VERSION, Request};
        use std::{collections::BTreeMap, io, thread, time::Duration};

        let _guard = crate::posix::tests::test_lock();
        let slots = WorkerSlots::isolated();
        let mut output = OutputWorker::new_in(slots, io::sink(), Duration::from_secs(2)).unwrap();
        let mut control = ControlWorker::new_in(slots, io::empty(), None).unwrap();
        let request = Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: 901,
            executable: "/bin/sh".into(),
            cwd: "/".into(),
            argv: vec!["-c".into(), "exec sleep 30".into()],
            env: BTreeMap::new(),
        };
        let limits = Deadlines {
            launch: Duration::from_secs(3),
            ready: Duration::from_secs(3),
            graceful: Duration::from_millis(1),
            force: Duration::from_secs(3),
            drain: Duration::from_secs(1),
        };
        let now = Instant::now();
        let generation = NativeGeneration::assemble_in(
            slots,
            OwnedLaunch::start(LaunchIntent::from_request(request).unwrap()).unwrap(),
            &mut output,
            &mut control,
            limits,
            now + limits.launch,
            || now,
        )
        .unwrap();
        let mut harness = GenerationHarness::new(generation);
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let report = harness.turn(TurnIntent::ForceCancel, Instant::now());
            if report.state.phase == Phase::Empty {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native owner did not confirm Empty"
            );
            thread::yield_now();
        }
        drop(harness);
        drop(output);
        crate::posix::tests::wait_for_reaper_idle();
    }
}
