//! Pure single-generation actor state policy. No process or pipe authority lives here.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    AwaitLaunch,
    Launching(Instant),
    AwaitReady(Instant),
    Running,
    GracefulStopping(Instant),
    ForceStopping(Instant),
    Draining(Instant),
    Empty,
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActorError {
    InvalidTransition,
    Unconfirmed,
}

impl ActorError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidTransition => "invalid_transition",
            Self::Unconfirmed => "unconfirmed",
        }
    }

    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::InvalidTransition => "operation_not_allowed",
            Self::Unconfirmed => "tree_not_confirmed_empty",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadlines {
    pub(crate) launch: Duration,
    pub(crate) ready: Duration,
    pub(crate) graceful: Duration,
    pub(crate) force: Duration,
    pub(crate) drain: Duration,
}

impl Phase {
    pub(crate) const fn initial() -> Self {
        Self::AwaitLaunch
    }

    pub(crate) fn launch(self, now: Instant, limits: Deadlines) -> Result<Self, ActorError> {
        match self {
            Self::AwaitLaunch => Ok(Self::Launching(now + limits.launch)),
            Self::Unconfirmed => Err(ActorError::Unconfirmed),
            _ => Err(ActorError::InvalidTransition),
        }
    }

    pub(crate) fn owned(self, now: Instant, limits: Deadlines) -> Result<Self, ActorError> {
        match self {
            Self::Launching(deadline) if now < deadline => Ok(Self::AwaitReady(now + limits.ready)),
            Self::Launching(_) => Ok(Self::ForceStopping(now + limits.force)),
            _ => Err(ActorError::InvalidTransition),
        }
    }

    pub(crate) fn ready(self, now: Instant, limits: Deadlines) -> Result<Self, ActorError> {
        match self {
            Self::AwaitReady(deadline) if now < deadline => Ok(Self::Running),
            Self::AwaitReady(_) => Ok(Self::ForceStopping(now + limits.force)),
            _ => Err(ActorError::InvalidTransition),
        }
    }

    pub(crate) fn stop(
        self,
        force: bool,
        now: Instant,
        limits: Deadlines,
    ) -> Result<Self, ActorError> {
        let next = if force {
            Self::ForceStopping(now + limits.force)
        } else {
            Self::GracefulStopping(now + limits.graceful)
        };
        match self {
            Self::Empty => Ok(Self::Empty),
            Self::Unconfirmed => Err(ActorError::Unconfirmed),
            Self::ForceStopping(_) | Self::Draining(_) => Ok(self),
            Self::GracefulStopping(deadline) if !force => Ok(Self::GracefulStopping(deadline)),
            Self::AwaitLaunch
            | Self::Launching(_)
            | Self::AwaitReady(_)
            | Self::Running
            | Self::GracefulStopping(_) => Ok(next),
        }
    }

    pub(crate) fn advance(self, now: Instant, limits: Deadlines) -> Self {
        match self {
            Self::Launching(deadline)
            | Self::AwaitReady(deadline)
            | Self::GracefulStopping(deadline)
                if now >= deadline =>
            {
                Self::ForceStopping(now + limits.force)
            }
            Self::ForceStopping(deadline) if now >= deadline => Self::Draining(now + limits.drain),
            Self::Draining(deadline) if now >= deadline => Self::Unconfirmed,
            _ => self,
        }
    }

    pub(crate) fn observe_empty(self) -> Result<Self, ActorError> {
        match self {
            Self::GracefulStopping(_)
            | Self::ForceStopping(_)
            | Self::Draining(_)
            | Self::Unconfirmed
            | Self::Empty => Ok(Self::Empty),
            _ => Err(ActorError::InvalidTransition),
        }
    }

    pub(crate) const fn restart_allowed(self) -> bool {
        matches!(self, Self::Empty)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    const L: Deadlines = Deadlines {
        launch: Duration::from_secs(10),
        ready: Duration::from_secs(30),
        graceful: Duration::from_secs(5),
        force: Duration::from_secs(2),
        drain: Duration::from_secs(3),
    };

    #[test]
    fn state_policy_preserves_safety_invariants() {
        let now = Instant::now();
        let graceful = Phase::Running.stop(false, now, L).expect("graceful");
        let repeated = graceful
            .stop(false, now + Duration::from_secs(4), L)
            .expect("repeat");
        assert_eq!(graceful, repeated);
        let forced = repeated.stop(true, now, L).expect("force");
        assert!(matches!(forced, Phase::ForceStopping(_)));
        assert_eq!(forced.stop(false, now, L).expect("no downgrade"), forced);

        let stopping = Phase::Running.stop(true, now, L).expect("stop");
        let empty = stopping.observe_empty().expect("empty");
        assert_eq!(empty.observe_empty().expect("repeat"), Phase::Empty);
        assert_eq!(empty.stop(false, now, L).expect("tombstone"), Phase::Empty);
        assert!(empty.restart_allowed());

        let draining = Phase::Running
            .stop(true, now, L)
            .expect("stop")
            .advance(now + Duration::from_secs(2), L);
        let unconfirmed = draining.advance(now + Duration::from_secs(5), L);
        assert_eq!(unconfirmed, Phase::Unconfirmed);
        assert!(!unconfirmed.restart_allowed());
        assert_eq!(unconfirmed.launch(now, L), Err(ActorError::Unconfirmed));
    }

    #[test]
    fn owned_and_ready_deadlines_are_inclusive() {
        let now = Instant::now();
        let launching = Phase::initial().launch(now, L).expect("launch");
        let deadline = now + L.launch;
        let before = deadline - Duration::from_nanos(1);
        assert_eq!(
            launching.owned(before, L),
            Ok(Phase::AwaitReady(before + L.ready))
        );
        assert_eq!(
            launching.owned(deadline, L),
            Ok(Phase::ForceStopping(deadline + L.force))
        );
        let waiting = Phase::AwaitReady(deadline);
        assert_eq!(
            waiting.ready(deadline - Duration::from_nanos(1), L),
            Ok(Phase::Running)
        );
        assert_eq!(
            waiting.ready(deadline, L),
            Ok(Phase::ForceStopping(deadline + L.force))
        );
    }

    #[test]
    fn unconfirmed_observation_is_terminal_and_restart_safe() {
        let now = Instant::now();
        let force_deadline = now + L.force;
        let draining = Phase::Running
            .stop(true, now, L)
            .expect("force stop")
            .advance(force_deadline, L);
        let drain_deadline = force_deadline + L.drain;
        assert_eq!(draining, Phase::Draining(drain_deadline));

        let unconfirmed = draining.advance(drain_deadline, L);
        assert_eq!(unconfirmed, Phase::Unconfirmed);
        assert_eq!(
            unconfirmed.advance(drain_deadline + Duration::from_secs(60), L),
            Phase::Unconfirmed
        );
        assert!(!unconfirmed.restart_allowed());
        assert_eq!(unconfirmed.launch(now, L), Err(ActorError::Unconfirmed));
        assert_eq!(
            unconfirmed.stop(false, now, L),
            Err(ActorError::Unconfirmed)
        );
        assert_eq!(unconfirmed.stop(true, now, L), Err(ActorError::Unconfirmed));

        let empty = unconfirmed.observe_empty().expect("observed empty");
        assert_eq!(empty, Phase::Empty);
        assert_eq!(empty.observe_empty(), Ok(Phase::Empty));
        assert_eq!(empty.stop(false, now, L), Ok(Phase::Empty));
        assert_eq!(empty.launch(now, L), Err(ActorError::InvalidTransition));
    }
}
