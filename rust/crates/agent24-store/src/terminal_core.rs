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
    use crate::legacy_recovery::tests::strict_facts_fixture;
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
}
