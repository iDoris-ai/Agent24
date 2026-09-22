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
            Self::Launching(_) => Ok(Self::AwaitReady(now + limits.ready)),
            _ => Err(ActorError::InvalidTransition),
        }
    }

    pub(crate) fn ready(self) -> Result<Self, ActorError> {
        match self {
            Self::AwaitReady(_) => Ok(Self::Running),
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
            Self::Launching(deadline) | Self::AwaitReady(deadline) if now >= deadline => {
                Self::ForceStopping(now + limits.force)
            }
            Self::GracefulStopping(deadline) if now >= deadline => {
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
            | Self::Empty => Ok(Self::Empty),
            _ => Err(ActorError::InvalidTransition),
        }
    }

    pub(crate) const fn restart_allowed(self) -> bool {
        matches!(self, Self::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::expect_used)]
    fn limits() -> Deadlines {
        Deadlines {
            launch: Duration::from_secs(10),
            ready: Duration::from_secs(30),
            graceful: Duration::from_secs(5),
            force: Duration::from_secs(2),
            drain: Duration::from_secs(3),
        }
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn graceful_retry_keeps_original_deadline_and_force_cannot_downgrade() {
        let now = Instant::now();
        let graceful = Phase::Running.stop(false, now, limits()).expect("graceful");
        let repeated = graceful
            .stop(false, now + Duration::from_secs(4), limits())
            .expect("repeat");
        assert_eq!(graceful, repeated);
        let forced = repeated.stop(true, now, limits()).expect("force");
        assert!(matches!(forced, Phase::ForceStopping(_)));
        assert_eq!(
            forced.stop(false, now, limits()).expect("no downgrade"),
            forced
        );
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn confirmed_empty_is_an_idempotent_tombstone() {
        let now = Instant::now();
        let stopping = Phase::Running.stop(true, now, limits()).expect("stop");
        let empty = stopping.observe_empty().expect("empty");
        assert_eq!(empty.observe_empty().expect("idempotent"), Phase::Empty);
        assert_eq!(
            empty.stop(false, now, limits()).expect("tombstone"),
            Phase::Empty
        );
        assert!(empty.restart_allowed());
    }

    #[allow(clippy::expect_used)]
    #[test]
    fn unconfirmed_never_authorizes_a_new_generation() {
        let now = Instant::now();
        let draining = Phase::Running
            .stop(true, now, limits())
            .expect("stop")
            .advance(now + Duration::from_secs(2), limits());
        let unconfirmed = draining.advance(now + Duration::from_secs(5), limits());
        assert_eq!(unconfirmed, Phase::Unconfirmed);
        assert!(!unconfirmed.restart_allowed());
        assert_eq!(
            unconfirmed.launch(now, limits()),
            Err(ActorError::Unconfirmed)
        );
    }
}
