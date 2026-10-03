use std::io;

use crate::{
    actor::{ActorError, Phase},
    target::{OwnedTarget, TreeObservation},
};

#[derive(Debug)]
pub(crate) enum CleanupStepError {
    Phase(ActorError),
    Reap(io::Error),
}

fn cleanup_step_with<T, F>(
    phase: &mut Phase,
    target: &mut T,
    reap: F,
) -> Result<TreeObservation, CleanupStepError>
where
    F: FnOnce(&mut T) -> io::Result<TreeObservation>,
{
    let empty = phase.observe_empty().map_err(CleanupStepError::Phase)?;
    let observation = reap(target).map_err(CleanupStepError::Reap)?;
    if observation == TreeObservation::ConfirmedEmpty {
        *phase = empty;
    }
    Ok(observation)
}

pub(crate) fn cleanup_step(
    phase: &mut Phase,
    target: &mut OwnedTarget,
) -> Result<TreeObservation, CleanupStepError> {
    cleanup_step_with(phase, target, |target| target.reap_step())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    struct ScriptedTarget {
        results: VecDeque<io::Result<TreeObservation>>,
        attempts: usize,
    }

    impl ScriptedTarget {
        fn new(results: impl IntoIterator<Item = io::Result<TreeObservation>>) -> Self {
            Self {
                results: results.into_iter().collect(),
                attempts: 0,
            }
        }

        fn reap_step(&mut self) -> io::Result<TreeObservation> {
            self.attempts += 1;
            self.results
                .pop_front()
                .expect("scripted result for every cleanup attempt")
        }
    }

    fn run_script(
        phase: &mut Phase,
        target: &mut ScriptedTarget,
    ) -> Result<TreeObservation, CleanupStepError> {
        cleanup_step_with(phase, target, ScriptedTarget::reap_step)
    }

    #[test]
    fn observations_preserve_phase_until_confirmed_empty() {
        let deadline = Instant::now() + Duration::from_secs(3);
        for expected in [
            Phase::ForceStopping(deadline),
            Phase::Draining(deadline),
            Phase::Unconfirmed,
        ] {
            for observation in [TreeObservation::Present, TreeObservation::Unconfirmed] {
                let mut phase = expected;
                let mut target = ScriptedTarget::new([Ok(observation)]);
                assert!(
                    matches!(run_script(&mut phase, &mut target), Ok(found) if found == observation)
                );
                assert_eq!(phase, expected);
                assert_eq!(target.attempts, 1);
            }
        }
    }

    #[test]
    fn confirmed_empty_promotes_every_applicable_phase_to_empty() {
        let deadline = Instant::now() + Duration::from_secs(3);
        for mut phase in [
            Phase::GracefulStopping(deadline),
            Phase::ForceStopping(deadline),
            Phase::Draining(deadline),
            Phase::Unconfirmed,
            Phase::Empty,
        ] {
            let mut target = ScriptedTarget::new([Ok(TreeObservation::ConfirmedEmpty)]);
            assert!(matches!(
                run_script(&mut phase, &mut target),
                Ok(TreeObservation::ConfirmedEmpty)
            ));
            assert_eq!(phase, Phase::Empty);
            assert_eq!(target.attempts, 1);
        }
    }

    #[test]
    fn reap_error_preserves_phase_and_retry_uses_same_target_once_each() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut phase = Phase::Draining(deadline);
        let mut target = ScriptedTarget::new([
            Err(io::Error::other("scripted reap failure")),
            Ok(TreeObservation::Present),
            Ok(TreeObservation::ConfirmedEmpty),
        ]);
        assert!(matches!(
            run_script(&mut phase, &mut target),
            Err(CleanupStepError::Reap(_))
        ));
        assert_eq!(phase, Phase::Draining(deadline));
        assert_eq!(target.attempts, 1);
        assert!(matches!(
            run_script(&mut phase, &mut target),
            Ok(TreeObservation::Present)
        ));
        assert_eq!(phase, Phase::Draining(deadline));
        assert_eq!(target.attempts, 2);
        assert!(matches!(
            run_script(&mut phase, &mut target),
            Ok(TreeObservation::ConfirmedEmpty)
        ));
        assert_eq!(phase, Phase::Empty);
        assert_eq!(target.attempts, 3);
    }

    #[test]
    fn invalid_phase_does_not_attempt_reap() {
        let mut phase = Phase::AwaitLaunch;
        let mut target = ScriptedTarget::new([Ok(TreeObservation::Present)]);
        assert!(matches!(
            run_script(&mut phase, &mut target),
            Err(CleanupStepError::Phase(ActorError::InvalidTransition))
        ));
        assert_eq!(target.attempts, 0);
    }

    #[test]
    fn repeated_confirmed_empty_reuses_target_and_stays_empty() {
        let mut phase = Phase::Unconfirmed;
        let mut target = ScriptedTarget::new([
            Ok(TreeObservation::ConfirmedEmpty),
            Ok(TreeObservation::ConfirmedEmpty),
        ]);
        assert!(matches!(
            run_script(&mut phase, &mut target),
            Ok(TreeObservation::ConfirmedEmpty)
        ));
        assert_eq!(phase, Phase::Empty);
        assert_eq!(target.attempts, 1);
        assert!(matches!(
            run_script(&mut phase, &mut target),
            Ok(TreeObservation::ConfirmedEmpty)
        ));
        assert_eq!(phase, Phase::Empty);
        assert_eq!(target.attempts, 2);
    }
}
