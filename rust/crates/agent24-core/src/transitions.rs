//! Legal status transitions (SPEC-002 §1) as pure, exhaustive functions.

use agent24_protocol::{ApprovalStatus, RunStatus, ToolCallStatus};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("illegal run transition: {from:?} -> {to:?}")]
    Run { from: RunStatus, to: RunStatus },
    #[error("illegal approval transition: {from:?} -> {to:?}")]
    Approval {
        from: ApprovalStatus,
        to: ApprovalStatus,
    },
    #[error("illegal tool-call transition: {from:?} -> {to:?}")]
    ToolCall {
        from: ToolCallStatus,
        to: ToolCallStatus,
    },
}

/// SPEC-002 §1.2 — the ONLY legal run transitions:
/// queued → running → completed | failed | cancelled;
/// running ⇄ awaiting_approval;
/// queued | running | awaiting_approval → cancelled.
pub fn run_transition_allowed(from: RunStatus, to: RunStatus) -> bool {
    use RunStatus::*;
    matches!(
        (from, to),
        (Queued, Running)
            | (Queued, Cancelled)
            | (Running, Completed)
            | (Running, Failed)
            | (Running, Cancelled)
            | (Running, AwaitingApproval)
            | (AwaitingApproval, Running)
            | (AwaitingApproval, Cancelled)
    )
}

pub fn check_run_transition(from: RunStatus, to: RunStatus) -> Result<(), TransitionError> {
    if run_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Run { from, to })
    }
}

/// Approvals resolve exactly once: pending → approved | denied | aborted |
/// timed_out. Every resolved state is terminal (fail-closed: timed_out and
/// aborted are denials).
pub fn approval_transition_allowed(from: ApprovalStatus, to: ApprovalStatus) -> bool {
    use ApprovalStatus::*;
    matches!(
        (from, to),
        (Pending, Approved) | (Pending, Denied) | (Pending, Aborted) | (Pending, TimedOut)
    )
}

pub fn check_approval_transition(
    from: ApprovalStatus,
    to: ApprovalStatus,
) -> Result<(), TransitionError> {
    if approval_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::Approval { from, to })
    }
}

/// Tool calls finish exactly once: running → completed | failed | denied.
pub fn tool_call_transition_allowed(from: ToolCallStatus, to: ToolCallStatus) -> bool {
    use ToolCallStatus::*;
    matches!(
        (from, to),
        (Running, Completed) | (Running, Failed) | (Running, Denied)
    )
}

pub fn check_tool_call_transition(
    from: ToolCallStatus,
    to: ToolCallStatus,
) -> Result<(), TransitionError> {
    if tool_call_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(TransitionError::ToolCall { from, to })
    }
}

/// A run status from which no further transition is legal.
pub fn run_is_terminal(status: RunStatus) -> bool {
    use RunStatus::*;
    matches!(status, Completed | Failed | Cancelled)
}

/// Schedules auto-disable after this many consecutive failures
/// (SPEC-002 §1.5; emits schedule.disabled).
pub const MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// Outcome of recording one schedule run result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleHealth {
    Healthy,
    /// consecutive_failures reached the limit — caller must disable and emit
    /// schedule.disabled
    MustDisable,
}

/// Pure counter policy: success resets, failure increments; hitting
/// MAX_CONSECUTIVE_FAILURES demands a disable.
pub fn record_schedule_result(consecutive_failures: &mut u32, success: bool) -> ScheduleHealth {
    if success {
        *consecutive_failures = 0;
        ScheduleHealth::Healthy
    } else {
        *consecutive_failures = consecutive_failures.saturating_add(1);
        if *consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            ScheduleHealth::MustDisable
        } else {
            ScheduleHealth::Healthy
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_protocol::{ApprovalStatus, RunStatus, ToolCallStatus};

    const RUN_ALL: [RunStatus; 6] = [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::AwaitingApproval,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Cancelled,
    ];

    #[test]
    fn run_matrix_is_exactly_the_spec() {
        use RunStatus::*;
        // The complete legal set, straight from SPEC-002 §1.2
        let legal = [
            (Queued, Running),
            (Queued, Cancelled),
            (Running, Completed),
            (Running, Failed),
            (Running, Cancelled),
            (Running, AwaitingApproval),
            (AwaitingApproval, Running),
            (AwaitingApproval, Cancelled),
        ];
        for from in RUN_ALL {
            for to in RUN_ALL {
                let expected = legal.contains(&(from, to));
                assert_eq!(
                    run_transition_allowed(from, to),
                    expected,
                    "({from:?} -> {to:?}) should be {expected}"
                );
                assert_eq!(check_run_transition(from, to).is_ok(), expected);
            }
        }
        // exactly 8 legal edges out of 36
        let count = RUN_ALL
            .iter()
            .flat_map(|f| RUN_ALL.iter().map(move |t| (*f, *t)))
            .filter(|(f, t)| run_transition_allowed(*f, *t))
            .count();
        assert_eq!(count, 8);
    }

    #[test]
    fn terminal_runs_have_no_outgoing_edges() {
        for from in RUN_ALL {
            if run_is_terminal(from) {
                for to in RUN_ALL {
                    assert!(!run_transition_allowed(from, to));
                }
            }
        }
    }

    #[test]
    fn approval_matrix_pending_resolves_once() {
        use ApprovalStatus::*;
        let all = [Pending, Approved, Denied, Aborted, TimedOut];
        for from in all {
            for to in all {
                let expected = from == Pending && to != Pending;
                assert_eq!(
                    approval_transition_allowed(from, to),
                    expected,
                    "({from:?} -> {to:?})"
                );
            }
        }
    }

    #[test]
    fn tool_call_matrix_running_finishes_once() {
        use ToolCallStatus::*;
        let all = [Running, Completed, Failed, Denied];
        for from in all {
            for to in all {
                let expected = from == Running && to != Running;
                assert_eq!(
                    tool_call_transition_allowed(from, to),
                    expected,
                    "({from:?} -> {to:?})"
                );
            }
        }
    }

    #[test]
    fn schedule_failure_counter_policy() {
        let mut failures = 0u32;
        for i in 1..MAX_CONSECUTIVE_FAILURES {
            assert_eq!(
                record_schedule_result(&mut failures, false),
                ScheduleHealth::Healthy
            );
            assert_eq!(failures, i);
        }
        assert_eq!(
            record_schedule_result(&mut failures, false),
            ScheduleHealth::MustDisable
        );
        assert_eq!(failures, MAX_CONSECUTIVE_FAILURES);
        // success resets
        assert_eq!(
            record_schedule_result(&mut failures, true),
            ScheduleHealth::Healthy
        );
        assert_eq!(failures, 0);
    }
}
