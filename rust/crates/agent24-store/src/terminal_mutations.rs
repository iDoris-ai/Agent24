#![allow(dead_code)]
use crate::{
    LegacyRecoveryHold, RecoveryState, Store, WorkspaceInstant, WorkspaceLeaseId,
    WorkspaceLeaseRow, WorkspaceResult, WorkspaceStoreError,
    legacy_recovery::read_hold_tx,
    terminal_helpers::{
        TerminalApprovalFacts, TerminalAuditFacts, read_all_approvals_tx, read_audit_tx,
        read_cohort_tx, verify_audit_tx,
    },
};
use agent24_protocol::RunStatus;
use sqlx::{Sqlite, Transaction};
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TerminalMutation<T> {
    Applied(T),
    Conflict,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalAuditInput {
    pub(crate) run_id: String,
    pub(crate) cohort_id: String,
    pub(crate) workspace_id: String,
    pub(crate) result: RunStatus,
    pub(crate) ended_at: WorkspaceInstant,
}
fn bad(table: &'static str, field: &'static str) -> WorkspaceStoreError {
    WorkspaceStoreError::CorruptRow { table, field }
}
fn terminal_action(status: RunStatus) -> Option<&'static str> {
    match status {
        RunStatus::Completed => Some("completed"),
        RunStatus::Failed => Some("failed"),
        RunStatus::Cancelled => Some("cancelled"),
        RunStatus::Queued | RunStatus::Running | RunStatus::AwaitingApproval => None,
    }
}
fn run_status_text(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::AwaitingApproval => "awaiting_approval",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

pub(crate) async fn abort_all_pending_terminal_approvals_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    ended_at: &WorkspaceInstant,
) -> WorkspaceResult<Vec<TerminalApprovalFacts>> {
    let mut expected = read_all_approvals_tx(tx, run_id).await?;
    let pending = expected
        .iter()
        .filter(|row| row.status == agent24_protocol::ApprovalStatus::Pending)
        .count();
    for row in expected
        .iter_mut()
        .filter(|row| row.status == agent24_protocol::ApprovalStatus::Pending)
    {
        row.status = agent24_protocol::ApprovalStatus::Aborted;
        row.decision = None;
        row.decided_at = Some(ended_at.clone());
    }
    let changed = sqlx::query(
        "UPDATE approvals SET status='aborted',decision=NULL,decided_at=?
         WHERE run_id=? COLLATE BINARY AND status='pending' COLLATE BINARY
           AND decision IS NULL AND decided_at IS NULL",
    )
    .bind(ended_at.as_str())
    .bind(run_id)
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if changed.rows_affected() != pending as u64 {
        return Err(bad("approvals", "row"));
    }
    (read_all_approvals_tx(tx, run_id).await? == expected)
        .then_some(expected)
        .ok_or_else(|| bad("approvals", "row"))
}
async fn read_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &WorkspaceLeaseId,
) -> WorkspaceResult<Option<WorkspaceLeaseRow>> {
    sqlx::query("SELECT * FROM workspace_leases WHERE lease_id=? COLLATE BINARY")
        .bind(id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .map(|row| WorkspaceLeaseRow::decode(&row))
        .transpose()
}
pub(crate) async fn release_exact_terminal_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    lease: &WorkspaceLeaseRow,
    ended_at: &WorkspaceInstant,
) -> WorkspaceResult<TerminalMutation<WorkspaceLeaseRow>> {
    let r = &lease.record;
    let changed = sqlx::query(
        "UPDATE workspace_leases SET released_at=? WHERE lease_id=? COLLATE BINARY
         AND workspace_id=? COLLATE BINARY AND root_generation=? COLLATE BINARY
         AND owner_id=? COLLATE BINARY AND kind='run' COLLATE BINARY AND released_at IS NULL",
    )
    .bind(ended_at.as_str())
    .bind(r.id.as_str())
    .bind(r.workspace_id.as_str())
    .bind(&r.root_generation)
    .bind(&r.owner_id)
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if changed.rows_affected() != 1 {
        return Ok(TerminalMutation::Conflict);
    }
    let mut expected = lease.clone();
    expected.record.released_at = Some(ended_at.clone());
    match read_lease_tx(tx, &r.id).await? {
        Some(actual) if actual == expected => Ok(TerminalMutation::Applied(expected)),
        _ => Err(bad("workspace_leases", "row")),
    }
}
pub(crate) async fn append_terminal_audit_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &TerminalAuditInput,
) -> WorkspaceResult<TerminalMutation<TerminalAuditFacts>> {
    let Some(result) = terminal_action(input.result) else {
        return Ok(TerminalMutation::Conflict);
    };
    let action = format!("legacy_recovery.{result}");
    let detail = serde_json::json!({"run_id": input.run_id, "cohort_id": input.cohort_id,
        "workspace_id": input.workspace_id, "result_state": result});
    let raw_detail = serde_json::to_string(&detail).map_err(|_| WorkspaceStoreError::Database)?;
    if let Some(seq) =
        sqlx::query_scalar::<_, i64>("SELECT seq FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
    {
        read_audit_tx(tx, seq).await?;
    }
    let appended = Store::append_audit_tx(
        tx,
        input.ended_at.as_str(),
        "legacy_recovery",
        &action,
        &detail,
    )
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if appended.ts != input.ended_at.as_str()
        || appended.actor != "legacy_recovery"
        || appended.action != action
        || appended.detail != detail
    {
        return Err(bad("audit_log", "row"));
    }
    let expected = TerminalAuditFacts {
        seq: appended.seq,
        ts: input.ended_at.clone(),
        actor: "legacy_recovery".into(),
        action,
        raw_detail,
        prev_hash: appended.prev_hash,
        hash: appended.hash,
    };
    if read_audit_tx(tx, expected.seq).await? != expected {
        return Err(bad("audit_log", "row"));
    }
    verify_audit_tx(tx, &expected).await?;
    Ok(TerminalMutation::Applied(expected))
}
fn expected_released_hold(
    held: &LegacyRecoveryHold,
    ended_at: &WorkspaceInstant,
) -> LegacyRecoveryHold {
    let mut expected = held.clone();
    expected.recovery_state = RecoveryState::Released;
    expected.ready_at = None;
    expected.active_resume_approval_id = None;
    expected.released_at = Some(ended_at.clone());
    expected
}
pub(crate) async fn release_terminal_hold_tx(
    tx: &mut Transaction<'_, Sqlite>,
    held: &LegacyRecoveryHold,
    ended_at: &WorkspaceInstant,
) -> WorkspaceResult<TerminalMutation<LegacyRecoveryHold>> {
    if held.recovery_state == RecoveryState::Released {
        return Ok(TerminalMutation::Conflict);
    }
    let expected = expected_released_hold(held, ended_at);
    let changed = sqlx::query(
        "UPDATE legacy_recovery_holds SET recovery_state='released',ready_at=NULL,
             released_at=?,active_resume_approval_id=NULL
         WHERE run_id=? COLLATE BINARY AND cohort_id=? COLLATE BINARY
           AND workspace_id=? COLLATE BINARY AND root_generation=? COLLATE BINARY
           AND original_status=? COLLATE BINARY AND recovery_state=? COLLATE BINARY
           AND approval_id IS ? AND ready_at IS ? AND reason_code IS ?
           AND released_at IS NULL AND active_resume_approval_id IS ?",
    )
    .bind(ended_at.as_str())
    .bind(held.run_id())
    .bind(held.cohort_id())
    .bind(held.workspace_id().as_str())
    .bind(held.root_generation())
    .bind(run_status_text(held.original_status()))
    .bind(held.recovery_state().as_str())
    .bind(held.approval_id())
    .bind(held.ready_at().map(WorkspaceInstant::as_str))
    .bind(held.reason_code())
    .bind(held.active_resume_approval_id())
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if changed.rows_affected() != 1 {
        return Ok(TerminalMutation::Conflict);
    }
    match read_hold_tx(tx, held.run_id()).await? {
        actual if actual == expected => Ok(TerminalMutation::Applied(expected)),
        _ => Err(bad("legacy_recovery_holds", "row")),
    }
}
pub(crate) async fn close_terminal_cohort_if_last_tx(
    tx: &mut Transaction<'_, Sqlite>,
    held: &LegacyRecoveryHold,
    ended_at: &WorkspaceInstant,
) -> WorkspaceResult<TerminalMutation<Option<crate::terminal_helpers::TerminalCohortFacts>>> {
    if held.recovery_state != RecoveryState::Released
        || held.released_at.as_ref() != Some(ended_at)
        || read_hold_tx(tx, held.run_id()).await? != *held
    {
        return Ok(TerminalMutation::Conflict);
    }
    let cohort = read_cohort_tx(tx, held).await?;
    if cohort.completed_at.is_some() {
        return Ok(TerminalMutation::Conflict);
    }
    let (open, latest): (i64, Option<String>) = sqlx::query_as(
        "SELECT count(CASE WHEN released_at IS NULL THEN 1 END), max(released_at)
         FROM legacy_recovery_holds WHERE cohort_id=? COLLATE BINARY",
    )
    .bind(held.cohort_id())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if open != 0 {
        return Ok(TerminalMutation::Applied(None));
    }
    if latest.as_deref().is_some_and(|at| at > ended_at.as_str()) {
        return Ok(TerminalMutation::Conflict);
    }
    let mut expected = cohort.clone();
    expected.completed_at = Some(ended_at.clone());
    let changed = sqlx::query(
        "UPDATE legacy_recovery_cohorts SET completed_at=? WHERE cohort_id=? COLLATE BINARY
         AND migration_version=? AND legacy_workspace_id=? COLLATE BINARY
         AND root_generation=? COLLATE BINARY AND created_at=? COLLATE BINARY
         AND completed_at IS NULL",
    )
    .bind(ended_at.as_str())
    .bind(&cohort.cohort_id)
    .bind(cohort.migration_version as i64)
    .bind(cohort.workspace_id.as_str())
    .bind(&cohort.root_generation)
    .bind(cohort.created_at.as_str())
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if changed.rows_affected() != 1 {
        return Ok(TerminalMutation::Conflict);
    }
    match read_cohort_tx(tx, held).await? {
        actual if actual == expected => Ok(TerminalMutation::Applied(Some(expected))),
        _ => Err(bad("legacy_recovery_cohorts", "row")),
    }
}
pub(crate) async fn observe_released_terminal_tx(
    tx: &mut Transaction<'_, Sqlite>,
    held: &LegacyRecoveryHold,
    ended_at: &WorkspaceInstant,
    expected_lease_id: Option<&WorkspaceLeaseId>,
) -> WorkspaceResult<TerminalMutation<()>> {
    let current = read_hold_tx(tx, held.run_id()).await?;
    if current != expected_released_hold(held, ended_at) {
        return Ok(TerminalMutation::Conflict);
    }
    let leases = sqlx::query(
        "SELECT * FROM workspace_leases WHERE owner_id=? COLLATE BINARY AND kind='run' COLLATE BINARY
         ORDER BY lease_id COLLATE BINARY",
    )
    .bind(held.run_id())
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    let leases: Vec<WorkspaceLeaseRow> = leases
        .iter()
        .map(WorkspaceLeaseRow::decode)
        .collect::<WorkspaceResult<_>>()?;
    if leases
        .iter()
        .any(|lease| lease.record.released_at.is_none())
    {
        return Ok(TerminalMutation::Conflict);
    }
    if let Some(id) = expected_lease_id {
        let exact = leases
            .iter()
            .filter(|lease| lease.record.id == *id)
            .collect::<Vec<_>>();
        if exact.len() != 1
            || exact[0].record.workspace_id != *held.workspace_id()
            || exact[0].record.root_generation != held.root_generation()
            || exact[0].record.released_at.as_ref() != Some(ended_at)
        {
            return Ok(TerminalMutation::Conflict);
        }
    }
    let cohort = read_cohort_tx(tx, &current).await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM legacy_recovery_holds WHERE cohort_id=? COLLATE BINARY AND released_at IS NULL",
    )
    .bind(current.cohort_id())
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    let complete = if open == 0 {
        cohort
            .completed_at
            .as_ref()
            .is_some_and(|at| at >= ended_at)
    } else {
        cohort.completed_at.is_none()
    };
    Ok(if complete {
        TerminalMutation::Applied(())
    } else {
        TerminalMutation::Conflict
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    async fn facts(store: &Store) -> (LegacyRecoveryHold, WorkspaceLeaseRow) {
        let mut tx = store.pool().begin().await.unwrap();
        let hold = read_hold_tx(&mut tx, "run-strict").await.unwrap();
        let lease = read_lease_tx(
            &mut tx,
            &WorkspaceLeaseId::parse("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        tx.commit().await.unwrap();
        (hold, lease)
    }

    #[tokio::test]
    async fn mutations_reread_triggered_rows_and_classify_illegal_audit() {
        let store = crate::legacy_recovery::tests::strict_facts_fixture().await;
        sqlx::raw_sql("UPDATE runs SET input='{\"prompt\":\"x\"}',usage='{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}'; CREATE TRIGGER twist AFTER UPDATE OF status ON approvals BEGIN UPDATE approvals SET summary='twist' WHERE id=NEW.id; END;").execute(store.pool()).await.unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(matches!(
            abort_all_pending_terminal_approvals_tx(
                &mut tx,
                "run-strict",
                &WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap()
            )
            .await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "approvals",
                ..
            })
        ));
        tx.rollback().await.unwrap();
        sqlx::query("DROP TRIGGER twist")
            .execute(store.pool())
            .await
            .unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let ended = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        assert!(matches!(
            append_terminal_audit_tx(
                &mut tx,
                &TerminalAuditInput {
                    run_id: "run-strict".into(),
                    cohort_id: "cohort-strict".into(),
                    workspace_id: "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5".into(),
                    result: RunStatus::Queued,
                    ended_at: ended
                }
            )
            .await
            .unwrap(),
            TerminalMutation::Conflict
        ));
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn observation_ignores_other_hold_lease_and_allows_later_completion() {
        let store = crate::legacy_recovery::tests::strict_facts_fixture().await;
        let (hold, _) = facts(&store).await;
        sqlx::raw_sql("UPDATE legacy_recovery_holds SET recovery_state='released',released_at='2026-09-19T00:00:01.000Z'; UPDATE workspace_leases SET released_at='2026-09-19T00:00:01.000Z'; INSERT INTO runs (id,status,input,usage,created_at,workspace_id) VALUES ('other','running','{\"prompt\":\"x\"}','{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'); INSERT INTO legacy_recovery_holds (run_id,cohort_id,workspace_id,root_generation,original_status,recovery_state,reason_code) VALUES ('other','cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','running','needs_attention','x'); INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','other','run','2026-09-19T00:00:00.000Z');").execute(store.pool()).await.unwrap();
        let mut tx = store.pool().begin().await.unwrap();
        let result = observe_released_terminal_tx(
            &mut tx,
            &hold,
            &WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap(),
            Some(&WorkspaceLeaseId::parse("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6").unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(result, TerminalMutation::Applied(()));
        tx.rollback().await.unwrap();
        sqlx::raw_sql("UPDATE legacy_recovery_holds SET recovery_state='released',released_at='2026-09-19T00:00:02.000Z' WHERE run_id='other'; UPDATE workspace_leases SET released_at='2026-09-19T00:00:02.000Z' WHERE owner_id='other'; UPDATE legacy_recovery_cohorts SET completed_at='2026-09-19T00:00:02.000Z';").execute(store.pool()).await.unwrap();
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(
            observe_released_terminal_tx(
                &mut tx,
                &hold,
                &WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap(),
                None
            )
            .await
            .unwrap(),
            TerminalMutation::Applied(())
        );
        tx.rollback().await.unwrap();
        for statement in [
            "UPDATE legacy_recovery_holds SET reason_code='tampered' WHERE run_id='run-strict'",
            "UPDATE legacy_recovery_cohorts SET completed_at=NULL",
            "UPDATE legacy_recovery_cohorts SET completed_at='2026-09-19T00:00:00.000Z'",
        ] {
            sqlx::raw_sql(statement)
                .execute(store.pool())
                .await
                .unwrap();
            let mut tx = store.pool().begin().await.unwrap();
            assert_eq!(
                observe_released_terminal_tx(
                    &mut tx,
                    &hold,
                    &WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap(),
                    None
                )
                .await
                .unwrap(),
                TerminalMutation::Conflict
            );
            tx.rollback().await.unwrap();
            sqlx::raw_sql("UPDATE legacy_recovery_holds SET reason_code='legacy_reason' WHERE run_id='run-strict'; UPDATE legacy_recovery_cohorts SET completed_at='2026-09-19T00:00:02.000Z';").execute(store.pool()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn lease_release_rejects_acquired_at_tampering() {
        let store = crate::legacy_recovery::tests::strict_facts_fixture().await;
        let (_, lease) = facts(&store).await;
        let ended = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        let mut expected = lease.clone();
        expected.record.released_at = Some(ended.clone());
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let actual = release_exact_terminal_lease_tx(&mut tx, &lease, &ended)
            .await
            .unwrap();
        assert_eq!(actual, TerminalMutation::Applied(expected.clone()));
        let reread = read_lease_tx(&mut tx, &lease.record.id).await.unwrap();
        assert_eq!(reread, Some(expected));
        tx.rollback().await.unwrap();
        sqlx::raw_sql("CREATE TRIGGER twist_lease AFTER UPDATE OF released_at ON workspace_leases BEGIN UPDATE workspace_leases SET acquired_at='2026-09-19T00:00:00.001Z' WHERE lease_id=NEW.lease_id; END;").execute(store.pool()).await.unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(matches!(
            release_exact_terminal_lease_tx(&mut tx, &lease, &ended).await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                ..
            })
        ));
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn hold_and_last_cohort_writes_are_cas_reread() {
        let store = crate::legacy_recovery::tests::strict_facts_fixture().await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let held = read_hold_tx(&mut tx, "run-strict").await.unwrap();
        let ended = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        let released = match release_terminal_hold_tx(&mut tx, &held, &ended)
            .await
            .unwrap()
        {
            TerminalMutation::Applied(value) => value,
            TerminalMutation::Conflict => panic!("fresh hold must release"),
        };
        assert_eq!(read_hold_tx(&mut tx, "run-strict").await.unwrap(), released);
        sqlx::raw_sql(
            "INSERT INTO runs (id,status,input,usage,created_at,workspace_id)
             VALUES ('later','running','{}','{}','2026-09-19T00:00:00.000Z',
                     'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5');
             INSERT INTO legacy_recovery_holds
             (run_id,cohort_id,workspace_id,root_generation,original_status,recovery_state,released_at)
             VALUES ('later','cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5',
                     'g1','running','released','2026-09-19T00:00:02.000Z');",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        assert_eq!(
            close_terminal_cohort_if_last_tx(&mut tx, &released, &ended)
                .await
                .unwrap(),
            TerminalMutation::Conflict
        );
        sqlx::query("UPDATE legacy_recovery_holds SET released_at=? WHERE run_id='later'")
            .bind(ended.as_str())
            .execute(&mut *tx)
            .await
            .unwrap();
        let closed = close_terminal_cohort_if_last_tx(&mut tx, &released, &ended)
            .await
            .unwrap();
        assert!(matches!(closed, TerminalMutation::Applied(Some(_))));
        assert!(matches!(
            release_terminal_hold_tx(&mut tx, &held, &ended)
                .await
                .unwrap(),
            TerminalMutation::Conflict
        ));
    }

    #[tokio::test]
    async fn approvals_are_run_scoped_and_audit_expected_is_not_reread() {
        let store = crate::legacy_recovery::tests::strict_facts_fixture().await;
        sqlx::raw_sql("INSERT INTO approvals (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,expires_at,created_at,decided_at,workspace_id) VALUES ('extra','run-strict','t2','exec','s','{}','[]','pending','2026-09-20T00:00:00.000Z','2026-09-19T00:00:00.000Z',NULL,'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'),('resolved','run-strict','t3','exec','done','{}','[]','aborted','2026-09-20T00:00:00.000Z','2026-09-19T00:00:00.000Z','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'); INSERT INTO runs (id,status,input,usage,created_at,workspace_id) VALUES ('cross','running','{\"prompt\":\"x\"}','{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0}','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'); INSERT INTO approvals (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,expires_at,created_at,workspace_id) VALUES ('cross-a','cross','t','exec','s','{}','[]','pending','2026-09-20T00:00:00.000Z','2026-09-19T00:00:00.000Z','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5');").execute(store.pool()).await.unwrap();
        let ended = WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let before = read_all_approvals_tx(&mut tx, "run-strict").await.unwrap();
        let rows = abort_all_pending_terminal_approvals_tx(&mut tx, "run-strict", &ended)
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row.status == agent24_protocol::ApprovalStatus::Aborted)
                .count(),
            3
        );
        assert_eq!(
            rows.iter().find(|row| row.id == "resolved"),
            before.iter().find(|row| row.id == "resolved")
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM approvals WHERE id='cross-a'")
                .fetch_one(&mut *tx)
                .await
                .unwrap(),
            "pending"
        );
        tx.rollback().await.unwrap();
        let input = TerminalAuditInput {
            run_id: "r".into(),
            cohort_id: "c".into(),
            workspace_id: "w".into(),
            result: RunStatus::Completed,
            ended_at: ended,
        };
        for status in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
            let mut next = input.clone();
            next.result = status;
            assert!(matches!(
                append_terminal_audit_tx(&mut tx, &next).await.unwrap(),
                TerminalMutation::Applied(_)
            ));
            tx.rollback().await.unwrap();
        }
        let raw = "{\"tampered\":true}";
        let hash = crate::audit::entry_hash(
            crate::audit::GENESIS,
            input.ended_at.as_str(),
            "legacy_recovery",
            "legacy_recovery.completed",
            raw,
        );
        sqlx::raw_sql(&format!("CREATE TRIGGER audit_twist AFTER INSERT ON audit_log BEGIN UPDATE audit_log SET detail='{raw}',hash='{hash}' WHERE seq=NEW.seq; END;")).execute(store.pool()).await.unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(matches!(
            append_terminal_audit_tx(&mut tx, &input).await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "audit_log",
                ..
            })
        ));
        tx.rollback().await.unwrap();
        sqlx::raw_sql("DROP TRIGGER audit_twist; INSERT INTO audit_log (ts,actor,action,detail,prev_hash,hash) VALUES ('2026-09-19T00:00:00.000Z','a','x','{}','genesis',zeroblob(64))").execute(store.pool()).await.unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
            .fetch_one(store.pool())
            .await
            .unwrap();
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(matches!(
            append_terminal_audit_tx(&mut tx, &input).await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "audit_log",
                field: "hash"
            })
        ));
        tx.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log")
                .fetch_one(store.pool())
                .await
                .unwrap(),
            before
        );
    }
}
