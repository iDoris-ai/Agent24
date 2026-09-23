#![allow(dead_code)]
use crate::{
    LeaseKind, LegacyRecoveryHold, WorkspaceInstant, WorkspaceKind, WorkspaceLeaseRow,
    WorkspaceResult, WorkspaceRow, WorkspaceStoreError, workspace_decode_support,
};
use agent24_protocol::{
    ApprovalStatus, Decision, ErrorBody, Run, RunInput, RunOutput, RunStatus, Usage, WorkspaceId,
};
use serde_json::{Map, Value};
use sqlx::{Row, Sqlite, Transaction, sqlite::SqliteRow};
fn bad(table: &'static str, field: &'static str) -> WorkspaceStoreError {
    workspace_decode_support::bad_table(table, field)
}
fn text(row: &SqliteRow, table: &'static str, field: &'static str) -> WorkspaceResult<String> {
    workspace_decode_support::text(row, field).map_err(|_| bad(table, field))
}
fn opt_text(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<Option<String>> {
    workspace_decode_support::opt_text(row, field)
        .map_err(|_| bad(table, field))
        .and_then(|value| {
            (!value.as_deref().is_some_and(str::is_empty))
                .then_some(value)
                .ok_or_else(|| bad(table, field))
        })
}
fn instant(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<WorkspaceInstant> {
    workspace_decode_support::instant(row, field).map_err(|_| bad(table, field))
}
fn opt_instant(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<Option<WorkspaceInstant>> {
    workspace_decode_support::opt_instant(row, field).map_err(|_| bad(table, field))
}
fn json<T: serde::de::DeserializeOwned>(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<T> {
    serde_json::from_str(&text(row, table, field)?).map_err(|_| bad(table, field))
}
fn optional_json<T: serde::de::DeserializeOwned>(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<Option<T>> {
    opt_text(row, table, field)?
        .map(|value| serde_json::from_str(&value).map_err(|_| bad(table, field)))
        .transpose()
}
fn run_status(row: &SqliteRow) -> WorkspaceResult<RunStatus> {
    serde_json::from_value(serde_json::Value::String(text(row, "runs", "status")?))
        .map_err(|_| bad("runs", "status"))
}
fn approval_status(row: &SqliteRow) -> WorkspaceResult<ApprovalStatus> {
    serde_json::from_value(serde_json::Value::String(text(row, "approvals", "status")?))
        .map_err(|_| bad("approvals", "status"))
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalRunFacts {
    pub(crate) run: Run,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) created_at: WorkspaceInstant,
}
fn decode_run(row: &SqliteRow) -> WorkspaceResult<TerminalRunFacts> {
    let created_at = instant(row, "runs", "created_at")?;
    let started_at = opt_instant(row, "runs", "started_at")?;
    let ended_at = opt_instant(row, "runs", "ended_at")?;
    if started_at.as_ref().is_some_and(|value| value < &created_at)
        || ended_at.as_ref().is_some_and(|value| value < &created_at)
        || matches!((&started_at, &ended_at), (Some(started), Some(ended)) if ended < started)
    {
        return Err(bad("runs", "timestamp"));
    }
    let workspace_id = opt_text(row, "runs", "workspace_id")?
        .ok_or_else(|| bad("runs", "workspace_id"))
        .and_then(|value| WorkspaceId::parse(value).map_err(|_| bad("runs", "workspace_id")))?;
    let run = Run {
        id: text(row, "runs", "id")?,
        session_id: opt_text(row, "runs", "session_id")?,
        status: run_status(row)?,
        input: json::<RunInput>(row, "runs", "input")?,
        output: optional_json::<RunOutput>(row, "runs", "output")?,
        error: optional_json::<ErrorBody>(row, "runs", "error")?,
        usage: json::<Usage>(row, "runs", "usage")?,
        schedule_id: opt_text(row, "runs", "schedule_id")?,
        created_at: created_at.as_str().to_owned(),
        started_at: started_at.as_ref().map(|value| value.as_str().to_owned()),
        ended_at: ended_at.as_ref().map(|value| value.as_str().to_owned()),
    };
    Ok(TerminalRunFacts {
        run,
        workspace_id,
        created_at,
    })
}
pub(crate) async fn read_terminal_run_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> WorkspaceResult<TerminalRunFacts> {
    let row = sqlx::query(
        "SELECT id,session_id,status,input,output,error,usage,schedule_id,created_at,started_at,ended_at,workspace_id
         FROM runs WHERE id=? COLLATE BINARY LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::NotFound)?;
    let facts = decode_run(&row)?;
    (facts.run.id == run_id)
        .then_some(facts)
        .ok_or_else(|| bad("runs", "id"))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalCohortFacts {
    pub(crate) cohort_id: String,
    pub(crate) migration_version: u64,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) root_generation: String,
    pub(crate) created_at: WorkspaceInstant,
    pub(crate) completed_at: Option<WorkspaceInstant>,
}
pub(crate) async fn read_cohort_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hold: &LegacyRecoveryHold,
) -> WorkspaceResult<TerminalCohortFacts> {
    let row = sqlx::query(
        "SELECT cohort_id,migration_version,legacy_workspace_id,root_generation,created_at,completed_at
         FROM legacy_recovery_cohorts WHERE cohort_id=? COLLATE BINARY LIMIT 1",
    )
    .bind(hold.cohort_id())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or_else(|| bad("legacy_recovery_holds", "cohort_id"))?;
    let migration_version = workspace_decode_support::count(&row, "migration_version")
        .map_err(|_| bad("legacy_recovery_cohorts", "migration_version"))?;
    if migration_version == 0 {
        return Err(bad("legacy_recovery_cohorts", "migration_version"));
    }
    let facts = TerminalCohortFacts {
        cohort_id: text(&row, "legacy_recovery_cohorts", "cohort_id")?,
        migration_version,
        workspace_id: WorkspaceId::parse(text(
            &row,
            "legacy_recovery_cohorts",
            "legacy_workspace_id",
        )?)
        .map_err(|_| bad("legacy_recovery_cohorts", "legacy_workspace_id"))?,
        root_generation: workspace_decode_support::nonblank(&row, "root_generation")
            .map_err(|_| bad("legacy_recovery_cohorts", "root_generation"))?,
        created_at: instant(&row, "legacy_recovery_cohorts", "created_at")?,
        completed_at: opt_instant(&row, "legacy_recovery_cohorts", "completed_at")?,
    };
    if facts.cohort_id != hold.cohort_id()
        || facts.workspace_id != *hold.workspace_id()
        || facts.root_generation != hold.root_generation()
        || facts
            .completed_at
            .as_ref()
            .is_some_and(|value| value < &facts.created_at)
    {
        return Err(bad("legacy_recovery_cohorts", "row"));
    }
    Ok(facts)
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalApprovalFacts {
    pub(crate) id: String,
    pub(crate) run_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) kind: String,
    pub(crate) summary: String,
    pub(crate) payload: Map<String, Value>,
    pub(crate) available_decisions: Vec<String>,
    pub(crate) standing_target: Option<String>,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) status: ApprovalStatus,
    pub(crate) decision: Option<Decision>,
    pub(crate) created_at: WorkspaceInstant,
    pub(crate) expires_at: WorkspaceInstant,
    pub(crate) decided_at: Option<WorkspaceInstant>,
}
fn decode_approval(row: &SqliteRow) -> WorkspaceResult<TerminalApprovalFacts> {
    let created_at = instant(row, "approvals", "created_at")?;
    let expires_at = instant(row, "approvals", "expires_at")?;
    let decided_at = opt_instant(row, "approvals", "decided_at")?;
    let status = approval_status(row)?;
    let decision = optional_json::<Decision>(row, "approvals", "decision")?;
    let available_decisions = json::<Vec<String>>(row, "approvals", "available_decisions")?;
    if expires_at < created_at
        || decided_at.as_ref().is_some_and(|value| value < &created_at)
        || (status == ApprovalStatus::Pending && (decision.is_some() || decided_at.is_some()))
        || (status != ApprovalStatus::Pending && decided_at.is_none())
    {
        return Err(bad("approvals", "timestamp"));
    }
    Ok(TerminalApprovalFacts {
        id: text(row, "approvals", "id")?,
        run_id: text(row, "approvals", "run_id")?,
        tool_call_id: text(row, "approvals", "tool_call_id")?,
        kind: text(row, "approvals", "kind")?,
        summary: text(row, "approvals", "summary")?,
        payload: json::<Map<String, Value>>(row, "approvals", "payload")?,
        available_decisions,
        standing_target: opt_text(row, "approvals", "standing_target")?,
        workspace_id: opt_text(row, "approvals", "workspace_id")?
            .ok_or_else(|| bad("approvals", "workspace_id"))
            .and_then(|value| {
                WorkspaceId::parse(value).map_err(|_| bad("approvals", "workspace_id"))
            })?,
        status,
        decision,
        created_at,
        expires_at,
        decided_at,
    })
}
pub(crate) async fn read_approval_tx(
    tx: &mut Transaction<'_, Sqlite>,
    approval_id: &str,
) -> WorkspaceResult<TerminalApprovalFacts> {
    let row = sqlx::query(
        "SELECT id,run_id,tool_call_id,kind,summary,payload,available_decisions,standing_target,
                workspace_id,status,decision,created_at,expires_at,decided_at
         FROM approvals WHERE id=? COLLATE BINARY LIMIT 1",
    )
    .bind(approval_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or_else(|| bad("approvals", "id"))?;
    let facts = decode_approval(&row)?;
    (facts.id == approval_id)
        .then_some(facts)
        .ok_or_else(|| bad("approvals", "id"))
}
pub(crate) async fn read_pending_approvals_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> WorkspaceResult<Vec<TerminalApprovalFacts>> {
    let rows = sqlx::query(
        "SELECT id,run_id,tool_call_id,kind,summary,payload,available_decisions,standing_target,
                workspace_id,status,decision,created_at,expires_at,decided_at
         FROM approvals WHERE run_id=? COLLATE BINARY AND status='pending' COLLATE BINARY
         ORDER BY created_at COLLATE BINARY, id COLLATE BINARY",
    )
    .bind(run_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    rows.into_iter()
        .map(|row| {
            let facts = decode_approval(&row)?;
            (facts.run_id == run_id && facts.status == ApprovalStatus::Pending)
                .then_some(facts)
                .ok_or_else(|| bad("approvals", "row"))
        })
        .collect()
}
pub(crate) async fn read_all_approvals_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> WorkspaceResult<Vec<TerminalApprovalFacts>> {
    let rows = sqlx::query(
        "SELECT id,run_id,tool_call_id,kind,summary,payload,available_decisions,standing_target,
                workspace_id,status,decision,created_at,expires_at,decided_at
         FROM approvals WHERE run_id=? COLLATE BINARY
         ORDER BY created_at COLLATE BINARY, id COLLATE BINARY",
    )
    .bind(run_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    rows.into_iter()
        .map(|row| {
            let facts = decode_approval(&row)?;
            (facts.run_id == run_id)
                .then_some(facts)
                .ok_or_else(|| bad("approvals", "row"))
        })
        .collect()
}
pub(crate) async fn read_workspace_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hold: &LegacyRecoveryHold,
) -> WorkspaceResult<WorkspaceRow> {
    let row = sqlx::query("SELECT * FROM workspaces WHERE id=? COLLATE BINARY LIMIT 1")
        .bind(hold.workspace_id().as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or_else(|| bad("legacy_recovery_holds", "workspace_id"))?;
    let workspace = WorkspaceRow::decode(&row)?;
    if workspace.id != *hold.workspace_id()
        || workspace.kind != WorkspaceKind::LegacyCompat
        || workspace.root.root_generation() != hold.root_generation()
    {
        return Err(bad("workspaces", "row"));
    }
    Ok(workspace)
}
pub(crate) async fn read_run_lease_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hold: &LegacyRecoveryHold,
    run: &TerminalRunFacts,
    workspace: &WorkspaceRow,
) -> WorkspaceResult<Option<WorkspaceLeaseRow>> {
    let rows = sqlx::query(
        "SELECT * FROM workspace_leases WHERE kind='run' AND released_at IS NULL
         AND owner_id=? COLLATE BINARY
         ORDER BY lease_id COLLATE BINARY",
    )
    .bind(hold.run_id())
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if rows.len() > 1 {
        return Err(bad("workspace_leases", "row"));
    }
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let lease = WorkspaceLeaseRow::decode(row)?;
    if lease.record.kind != LeaseKind::Run
        || lease.record.owner_id != hold.run_id()
        || lease.record.workspace_id != *hold.workspace_id()
        || lease.record.root_generation != hold.root_generation()
        || lease.record.acquired_at < run.created_at
        || lease.record.acquired_at < workspace.created_at
    {
        return Err(bad("workspace_leases", "row"));
    }
    Ok(Some(lease))
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalAuditFacts {
    pub(crate) seq: i64,
    pub(crate) ts: WorkspaceInstant,
    pub(crate) actor: String,
    pub(crate) action: String,
    pub(crate) raw_detail: String,
    pub(crate) prev_hash: String,
    pub(crate) hash: String,
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn decode_audit(row: &SqliteRow) -> WorkspaceResult<TerminalAuditFacts> {
    let seq = row
        .try_get::<i64, _>("seq")
        .map_err(|_| bad("audit_log", "seq"))?;
    let raw_detail = text(row, "audit_log", "detail")?;
    serde_json::from_str::<serde_json::Value>(&raw_detail)
        .map_err(|_| bad("audit_log", "detail"))?;
    let facts = TerminalAuditFacts {
        seq,
        ts: instant(row, "audit_log", "ts")?,
        actor: text(row, "audit_log", "actor")?,
        action: text(row, "audit_log", "action")?,
        raw_detail,
        prev_hash: text(row, "audit_log", "prev_hash")?,
        hash: text(row, "audit_log", "hash")?,
    };
    if facts.seq <= 0
        || !valid_hash(&facts.hash)
        || (facts.prev_hash != crate::audit::GENESIS && !valid_hash(&facts.prev_hash))
    {
        return Err(bad("audit_log", "row"));
    }
    Ok(facts)
}
pub(crate) async fn read_audit_tx(
    tx: &mut Transaction<'_, Sqlite>,
    seq: i64,
) -> WorkspaceResult<TerminalAuditFacts> {
    let row =
        sqlx::query("SELECT seq,ts,actor,action,detail,prev_hash,hash FROM audit_log WHERE seq=?")
            .bind(seq)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?
            .ok_or_else(|| bad("audit_log", "seq"))?;
    let facts = decode_audit(&row)?;
    (facts.seq == seq)
        .then_some(facts)
        .ok_or_else(|| bad("audit_log", "seq"))
}
/// Looks up only the canonical terminal record.  Retrying terminalization must
/// never treat a merely similar audit entry as evidence of a prior apply.
pub(crate) async fn read_exact_terminal_audits_tx(
    tx: &mut Transaction<'_, Sqlite>,
    ts: &WorkspaceInstant,
    action: &str,
    raw_detail: &str,
) -> WorkspaceResult<Vec<TerminalAuditFacts>> {
    let rows = sqlx::query(
        "SELECT seq,ts,actor,action,detail,prev_hash,hash FROM audit_log
         WHERE ts=? COLLATE BINARY AND actor='legacy_recovery' COLLATE BINARY
           AND action=? COLLATE BINARY AND detail=? COLLATE BINARY ORDER BY seq",
    )
    .bind(ts.as_str())
    .bind(action)
    .bind(raw_detail)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    rows.iter().map(decode_audit).collect()
}
pub(crate) async fn verify_audit_tx(
    tx: &mut Transaction<'_, Sqlite>,
    expected: &TerminalAuditFacts,
) -> WorkspaceResult<()> {
    let actual = read_audit_tx(tx, expected.seq).await?;
    if &actual != expected {
        return Err(bad("audit_log", "row"));
    }
    let mut prev = crate::audit::GENESIS.to_owned();
    for seq in 1..=actual.seq {
        let entry = read_audit_tx(tx, seq).await?;
        if entry.prev_hash != prev
            || crate::audit::entry_hash(
                &prev,
                entry.ts.as_str(),
                &entry.actor,
                &entry.action,
                &entry.raw_detail,
            ) != entry.hash
        {
            return Err(bad("audit_log", "row"));
        }
        prev = entry.hash;
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TerminalReleaseSnapshot {
    pub(crate) run: TerminalRunFacts,
    pub(crate) hold: LegacyRecoveryHold,
    pub(crate) cohort: TerminalCohortFacts,
    pub(crate) workspace: WorkspaceRow,
    pub(crate) lease: Option<WorkspaceLeaseRow>,
    pub(crate) hold_approval: Option<TerminalApprovalFacts>,
    pub(crate) pending_approvals: Vec<TerminalApprovalFacts>,
    pub(crate) latest_audit: Option<TerminalAuditFacts>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::Store;

    #[rustfmt::skip]
    #[tokio::test]
    async fn audit_verify_rejects_a_rehashed_broken_link() {
        let store = Store::open_memory().await.unwrap();
        let mut tx = store.pool().begin().await.unwrap();
        let first = Store::append_audit_tx(&mut tx, "2026-09-19T00:00:00.000Z", "a", "x", &serde_json::json!({})).await.unwrap();
        let second = Store::append_audit_tx(&mut tx, "2026-09-19T00:00:01.000Z", "a", "y", &serde_json::json!({})).await.unwrap();
        let detail = serde_json::to_string(&second.detail).unwrap();
        let hash = crate::audit::entry_hash(crate::audit::GENESIS, &second.ts, &second.actor, &second.action, &detail);
        sqlx::query("UPDATE audit_log SET prev_hash=?,hash=? WHERE seq=?").bind(crate::audit::GENESIS).bind(hash).bind(second.seq).execute(&mut *tx).await.unwrap();
        let actual = read_audit_tx(&mut tx, second.seq).await.unwrap();
        assert_ne!(first.hash, actual.prev_hash);
        assert!(matches!(verify_audit_tx(&mut tx, &actual).await, Err(WorkspaceStoreError::CorruptRow { .. })));
    }
}
