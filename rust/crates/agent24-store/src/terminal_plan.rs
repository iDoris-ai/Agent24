#![allow(dead_code)]

use crate::{
    LegacyRecoveryHold, RecoveryState, WorkspaceInstant, WorkspaceLeaseRow, WorkspaceResult,
    WorkspaceStoreError, terminal_helpers::TerminalReleaseSnapshot,
};
use agent24_protocol::RunStatus;

fn bad(table: &'static str, field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow { table, field }
}

/// Immutable terminal intent. It is deliberately independent of a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalAttempt {
    result: RunStatus,
    ended_at: WorkspaceInstant,
}

impl TerminalAttempt {
    pub(crate) fn new(result: RunStatus, ended_at: WorkspaceInstant) -> Self {
        Self { result, ended_at }
    }
    pub(crate) fn result(&self) -> RunStatus {
        self.result
    }
    pub(crate) fn ended_at(&self) -> &WorkspaceInstant {
        &self.ended_at
    }
}

/// The recovery-hold fields a successful terminal release changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalHoldPatch {
    released_at: WorkspaceInstant,
}

impl TerminalHoldPatch {
    fn released(ended_at: &WorkspaceInstant) -> Self {
        Self {
            released_at: ended_at.clone(),
        }
    }
    pub(crate) fn apply(&self, held: &LegacyRecoveryHold) -> LegacyRecoveryHold {
        let mut expected = held.clone();
        expected.recovery_state = RecoveryState::Released;
        expected.ready_at = None;
        expected.active_resume_approval_id = None;
        expected.released_at = Some(self.released_at.clone());
        expected
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalReleasePlan {
    attempt: TerminalAttempt,
    hold: LegacyRecoveryHold,
    lease: Option<WorkspaceLeaseRow>,
    hold_patch: TerminalHoldPatch,
}

impl TerminalReleasePlan {
    pub(crate) fn attempt(&self) -> &TerminalAttempt {
        &self.attempt
    }
    pub(crate) fn hold(&self) -> &LegacyRecoveryHold {
        &self.hold
    }
    pub(crate) fn lease(&self) -> Option<&WorkspaceLeaseRow> {
        self.lease.as_ref()
    }
    pub(crate) fn hold_patch(&self) -> &TerminalHoldPatch {
        &self.hold_patch
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalObservationPlan {
    attempt: TerminalAttempt,
    hold: LegacyRecoveryHold,
    lease: Option<WorkspaceLeaseRow>,
}

impl TerminalObservationPlan {
    pub(crate) fn attempt(&self) -> &TerminalAttempt {
        &self.attempt
    }
    pub(crate) fn hold(&self) -> &LegacyRecoveryHold {
        &self.hold
    }
    pub(crate) fn lease(&self) -> Option<&WorkspaceLeaseRow> {
        self.lease.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TerminalPlan {
    Release(TerminalReleasePlan),
    Observe(TerminalObservationPlan),
}

fn terminal(result: RunStatus) -> bool {
    matches!(
        result,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    )
}

fn time_is_valid(snapshot: &TerminalReleaseSnapshot, attempt: &TerminalAttempt) -> bool {
    attempt.ended_at >= snapshot.run.created_at
        && attempt.ended_at >= snapshot.cohort.created_at
        && attempt.ended_at >= snapshot.workspace.created_at
        && snapshot
            .run
            .run
            .started_at
            .as_deref()
            .is_none_or(|at| attempt.ended_at.as_str() >= at)
        && snapshot
            .hold
            .ready_at()
            .is_none_or(|at| attempt.ended_at >= *at)
        && snapshot
            .pending_approvals
            .iter()
            .all(|approval| attempt.ended_at >= approval.created_at)
        && snapshot.lease.as_ref().is_none_or(|lease| {
            attempt.ended_at >= lease.record.acquired_at
                && lease
                    .record
                    .renewed_at
                    .as_ref()
                    .is_none_or(|at| attempt.ended_at >= *at)
        })
}

fn matching_lease(snapshot: &TerminalReleaseSnapshot) -> bool {
    snapshot.lease.as_ref().is_none_or(|lease| {
        lease.record.owner_id == snapshot.hold.run_id()
            && lease.record.workspace_id == *snapshot.hold.workspace_id()
            && lease.record.root_generation == snapshot.hold.root_generation()
    })
}

/// Maps an exact read snapshot to a terminal release or retry-observation plan.
/// Workspace lifecycle state is intentionally not a precondition: an existing
/// run lease may be terminalized while its workspace is releasing or expired.
pub(crate) fn plan_terminal(
    snapshot: &TerminalReleaseSnapshot,
    attempt: TerminalAttempt,
) -> WorkspaceResult<TerminalPlan> {
    if !terminal(attempt.result) || !time_is_valid(snapshot, &attempt) || !matching_lease(snapshot)
    {
        return Err(bad("legacy_recovery_holds", "terminal"));
    }
    if snapshot.hold.recovery_state() == RecoveryState::Released {
        // This is only an observation candidate. Composition still verifies every approval,
        // audit record, released lease, and cohort completion in the same transaction.
        return (snapshot.run.run.status == attempt.result
            && snapshot.run.run.ended_at.as_deref() == Some(attempt.ended_at.as_str())
            && snapshot.hold.released_at() == Some(attempt.ended_at())
            && snapshot.lease.is_none())
        .then_some(TerminalPlan::Observe(TerminalObservationPlan {
            attempt,
            hold: snapshot.hold.clone(),
            lease: snapshot.lease.clone(),
        }))
        .ok_or_else(|| bad("legacy_recovery_holds", "released_at"));
    }
    if !agent24_core::run_transition_allowed(snapshot.run.run.status, attempt.result) {
        return Err(bad("legacy_recovery_holds", "terminal"));
    }
    let active_completion = matches!(attempt.result, RunStatus::Completed | RunStatus::Failed)
        && snapshot.hold.recovery_state() == RecoveryState::Active
        && snapshot.lease.is_some();
    let cancellable = attempt.result == RunStatus::Cancelled
        && matches!(
            snapshot.hold.recovery_state(),
            RecoveryState::AwaitingDecision
                | RecoveryState::Ready
                | RecoveryState::Active
                | RecoveryState::NeedsAttention
        )
        && (snapshot.hold.recovery_state() != RecoveryState::Active || snapshot.lease.is_some());
    (active_completion || cancellable)
        .then_some(TerminalPlan::Release(TerminalReleasePlan {
            hold_patch: TerminalHoldPatch::released(attempt.ended_at()),
            attempt,
            hold: snapshot.hold.clone(),
            lease: snapshot.lease.clone(),
        }))
        .ok_or_else(|| bad("legacy_recovery_holds", "terminal"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::legacy_recovery::tests::strict_facts_fixture;
    use crate::terminal_helpers::{
        read_all_approvals_tx, read_cohort_tx, read_pending_approvals_tx, read_run_lease_tx,
        read_terminal_run_tx, read_workspace_tx,
    };

    fn status_text(status: RunStatus) -> &'static str {
        match status {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::AwaitingApproval => "awaiting_approval",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }

    async fn snapshot(
        status: RunStatus,
        state: RecoveryState,
        lease: bool,
    ) -> TerminalReleaseSnapshot {
        let store = strict_facts_fixture().await;
        let mut tx = store.pool().begin().await.unwrap();
        sqlx::query("UPDATE runs SET status=?,started_at='2026-09-19T00:00:00.000Z',input='{\"prompt\":\"x\"}',usage='{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}' WHERE id='run-strict'")
            .bind(status_text(status))
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE legacy_recovery_holds SET recovery_state=?,approval_id=CASE WHEN ?='active' THEN approval_id ELSE approval_id END,ready_at=CASE WHEN ?='ready' THEN '2026-09-19T00:00:00.000Z' ELSE NULL END,reason_code=CASE WHEN ?='needs_attention' THEN 'x' ELSE NULL END,active_resume_approval_id=CASE WHEN ?='active' THEN approval_id ELSE NULL END WHERE run_id='run-strict'")
            .bind(state.as_str()).bind(state.as_str()).bind(state.as_str()).bind(state.as_str()).bind(state.as_str()).execute(&mut *tx).await.unwrap();
        if !lease {
            sqlx::query("UPDATE workspace_leases SET released_at='2026-09-19T00:00:00.000Z'")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let hold = crate::legacy_recovery::read_hold_tx(&mut tx, "run-strict")
            .await
            .unwrap();
        let run = read_terminal_run_tx(&mut tx, "run-strict").await.unwrap();
        let workspace = read_workspace_tx(&mut tx, &hold).await.unwrap();
        let current_lease = read_run_lease_tx(&mut tx, &hold, &run, &workspace)
            .await
            .unwrap();
        let cohort = read_cohort_tx(&mut tx, &hold).await.unwrap();
        let pending_approvals = read_pending_approvals_tx(&mut tx, "run-strict")
            .await
            .unwrap();
        let all = read_all_approvals_tx(&mut tx, "run-strict").await.unwrap();
        tx.commit().await.unwrap();
        TerminalReleaseSnapshot {
            run,
            hold,
            cohort,
            workspace,
            lease: current_lease,
            hold_approval: all.into_iter().next(),
            pending_approvals,
            latest_audit: None,
        }
    }

    #[tokio::test]
    async fn terminal_policy_is_table_driven() {
        use RecoveryState::*;
        let cases = [
            (RunStatus::Running, Active, true, RunStatus::Completed, true),
            (RunStatus::Running, Active, false, RunStatus::Failed, false),
            (
                RunStatus::Queued,
                AwaitingDecision,
                false,
                RunStatus::Cancelled,
                true,
            ),
            (RunStatus::Running, Ready, false, RunStatus::Cancelled, true),
            (
                RunStatus::AwaitingApproval,
                NeedsAttention,
                false,
                RunStatus::Cancelled,
                true,
            ),
            (
                RunStatus::Running,
                NeedsAttention,
                true,
                RunStatus::Cancelled,
                true,
            ),
            (
                RunStatus::Running,
                Ready,
                false,
                RunStatus::Completed,
                false,
            ),
        ];
        for (run, hold, lease, result, allowed) in cases {
            let actual = plan_terminal(
                &snapshot(run, hold, lease).await,
                TerminalAttempt::new(
                    result,
                    WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap(),
                ),
            );
            assert_eq!(actual.is_ok(), allowed, "{run:?}/{hold:?}/{result:?}");
        }
    }

    #[tokio::test]
    async fn released_hold_is_an_observation_retry() {
        let ended = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        let mut facts = snapshot(RunStatus::Running, RecoveryState::Active, true).await;
        let live_lease = facts.lease.take();
        facts.run.run.status = RunStatus::Completed;
        facts.run.run.ended_at = Some(ended.as_str().to_owned());
        facts.hold.recovery_state = RecoveryState::Released;
        facts.hold.released_at = Some(ended.clone());
        assert!(matches!(
            plan_terminal(
                &facts,
                TerminalAttempt::new(RunStatus::Completed, ended.clone())
            ),
            Ok(TerminalPlan::Observe(_))
        ));
        facts.run.run.ended_at = Some("2026-09-19T00:00:02.000Z".into());
        assert!(
            plan_terminal(
                &facts,
                TerminalAttempt::new(RunStatus::Completed, ended.clone())
            )
            .is_err()
        );
        facts.run.run.ended_at = Some(ended.as_str().to_owned());
        facts.lease = live_lease;
        assert!(plan_terminal(&facts, TerminalAttempt::new(RunStatus::Completed, ended)).is_err());
    }
}
