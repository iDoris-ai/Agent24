use crate::{
    Store, WorkspaceInstant, WorkspaceResult, WorkspaceStoreError, workspace_decode_support,
};
use agent24_protocol::{ApprovalStatus, RunStatus, WorkspaceId};
use sqlx::{Sqlite, Transaction, sqlite::SqliteRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    AwaitingDecision,
    Ready,
    Active,
    NeedsAttention,
    Released,
}

impl RecoveryState {
    pub fn parse(value: &str) -> WorkspaceResult<Self> {
        match value {
            "awaiting_decision" => Ok(Self::AwaitingDecision),
            "ready" => Ok(Self::Ready),
            "active" => Ok(Self::Active),
            "needs_attention" => Ok(Self::NeedsAttention),
            "released" => Ok(Self::Released),
            _ => Err(WorkspaceStoreError::InvalidValue {
                field: "recovery_state",
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingDecision => "awaiting_decision",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::NeedsAttention => "needs_attention",
            Self::Released => "released",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRecoveryHold {
    pub(crate) run_id: String,
    pub(crate) cohort_id: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) root_generation: String,
    pub(crate) original_status: RunStatus,
    pub(crate) recovery_state: RecoveryState,
    pub(crate) approval_id: Option<String>,
    pub(crate) ready_at: Option<WorkspaceInstant>,
    pub(crate) reason_code: Option<String>,
    pub(crate) released_at: Option<WorkspaceInstant>,
    pub(crate) active_resume_approval_id: Option<String>,
}

fn bad(field: &'static str) -> WorkspaceStoreError {
    workspace_decode_support::bad_table("legacy_recovery_holds", field)
}

fn facts_text(
    row: &SqliteRow,
    table: &'static str,
    field: &'static str,
) -> WorkspaceResult<String> {
    workspace_decode_support::text(row, field)
        .map_err(|_| workspace_decode_support::bad_table(table, field))
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
struct RunFacts {
    id: String,
    status: RunStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
struct ApprovalFacts {
    id: String,
    run_id: String,
    status: ApprovalStatus,
}

#[allow(dead_code)]
fn parse_run_status(value: &str) -> Option<RunStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
}

#[allow(dead_code)]
fn parse_approval_status(value: &str) -> Option<ApprovalStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
}

#[allow(dead_code)]
async fn read_run_facts(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> WorkspaceResult<Option<RunFacts>> {
    let row = sqlx::query("SELECT id,status FROM runs WHERE id = ? COLLATE BINARY LIMIT 1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
    let Some(row) = row else { return Ok(None) };
    let actual_id = facts_text(&row, "runs", "id")?;
    if actual_id != id {
        return Err(workspace_decode_support::bad_table("runs", "id"));
    }
    let status = parse_run_status(&facts_text(&row, "runs", "status")?)
        .ok_or_else(|| workspace_decode_support::bad_table("runs", "status"))?;
    Ok(Some(RunFacts {
        id: actual_id,
        status,
    }))
}

#[allow(dead_code)]
async fn read_approval_facts(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> WorkspaceResult<Option<ApprovalFacts>> {
    let row =
        sqlx::query("SELECT id,run_id,status FROM approvals WHERE id = ? COLLATE BINARY LIMIT 1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
    let Some(row) = row else { return Ok(None) };
    let actual_id = facts_text(&row, "approvals", "id")?;
    if actual_id != id {
        return Err(workspace_decode_support::bad_table("approvals", "id"));
    }
    let run_id = facts_text(&row, "approvals", "run_id")?;
    let status = parse_approval_status(&facts_text(&row, "approvals", "status")?)
        .ok_or_else(|| workspace_decode_support::bad_table("approvals", "status"))?;
    Ok(Some(ApprovalFacts {
        id: actual_id,
        run_id,
        status,
    }))
}

fn check<T>(result: WorkspaceResult<T>, field: &'static str) -> WorkspaceResult<T> {
    result.map_err(|_| bad(field))
}

fn opt_nonempty(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<String>> {
    let value = workspace_decode_support::opt_text(row, field)?;
    if value.as_deref().is_some_and(|v| v.is_empty()) {
        return Err(workspace_decode_support::bad(field));
    }
    Ok(value)
}

impl LegacyRecoveryHold {
    pub(crate) fn decode(row: SqliteRow) -> WorkspaceResult<Self> {
        let run_id = check(workspace_decode_support::text(&row, "run_id"), "run_id")?;
        let cohort_id = check(
            workspace_decode_support::text(&row, "cohort_id"),
            "cohort_id",
        )?;
        let workspace_id = WorkspaceId::parse(&check(
            workspace_decode_support::text(&row, "workspace_id"),
            "workspace_id",
        )?)
        .map_err(|_| bad("workspace_id"))?;
        let root_generation = check(
            workspace_decode_support::nonblank(&row, "root_generation"),
            "root_generation",
        )?;
        let original_status = match check(
            workspace_decode_support::text(&row, "original_status"),
            "original_status",
        )?
        .as_str()
        {
            "queued" => RunStatus::Queued,
            "running" => RunStatus::Running,
            "awaiting_approval" => RunStatus::AwaitingApproval,
            _ => return Err(bad("original_status")),
        };
        let recovery_state = RecoveryState::parse(&check(
            workspace_decode_support::text(&row, "recovery_state"),
            "recovery_state",
        )?)
        .map_err(|_| bad("recovery_state"))?;
        let approval_id = check(opt_nonempty(&row, "approval_id"), "approval_id")?;
        let ready_at = check(
            workspace_decode_support::opt_instant(&row, "ready_at"),
            "ready_at",
        )?;
        let reason_code = check(
            workspace_decode_support::opt_nonblank(&row, "reason_code"),
            "reason_code",
        )?;
        if reason_code.as_deref().is_some_and(|v| {
            !v.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        }) {
            return Err(bad("reason_code"));
        }
        let released_at = check(
            workspace_decode_support::opt_instant(&row, "released_at"),
            "released_at",
        )?;
        let active_resume_approval_id = check(
            opt_nonempty(&row, "active_resume_approval_id"),
            "active_resume_approval_id",
        )?;
        match recovery_state {
            RecoveryState::AwaitingDecision if approval_id.is_none() => {
                return Err(bad("approval_id"));
            }
            RecoveryState::Ready if approval_id.is_none() => return Err(bad("approval_id")),
            RecoveryState::Ready if ready_at.is_none() => return Err(bad("ready_at")),
            RecoveryState::NeedsAttention if reason_code.is_none() => {
                return Err(bad("reason_code"));
            }
            RecoveryState::Released if released_at.is_none() => return Err(bad("released_at")),
            RecoveryState::Active if approval_id.is_none() => return Err(bad("approval_id")),
            RecoveryState::Active if active_resume_approval_id.is_none() => {
                return Err(bad("active_resume_approval_id"));
            }
            RecoveryState::Active
                if active_resume_approval_id.as_deref() != approval_id.as_deref() =>
            {
                return Err(bad("active_resume_approval_id"));
            }
            _ => {}
        }
        if !matches!(recovery_state, RecoveryState::Released) && released_at.is_some()
            || matches!(recovery_state, RecoveryState::Released) && released_at.is_none()
        {
            return Err(bad("released_at"));
        }
        if !matches!(recovery_state, RecoveryState::Active) && active_resume_approval_id.is_some() {
            return Err(bad("active_resume_approval_id"));
        }
        Ok(Self {
            run_id,
            cohort_id,
            workspace_id,
            root_generation,
            original_status,
            recovery_state,
            approval_id,
            ready_at,
            reason_code,
            released_at,
            active_resume_approval_id,
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn cohort_id(&self) -> &str {
        &self.cohort_id
    }
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
    pub fn root_generation(&self) -> &str {
        &self.root_generation
    }
    pub fn original_status(&self) -> RunStatus {
        self.original_status
    }
    pub fn recovery_state(&self) -> RecoveryState {
        self.recovery_state
    }
    pub fn approval_id(&self) -> Option<&str> {
        self.approval_id.as_deref()
    }
    pub fn ready_at(&self) -> Option<&WorkspaceInstant> {
        self.ready_at.as_ref()
    }
    pub fn reason_code(&self) -> Option<&str> {
        self.reason_code.as_deref()
    }
    pub fn released_at(&self) -> Option<&WorkspaceInstant> {
        self.released_at.as_ref()
    }
    pub fn active_resume_approval_id(&self) -> Option<&str> {
        self.active_resume_approval_id.as_deref()
    }
}

impl Store {
    /// Reads a persisted hold without applying recovery or admission policy.
    pub async fn get_legacy_recovery_hold(
        &self,
        run_id: &str,
    ) -> WorkspaceResult<LegacyRecoveryHold> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        let hold = read_hold_tx(&mut tx, run_id).await?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(hold)
    }
}

/// Reads a hold under a caller-owned transaction. The explicit projection is
/// part of the decoder contract: schema drift is reported as `Database`.
pub(crate) async fn read_hold_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
) -> WorkspaceResult<LegacyRecoveryHold> {
    let row = sqlx::query(
        "SELECT run_id,cohort_id,workspace_id,root_generation,original_status,
         recovery_state,approval_id,ready_at,reason_code,released_at,active_resume_approval_id
         FROM legacy_recovery_holds WHERE run_id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or(WorkspaceStoreError::NotFound)?;
    let hold = LegacyRecoveryHold::decode(row)?;
    if hold.run_id != run_id {
        return Err(bad("run_id"));
    }
    Ok(hold)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDecisionEffect {
    Unchanged,
    MarkReady,
    MarkActiveResume,
}

/// Computes a persistence plan only. It does not validate a checkpoint, root,
/// tool row, TTL, or execution authority, and must not be used as admission.
pub fn recovery_decision_effect(
    run_status: RunStatus,
    recovery_state: RecoveryState,
    approval_resolved: bool,
) -> RecoveryDecisionEffect {
    if matches!(
        run_status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    ) || !approval_resolved
    {
        return RecoveryDecisionEffect::Unchanged;
    }
    match recovery_state {
        RecoveryState::AwaitingDecision => RecoveryDecisionEffect::MarkReady,
        RecoveryState::Active => RecoveryDecisionEffect::MarkActiveResume,
        RecoveryState::Ready | RecoveryState::NeedsAttention | RecoveryState::Released => {
            RecoveryDecisionEffect::Unchanged
        }
    }
}

/// A crate-private, immutable plan for the only field changes made by the
/// legacy recovery Ready transition. This is persistence data, not an
/// eligibility or capability token.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ReadyMutation {
    run_id: String,
    cohort_id: String,
    workspace_id: WorkspaceId,
    root_generation: String,
    original_status: RunStatus,
    approval_id: String,
    recovery_state: RecoveryState,
    ready_at: WorkspaceInstant,
}

/// Plans `awaiting_decision -> ready` without performing any I/O.
#[allow(dead_code)]
pub(crate) fn plan_ready_mutation(
    hold: &LegacyRecoveryHold,
    run_status: RunStatus,
    approval_resolved: bool,
    approval_id: &str,
    ready_at: WorkspaceInstant,
) -> WorkspaceResult<Option<ReadyMutation>> {
    if recovery_decision_effect(run_status, hold.recovery_state, approval_resolved)
        != RecoveryDecisionEffect::MarkReady
    {
        return Ok(None);
    }
    if hold.approval_id.as_deref() != Some(approval_id) {
        return Err(WorkspaceStoreError::InvalidValue {
            field: "approval_id",
        });
    }
    Ok(Some(ReadyMutation {
        run_id: hold.run_id.clone(),
        cohort_id: hold.cohort_id.clone(),
        workspace_id: hold.workspace_id.clone(),
        root_generation: hold.root_generation.clone(),
        original_status: hold.original_status,
        approval_id: approval_id.to_owned(),
        recovery_state: RecoveryState::Ready,
        ready_at,
    }))
}

#[allow(dead_code)]
pub(crate) async fn apply_ready_tx(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    approval_id: &str,
    ready_at: WorkspaceInstant,
) -> WorkspaceResult<Option<ReadyMutation>> {
    let hold = read_hold_tx(tx, run_id).await?;
    if hold.approval_id.as_deref() != Some(approval_id) {
        return Err(WorkspaceStoreError::InvalidValue {
            field: "approval_id",
        });
    }
    let run = read_run_facts(tx, &hold.run_id)
        .await?
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "legacy_recovery_holds",
            field: "run_id",
        })?;
    let approval =
        read_approval_facts(tx, approval_id)
            .await?
            .ok_or(WorkspaceStoreError::CorruptRow {
                table: "legacy_recovery_holds",
                field: "approval_id",
            })?;
    if approval.run_id != run.id {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "approvals",
            field: "run_id",
        });
    }
    let resolved = matches!(
        approval.status,
        ApprovalStatus::Approved
            | ApprovalStatus::Denied
            | ApprovalStatus::Aborted
            | ApprovalStatus::TimedOut
    );
    let Some(mutation) =
        plan_ready_mutation(&hold, run.status, resolved, approval_id, ready_at.clone())?
    else {
        return Ok(None);
    };
    let original_status =
        serde_json::to_value(hold.original_status).map_err(|_| WorkspaceStoreError::Database)?;
    let original_status = original_status
        .as_str()
        .ok_or(WorkspaceStoreError::Database)?;
    let affected = sqlx::query(
        "UPDATE legacy_recovery_holds SET recovery_state = ?, ready_at = ?
         WHERE run_id = ? COLLATE BINARY AND cohort_id = ? COLLATE BINARY
           AND workspace_id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY
           AND original_status = ? COLLATE BINARY
           AND recovery_state = ? COLLATE BINARY
           AND approval_id = ? COLLATE BINARY
           AND ready_at IS NULL AND reason_code IS ?
           AND released_at IS NULL AND active_resume_approval_id IS NULL",
    )
    .bind(RecoveryState::Ready.as_str())
    .bind(ready_at.as_str())
    .bind(&hold.run_id)
    .bind(&hold.cohort_id)
    .bind(hold.workspace_id.as_str())
    .bind(&hold.root_generation)
    .bind(original_status)
    .bind(hold.recovery_state.as_str())
    .bind(approval_id)
    .bind(hold.reason_code.as_deref())
    .execute(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    if affected.rows_affected() != 1 {
        return Err(WorkspaceStoreError::Database);
    }
    let mut expected = hold.clone();
    expected.recovery_state = RecoveryState::Ready;
    expected.ready_at = Some(ready_at.clone());
    if read_hold_tx(tx, run_id).await? != expected {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "legacy_recovery_holds",
            field: "row",
        });
    }
    let detail = serde_json::json!({
        "run_id": mutation.run_id,
        "cohort_id": mutation.cohort_id,
        "workspace_id": mutation.workspace_id.as_str(),
        "approval_id": approval_id,
        "result_state": mutation.recovery_state.as_str(),
    });
    Store::append_audit_tx(
        tx,
        ready_at.as_str(),
        "legacy_recovery",
        "legacy_recovery.ready",
        &detail,
    )
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    Ok(Some(mutation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    #[allow(clippy::unwrap_used)]
    async fn strict_facts_fixture() -> Store {
        let store = Store::open_memory().await.unwrap();
        let pool = store.pool();
        let fk_enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(fk_enabled, 1);
        sqlx::raw_sql(
            "INSERT INTO workspaces
             (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,
              lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,
              canonical_root,root_generation,root_identity_kind,unix_device,unix_inode)
             VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','legacy_compat','active','test',
                     'external','orchestrator','owner','serial','2026-09-19T00:00:00.000Z',
                     '2026-09-20T00:00:00.000Z',1,'/tmp/strict','g1','unix',zeroblob(8),zeroblob(8));
             INSERT INTO legacy_recovery_cohorts
             (cohort_id,migration_version,legacy_workspace_id,root_generation,created_at)
             VALUES ('cohort-strict',1,'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1',
                     '2026-09-19T00:00:00.000Z');
             INSERT INTO runs (id,status,input,usage,created_at,workspace_id)
             VALUES ('run-strict','running','{}','{}','2026-09-19T00:00:00.000Z',
                     'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5');
             INSERT INTO approvals
             (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,
              expires_at,created_at,workspace_id)
             VALUES ('approval-strict','run-strict','tool','exec','test','{}','[]','pending',
                     '2026-09-20T00:00:00.000Z','2026-09-19T00:00:00.000Z',
                     'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5');
             INSERT INTO legacy_recovery_holds
             (run_id,cohort_id,workspace_id,root_generation,original_status,recovery_state,approval_id)
             VALUES ('run-strict','cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5',
                     'g1','running','awaiting_decision','approval-strict');
             UPDATE legacy_recovery_holds SET reason_code='legacy_reason';
             INSERT INTO workspace_leases
             (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at)
             VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6',
                     'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','run-strict','run',
                     '2026-09-19T00:00:00.000Z');",
        )
        .execute(pool)
        .await
        .unwrap();
        let violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(pool)
            .await
            .unwrap();
        assert!(violations.is_empty());
        store
    }

    type LeaseFacts = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    #[allow(clippy::unwrap_used)]
    async fn lease_facts(tx: &mut Transaction<'_, Sqlite>) -> LeaseFacts {
        sqlx::query_as(
            "SELECT lease_id,workspace_id,root_generation,owner_id,kind,daemon_generation,
                    host_instance_id,acquired_at,expires_at,renewed_at,released_at
             FROM workspace_leases WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6'",
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn strict_facts_are_tx_local_and_read_only() {
        let store = strict_facts_fixture().await;
        let pool = store.pool();
        let before: (String, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT recovery_state FROM legacy_recovery_holds WHERE run_id='run-strict'),
                    (SELECT count(*) FROM audit_log), (SELECT count(*) FROM workspace_leases), total_changes()",
        )
        .fetch_one(pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            read_hold_tx(&mut tx, "run-strict").await.unwrap().run_id(),
            "run-strict"
        );
        assert_eq!(
            read_hold_tx(&mut tx, "RUN-STRICT").await,
            Err(WorkspaceStoreError::NotFound)
        );
        assert_eq!(read_run_facts(&mut tx, "RUN-STRICT").await.unwrap(), None);
        assert_eq!(
            read_run_facts(&mut tx, "run-strict").await.unwrap(),
            Some(RunFacts {
                id: "run-strict".to_owned(),
                status: RunStatus::Running
            })
        );
        assert_eq!(
            read_approval_facts(&mut tx, "approval-strict")
                .await
                .unwrap(),
            Some(ApprovalFacts {
                id: "approval-strict".to_owned(),
                run_id: "run-strict".to_owned(),
                status: ApprovalStatus::Pending,
            })
        );
        assert_eq!(
            read_approval_facts(&mut tx, "APPROVAL-STRICT").await,
            Ok(None)
        );
        let after: (String, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT recovery_state FROM legacy_recovery_holds WHERE run_id='run-strict'),
                    (SELECT count(*) FROM audit_log), (SELECT count(*) FROM workspace_leases), total_changes()",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(after, before);
        sqlx::query("UPDATE approvals SET status='approved' WHERE id='approval-strict'")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            read_approval_facts(&mut tx, "approval-strict")
                .await
                .unwrap()
                .unwrap()
                .status,
            ApprovalStatus::Approved
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn apply_ready_tx_promotes_once_and_audits_atomically() {
        let store = strict_facts_fixture().await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let hold_before = read_hold_tx(&mut tx, "run-strict").await.unwrap();
        let run_before = read_run_facts(&mut tx, "run-strict").await.unwrap();
        let approval_before = read_approval_facts(&mut tx, "approval-strict")
            .await
            .unwrap();
        let lease_before = lease_facts(&mut tx).await;
        sqlx::query("UPDATE approvals SET status='approved' WHERE id='approval-strict'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let ready_at = WorkspaceInstant::parse("2026-09-20T00:00:00.000Z").unwrap();
        apply_ready_tx(&mut tx, "run-strict", "approval-strict", ready_at.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            apply_ready_tx(
                &mut tx,
                "run-strict",
                "approval-strict",
                WorkspaceInstant::parse("2026-09-20T00:01:00.000Z").unwrap(),
            )
            .await
            .unwrap(),
            None
        );
        let mut expected_hold = hold_before.clone();
        expected_hold.recovery_state = RecoveryState::Ready;
        expected_hold.ready_at = Some(ready_at);
        assert_eq!(
            read_hold_tx(&mut tx, "run-strict").await.unwrap(),
            expected_hold
        );
        assert_eq!(
            read_run_facts(&mut tx, "run-strict").await.unwrap(),
            run_before
        );
        let mut expected_approval = approval_before.unwrap();
        expected_approval.status = ApprovalStatus::Approved;
        assert_eq!(
            read_approval_facts(&mut tx, "approval-strict")
                .await
                .unwrap(),
            Some(expected_approval)
        );
        let lease_after = lease_facts(&mut tx).await;
        assert_eq!(lease_after, lease_before);
        tx.commit().await.unwrap();
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].actor, "legacy_recovery");
        assert_eq!(audit[0].action, "legacy_recovery.ready");
        assert_eq!(
            audit[0].detail,
            serde_json::json!({
                "run_id": "run-strict",
                "cohort_id": "cohort-strict",
                "workspace_id": "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                "approval_id": "approval-strict",
                "result_state": "ready",
            })
        );
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn strict_facts_classify_storage_enums_and_sql_failures() {
        let store = Store {
            pool: sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap(),
        };
        execute(
            &store,
            "CREATE TABLE runs (id INTEGER,status);
             INSERT INTO runs VALUES
                ('run-storage',1),('run-enum','bogus'),('1','running');
             CREATE TABLE approvals (id INTEGER,run_id,status);
             INSERT INTO approvals VALUES
                ('approval-storage','run',1),
                ('approval-enum','run','bogus'),
                ('approval-run-storage','run', 'pending'),
                ('1','run','pending');
             UPDATE approvals SET run_id=1 WHERE id='approval-run-storage';",
        )
        .await;
        let mut tx = store.pool().begin().await.unwrap();
        for (id, field) in [
            ("run-storage", "status"),
            ("run-enum", "status"),
            ("1", "id"),
        ] {
            assert_eq!(
                read_run_facts(&mut tx, id).await,
                Err(WorkspaceStoreError::CorruptRow {
                    table: "runs",
                    field
                })
            );
        }
        for (id, field) in [
            ("approval-storage", "status"),
            ("approval-enum", "status"),
            ("approval-run-storage", "run_id"),
            ("1", "id"),
        ] {
            assert_eq!(
                read_approval_facts(&mut tx, id).await,
                Err(WorkspaceStoreError::CorruptRow {
                    table: "approvals",
                    field
                })
            );
        }
        assert_eq!(read_run_facts(&mut tx, "missing").await, Ok(None));
        assert_eq!(read_approval_facts(&mut tx, "MISSING").await, Ok(None));
        assert_eq!(
            read_hold_tx(&mut tx, "run").await,
            Err(WorkspaceStoreError::Database)
        );
        tx.rollback().await.unwrap();

        execute(&store, "DROP TABLE runs; DROP TABLE approvals").await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(
            read_run_facts(&mut tx, "run").await,
            Err(WorkspaceStoreError::Database)
        );
        assert_eq!(
            read_approval_facts(&mut tx, "approval").await,
            Err(WorkspaceStoreError::Database)
        );
        tx.rollback().await.unwrap();

        execute(&store, "CREATE TABLE legacy_recovery_holds (run_id)").await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(
            read_hold_tx(&mut tx, "run").await,
            Err(WorkspaceStoreError::Database)
        );
        tx.rollback().await.unwrap();
    }

    #[allow(clippy::unwrap_used)]
    async fn execute(store: &Store, statement: &str) {
        sqlx::raw_sql(statement)
            .execute(store.pool())
            .await
            .unwrap();
    }

    #[allow(clippy::unwrap_used)]
    async fn changes(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(store.pool())
            .await
            .unwrap()
    }

    #[allow(clippy::unwrap_used)]
    async fn damaged(store: &Store, field: &'static str, value: &str, storage: &str) {
        execute(
            store,
            &format!(
                "DELETE FROM legacy_recovery_holds;
             INSERT INTO legacy_recovery_holds VALUES
             ('run','cohort','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','queued',
              'awaiting_decision','a',NULL,NULL,NULL,NULL);
             UPDATE legacy_recovery_holds SET {field}={value}"
            ),
        )
        .await;
        let actual: String = sqlx::query_scalar(&format!(
            "SELECT typeof({field}) FROM legacy_recovery_holds"
        ))
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(actual, storage);
        let before = changes(store).await;
        let expected = Err(WorkspaceStoreError::CorruptRow {
            table: "legacy_recovery_holds",
            field,
        });
        let key = if field == "run_id" {
            match value {
                "1" => "1",
                "1.5" => "1.5",
                "x'61'" => "a",
                "''" => "",
                "'a'||char(0)" => "a\0",
                _ => "run",
            }
        } else {
            "run"
        };
        let result = store.get_legacy_recovery_hold(key).await;
        if field == "run_id" && storage != "text" {
            assert_eq!(result, Err(WorkspaceStoreError::NotFound));
            let row = sqlx::query("SELECT * FROM legacy_recovery_holds")
                .fetch_one(store.pool())
                .await
                .unwrap();
            assert_eq!(LegacyRecoveryHold::decode(row), expected);
        } else if value == "NULL"
            && matches!(
                field,
                "ready_at" | "reason_code" | "released_at" | "active_resume_approval_id"
            )
        {
            assert!(result.is_ok(), "{field}");
        } else {
            assert_eq!(result, expected, "{field}={value}");
        }
        assert_eq!(changes(store).await, before);
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn getter_rejects_storage_and_value_corruption_without_writes() {
        let store = Store {
            pool: sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap(),
        };
        // No affinity: a TEXT column would silently coerce INTEGER/REAL samples.
        let fields = [
            "run_id",
            "cohort_id",
            "workspace_id",
            "root_generation",
            "original_status",
            "recovery_state",
            "approval_id",
            "ready_at",
            "reason_code",
            "released_at",
            "active_resume_approval_id",
        ];
        execute(
            &store,
            &format!("CREATE TABLE legacy_recovery_holds ({})", fields.join(",")),
        )
        .await;
        for field in fields {
            for (value, storage) in [
                ("1", "integer"),
                ("1.5", "real"),
                ("x'61'", "blob"),
                ("NULL", "null"),
                ("'a'||char(0)", "text"),
                ("''", "text"),
            ] {
                damaged(&store, field, value, storage).await;
            }
        }
        for (field, value) in [
            ("workspace_id", "'ws_invalid'"),
            ("root_generation", "'   '"),
            ("original_status", "'completed'"),
            ("recovery_state", "'READY'"),
            ("reason_code", "'Bad reason'"),
            ("ready_at", "'2026-09-19T00:00:00Z'"),
            ("released_at", "'2026-02-30T00:00:00.000Z'"),
        ] {
            damaged(&store, field, value, "text").await;
        }
        execute(&store, "DROP TABLE legacy_recovery_holds").await;
        assert_eq!(
            store.get_legacy_recovery_hold("run").await,
            Err(WorkspaceStoreError::Database)
        );
        store.pool().close().await;
        assert_eq!(
            store.get_legacy_recovery_hold("run").await,
            Err(WorkspaceStoreError::Database)
        );
    }

    const STATES: [RecoveryState; 5] = [
        RecoveryState::AwaitingDecision,
        RecoveryState::Ready,
        RecoveryState::Active,
        RecoveryState::NeedsAttention,
        RecoveryState::Released,
    ];
    const STATUSES: [RunStatus; 6] = [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::AwaitingApproval,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Cancelled,
    ];

    #[test]
    fn recovery_states_round_trip_and_reject_unknown_values() {
        for state in STATES {
            assert_eq!(RecoveryState::parse(state.as_str()), Ok(state));
        }
        assert_eq!(
            RecoveryState::parse("ACTIVE"),
            Err(WorkspaceStoreError::InvalidValue {
                field: "recovery_state"
            })
        );
    }

    #[test]
    fn approval_decision_matrix_is_fail_closed() {
        for status in STATUSES {
            for state in STATES {
                for resolved in [false, true] {
                    let terminal = matches!(
                        status,
                        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
                    );
                    let expected = if terminal || !resolved {
                        RecoveryDecisionEffect::Unchanged
                    } else {
                        match state {
                            RecoveryState::AwaitingDecision => RecoveryDecisionEffect::MarkReady,
                            RecoveryState::Active => RecoveryDecisionEffect::MarkActiveResume,
                            _ => RecoveryDecisionEffect::Unchanged,
                        }
                    };
                    assert_eq!(recovery_decision_effect(status, state, resolved), expected);
                }
            }
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_mutation_preserves_identity_and_changes_only_ready_fields() {
        let hold = fixture(
            "awaiting_decision",
            Some("approval-1"),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let ready_at = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
        let mutation = plan_ready_mutation(
            &hold,
            RunStatus::Running,
            true,
            "approval-1",
            ready_at.clone(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(mutation.run_id, hold.run_id);
        assert_eq!(mutation.cohort_id, hold.cohort_id);
        assert_eq!(mutation.workspace_id, hold.workspace_id);
        assert_eq!(mutation.root_generation, hold.root_generation);
        assert_eq!(mutation.original_status, hold.original_status);
        assert_eq!(mutation.approval_id, "approval-1");
        assert_eq!(mutation.recovery_state, RecoveryState::Ready);
        assert_eq!(mutation.ready_at, ready_at);
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_mutation_rejects_wrong_approval_and_ignores_pending_or_other_states() {
        let hold = fixture(
            "awaiting_decision",
            Some("approval-1"),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let ready_at = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
        assert_eq!(
            plan_ready_mutation(
                &hold,
                RunStatus::Running,
                false,
                "approval-1",
                ready_at.clone()
            )
            .unwrap(),
            None
        );
        assert_eq!(
            plan_ready_mutation(&hold, RunStatus::Completed, true, "wrong", ready_at.clone())
                .unwrap(),
            None
        );
        assert_eq!(
            plan_ready_mutation(&hold, RunStatus::Running, true, "wrong", ready_at.clone())
                .unwrap_err(),
            WorkspaceStoreError::InvalidValue {
                field: "approval_id"
            }
        );
        let ready = fixture(
            "ready",
            Some("approval-1"),
            Some("2026-09-18T00:00:00.000Z"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            plan_ready_mutation(&ready, RunStatus::Running, true, "approval-1", ready_at).unwrap(),
            None
        );
    }
    #[allow(clippy::unwrap_used)]
    async fn fixture(
        state: &str,
        approval: Option<&str>,
        ready: Option<&str>,
        reason: Option<&str>,
        released: Option<&str>,
        marker: Option<&str>,
    ) -> WorkspaceResult<LegacyRecoveryHold> {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let row = sqlx::query(
            "SELECT ? AS run_id,? AS cohort_id,? AS workspace_id,? AS root_generation,
             ? AS original_status,? AS recovery_state,? AS approval_id,? AS ready_at,
             ? AS reason_code,? AS released_at,? AS active_resume_approval_id",
        )
        .bind("run")
        .bind("cohort")
        .bind("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5")
        .bind("g1")
        .bind("queued")
        .bind(state)
        .bind(approval)
        .bind(ready)
        .bind(reason)
        .bind(released)
        .bind(marker)
        .fetch_one(&pool)
        .await
        .unwrap();
        LegacyRecoveryHold::decode(row)
    }
    #[tokio::test]
    async fn decode_accepts_all_states_and_rejects_cross_field_tampering() {
        let ts = Some("2026-09-19T00:00:00.000Z");
        for (state, approval, ready, reason, released, marker) in [
            ("awaiting_decision", Some("a"), None, None, None, None),
            ("ready", Some("a"), ts, None, None, None),
            ("active", Some("a"), None, None, None, Some("a")),
            ("needs_attention", None, None, Some("bad_input"), None, None),
            ("released", None, None, None, ts, None),
        ] {
            assert!(
                fixture(state, approval, ready, reason, released, marker)
                    .await
                    .is_ok()
            );
        }
        macro_rules! reject {
            ($field:expr,$state:expr,$approval:expr,$ready:expr,$reason:expr,$released:expr,$marker:expr) => {
                assert_eq!(
                    fixture($state, $approval, $ready, $reason, $released, $marker)
                        .await
                        .err(),
                    Some(bad($field))
                )
            };
        }
        let none: Option<&str> = None;
        let marker = "active_resume_approval_id";
        let awaiting = "awaiting_decision";
        let attention = "needs_attention";
        reject!("approval_id", awaiting, none, none, none, none, none);
        reject!("approval_id", "ready", none, ts, none, none, none);
        reject!("ready_at", "ready", Some("a"), none, none, none, none);
        reject!("approval_id", "active", none, none, none, none, Some("a"));
        reject!(marker, "active", Some("a"), none, none, none, none);
        reject!(marker, "active", Some("a"), none, none, none, Some("b"));
        reject!("reason_code", attention, none, none, none, none, none);
        reject!("released_at", "released", none, none, none, none, none);
        reject!("released_at", "ready", Some("a"), ts, none, ts, none);
        reject!(marker, "ready", Some("a"), ts, none, none, Some("a"));
    }
}
