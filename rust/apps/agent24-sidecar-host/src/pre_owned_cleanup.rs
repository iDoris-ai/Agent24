use crate::{
    actor::{Deadlines, Phase},
    cleanup::{CleanupStepError, cleanup_step},
    pipe_access::TargetPipes,
    target::{ExitObservation, OwnedTarget, TreeObservation},
};
use std::io;
pub(crate) trait CleanupTarget {
    fn observe_exit(&mut self) -> io::Result<ExitObservation>;
    fn stop(&mut self) -> io::Result<()>;
    fn reap(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError>;
}
pub(crate) trait IntoCleanupOwner {
    type Owner: CleanupTarget;
    fn into_cleanup_owner(self) -> Self::Owner;
}
pub(crate) struct CleanupOwner {
    target: OwnedTarget,
    pipes: Option<TargetPipes>,
}
impl CleanupOwner {
    pub(crate) fn new(target: OwnedTarget, pipes: Option<TargetPipes>) -> Self {
        Self { target, pipes }
    }
}
impl CleanupTarget for CleanupOwner {
    fn observe_exit(&mut self) -> io::Result<ExitObservation> {
        self.target.observe_exit()
    }
    fn stop(&mut self) -> io::Result<()> {
        self.target.request_stop(true)
    }
    fn reap(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError> {
        cleanup_step(phase, &mut self.target)
    }
}
impl IntoCleanupOwner for crate::launch::OwnedLaunch {
    type Owner = CleanupOwner;
    fn into_cleanup_owner(self) -> CleanupOwner {
        let (target, pipes) = self.into_cleanup_parts();
        CleanupOwner::new(target, Some(pipes))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreOwnedCleanupError {
    Stop(io::ErrorKind),
    Observe(io::ErrorKind),
    Reap(io::ErrorKind),
    InvalidPhase,
}
pub(crate) struct PreOwnedCleanup<L = CleanupOwner> {
    owner: L,
    phase: Phase,
    limits: Deadlines,
    force_ok: bool,
}
impl<L: CleanupTarget> PreOwnedCleanup<L> {
    pub(crate) fn new(owner: L, phase: Phase, limits: Deadlines, force_ok: bool) -> Self {
        Self {
            owner,
            phase,
            limits,
            force_ok,
        }
    }
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }
    #[cfg(test)]
    pub(crate) fn owner(&self) -> &L {
        &self.owner
    }
    pub(crate) fn step(
        &mut self,
        now: std::time::Instant,
    ) -> Result<TreeObservation, PreOwnedCleanupError> {
        if matches!(self.phase, Phase::Empty) {
            return Ok(TreeObservation::ConfirmedEmpty);
        }
        if matches!(
            self.phase,
            Phase::AwaitLaunch | Phase::Launching(_) | Phase::AwaitReady(_) | Phase::Running
        ) {
            self.phase = self
                .phase
                .stop(true, now, self.limits)
                .map_err(|_| PreOwnedCleanupError::InvalidPhase)?;
        }
        self.phase = self.phase.advance(now, self.limits);
        if matches!(self.phase, Phase::GracefulStopping(_)) {
            match self
                .owner
                .observe_exit()
                .map_err(|e| PreOwnedCleanupError::Observe(e.kind()))?
            {
                ExitObservation::Running => return Ok(TreeObservation::Present),
                ExitObservation::Exited { .. } => {
                    self.phase = self
                        .phase
                        .stop(true, now, self.limits)
                        .map_err(|_| PreOwnedCleanupError::InvalidPhase)?
                }
            }
        }
        if !self.force_ok {
            self.owner
                .stop()
                .map_err(|e| PreOwnedCleanupError::Stop(e.kind()))?;
            self.force_ok = true;
        }
        let observation = self.owner.reap(&mut self.phase).map_err(|e| match e {
            CleanupStepError::Reap(error) => PreOwnedCleanupError::Reap(error.kind()),
            CleanupStepError::Phase(_) => PreOwnedCleanupError::InvalidPhase,
        })?;
        if observation == TreeObservation::ConfirmedEmpty {
            self.phase = Phase::Empty;
        }
        Ok(observation)
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        time::{Duration, Instant},
    };
    const LIMITS: Deadlines = Deadlines {
        launch: Duration::from_secs(2),
        ready: Duration::from_secs(3),
        graceful: Duration::from_secs(1),
        force: Duration::from_secs(4),
        drain: Duration::from_secs(5),
    };
    #[derive(Default)]
    struct Fake {
        stops: VecDeque<io::Result<()>>,
        observations: VecDeque<io::Result<ExitObservation>>,
        trees: VecDeque<io::Result<TreeObservation>>,
    }
    impl CleanupTarget for Fake {
        fn observe_exit(&mut self) -> io::Result<ExitObservation> {
            self.observations.pop_front().unwrap()
        }
        fn stop(&mut self) -> io::Result<()> {
            self.stops.pop_front().unwrap()
        }
        fn reap(&mut self, phase: &mut Phase) -> Result<TreeObservation, CleanupStepError> {
            let empty = phase.observe_empty().map_err(CleanupStepError::Phase)?;
            self.trees
                .pop_front()
                .unwrap()
                .inspect(|tree| {
                    if *tree == TreeObservation::ConfirmedEmpty {
                        *phase = empty;
                    }
                })
                .map_err(CleanupStepError::Reap)
        }
    }
    fn active(phase: Phase) -> PreOwnedCleanup<Fake> {
        let mut owner = Fake::default();
        owner.stops.push_back(Ok(()));
        owner.trees.push_back(Ok(TreeObservation::ConfirmedEmpty));
        PreOwnedCleanup::new(owner, phase, LIMITS, false)
    }
    macro_rules! expect_error {
        ($cleanup:expr, $variant:ident, $kind:ident) => {
            assert_eq!(
                $cleanup.step(Instant::now()),
                Err(PreOwnedCleanupError::$variant(io::ErrorKind::$kind))
            );
        };
    }
    fn expect_tree<L: CleanupTarget>(
        cleanup: &mut PreOwnedCleanup<L>,
        now: Instant,
        tree: TreeObservation,
    ) {
        assert_eq!(cleanup.step(now), Ok(tree));
    }
    #[test]
    fn force_retries_errors_and_success_preserve_deadline_until_success() {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut owner = Fake::default();
        owner.stops.extend([
            Err(io::ErrorKind::WouldBlock.into()),
            Err(io::ErrorKind::PermissionDenied.into()),
            Ok(()),
        ]);
        owner.trees.push_back(Ok(TreeObservation::Present));
        let mut cleanup =
            PreOwnedCleanup::new(owner, Phase::ForceStopping(deadline), LIMITS, false);
        expect_error!(&mut cleanup, Stop, WouldBlock);
        expect_error!(&mut cleanup, Stop, PermissionDenied);
        assert_eq!(cleanup.step(Instant::now()), Ok(TreeObservation::Present));
        assert_eq!(cleanup.phase(), Phase::ForceStopping(deadline));
        let mut active = active(Phase::AwaitReady(deadline));
        let now = Instant::now();
        assert_eq!(active.step(now), Ok(TreeObservation::ConfirmedEmpty));
        active.phase = Phase::Running;
        active.force_ok = false;
        active.owner.stops.push_back(Ok(()));
        active
            .owner
            .trees
            .push_back(Ok(TreeObservation::ConfirmedEmpty));
        assert_eq!(active.step(now), Ok(TreeObservation::ConfirmedEmpty));
    }
    #[test]
    fn reap_retries_preserve_owner_until_idempotent_confirmed_empty() {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut owner = Fake::default();
        owner
            .observations
            .push_back(Err(io::ErrorKind::Other.into()));
        owner.stops.push_back(Ok(()));
        owner.trees.extend([
            Err(io::ErrorKind::Other.into()),
            Ok(TreeObservation::Present),
            Ok(TreeObservation::Unconfirmed),
            Ok(TreeObservation::ConfirmedEmpty),
        ]);
        let mut cleanup =
            PreOwnedCleanup::new(owner, Phase::GracefulStopping(deadline), LIMITS, false);
        expect_error!(&mut cleanup, Observe, Other);
        assert_eq!(cleanup.phase(), Phase::GracefulStopping(deadline));
        cleanup.phase = Phase::ForceStopping(deadline);
        expect_error!(&mut cleanup, Reap, Other);
        expect_tree(&mut cleanup, deadline, TreeObservation::Present);
        expect_tree(
            &mut cleanup,
            deadline + LIMITS.drain,
            TreeObservation::Unconfirmed,
        );
        assert_eq!(cleanup.phase(), Phase::Unconfirmed);
        expect_tree(
            &mut cleanup,
            deadline + LIMITS.drain,
            TreeObservation::ConfirmedEmpty,
        );
        expect_tree(
            &mut cleanup,
            deadline + LIMITS.drain,
            TreeObservation::ConfirmedEmpty,
        );
    }
}
