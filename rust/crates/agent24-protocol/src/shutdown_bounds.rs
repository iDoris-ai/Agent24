//! The worst a daemon shutdown can take, in one place for everything that must outlast it:
//! the CLI's wait for the old daemon to release its lock, and the LaunchAgent's `ExitTimeOut`
//! (launchd SIGKILLs a service that is still running that long after SIGTERM). The daemon's
//! `lifecycle.rs` takes its caps from here and asserts at compile time that its fixed tail
//! fits, so the two cannot drift apart silently.

/// Largest `A24_MODULE_DRAIN_MS` the daemon accepts.
pub const DRAIN_MAX_MS: u64 = 10_000;
/// Largest `A24_COMPONENT_STOP_GRACE_MS` the daemon accepts (ADR-038 D5 needs at least 15s).
pub const GRACE_MAX_MS: u64 = 30_000;
/// How long a shutdown may keep the summary open for a managed iDoris (it can exceed the
/// module budgets above).
pub const IDORIS_BUDGET_MS: u64 = 50_000;
/// Everything after the module budgets: confirm 200 + persist 200 + watchdog margin 300 +
/// runtime teardown 300, rounded up.
pub const TAIL_MS: u64 = 1_000;

const fn max(a: u64, b: u64) -> u64 {
    if a > b { a } else { b }
}

/// From SIGTERM to the process gone, at the latest, with every knob at its cap.
pub const WORST_CASE_EXIT_MS: u64 = max(DRAIN_MAX_MS + GRACE_MAX_MS, IDORIS_BUDGET_MS) + TAIL_MS;
/// What a waiter adds on top of the worst case.
pub const MARGIN_MS: u64 = 4_000;
/// How long to wait for a daemon to be gone: the worst case plus the margin, in seconds.
/// Used by `agent24 daemon stop` and as the plist's `ExitTimeOut`.
pub const WAIT_SECS: u64 = (WORST_CASE_EXIT_MS + MARGIN_MS).div_ceil(1000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_outlasts_the_worst_case_with_a_margin() {
        assert_eq!(WORST_CASE_EXIT_MS, 51_000);
        assert_eq!(WAIT_SECS, 55);
        const { assert!(WAIT_SECS * 1000 >= WORST_CASE_EXIT_MS + MARGIN_MS) };
        // launchd's default ExitTimeOut is 20s: the cap really does exceed it.
        const { assert!(WAIT_SECS > 20) };
    }
}
