#![allow(dead_code)]
use crate::{
    LegacyRecoveryHold, RecoveryState, WorkspaceInstant, WorkspaceLeaseRow, WorkspaceResult,
    WorkspaceStoreError,
    legacy_recovery::read_hold_tx,
    repo::{RunPatch, transition_run_tx},
    terminal_helpers::{
        TerminalAuditFacts, TerminalReleaseSnapshot, read_all_approvals_tx, read_audit_tx,
        read_cohort_tx, read_exact_terminal_audits_tx, read_pending_approvals_tx,
        read_run_lease_history_tx, read_run_lease_tx, read_terminal_run_tx, read_workspace_tx,
        verify_audit_tx,
    },
    terminal_mutations::{
        TerminalAuditInput, TerminalMutation, abort_all_pending_terminal_approvals_tx,
        append_terminal_audit_tx, close_terminal_cohort_if_last_tx, observe_released_terminal_tx,
        release_exact_terminal_lease_tx, release_terminal_hold_tx,
    },
    terminal_plan::{TerminalAttempt, TerminalPlan, plan_terminal},
};
use agent24_protocol::{ApprovalStatus, RunStatus};
use sqlx::{Acquire, Sqlite, Transaction};
#[rustfmt::skip]
#[cfg(test)] type Gate = (std::sync::Arc<tokio::sync::Notify>, std::sync::Arc<tokio::sync::Notify>);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalCompositionOutcome {
    Applied,
    ObservedApplied,
    Conflict,
}
#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq)] pub(crate) enum ExpectedLease { None, Exact(Box<WorkspaceLeaseRow>) }
#[rustfmt::skip]
#[derive(Debug, Clone)] pub(crate) struct CompositionAttempt { terminal: TerminalAttempt, lease: ExpectedLease, #[cfg(test)] pause: Option<Gate> }
#[rustfmt::skip]
impl CompositionAttempt {
    pub(crate) fn new(terminal: TerminalAttempt, lease: ExpectedLease) -> Self { Self { terminal, lease, #[cfg(test)] pause: None } }
    fn terminal(&self) -> &TerminalAttempt { &self.terminal }
    fn lease(&self) -> &ExpectedLease { &self.lease }
    #[cfg(test)] fn pausing(mut self, entered: std::sync::Arc<tokio::sync::Notify>, resume: std::sync::Arc<tokio::sync::Notify>) -> Self { self.pause = Some((entered, resume)); self }
}
#[rustfmt::skip]
#[cfg(test)] async fn pause_after_terminal_update(attempt: &CompositionAttempt) { if let Some((entered, resume)) = &attempt.pause { entered.notify_one(); resume.notified().await; } }
fn bad(table: &'static str, field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow { table, field }
}
fn audit_parts(
    hold: &LegacyRecoveryHold,
    attempt: &TerminalAttempt,
) -> WorkspaceResult<(String, String)> {
    let result = match attempt.result() {
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        _ => return Err(bad("runs", "status")),
    };
    let detail = serde_json::json!({
        "run_id": hold.run_id(), "cohort_id": hold.cohort_id(),
        "workspace_id": hold.workspace_id().as_str(), "result_state": result,
    });
    Ok((
        format!("legacy_recovery.{result}"),
        serde_json::to_string(&detail).map_err(|_| WorkspaceStoreError::Database)?,
    ))
}
async fn snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> WorkspaceResult<TerminalReleaseSnapshot> {
    let hold = read_hold_tx(tx, run_id).await?;
    let run = read_terminal_run_tx(tx, run_id).await?;
    if run.workspace_id != *hold.workspace_id() {
        return Err(bad("runs", "workspace_id"));
    }
    let workspace = read_workspace_tx(tx, &hold).await?;
    let lease = read_run_lease_tx(tx, &hold, &run, &workspace).await?;
    let cohort = read_cohort_tx(tx, &hold).await?;
    let hold_approval = match hold.approval_id() {
        Some(id) => Some(crate::terminal_helpers::read_approval_tx(tx, id).await?),
        None => None,
    };
    let pending_approvals = read_pending_approvals_tx(tx, run_id).await?;
    let latest_seq = sqlx::query_scalar::<_, Option<i64>>("SELECT max(seq) FROM audit_log")
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    let latest_audit = match latest_seq {
        Some(seq) => Some(read_audit_tx(tx, seq).await?),
        None => None,
    };
    if let Some(audit) = &latest_audit {
        verify_audit_tx(tx, audit).await?;
    }
    Ok(TerminalReleaseSnapshot {
        run,
        hold,
        cohort,
        workspace,
        lease,
        hold_approval,
        pending_approvals,
        latest_audit,
    })
}
#[rustfmt::skip]
async fn released_attempt_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hold: &LegacyRecoveryHold,
    ended_at: &WorkspaceInstant,
    result: RunStatus,
    expected: &ExpectedLease,
) -> WorkspaceResult<TerminalMutation<Option<WorkspaceLeaseRow>>> {
    let leases = read_run_lease_history_tx(tx, hold).await?;
    if leases.iter().any(|lease| lease.record.released_at.is_none()) {
        return Ok(TerminalMutation::Conflict);
    }
    let exact = match expected {
        ExpectedLease::Exact(lease) if lease.record.released_at.is_none() => {
            let mut released = (**lease).clone();
            released.record.released_at = Some(ended_at.clone());
            Some(released)
        }
        ExpectedLease::Exact(_) => return Ok(TerminalMutation::Conflict),
        ExpectedLease::None if result == RunStatus::Cancelled => None,
        ExpectedLease::None => return Ok(TerminalMutation::Conflict),
    };
    Ok(match exact {
        Some(lease) if leases.contains(&lease) => TerminalMutation::Applied(Some(lease)),
        None => TerminalMutation::Applied(None),
        Some(_) => TerminalMutation::Conflict,
    })
}
async fn closed_cohort_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hold: &LegacyRecoveryHold,
    ended: &WorkspaceInstant,
) -> WorkspaceResult<bool> {
    let cohort = read_cohort_tx(tx, hold).await?;
    let (open, latest): (i64, Option<String>) = sqlx::query_as("SELECT count(CASE WHEN released_at IS NULL THEN 1 END),max(released_at) FROM legacy_recovery_holds WHERE cohort_id=? COLLATE BINARY")
        .bind(hold.cohort_id()).fetch_one(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
    Ok(match cohort.completed_at.as_ref() {
        None => open > 0,
        Some(done) => {
            open == 0 && done >= ended && latest.as_deref().is_some_and(|at| at <= done.as_str())
        }
    })
}
async fn observe_tx(
    tx: &mut Transaction<'_, Sqlite>,
    composition: &CompositionAttempt,
    snapshot: &TerminalReleaseSnapshot,
) -> WorkspaceResult<TerminalCompositionOutcome> {
    let attempt = composition.terminal();
    let all = read_all_approvals_tx(tx, snapshot.hold.run_id()).await?;
    if all.iter().any(|row| row.status == ApprovalStatus::Pending) {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    let lease_id = match released_attempt_lease_tx(
        tx,
        &snapshot.hold,
        attempt.ended_at(),
        attempt.result(),
        composition.lease(),
    )
    .await?
    {
        TerminalMutation::Applied(lease) => lease,
        TerminalMutation::Conflict => return Ok(TerminalCompositionOutcome::Conflict),
    };
    let (action, detail) = audit_parts(&snapshot.hold, attempt)?;
    let audits = read_exact_terminal_audits_tx(tx, attempt.ended_at(), &action, &detail).await?;
    if audits.len() != 1 {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    verify_audit_tx(tx, &audits[0]).await?;
    if !closed_cohort_tx(tx, &snapshot.hold, attempt.ended_at()).await? {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    match observe_released_terminal_tx(
        tx,
        &snapshot.hold,
        attempt.ended_at(),
        lease_id.as_ref().map(|lease| &lease.record.id),
    )
    .await?
    {
        TerminalMutation::Applied(()) => Ok(TerminalCompositionOutcome::ObservedApplied),
        TerminalMutation::Conflict => Ok(TerminalCompositionOutcome::Conflict),
    }
}
async fn fresh_apply_tx(
    tx: &mut Transaction<'_, Sqlite>,
    composition: &CompositionAttempt,
    snapshot: &TerminalReleaseSnapshot,
) -> WorkspaceResult<TerminalCompositionOutcome> {
    let attempt = composition.terminal();
    let lease_history = read_run_lease_history_tx(tx, &snapshot.hold).await?;
    if matches!(attempt.result(), RunStatus::Completed | RunStatus::Failed)
        && !matches!(composition.lease(), ExpectedLease::Exact(_))
        || match composition.lease() {
            ExpectedLease::None => snapshot.lease.is_some(),
            ExpectedLease::Exact(lease) => snapshot.lease.as_ref() != Some(&**lease),
        }
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    let all_before = read_all_approvals_tx(tx, snapshot.hold.run_id()).await?;
    if matches!(attempt.result(), RunStatus::Completed | RunStatus::Failed)
        && all_before
            .iter()
            .any(|row| row.status == ApprovalStatus::Pending)
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    let mut approvals = all_before.clone();
    if attempt.result() == RunStatus::Cancelled {
        for approval in &mut approvals {
            if approval.status == ApprovalStatus::Pending {
                approval.status = ApprovalStatus::Aborted;
                approval.decision = None;
                approval.decided_at = Some(attempt.ended_at().clone());
            }
        }
    }
    let mut expected = snapshot.clone();
    expected.run.run.status = attempt.result();
    expected.run.run.ended_at = Some(attempt.ended_at().as_str().to_owned());
    expected.hold.recovery_state = RecoveryState::Released;
    expected.hold.ready_at = None;
    expected.hold.active_resume_approval_id = None;
    expected.hold.released_at = Some(attempt.ended_at().clone());
    expected.hold_approval = match expected.hold.approval_id() {
        Some(id) => Some(
            approvals
                .iter()
                .find(|approval| approval.id == id)
                .cloned()
                .ok_or_else(|| bad("approvals", "id"))?,
        ),
        None => None,
    };
    expected.pending_approvals = approvals
        .iter()
        .filter(|row| row.status == ApprovalStatus::Pending)
        .cloned()
        .collect();
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM legacy_recovery_holds WHERE cohort_id=? COLLATE BINARY AND released_at IS NULL")
        .bind(snapshot.hold.cohort_id()).fetch_one(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
    if open == 1 {
        if expected.cohort.completed_at.is_some() {
            return Ok(TerminalCompositionOutcome::Conflict);
        }
        expected.cohort.completed_at = Some(attempt.ended_at().clone());
    }
    let expected_lease = match composition.lease() {
        ExpectedLease::None => None,
        ExpectedLease::Exact(lease) => {
            let mut lease = (**lease).clone();
            lease.record.released_at = Some(attempt.ended_at().clone());
            Some(lease)
        }
    };
    let expected_history = lease_history
        .into_iter()
        .map(|mut lease| {
            if Some(&lease) == snapshot.lease.as_ref() {
                lease.record.released_at = Some(attempt.ended_at().clone());
            }
            lease
        })
        .collect::<Vec<_>>();
    expected.lease = None;
    let (action, detail) = audit_parts(&snapshot.hold, attempt)?;
    let prev_hash = snapshot
        .latest_audit
        .as_ref()
        .map_or_else(|| crate::audit::GENESIS.into(), |audit| audit.hash.clone());
    let audit_expected = TerminalAuditFacts {
        seq: snapshot
            .latest_audit
            .as_ref()
            .map_or(1, |audit| audit.seq + 1),
        ts: attempt.ended_at().clone(),
        actor: "legacy_recovery".into(),
        action: action.clone(),
        raw_detail: detail.clone(),
        hash: crate::audit::entry_hash(
            &prev_hash,
            attempt.ended_at().as_str(),
            "legacy_recovery",
            &action,
            &detail,
        ),
        prev_hash,
    };
    if !read_exact_terminal_audits_tx(tx, attempt.ended_at(), &action, &detail)
        .await?
        .is_empty()
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    if attempt.result() == RunStatus::Cancelled
        && abort_all_pending_terminal_approvals_tx(tx, snapshot.hold.run_id(), attempt.ended_at())
            .await?
            != approvals
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    let transitioned = transition_run_tx(
        tx,
        snapshot.hold.run_id(),
        snapshot.run.run.status,
        attempt.result(),
        &RunPatch {
            ended_at: Some(attempt.ended_at().as_str().to_owned()),
            ..RunPatch::default()
        },
    )
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if !transitioned || read_terminal_run_tx(tx, snapshot.hold.run_id()).await? != expected.run {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    #[cfg(test)]
    pause_after_terminal_update(composition).await;
    if let Some(lease) = &snapshot.lease
        && !matches!(
            release_exact_terminal_lease_tx(tx, lease, attempt.ended_at()).await?,
            TerminalMutation::Applied(_)
        )
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    let released = match release_terminal_hold_tx(tx, &snapshot.hold, attempt.ended_at()).await? {
        TerminalMutation::Applied(hold) => hold,
        TerminalMutation::Conflict => return Ok(TerminalCompositionOutcome::Conflict),
    };
    match close_terminal_cohort_if_last_tx(tx, &released, attempt.ended_at()).await? {
        TerminalMutation::Applied(_) => {}
        TerminalMutation::Conflict => return Ok(TerminalCompositionOutcome::Conflict),
    }
    let after = snapshot_tx(tx, snapshot.hold.run_id()).await?;
    if after != expected
        || read_all_approvals_tx(tx, snapshot.hold.run_id()).await? != approvals
        || !matches!(released_attempt_lease_tx(tx, &expected.hold, attempt.ended_at(), attempt.result(), composition.lease()).await?, TerminalMutation::Applied(actual) if actual == expected_lease)
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    let audit = TerminalAuditInput {
        run_id: snapshot.hold.run_id().into(),
        cohort_id: snapshot.hold.cohort_id().into(),
        workspace_id: snapshot.hold.workspace_id().as_str().into(),
        result: attempt.result(),
        ended_at: attempt.ended_at().clone(),
    };
    let appended = match append_terminal_audit_tx(tx, &audit).await? {
        TerminalMutation::Applied(entry) => entry,
        TerminalMutation::Conflict => return Ok(TerminalCompositionOutcome::Conflict),
    };
    let audits = read_exact_terminal_audits_tx(tx, attempt.ended_at(), &action, &detail).await?;
    if appended != audit_expected || audits.len() != 1 || audits[0] != audit_expected {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    verify_audit_tx(tx, &appended).await?;
    expected.latest_audit = Some(audit_expected);
    let final_facts = snapshot_tx(tx, snapshot.hold.run_id()).await?;
    if final_facts != expected
        || read_all_approvals_tx(tx, snapshot.hold.run_id()).await? != approvals
        || read_run_lease_history_tx(tx, &snapshot.hold).await? != expected_history
        || !matches!(released_attempt_lease_tx(tx, &expected.hold, attempt.ended_at(), attempt.result(), composition.lease()).await?, TerminalMutation::Applied(actual) if actual == expected_lease)
        || !closed_cohort_tx(tx, &expected.hold, attempt.ended_at()).await?
    {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    Ok(TerminalCompositionOutcome::Applied)
}
async fn compose_inner_tx(
    tx: &mut Transaction<'_, Sqlite>,
    composition: &CompositionAttempt,
    snapshot: &TerminalReleaseSnapshot,
) -> WorkspaceResult<TerminalCompositionOutcome> {
    let fresh = snapshot_tx(tx, snapshot.hold.run_id()).await?;
    if fresh != *snapshot {
        return Ok(TerminalCompositionOutcome::Conflict);
    }
    match plan_terminal(&fresh, composition.terminal().clone()) {
        Ok(TerminalPlan::Observe(_)) => observe_tx(tx, composition, &fresh).await,
        Ok(TerminalPlan::Release(_)) => fresh_apply_tx(tx, composition, &fresh).await,
        Err(_) => Ok(TerminalCompositionOutcome::Conflict),
    }
}
pub(crate) async fn compose_terminal_tx(
    tx: &mut Transaction<'_, Sqlite>,
    composition: &CompositionAttempt,
    snapshot: &TerminalReleaseSnapshot,
) -> WorkspaceResult<TerminalCompositionOutcome> {
    let mut nested = tx
        .begin()
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    let result = compose_inner_tx(&mut nested, composition, snapshot).await;
    match result {
        Ok(
            outcome @ (TerminalCompositionOutcome::Applied
            | TerminalCompositionOutcome::ObservedApplied),
        ) => {
            nested
                .commit()
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            Ok(outcome)
        }
        Ok(TerminalCompositionOutcome::Conflict) | Err(_) => result,
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::legacy_recovery::tests::{strict_facts_file, strict_facts_fixture};
    use sqlx::Connection;
    use std::{path::Path, sync::Arc, time::Duration};
    use tokio::sync::Barrier;
    #[rustfmt::skip] async fn facts(tx: &mut Transaction<'_, Sqlite>) -> TerminalReleaseSnapshot { snapshot_tx(tx, "run-strict").await.unwrap() }
    #[rustfmt::skip] fn attempt(status: RunStatus) -> TerminalAttempt { TerminalAttempt::new(status, WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap()) }
    #[rustfmt::skip]
    fn composition(status: RunStatus, snapshot: &TerminalReleaseSnapshot) -> CompositionAttempt { CompositionAttempt::new(attempt(status), snapshot.lease.clone().map_or(ExpectedLease::None, |lease| ExpectedLease::Exact(Box::new(lease)))) }
    #[rustfmt::skip] async fn usable(store: &crate::Store) { sqlx::query("UPDATE runs SET input='{\"prompt\":\"x\"}',usage='{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}' WHERE id='run-strict'").execute(store.pool()).await.unwrap(); }
    #[rustfmt::skip]
    #[tokio::test]
    async fn conflict_leaves_outer_transaction_committable() {
        let store = strict_facts_fixture().await; usable(&store).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap(); let before = facts(&mut tx).await;
        sqlx::query("UPDATE approvals SET summary='race' WHERE id='approval-strict'").execute(&mut *tx).await.unwrap();
        assert!(matches!(compose_terminal_tx(&mut tx, &composition(RunStatus::Cancelled, &before), &before).await, Ok(TerminalCompositionOutcome::Conflict)));
        sqlx::query("UPDATE approvals SET summary='outer' WHERE id='approval-strict'").execute(&mut *tx).await.unwrap(); tx.commit().await.unwrap();
        assert_eq!(sqlx::query_scalar::<_, String>("SELECT summary FROM approvals WHERE id='approval-strict'").fetch_one(store.pool()).await.unwrap(), "outer");
    }
    #[rustfmt::skip]
    #[tokio::test]
    async fn trigger_rereads_rollback_without_partial_writes() {
        for (event, body) in [("UPDATE OF completed_at ON legacy_recovery_cohorts", "UPDATE legacy_recovery_holds SET reason_code='twist' WHERE run_id='run-strict'"), ("INSERT ON audit_log", "UPDATE approvals SET summary='twist' WHERE id='approval-strict'"), ("UPDATE OF released_at ON workspace_leases", "UPDATE workspace_leases SET acquired_at='2026-09-19T00:00:00.001Z' WHERE lease_id=NEW.lease_id"), ("UPDATE OF released_at ON workspace_leases", "DELETE FROM workspace_leases WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'"), ("UPDATE OF released_at ON workspace_leases", "UPDATE workspace_leases SET acquired_at='2026-09-19T00:00:00.001Z' WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'"), ("INSERT ON audit_log", "UPDATE legacy_recovery_holds SET recovery_state='needs_attention',released_at=NULL,reason_code='x' WHERE run_id='peer'"), ("INSERT ON audit_log", "UPDATE legacy_recovery_holds SET released_at='2026-09-19T00:00:02.000Z' WHERE run_id='peer'"), ("INSERT ON audit_log", "DELETE FROM legacy_recovery_holds WHERE run_id='peer'")] {
            let store = strict_facts_fixture().await; usable(&store).await; let (state, released) = if body.starts_with("DELETE") { ("needs_attention", "NULL") } else { ("released", "'2026-09-19T00:00:01.000Z'") };
            sqlx::raw_sql(&format!("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at,released_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','run-strict','run','2026-09-19T00:00:00.000Z','2026-09-19T00:00:00.000Z'); INSERT INTO runs (id,status,input,usage,created_at,workspace_id) VALUES ('peer','running','{{}}','{{}}','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'); INSERT INTO legacy_recovery_holds (run_id,cohort_id,workspace_id,root_generation,original_status,recovery_state,reason_code,released_at) VALUES ('peer','cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','running','{state}','x',{released})")).execute(store.pool()).await.unwrap();
            sqlx::raw_sql(&format!("CREATE TRIGGER twist AFTER {event} BEGIN {body}; END")).execute(store.pool()).await.unwrap();
            let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap(); let before = facts(&mut tx).await;
            assert!(!matches!(compose_terminal_tx(&mut tx, &composition(RunStatus::Cancelled, &before), &before).await, Ok(TerminalCompositionOutcome::Applied)), "{body}");
            tx.commit().await.unwrap(); let facts: (String,String,i64) = sqlx::query_as("SELECT (SELECT status FROM runs WHERE id='run-strict'),(SELECT acquired_at FROM workspace_leases WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'),(SELECT count(*) FROM audit_log)").fetch_one(store.pool()).await.unwrap(); assert_eq!(facts, ("running".into(), "2026-09-19T00:00:00.000Z".into(), 0));
        }
    }
    #[rustfmt::skip]
    #[tokio::test]
    async fn partial_cohort_applies_then_observes() {
        let store = strict_facts_fixture().await; usable(&store).await; sqlx::raw_sql("DELETE FROM workspace_leases; INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at,released_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','run-strict','run','2026-09-19T00:00:00.000Z','2026-09-19T00:00:00.000Z'); INSERT INTO runs (id,status,input,usage,created_at,workspace_id) VALUES ('peer-b','running','{}','{}','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'),('peer-c','running','{}','{}','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'); INSERT INTO legacy_recovery_holds (run_id,cohort_id,workspace_id,root_generation,original_status,recovery_state,reason_code,released_at) VALUES ('peer-b','cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','running','released','x','2026-09-19T00:00:02.000Z'),('peer-c','cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','running','needs_attention','x',NULL)").execute(store.pool()).await.unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap(); let before = facts(&mut tx).await; let terminal = composition(RunStatus::Cancelled, &before);
        assert_eq!(compose_terminal_tx(&mut tx, &terminal, &before).await.unwrap(), TerminalCompositionOutcome::Applied); let released = facts(&mut tx).await; assert!(released.cohort.completed_at.is_none()); assert_eq!(sqlx::query_scalar::<_, String>("SELECT released_at FROM workspace_leases WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'").fetch_one(&mut *tx).await.unwrap(), "2026-09-19T00:00:00.000Z");
        assert_eq!(compose_terminal_tx(&mut tx, &terminal, &released).await.unwrap(), TerminalCompositionOutcome::ObservedApplied);
    }
    #[rustfmt::skip]
    #[tokio::test]
    async fn exact_lease_terminal_retries_reject_lost_or_changed_history() {
        for (result, damage) in [(RunStatus::Completed, "DELETE FROM workspace_leases WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6'"), (RunStatus::Failed, "UPDATE workspace_leases SET acquired_at='2026-09-19T00:00:00.001Z' WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6'")] { let store = strict_facts_fixture().await; usable(&store).await;
            sqlx::raw_sql("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at,released_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','run-strict','run','2026-09-19T00:00:00.000Z','2026-09-19T00:00:00.000Z'); UPDATE approvals SET status='approved',decision='{\"type\":\"approve\"}',available_decisions='[\"approve\"]',decided_at='2026-09-19T00:00:00.000Z'; UPDATE legacy_recovery_holds SET recovery_state='active',active_resume_approval_id='approval-strict',reason_code=NULL WHERE run_id='run-strict'").execute(store.pool()).await.unwrap();
            let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap(); let before = facts(&mut tx).await; let terminal = composition(result, &before);
            assert_eq!(compose_terminal_tx(&mut tx, &terminal, &before).await.unwrap(), TerminalCompositionOutcome::Applied); let released = facts(&mut tx).await; assert_eq!(sqlx::query_scalar::<_, String>("SELECT released_at FROM workspace_leases WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'").fetch_one(&mut *tx).await.unwrap(), "2026-09-19T00:00:00.000Z");
            assert_eq!(compose_terminal_tx(&mut tx, &terminal, &released).await.unwrap(), TerminalCompositionOutcome::ObservedApplied); sqlx::raw_sql(damage).execute(&mut *tx).await.unwrap();
            assert_eq!(compose_terminal_tx(&mut tx, &terminal, &released).await.unwrap(), TerminalCompositionOutcome::Conflict);
        }
    }
    #[rustfmt::skip]
    #[tokio::test]
    async fn cancelled_compose_future_leaves_outer_facts_unchanged() {
        for _ in 0..20 { let store = strict_facts_fixture().await; usable(&store).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap(); let before = facts(&mut tx).await;
        let entered = std::sync::Arc::new(tokio::sync::Notify::new()); let resume = std::sync::Arc::new(tokio::sync::Notify::new()); let terminal = composition(RunStatus::Cancelled, &before).pausing(entered.clone(), resume);
        { let future = compose_terminal_tx(&mut tx, &terminal, &before); tokio::pin!(future); tokio::select! { _ = entered.notified() => {}, _ = &mut future => panic!("compose completed") } }
        tx.commit().await.unwrap(); let mut check = store.pool().begin().await.unwrap(); assert_eq!(facts(&mut check).await, before); check.rollback().await.unwrap(); }
    }

    #[tokio::test]
    async fn file_backed_terminal_commit_reopens_and_observes_original_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-commit.db");
        let store = strict_facts_file(&path).await;
        usable(&store).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE legacy_recovery_holds SET reason_code='caller_owned_commit' WHERE run_id='run-strict'")
            .execute(&mut *tx).await.unwrap();
        let before = facts(&mut tx).await;
        let lease = before.lease.clone().unwrap();
        let history = read_run_lease_history_tx(&mut tx, &before.hold)
            .await
            .unwrap();
        let terminal = composition(RunStatus::Cancelled, &before);
        let expected_lease = terminal.lease().clone();
        assert_eq!(
            compose_terminal_tx(&mut tx, &terminal, &before)
                .await
                .unwrap(),
            TerminalCompositionOutcome::Applied
        );
        tx.commit().await.unwrap();
        store.pool().close().await;

        let reopened = crate::Store::open(&path).await.unwrap();
        let mut check = reopened.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let released = facts(&mut check).await;
        let ended = terminal.terminal().ended_at().clone();
        assert_eq!(released.run.run.status, RunStatus::Cancelled);
        assert_eq!(released.hold.recovery_state(), RecoveryState::Released);
        assert_eq!(released.hold.reason_code(), Some("caller_owned_commit"));
        assert_eq!(released.hold.released_at(), Some(&ended));
        assert_eq!(released.cohort.completed_at, Some(ended.clone()));
        assert!(released.lease.is_none());
        assert!(released.pending_approvals.is_empty());
        let approval = released.hold_approval.as_ref().unwrap();
        assert_eq!(approval.status, ApprovalStatus::Aborted);
        assert_eq!(approval.decided_at.as_ref(), Some(&ended));
        let mut expected_history = history;
        expected_history[0].record.released_at = Some(ended.clone());
        assert_eq!(
            read_run_lease_history_tx(&mut check, &released.hold)
                .await
                .unwrap(),
            expected_history
        );
        assert_eq!(expected_history[0].record.id, lease.record.id);
        assert_eq!(
            expected_history[0].record.released_at.as_ref(),
            Some(&ended)
        );
        assert_eq!(terminal.lease(), &expected_lease);
        assert_eq!(
            compose_terminal_tx(&mut check, &terminal, &before)
                .await
                .unwrap(),
            TerminalCompositionOutcome::Conflict
        );
        assert_eq!(
            compose_terminal_tx(&mut check, &terminal, &released)
                .await
                .unwrap(),
            TerminalCompositionOutcome::ObservedApplied
        );
        check.commit().await.unwrap();
        let audit = reopened.list_audit().await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            (
                audit[0].seq,
                audit[0].ts.as_str(),
                audit[0].actor.as_str(),
                audit[0].action.as_str(),
                audit[0].prev_hash.as_str()
            ),
            (
                1,
                ended.as_str(),
                "legacy_recovery",
                "legacy_recovery.cancelled",
                crate::audit::GENESIS
            )
        );
        assert_eq!(
            audit[0].detail,
            serde_json::json!({"run_id":"run-strict","cohort_id":"cohort-strict","workspace_id":"ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5","result_state":"cancelled"})
        );
        assert_eq!(audit[0].hash.len(), 64);
        reopened.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn file_backed_terminal_rollback_restores_facts_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-rollback.db");
        let store = strict_facts_file(&path).await;
        usable(&store).await;
        let mut initial_tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let original = facts(&mut initial_tx).await;
        let original_history = read_run_lease_history_tx(&mut initial_tx, &original.hold)
            .await
            .unwrap();
        initial_tx.rollback().await.unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE legacy_recovery_holds SET reason_code='caller_owned_rollback' WHERE run_id='run-strict'")
            .execute(&mut *tx).await.unwrap();
        let before = facts(&mut tx).await;
        let terminal = composition(RunStatus::Cancelled, &before);
        let expected_lease = terminal.lease().clone();
        assert_eq!(
            compose_terminal_tx(&mut tx, &terminal, &before)
                .await
                .unwrap(),
            TerminalCompositionOutcome::Applied
        );
        tx.rollback().await.unwrap();
        store.pool().close().await;

        let reopened = crate::Store::open(&path).await.unwrap();
        let mut retry_tx = reopened.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let fresh = facts(&mut retry_tx).await;
        assert_eq!(fresh, original);
        assert_eq!(
            read_run_lease_history_tx(&mut retry_tx, &fresh.hold)
                .await
                .unwrap(),
            original_history
        );
        assert_eq!(terminal.lease(), &expected_lease);
        assert_eq!(
            compose_terminal_tx(&mut retry_tx, &terminal, &fresh)
                .await
                .unwrap(),
            TerminalCompositionOutcome::Applied
        );
        retry_tx.commit().await.unwrap();
        assert_eq!(reopened.list_audit().await.unwrap().len(), 1);
        reopened.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    async fn rejected_outer_commit_rolls_back_terminal_writes_on_its_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-commit-reject.db");
        let store = strict_facts_file(&path).await;
        usable(&store).await;
        let mut baseline_tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let baseline = facts(&mut baseline_tx).await;
        let history = read_run_lease_history_tx(&mut baseline_tx, &baseline.hold)
            .await
            .unwrap();
        baseline_tx.rollback().await.unwrap();
        let mut connection = store.pool().acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_commit_hook(|| false);
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE legacy_recovery_holds SET reason_code='caller_owned_reject' WHERE run_id='run-strict'")
            .execute(&mut *tx).await.unwrap();
        let before = facts(&mut tx).await;
        let terminal = composition(RunStatus::Cancelled, &before);
        let expected_lease = terminal.lease().clone();
        assert_eq!(
            compose_terminal_tx(&mut tx, &terminal, &before)
                .await
                .unwrap(),
            TerminalCompositionOutcome::Applied
        );
        assert!(tx.commit().await.is_err());
        drop(connection);
        store.pool().close().await;

        let reopened = crate::Store::open(&path).await.unwrap();
        let mut retry_tx = reopened.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let fresh = facts(&mut retry_tx).await;
        assert_eq!(fresh, baseline);
        assert_eq!(
            read_run_lease_history_tx(&mut retry_tx, &fresh.hold)
                .await
                .unwrap(),
            history
        );
        assert!(reopened.list_audit().await.unwrap().is_empty());
        assert_eq!(terminal.lease(), &expected_lease);
        assert_eq!(
            compose_terminal_tx(&mut retry_tx, &terminal, &fresh)
                .await
                .unwrap(),
            TerminalCompositionOutcome::Applied
        );
        retry_tx.commit().await.unwrap();
        assert_eq!(reopened.list_audit().await.unwrap().len(), 1);
        reopened.verify_audit_chain().await.unwrap();
    }

    #[derive(Debug)]
    struct WalContender {
        attempt: TerminalAttempt,
        outcome: TerminalCompositionOutcome,
        snapshot: TerminalReleaseSnapshot,
    }

    async fn wal_contender(
        store: crate::Store,
        barrier: Arc<Barrier>,
        attempt: TerminalAttempt,
        expected_lease: ExpectedLease,
    ) -> WalContender {
        // The gate is deliberately before the caller-owned write transaction.
        barrier.wait().await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        // This write belongs to the caller, not compose_terminal_tx's savepoint.
        sqlx::query(
            "UPDATE workspaces SET revision=revision+1 WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        // Read only after BEGIN IMMEDIATE has acquired the WAL writer lock.
        let snapshot = facts(&mut tx).await;
        let outcome = compose_terminal_tx(
            &mut tx,
            &CompositionAttempt::new(attempt.clone(), expected_lease),
            &snapshot,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        WalContender {
            attempt,
            outcome,
            snapshot,
        }
    }

    async fn seeded_terminal_wal_file(
        path: &Path,
    ) -> (TerminalReleaseSnapshot, Vec<WorkspaceLeaseRow>) {
        let seed = strict_facts_file(path).await;
        usable(&seed).await;
        // Preserve an already-released owner lease so the race proves history is
        // retained, rather than merely checking the one live lease disappears.
        sqlx::query(
            "INSERT INTO workspace_leases
             (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at,released_at)
             VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7',
                     'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','run-strict','run',
                     '2026-09-19T00:00:00.000Z','2026-09-19T00:00:00.000Z')",
        )
        .execute(seed.pool())
        .await
        .unwrap();
        let mut tx = seed.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let snapshot = facts(&mut tx).await;
        let history = read_run_lease_history_tx(&mut tx, &snapshot.hold)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        seed.pool().close().await;
        (snapshot, history)
    }

    async fn run_terminal_wal_race(
        path: &Path,
        attempts: [TerminalAttempt; 2],
    ) -> (
        TerminalReleaseSnapshot,
        Vec<WorkspaceLeaseRow>,
        [WalContender; 2],
    ) {
        let (baseline, history) = seeded_terminal_wal_file(path).await;
        // The attempt's lease precondition is immutable caller input, not a
        // value reconstructed from either contender's post-lock validation read.
        let expected_lease = baseline.lease.clone().map_or(ExpectedLease::None, |lease| {
            ExpectedLease::Exact(Box::new(lease))
        });
        // These are independently opened pools/connections over the same WAL file.
        let left = crate::Store::open(path).await.unwrap();
        let right = crate::Store::open(path).await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let (left_result, right_result) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                wal_contender(
                    left.clone(),
                    Arc::clone(&barrier),
                    attempts[0].clone(),
                    expected_lease.clone(),
                ),
                wal_contender(
                    right.clone(),
                    Arc::clone(&barrier),
                    attempts[1].clone(),
                    expected_lease.clone(),
                ),
            )
        })
        .await
        .unwrap();
        left.pool().close().await;
        right.pool().close().await;
        (baseline, history, [left_result, right_result])
    }

    fn expected_terminal_audit(
        hold: &LegacyRecoveryHold,
        attempt: &TerminalAttempt,
    ) -> TerminalAuditFacts {
        let (action, raw_detail) = audit_parts(hold, attempt).unwrap();
        TerminalAuditFacts {
            seq: 1,
            ts: attempt.ended_at().clone(),
            actor: "legacy_recovery".into(),
            prev_hash: crate::audit::GENESIS.into(),
            hash: crate::audit::entry_hash(
                crate::audit::GENESIS,
                attempt.ended_at().as_str(),
                "legacy_recovery",
                &action,
                &raw_detail,
            ),
            action,
            raw_detail,
        }
    }

    async fn assert_reopened_terminal_race_facts(
        path: &Path,
        baseline: &TerminalReleaseSnapshot,
        history: &[WorkspaceLeaseRow],
        attempt: &TerminalAttempt,
    ) {
        let reopened = crate::Store::open(path).await.unwrap();
        let mut check = reopened.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let actual = facts(&mut check).await;
        let audit = expected_terminal_audit(&baseline.hold, attempt);
        let mut expected = baseline.clone();
        expected.workspace.revision += 2; // both caller-owned commits persisted
        expected.run.run.status = attempt.result();
        expected.run.run.ended_at = Some(attempt.ended_at().as_str().to_owned());
        expected.hold.recovery_state = RecoveryState::Released;
        expected.hold.ready_at = None;
        expected.hold.active_resume_approval_id = None;
        expected.hold.released_at = Some(attempt.ended_at().clone());
        expected.hold_approval.as_mut().unwrap().status = ApprovalStatus::Aborted;
        expected.hold_approval.as_mut().unwrap().decision = None;
        expected.hold_approval.as_mut().unwrap().decided_at = Some(attempt.ended_at().clone());
        expected.pending_approvals.clear();
        expected.cohort.completed_at = Some(attempt.ended_at().clone());
        expected.lease = None;
        expected.latest_audit = Some(audit.clone());
        assert_eq!(actual, expected);
        assert_eq!(
            read_all_approvals_tx(&mut check, "run-strict")
                .await
                .unwrap(),
            vec![expected.hold_approval.clone().unwrap()]
        );
        let mut expected_history = history.to_vec();
        expected_history
            .iter_mut()
            .find(|lease| lease.record.released_at.is_none())
            .unwrap()
            .record
            .released_at = Some(attempt.ended_at().clone());
        assert_eq!(
            read_run_lease_history_tx(&mut check, &actual.hold)
                .await
                .unwrap(),
            expected_history
        );
        check.commit().await.unwrap();
        assert_eq!(reopened.list_audit().await.unwrap().len(), 1);
        reopened.verify_audit_chain().await.unwrap();
    }

    fn assert_fresh_wal_snapshots(contenders: &[WalContender; 2]) {
        assert_eq!(
            contenders
                .iter()
                .filter(|contender| contender.snapshot.run.run.status == RunStatus::Running)
                .count(),
            1
        );
        assert_eq!(
            contenders
                .iter()
                .filter(
                    |contender| contender.snapshot.hold.recovery_state() == RecoveryState::Released
                )
                .count(),
            1
        );
        let mut revisions = contenders
            .iter()
            .map(|contender| contender.snapshot.workspace.revision)
            .collect::<Vec<_>>();
        revisions.sort_unstable();
        assert_eq!(revisions, vec![2, 3]);
    }

    #[tokio::test]
    async fn file_backed_wal_identical_terminal_attempt_applies_once_and_observes_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-identical-wal.db");
        let attempt = attempt(RunStatus::Cancelled);
        let (baseline, history, contenders) =
            run_terminal_wal_race(&path, [attempt.clone(), attempt.clone()]).await;
        assert_eq!(
            contenders
                .iter()
                .filter(|contender| contender.outcome == TerminalCompositionOutcome::Applied)
                .count(),
            1
        );
        assert_eq!(
            contenders
                .iter()
                .filter(|contender| contender.outcome == TerminalCompositionOutcome::ObservedApplied)
                .count(),
            1
        );
        assert_fresh_wal_snapshots(&contenders);
        assert_reopened_terminal_race_facts(&path, &baseline, &history, &attempt).await;
    }

    #[tokio::test]
    async fn file_backed_wal_different_terminal_timestamps_apply_once_and_conflict_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-different-wal.db");
        let first = attempt(RunStatus::Cancelled);
        let second = TerminalAttempt::new(
            RunStatus::Cancelled,
            WorkspaceInstant::parse("2026-09-19T00:00:02.000Z").unwrap(),
        );
        let (baseline, history, contenders) =
            run_terminal_wal_race(&path, [first.clone(), second.clone()]).await;
        assert_eq!(
            contenders
                .iter()
                .filter(|contender| contender.outcome == TerminalCompositionOutcome::Applied)
                .count(),
            1
        );
        assert_eq!(
            contenders
                .iter()
                .filter(|contender| contender.outcome == TerminalCompositionOutcome::Conflict)
                .count(),
            1
        );
        assert_fresh_wal_snapshots(&contenders);
        let winner = contenders
            .iter()
            .find(|contender| contender.outcome == TerminalCompositionOutcome::Applied)
            .unwrap();
        assert_reopened_terminal_race_facts(&path, &baseline, &history, &winner.attempt).await;
    }
}
