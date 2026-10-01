//! COMM-4a: a small, self-contained copy of
//! `agent24_os_proto::supervise::RestartPolicy`'s pure numbers (500ms base,
//! doubling, a 5-failure breaker, a 60s healthy-run reset).
//!
//! This is copied rather than depended on: `agent24-os-proto` pulls in
//! `hyper`/`hyper-util` with the `client` feature (its kernel-side outbound
//! proxy to domain-OS modules), which is exactly what COMM-HYPHAE.md §7's
//! upcoming `comm_dependency_allowlist` test (COMM-5b) forbids anywhere in
//! `agent24-comm`'s dependency graph. Depending on it here would make this
//! crate fail that test the day it is written, over a dependency the comm
//! daemon supervisor does not otherwise need. The task text's own
//! instruction ("RestartPolicy 复用现有实现...以 os-proto 或现有代码为准")
//! allows matching its behavior without importing the crate; the numbers
//! below are identical to `agent24_os_proto::supervise`'s.

use std::time::{Duration, Instant};

pub const BASE_BACKOFF: Duration = Duration::from_millis(500);
pub const BREAKER_THRESHOLD: u32 = 5;
pub const HEALTHY_RUN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    RestartAfter(Duration),
    GiveUp { after: u32, within: Duration },
}

#[derive(Debug, Clone)]
pub struct RestartPolicy {
    consecutive: u32,
    first_failure_at: Option<Instant>,
    base: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl RestartPolicy {
    #[must_use]
    pub fn new() -> Self {
        Self {
            consecutive: 0,
            first_failure_at: None,
            base: BASE_BACKOFF,
        }
    }

    /// For tests: a policy whose first delay is `base` rather than
    /// [`BASE_BACKOFF`].
    #[must_use]
    #[cfg(test)]
    pub fn with_base(base: Duration) -> Self {
        Self {
            consecutive: 0,
            first_failure_at: None,
            base,
        }
    }

    /// A run that lasted at least [`HEALTHY_RUN`] clears the failure count —
    /// call this when a supervised run ends, before [`Self::failed`] for the
    /// same exit.
    pub fn ran(&mut self, started: Instant, ended: Instant) {
        if ended.duration_since(started) >= HEALTHY_RUN {
            self.consecutive = 0;
            self.first_failure_at = None;
        }
    }

    /// Records a failure and decides what to do next.
    #[must_use]
    pub fn failed(&mut self, now: Instant) -> Decision {
        self.consecutive += 1;
        let first = *self.first_failure_at.get_or_insert(now);
        if self.consecutive >= BREAKER_THRESHOLD {
            return Decision::GiveUp {
                after: self.consecutive,
                within: now.duration_since(first),
            };
        }
        let factor = 1u32 << (self.consecutive - 1).min(16);
        Decision::RestartAfter(self.base.saturating_mul(factor))
    }

    #[must_use]
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_breaks_at_the_threshold() {
        let mut p = RestartPolicy::with_base(Duration::from_millis(100));
        let now = Instant::now();
        assert_eq!(
            p.failed(now),
            Decision::RestartAfter(Duration::from_millis(100))
        );
        assert_eq!(
            p.failed(now),
            Decision::RestartAfter(Duration::from_millis(200))
        );
        assert_eq!(
            p.failed(now),
            Decision::RestartAfter(Duration::from_millis(400))
        );
        assert_eq!(
            p.failed(now),
            Decision::RestartAfter(Duration::from_millis(800))
        );
        assert!(matches!(p.failed(now), Decision::GiveUp { after: 5, .. }));
    }

    #[test]
    fn a_healthy_run_resets_the_count() {
        let mut p = RestartPolicy::with_base(Duration::from_millis(100));
        let t0 = Instant::now();
        assert_eq!(
            p.failed(t0),
            Decision::RestartAfter(Duration::from_millis(100))
        );
        p.ran(t0, t0 + HEALTHY_RUN);
        assert_eq!(
            p.failed(t0),
            Decision::RestartAfter(Duration::from_millis(100))
        );
    }
}
