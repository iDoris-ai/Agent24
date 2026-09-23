use crate::workspace_decode_support::bad_table;
use crate::{
    LeaseKind, Store, StoreError, WorkspaceInstant, WorkspaceKind, WorkspaceLeaseId,
    WorkspaceLeaseRow, WorkspaceResult, WorkspaceRow, WorkspaceState, WorkspaceStoreError,
    workspace_decode_support,
};
use agent24_protocol::{Approval, ApprovalStatus, Decision, RunStatus, WorkspaceId};
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

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub(crate) enum LegacyApprovalResolution {
    ApprovalOnly(Approval),
    Ready(Approval),
}
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveLegacyApprovalError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Recovery(#[from] WorkspaceStoreError),
    #[error("legacy recovery unavailable")]
    RecoveryUnavailable,
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
    created_at: WorkspaceInstant,
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
    let row =
        sqlx::query("SELECT id,status,created_at FROM runs WHERE id = ? COLLATE BINARY LIMIT 1")
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
    let created_at = workspace_decode_support::instant(&row, "created_at")
        .map_err(|_| workspace_decode_support::bad_table("runs", "created_at"))?;
    Ok(Some(RunFacts {
        id: actual_id,
        status,
        created_at,
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

#[allow(dead_code)]
impl Store {
    pub(crate) async fn resolve_approval_with_legacy_ready(
        &self,
        id: &str,
        to: ApprovalStatus,
        decision: Option<&Decision>,
        resolved_at: WorkspaceInstant,
    ) -> Result<LegacyApprovalResolution, ResolveLegacyApprovalError> {
        agent24_core::check_approval_transition(ApprovalStatus::Pending, to)
            .map_err(StoreError::from)
            .map_err(ResolveLegacyApprovalError::Store)?;
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(StoreError::from)
            .map_err(ResolveLegacyApprovalError::Store)?;
        let approval = Store::resolve_approval_tx(&mut tx, id, to, decision, resolved_at.as_str())
            .await
            .map_err(ResolveLegacyApprovalError::Store)?;
        let held = sqlx::query(
            "SELECT 1 FROM legacy_recovery_holds
             WHERE run_id = ? COLLATE BINARY LIMIT 1",
        )
        .bind(&approval.run_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ResolveLegacyApprovalError::Recovery(WorkspaceStoreError::Database))?
        .is_some();
        if !held {
            tx.commit()
                .await
                .map_err(StoreError::from)
                .map_err(ResolveLegacyApprovalError::Store)?;
            return Ok(LegacyApprovalResolution::ApprovalOnly(approval));
        }
        match apply_ready_tx(&mut tx, &approval.run_id, id, resolved_at).await {
            Ok(Some(_)) => {
                tx.commit()
                    .await
                    .map_err(StoreError::from)
                    .map_err(ResolveLegacyApprovalError::Store)?;
                Ok(LegacyApprovalResolution::Ready(approval))
            }
            Ok(None) => {
                let _ = tx.rollback().await;
                Err(ResolveLegacyApprovalError::RecoveryUnavailable)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(ResolveLegacyApprovalError::Recovery(error))
            }
        }
    }
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

/// Reads the oldest persisted Ready hold under a caller-owned transaction.
/// Selection is deterministic and decoding is intentionally fail-closed: a
/// corrupt first candidate is returned as an error instead of being skipped.
#[allow(dead_code)]
pub(crate) async fn read_next_ready_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> WorkspaceResult<Option<LegacyRecoveryHold>> {
    let row = sqlx::query(
        "SELECT run_id,cohort_id,workspace_id,root_generation,original_status,
         recovery_state,approval_id,ready_at,reason_code,released_at,active_resume_approval_id
         FROM legacy_recovery_holds
         WHERE recovery_state = 'ready'
           AND recovery_state COLLATE BINARY = 'ready' COLLATE BINARY
         ORDER BY ready_at COLLATE BINARY ASC, run_id COLLATE BINARY ASC LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?;
    row.map(LegacyRecoveryHold::decode).transpose()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct LegacyRecoveryPromotionAttempt {
    pub(crate) hint: LegacyRecoveryHold,
    pub(crate) lease_id: WorkspaceLeaseId,
    pub(crate) acquired_at: WorkspaceInstant,
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct LegacyRecoveryAdmission {
    pub(crate) hold: LegacyRecoveryHold,
    pub(crate) lease: WorkspaceLeaseRow,
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum LegacyRecoveryPromotionOutcome {
    Admitted(LegacyRecoveryAdmission),
    ObservedCommitted(LegacyRecoveryAdmission),
    StaleHint,
    Terminal,
    Busy,
    WorkspaceUnavailable,
}
#[derive(Debug, Clone, PartialEq)]
struct PromotionFacts {
    run: RunFacts,
    approval_id: String,
    decision: Decision,
    workspace: WorkspaceRow,
    approval_created_at: WorkspaceInstant,
    approval_expires_at: WorkspaceInstant,
    approval_decided_at: WorkspaceInstant,
    cohort_created_at: WorkspaceInstant,
    cohort_migration_version: u64,
}

#[rustfmt::skip]
async fn select_recovery_workspace_tx(tx: &mut Transaction<'_, Sqlite>, id: &WorkspaceId) -> WorkspaceResult<WorkspaceRow> {
let row = sqlx::query("SELECT * FROM workspaces WHERE id = ? COLLATE BINARY LIMIT 1").bind(id.as_str()).fetch_optional(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?.ok_or_else(|| bad("workspace_id"))?;
WorkspaceRow::decode(&row)
}
#[rustfmt::skip]
async fn promotion_lease_tx(tx: &mut Transaction<'_, Sqlite>, lease_id: &WorkspaceLeaseId) -> WorkspaceResult<Option<WorkspaceLeaseRow>> {
let row = sqlx::query("SELECT * FROM workspace_leases WHERE lease_id = ? COLLATE BINARY LIMIT 1").bind(lease_id.as_str()).fetch_optional(&mut **tx).await.map_err(|_| WorkspaceStoreError::Database)?;
row.map(|row| WorkspaceLeaseRow::decode(&row)).transpose()
}

async fn promotion_facts_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hold: &LegacyRecoveryHold,
    ready_at: &WorkspaceInstant,
    now: &WorkspaceInstant,
) -> WorkspaceResult<Option<PromotionFacts>> {
    let Some(run) = read_run_facts(tx, &hold.run_id).await? else {
        return Err(bad("run_id"));
    };
    if matches!(
        run.status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    ) {
        return Ok(None);
    }
    let Some(approval_id) = hold.approval_id.as_deref() else {
        return Err(bad("approval_id"));
    };
    let approval = sqlx::query(
        "SELECT a.id,a.run_id,a.status,a.decision,a.available_decisions,a.created_at,a.expires_at,a.decided_at,a.workspace_id,
                r.workspace_id AS run_workspace_id
         FROM approvals a JOIN runs r ON r.id = a.run_id
         WHERE a.id = ? COLLATE BINARY AND r.id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(approval_id)
    .bind(&hold.run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or_else(|| bad("approval_id"))?;
    let actual_approval_id = facts_text(&approval, "approvals", "id")?;
    let approval_run_id = facts_text(&approval, "approvals", "run_id")?;
    let approval_status = parse_approval_status(&facts_text(&approval, "approvals", "status")?)
        .ok_or_else(|| bad_table("approvals", "status"))?;
    let available_decisions: Vec<String> =
        serde_json::from_str(&facts_text(&approval, "approvals", "available_decisions")?)
            .map_err(|_| bad_table("approvals", "available_decisions"))?;
    let raw_decision = workspace_decode_support::opt_text(&approval, "decision")
        .map_err(|_| bad_table("approvals", "decision"))?;
    let Some(raw_decision) = raw_decision else {
        return Ok(None);
    };
    let decision: Decision =
        serde_json::from_str(&raw_decision).map_err(|_| bad_table("approvals", "decision"))?;
    let approval_created_at = workspace_decode_support::instant(&approval, "created_at")
        .map_err(|_| bad_table("approvals", "created_at"))?;
    let approval_expires_at = workspace_decode_support::instant(&approval, "expires_at")
        .map_err(|_| bad_table("approvals", "expires_at"))?;
    let approval_decided_at = workspace_decode_support::opt_instant(&approval, "decided_at")
        .map_err(|_| bad_table("approvals", "decided_at"))?
        .ok_or_else(|| bad_table("approvals", "decided_at"))?;
    let approval_workspace = workspace_decode_support::opt_text(&approval, "workspace_id")
        .map_err(|_| bad_table("approvals", "workspace_id"))?;
    let run_workspace = workspace_decode_support::opt_text(&approval, "run_workspace_id")
        .map_err(|_| bad_table("runs", "workspace_id"))?;
    if actual_approval_id != approval_id
        || approval_run_id != hold.run_id
        || approval_workspace.as_deref() != Some(hold.workspace_id.as_str())
        || run_workspace.as_deref() != Some(hold.workspace_id.as_str())
        || approval_status != ApprovalStatus::Approved
        || !matches!(decision.kind.as_str(), "approve" | "approve_for_session")
        || !available_decisions
            .iter()
            .any(|offered| offered == &decision.kind)
        || approval_created_at > approval_decided_at
        || now < &approval_decided_at
        || approval_decided_at > approval_expires_at
    {
        return Ok(None);
    }

    let cohort = sqlx::query(
        "SELECT cohort_id,migration_version,legacy_workspace_id,root_generation,created_at,completed_at
         FROM legacy_recovery_cohorts WHERE cohort_id = ? COLLATE BINARY LIMIT 1",
    )
    .bind(&hold.cohort_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| WorkspaceStoreError::Database)?
    .ok_or_else(|| bad("cohort_id"))?;
    let cohort_migration_version = workspace_decode_support::count(&cohort, "migration_version")
        .map_err(|_| bad_table("legacy_recovery_cohorts", "migration_version"))?;
    if cohort_migration_version == 0 {
        return Err(bad_table("legacy_recovery_cohorts", "migration_version"));
    }
    let cohort_id = facts_text(&cohort, "legacy_recovery_cohorts", "cohort_id")?;
    let cohort_workspace = facts_text(&cohort, "legacy_recovery_cohorts", "legacy_workspace_id")?;
    let cohort_root = facts_text(&cohort, "legacy_recovery_cohorts", "root_generation")?;
    let created_at = workspace_decode_support::instant(&cohort, "created_at")
        .map_err(|_| bad_table("legacy_recovery_cohorts", "created_at"))?;
    let completed_at = workspace_decode_support::opt_instant(&cohort, "completed_at")
        .map_err(|_| bad_table("legacy_recovery_cohorts", "completed_at"))?;
    if cohort_id != hold.cohort_id
        || cohort_workspace != hold.workspace_id.as_str()
        || cohort_root != hold.root_generation
        || now < &created_at
        || ready_at < &created_at
        || ready_at < &run.created_at
        || ready_at < &approval_decided_at
        || completed_at.is_some()
    {
        return Ok(None);
    }

    let workspace = select_recovery_workspace_tx(tx, &hold.workspace_id).await?;
    if workspace.id != hold.workspace_id
        || workspace.kind != WorkspaceKind::LegacyCompat
        || workspace.root.root_generation() != hold.root_generation
        || workspace.state != WorkspaceState::Active
        || now < &workspace.created_at
        || ready_at < &workspace.created_at
        || now < ready_at
    {
        return Ok(None);
    }
    Ok(Some(PromotionFacts {
        run,
        approval_id: approval_id.to_owned(),
        decision,
        workspace,
        approval_created_at,
        approval_expires_at,
        approval_decided_at,
        cohort_created_at: created_at,
        cohort_migration_version,
    }))
}

#[rustfmt::skip]
fn expected_promotion_lease(attempt: &LegacyRecoveryPromotionAttempt, hold: &LegacyRecoveryHold) -> WorkspaceLeaseRow {
    WorkspaceLeaseRow { record: crate::WorkspaceLeaseRecord {
        id: attempt.lease_id.clone(), workspace_id: hold.workspace_id.clone(),
        root_generation: hold.root_generation.clone(), owner_id: hold.run_id.clone(), kind: LeaseKind::Run,
        daemon_generation: None, host_instance_id: None, acquired_at: attempt.acquired_at.clone(),
        expires_at: None, renewed_at: None, released_at: None,
    }}
}

#[rustfmt::skip]
async fn open_run_lease_exists_tx(tx: &mut Transaction<'_, Sqlite>, workspace_id: &WorkspaceId, run_id: &str) -> WorkspaceResult<bool> {
sqlx::query("SELECT 1 FROM workspace_leases WHERE kind='run' AND released_at IS NULL AND (workspace_id=? COLLATE BINARY OR owner_id=? COLLATE BINARY) LIMIT 1").bind(workspace_id.as_str()).bind(run_id).fetch_optional(&mut **tx).await.map(|row| row.is_some()).map_err(|_| WorkspaceStoreError::Database)
}

async fn admission_reread_tx(
    tx: &mut Transaction<'_, Sqlite>,
    expected_hold: &LegacyRecoveryHold,
    expected_lease: &WorkspaceLeaseRow,
) -> WorkspaceResult<LegacyRecoveryAdmission> {
    let hold = read_hold_tx(tx, &expected_hold.run_id).await?;
    let lease = promotion_lease_tx(tx, &expected_lease.record.id)
        .await?
        .ok_or(WorkspaceStoreError::CorruptRow {
            table: "workspace_leases",
            field: "lease_id",
        })?;
    if &hold != expected_hold || &lease != expected_lease {
        return Err(WorkspaceStoreError::CorruptRow {
            table: "legacy_recovery_holds",
            field: "row",
        });
    }
    Ok(LegacyRecoveryAdmission { hold, lease })
}

#[allow(dead_code)]
impl Store {
    /// Atomically promotes the exact oldest Ready hold into a legacy run lease.
    pub(crate) async fn promote_legacy_recovery(
        &self,
        attempt: LegacyRecoveryPromotionAttempt,
    ) -> WorkspaceResult<LegacyRecoveryPromotionOutcome> {
        let mut tx = self.begin_workspace_immediate().await?;

        if let Some(existing) = promotion_lease_tx(&mut tx, &attempt.lease_id).await? {
            let expected = expected_promotion_lease(&attempt, &attempt.hint);
            if existing != expected {
                return Ok(LegacyRecoveryPromotionOutcome::Busy);
            }
            let active = read_hold_tx(&mut tx, &attempt.hint.run_id).await?;
            let mut expected_active = attempt.hint.clone();
            expected_active.recovery_state = RecoveryState::Active;
            expected_active.ready_at = None;
            expected_active.active_resume_approval_id = expected_active.approval_id.clone();
            if active != expected_active
                || promotion_facts_tx(
                    &mut tx,
                    &active,
                    attempt.hint.ready_at.as_ref().ok_or(bad("ready_at"))?,
                    &attempt.acquired_at,
                )
                .await?
                .is_none()
            {
                return Ok(LegacyRecoveryPromotionOutcome::Busy);
            }
            let admission = admission_reread_tx(&mut tx, &active, &expected).await?;
            tx.commit()
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            return Ok(LegacyRecoveryPromotionOutcome::ObservedCommitted(admission));
        }

        let next = read_next_ready_tx(&mut tx).await?;
        if next.as_ref() != Some(&attempt.hint) {
            return Ok(LegacyRecoveryPromotionOutcome::StaleHint);
        }
        let run = read_run_facts(&mut tx, &attempt.hint.run_id).await?.ok_or(
            WorkspaceStoreError::CorruptRow {
                table: "legacy_recovery_holds",
                field: "run_id",
            },
        )?;
        if matches!(
            run.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        ) {
            return Ok(LegacyRecoveryPromotionOutcome::Terminal);
        }
        if attempt.acquired_at < *attempt.hint.ready_at.as_ref().ok_or(bad("ready_at"))? {
            return Ok(LegacyRecoveryPromotionOutcome::WorkspaceUnavailable);
        }
        let Some(facts) = promotion_facts_tx(
            &mut tx,
            &attempt.hint,
            attempt.hint.ready_at.as_ref().ok_or(bad("ready_at"))?,
            &attempt.acquired_at,
        )
        .await?
        else {
            return Ok(LegacyRecoveryPromotionOutcome::WorkspaceUnavailable);
        };
        if facts.run.id != attempt.hint.run_id
            || facts.approval_id != attempt.hint.approval_id.as_deref().unwrap_or_default()
        {
            return Ok(LegacyRecoveryPromotionOutcome::WorkspaceUnavailable);
        }
        if attempt.acquired_at >= facts.workspace.expires_at {
            crate::workspace_lifecycle::expire_workspace_tx(
                &mut tx,
                &attempt.hint.workspace_id,
                &attempt.acquired_at,
                &facts.workspace,
            )
            .await?;
            tx.commit()
                .await
                .map_err(|_| WorkspaceStoreError::Database)?;
            return Ok(LegacyRecoveryPromotionOutcome::WorkspaceUnavailable);
        }
        if open_run_lease_exists_tx(&mut tx, &attempt.hint.workspace_id, &attempt.hint.run_id)
            .await?
        {
            return Ok(LegacyRecoveryPromotionOutcome::Busy);
        }
        let lease = expected_promotion_lease(&attempt, &attempt.hint);
        sqlx::query(
            "INSERT INTO workspace_leases
             (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at)
             VALUES (?,?,?,?,'run',?)",
        )
        .bind(lease.record.id.as_str())
        .bind(lease.record.workspace_id.as_str())
        .bind(&lease.record.root_generation)
        .bind(&lease.record.owner_id)
        .bind(lease.record.acquired_at.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;

        let original_status = serde_json::to_value(attempt.hint.original_status)
            .map_err(|_| WorkspaceStoreError::Database)?;
        let original_status = original_status
            .as_str()
            .ok_or(WorkspaceStoreError::Database)?;
        let affected = sqlx::query(
            "UPDATE legacy_recovery_holds
             SET recovery_state='active', ready_at=NULL, active_resume_approval_id=approval_id
             WHERE run_id = ? COLLATE BINARY AND cohort_id = ? COLLATE BINARY
               AND workspace_id = ? COLLATE BINARY AND root_generation = ? COLLATE BINARY
               AND original_status = ? COLLATE BINARY AND recovery_state = 'ready'
               AND approval_id = ? COLLATE BINARY AND ready_at = ? COLLATE BINARY
               AND reason_code IS ? AND released_at IS NULL
               AND active_resume_approval_id IS NULL",
        )
        .bind(&attempt.hint.run_id)
        .bind(&attempt.hint.cohort_id)
        .bind(attempt.hint.workspace_id.as_str())
        .bind(&attempt.hint.root_generation)
        .bind(original_status)
        .bind(attempt.hint.approval_id.as_deref())
        .bind(attempt.hint.ready_at.as_ref().map(WorkspaceInstant::as_str))
        .bind(attempt.hint.reason_code.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        if affected.rows_affected() != 1 {
            return Err(WorkspaceStoreError::Database);
        }
        let mut active = attempt.hint.clone();
        active.recovery_state = RecoveryState::Active;
        active.ready_at = None;
        active.active_resume_approval_id = active.approval_id.clone();
        admission_reread_tx(&mut tx, &active, &lease).await?;
        let audit = Store::append_audit_tx(
            &mut tx,
            attempt.acquired_at.as_str(),
            "legacy_recovery",
            "legacy_recovery.active",
            &serde_json::json!({"run_id": active.run_id, "cohort_id": active.cohort_id,
                    "workspace_id": active.workspace_id.as_str(), "result_state": "active"}),
        )
        .await
        .map_err(|_| WorkspaceStoreError::Database)?;
        crate::workspace_lifecycle::verify_audit_entry_tx(&mut tx, &audit).await?;
        let post_hold = read_hold_tx(&mut tx, &active.run_id).await?;
        let post_lease = promotion_lease_tx(&mut tx, &lease.record.id).await?;
        let post_facts = promotion_facts_tx(
            &mut tx,
            &active,
            attempt.hint.ready_at.as_ref().ok_or(bad("ready_at"))?,
            &attempt.acquired_at,
        )
        .await?;
        if post_hold != active
            || post_lease.as_ref() != Some(&lease)
            || post_facts.as_ref() != Some(&facts)
        {
            return Err(WorkspaceStoreError::CorruptRow {
                table: "legacy_recovery_holds",
                field: "row",
            });
        }
        let admission = admission_reread_tx(&mut tx, &active, &lease).await?;
        tx.commit()
            .await
            .map_err(|_| WorkspaceStoreError::Database)?;
        Ok(LegacyRecoveryPromotionOutcome::Admitted(admission))
    }
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
    use sqlx::{Row, SqlitePool};
    use std::{path::Path, sync::Arc};
    use tokio::sync::Barrier;

    #[allow(clippy::unwrap_used)]
    async fn strict_facts_fixture() -> Store {
        let store = Store::open_memory().await.unwrap();
        strict_facts_seed(&store).await;
        store
    }

    #[allow(clippy::unwrap_used)]
    async fn strict_facts_file(path: &Path) -> Store {
        let store = Store::open(path).await.unwrap();
        strict_facts_seed(&store).await;
        store
    }

    #[allow(clippy::unwrap_used)]
    async fn strict_facts_seed(store: &Store) {
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

    type ReadyFacts = (LegacyRecoveryHold, RunFacts, ApprovalFacts, LeaseFacts, i64);

    #[allow(clippy::unwrap_used)]
    async fn ready_facts(tx: &mut Transaction<'_, Sqlite>) -> ReadyFacts {
        (
            read_hold_tx(tx, "run-strict").await.unwrap(),
            read_run_facts(tx, "run-strict").await.unwrap().unwrap(),
            read_approval_facts(tx, "approval-strict")
                .await
                .unwrap()
                .unwrap(),
            lease_facts(tx).await,
            sqlx::query_scalar("SELECT count(*) FROM audit_log")
                .fetch_one(&mut **tx)
                .await
                .unwrap(),
        )
    }

    #[allow(clippy::unwrap_used)]
    async fn approve_tx(tx: &mut Transaction<'_, Sqlite>) {
        sqlx::query("UPDATE approvals SET status='approved' WHERE id='approval-strict'")
            .execute(&mut **tx)
            .await
            .unwrap();
    }

    #[allow(clippy::unwrap_used)]
    async fn composed_facts(
        store: &Store,
    ) -> (String, Option<String>, String, Option<String>, i64) {
        sqlx::query_as(
            "SELECT a.status,a.decided_at,h.recovery_state,h.ready_at,
                    (SELECT count(*) FROM audit_log)
             FROM approvals a JOIN legacy_recovery_holds h ON h.run_id=a.run_id
             WHERE a.id='approval-strict'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap()
    }

    #[allow(clippy::unwrap_used)]
    async fn ready(tx: &mut Transaction<'_, Sqlite>) -> WorkspaceResult<Option<ReadyMutation>> {
        apply_ready_tx(
            tx,
            "run-strict",
            "approval-strict",
            WorkspaceInstant::parse("2026-09-20T00:00:00.000Z").unwrap(),
        )
        .await
    }

    #[allow(clippy::unwrap_used)]
    async fn ready_trigger_case(trigger: &str, name: &str, expected: WorkspaceStoreError) {
        let store = strict_facts_fixture().await;
        execute(&store, trigger).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let before = ready_facts(&mut tx).await;
        approve_tx(&mut tx).await;
        assert_eq!(ready(&mut tx).await, Err(expected));
        tx.rollback().await.unwrap();
        let mut check = store.pool().begin().await.unwrap();
        assert_eq!(ready_facts(&mut check).await, before);
        check.rollback().await.unwrap();
        assert!(store.list_audit().await.unwrap().is_empty());
        execute(&store, &format!("DROP TRIGGER {name}")).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        approve_tx(&mut tx).await;
        assert!(ready(&mut tx).await.unwrap().is_some());
        tx.commit().await.unwrap();
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
        store.verify_audit_chain().await.unwrap();
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
                status: RunStatus::Running,
                created_at: WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
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
    async fn composed_resolution_is_approval_only_without_a_hold() {
        let ts = WorkspaceInstant::parse("2026-09-20T00:00:00.123Z").unwrap();
        let store = strict_facts_fixture().await;
        let resolved = store
            .resolve_approval_with_legacy_ready(
                "approval-strict",
                ApprovalStatus::Approved,
                None,
                ts.clone(),
            )
            .await
            .unwrap();
        assert!(
            matches!(resolved, LegacyApprovalResolution::Ready(ref a) if a.status == ApprovalStatus::Approved)
        );
        let facts = composed_facts(&store).await;
        assert_eq!(facts.1.as_deref(), Some(ts.as_str()));
        assert_eq!(facts.3.as_deref(), Some(ts.as_str()));

        let store = strict_facts_fixture().await;
        execute(&store, "DELETE FROM legacy_recovery_holds").await;
        let resolved = store
            .resolve_approval_with_legacy_ready(
                "approval-strict",
                ApprovalStatus::Denied,
                None,
                ts.clone(),
            )
            .await
            .unwrap();
        assert!(
            matches!(resolved, LegacyApprovalResolution::ApprovalOnly(a) if a.status == ApprovalStatus::Denied)
        );
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn composed_resolution_fails_closed_for_active_or_terminal_recovery() {
        for sql in [
            "UPDATE legacy_recovery_holds SET recovery_state='active',active_resume_approval_id='approval-strict'",
            "UPDATE runs SET status='completed' WHERE id='run-strict'",
        ] {
            let store = strict_facts_fixture().await;
            execute(&store, sql).await;
            let before = composed_facts(&store).await;
            let result = store
                .resolve_approval_with_legacy_ready(
                    "approval-strict",
                    ApprovalStatus::Approved,
                    None,
                    WorkspaceInstant::parse("2026-09-20T00:00:00.123Z").unwrap(),
                )
                .await;
            assert!(matches!(
                result,
                Err(ResolveLegacyApprovalError::RecoveryUnavailable)
            ));
            let after = composed_facts(&store).await;
            assert_eq!(after, before);
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn composed_resolution_rolls_back_recovery_audit_failure_and_retries() {
        let store = strict_facts_fixture().await;
        execute(
            &store,
            "CREATE TRIGGER composed_audit_abort BEFORE INSERT ON audit_log
             BEGIN SELECT RAISE(ABORT, 'secret-audit'); END",
        )
        .await;
        let result = store
            .resolve_approval_with_legacy_ready(
                "approval-strict",
                ApprovalStatus::Approved,
                None,
                WorkspaceInstant::parse("2026-09-20T00:00:00.123Z").unwrap(),
            )
            .await;
        assert!(matches!(
            result,
            Err(ResolveLegacyApprovalError::Recovery(
                WorkspaceStoreError::Database
            ))
        ));
        let facts = composed_facts(&store).await;
        assert_eq!(
            facts,
            ("pending".into(), None, "awaiting_decision".into(), None, 0)
        );
        execute(&store, "DROP TRIGGER composed_audit_abort").await;
        let _result = store
            .resolve_approval_with_legacy_ready(
                "approval-strict",
                ApprovalStatus::Approved,
                None,
                WorkspaceInstant::parse("2026-09-20T00:00:00.123Z").unwrap(),
            )
            .await
            .unwrap();
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit[0].ts, "2026-09-20T00:00:00.123Z");
        store.verify_audit_chain().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn composed_resolution_classifies_hold_probe_sql_failure_as_recovery() {
        let store = strict_facts_fixture().await;
        execute(&store, "DROP TABLE legacy_recovery_holds").await;
        let error = store
            .resolve_approval_with_legacy_ready(
                "approval-strict",
                ApprovalStatus::Approved,
                None,
                WorkspaceInstant::parse("2026-09-20T00:00:00.123Z").unwrap(),
            )
            .await
            .unwrap_err();
        let display = error.to_string();
        assert!(matches!(
            error,
            ResolveLegacyApprovalError::Recovery(WorkspaceStoreError::Database)
        ));
        assert_eq!(display, "workspace database error");
        assert!(!display.contains("secret"));
        let facts: (String, Option<String>) =
            sqlx::query_as("SELECT status,decided_at FROM approvals WHERE id='approval-strict'")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(facts, ("pending".into(), None));
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ready_writer_faults_rollback_and_recover() {
        ready_trigger_case(
            "CREATE TRIGGER ready_abort BEFORE UPDATE OF recovery_state
             ON legacy_recovery_holds WHEN NEW.recovery_state='ready'
             BEGIN SELECT RAISE(ABORT, 'secret-update'); END",
            "ready_abort",
            WorkspaceStoreError::Database,
        )
        .await;
        ready_trigger_case(
            "CREATE TRIGGER ready_tamper AFTER UPDATE OF recovery_state
             ON legacy_recovery_holds WHEN NEW.recovery_state='ready'
             BEGIN UPDATE legacy_recovery_holds SET reason_code='tampered'
             WHERE run_id=NEW.run_id; END",
            "ready_tamper",
            WorkspaceStoreError::CorruptRow {
                table: "legacy_recovery_holds",
                field: "row",
            },
        )
        .await;
        ready_trigger_case(
            "CREATE TRIGGER audit_abort BEFORE INSERT ON audit_log
             BEGIN SELECT RAISE(ABORT, 'secret-audit'); END",
            "audit_abort",
            WorkspaceStoreError::Database,
        )
        .await;
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_writer_respects_caller_rollback_and_reuses_connection() {
        let store = strict_facts_fixture().await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        let before = ready_facts(&mut tx).await;
        approve_tx(&mut tx).await;
        assert!(ready(&mut tx).await.unwrap().is_some());
        let inside = ready_facts(&mut tx).await;
        assert_eq!(inside.0.recovery_state(), RecoveryState::Ready);
        assert_eq!(inside.2.status, ApprovalStatus::Approved);
        assert_eq!(inside.4, 1);
        tx.rollback().await.unwrap();
        let mut check = store.pool().begin().await.unwrap();
        assert_eq!(ready_facts(&mut check).await, before);
        check.rollback().await.unwrap();
        assert!(store.list_audit().await.unwrap().is_empty());
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        approve_tx(&mut tx).await;
        assert!(ready(&mut tx).await.unwrap().is_some());
        tx.commit().await.unwrap();
        assert_eq!(store.list_audit().await.unwrap().len(), 1);
        store.verify_audit_chain().await.unwrap();
    }

    #[allow(clippy::unwrap_used)]
    async fn ready_boundary_case(
        run_status: &str,
        approval_status: &str,
        hold_state: Option<&str>,
        expected_ready: bool,
    ) {
        let store = strict_facts_fixture().await;
        sqlx::query("UPDATE runs SET status=? WHERE id='run-strict'")
            .bind(run_status)
            .execute(store.pool())
            .await
            .unwrap();
        if let Some(state) = hold_state {
            let sql = match state {
                "ready" => {
                    "UPDATE legacy_recovery_holds SET recovery_state='ready', ready_at='2026-09-20T00:00:00.000Z', released_at=NULL, active_resume_approval_id=NULL"
                }
                "active" => {
                    "UPDATE legacy_recovery_holds SET recovery_state='active', ready_at=NULL, released_at=NULL, active_resume_approval_id='approval-strict'"
                }
                "needs_attention" => {
                    "UPDATE legacy_recovery_holds SET recovery_state='needs_attention', ready_at=NULL, reason_code='boundary_reason', released_at=NULL, active_resume_approval_id=NULL"
                }
                "released" => {
                    "UPDATE legacy_recovery_holds SET recovery_state='released', ready_at=NULL, released_at='2026-09-20T00:00:00.000Z', active_resume_approval_id=NULL"
                }
                _ => unreachable!(),
            };
            execute(&store, sql).await;
        }
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE approvals SET status=? WHERE id='approval-strict'")
            .bind(approval_status)
            .execute(&mut *tx)
            .await
            .unwrap();
        let before = ready_facts(&mut tx).await;
        let result = ready(&mut tx).await.unwrap();
        if !expected_ready {
            assert_eq!(result, None);
            assert_eq!(ready_facts(&mut tx).await, before);
        } else {
            assert!(result.is_some());
            let after = ready_facts(&mut tx).await;
            let mut expected_hold = before.0.clone();
            expected_hold.recovery_state = RecoveryState::Ready;
            expected_hold.ready_at =
                Some(WorkspaceInstant::parse("2026-09-20T00:00:00.000Z").unwrap());
            assert_eq!(after.0, expected_hold);
            assert_eq!(after.1, before.1);
            assert_eq!(after.2, before.2);
            assert_eq!(after.3, before.3);
            assert_eq!(after.4, before.4 + 1);
            let (actor, action, detail): (String, String, String) =
                sqlx::query_as("SELECT actor,action,detail FROM audit_log")
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap();
            assert_eq!(
                (actor, action),
                ("legacy_recovery".into(), "legacy_recovery.ready".into())
            );
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&detail).unwrap(),
                serde_json::json!({
                    "run_id": "run-strict", "cohort_id": "cohort-strict",
                    "workspace_id": "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
                    "approval_id": "approval-strict", "result_state": "ready",
                })
            );
        }
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_writer_covers_valid_status_and_hold_boundaries() {
        for (approval, expected) in [
            ("pending", false),
            ("approved", true),
            ("denied", true),
            ("aborted", true),
            ("timed_out", true),
        ] {
            ready_boundary_case("running", approval, None, expected).await;
        }
        for (run, expected) in [
            ("queued", true),
            ("awaiting_approval", true),
            ("completed", false),
            ("failed", false),
            ("cancelled", false),
        ] {
            ready_boundary_case(run, "approved", None, expected).await;
        }
        for state in ["ready", "active", "needs_attention", "released"] {
            ready_boundary_case("running", "approved", Some(state), false).await;
        }
    }

    type RawFacts = (i64, i64, Option<String>, Option<String>);

    #[allow(clippy::unwrap_used)]
    async fn raw_facts(store: &Store) -> RawFacts {
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM approvals),
                    (SELECT id FROM runs ORDER BY id LIMIT 1),
                    (SELECT id FROM approvals ORDER BY id LIMIT 1)",
        )
        .fetch_one(store.pool())
        .await
        .unwrap()
    }

    #[allow(clippy::unwrap_used)]
    async fn writer_error_case(
        run_id: &str,
        approval_id: &str,
        run_status: Option<&str>,
        expected: WorkspaceStoreError,
    ) {
        let store = strict_facts_fixture().await;
        if let Some(status) = run_status {
            sqlx::query("UPDATE runs SET status=? WHERE id='run-strict'")
                .bind(status)
                .execute(store.pool())
                .await
                .unwrap();
        }
        let before = raw_facts(&store).await;
        let changes_before = changes(&store).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert_eq!(
            apply_ready_tx(
                &mut tx,
                run_id,
                approval_id,
                WorkspaceInstant::parse("2026-09-20T00:00:00.000Z").unwrap(),
            )
            .await
            .unwrap_err(),
            expected
        );
        tx.rollback().await.unwrap();
        assert_eq!(raw_facts(&store).await, before);
        assert_eq!(changes(&store).await, changes_before);
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_writer_freezes_caller_identity_and_missing_hold_errors() {
        writer_error_case(
            "run-strict",
            "wrong",
            None,
            WorkspaceStoreError::InvalidValue {
                field: "approval_id",
            },
        )
        .await;
        writer_error_case(
            "run-strict",
            "APPROVAL-STRICT",
            None,
            WorkspaceStoreError::InvalidValue {
                field: "approval_id",
            },
        )
        .await;
        for run_id in ["RUN-STRICT", "missing"] {
            writer_error_case(
                run_id,
                "approval-strict",
                None,
                WorkspaceStoreError::NotFound,
            )
            .await;
        }
        writer_error_case(
            "run-strict",
            "wrong",
            Some("completed"),
            WorkspaceStoreError::InvalidValue {
                field: "approval_id",
            },
        )
        .await;
    }

    #[allow(clippy::unwrap_used)]
    async fn damaged_writer_case(
        damage: &str,
        disable_fk: bool,
        table: &'static str,
        field: &'static str,
    ) {
        let store = strict_facts_fixture().await;
        if disable_fk {
            // Adversarial corruption is applied only after the valid FK fixture exists.
            let mut conn = store.pool().acquire().await.unwrap();
            sqlx::query("PRAGMA foreign_keys=OFF")
                .execute(&mut *conn)
                .await
                .unwrap();
            sqlx::query(damage).execute(&mut *conn).await.unwrap();
        } else {
            sqlx::query(damage).execute(store.pool()).await.unwrap();
        }
        let before = raw_facts(&store).await;
        let changes_before = changes(&store).await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert_eq!(
            ready(&mut tx).await,
            Err(WorkspaceStoreError::CorruptRow { table, field })
        );
        tx.rollback().await.unwrap();
        assert_eq!(raw_facts(&store).await, before);
        assert_eq!(changes(&store).await, changes_before);
        assert!(store.list_audit().await.unwrap().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_writer_classifies_referenced_fact_damage_without_writes() {
        for (damage, fk, table, field) in [
            (
                "DELETE FROM runs WHERE id='run-strict'",
                true,
                "legacy_recovery_holds",
                "run_id",
            ),
            (
                "UPDATE runs SET status='bogus' WHERE id='run-strict'",
                false,
                "runs",
                "status",
            ),
            (
                "DELETE FROM approvals WHERE id='approval-strict'",
                true,
                "legacy_recovery_holds",
                "approval_id",
            ),
            (
                "UPDATE approvals SET status='bogus' WHERE id='approval-strict'",
                false,
                "approvals",
                "status",
            ),
            (
                "UPDATE approvals SET run_id='run-other' WHERE id='approval-strict'",
                true,
                "approvals",
                "run_id",
            ),
            (
                "UPDATE legacy_recovery_holds SET root_generation=''",
                true,
                "legacy_recovery_holds",
                "root_generation",
            ),
        ] {
            damaged_writer_case(damage, fk, table, field).await;
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ready_writer_rejects_schema_valid_non_null_ready_at_cas_source() {
        let store = strict_facts_fixture().await;
        execute(
            &store,
            "UPDATE legacy_recovery_holds SET ready_at='2026-09-20T00:00:00.000Z'",
        )
        .await;
        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        approve_tx(&mut tx).await;
        let before = ready_facts(&mut tx).await;
        let changes_before: i64 = sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(ready(&mut tx).await, Err(WorkspaceStoreError::Database));
        assert_eq!(ready_facts(&mut tx).await, before);
        let changes_after: i64 = sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(changes_after, changes_before);
        tx.rollback().await.unwrap();
        assert!(store.list_audit().await.unwrap().is_empty());
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
             ALTER TABLE runs ADD COLUMN created_at TEXT NOT NULL DEFAULT '2026-09-19T00:00:00Z';
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

    #[allow(clippy::unwrap_used)] #[rustfmt::skip]
    async fn ready_promotion_attempt(store: &Store) -> LegacyRecoveryPromotionAttempt {
        execute(store, "DELETE FROM workspace_leases; UPDATE approvals SET status='approved',decision='{\"type\":\"approve\"}',available_decisions='[\"approve\"]',decided_at='2026-09-19T00:00:00.000Z'; UPDATE legacy_recovery_holds SET recovery_state='ready',ready_at='2026-09-19T00:00:00.000Z' WHERE run_id='run-strict'").await;
        LegacyRecoveryPromotionAttempt { hint: store.get_legacy_recovery_hold("run-strict").await.unwrap(), lease_id: WorkspaceLeaseId::parse("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7").unwrap(), acquired_at: WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap() }
    }

    #[allow(clippy::unwrap_used)] #[rustfmt::skip]
    async fn expect_promotion(store: &Store, attempt: LegacyRecoveryPromotionAttempt, expected: LegacyRecoveryPromotionOutcome) { assert_eq!(store.promote_legacy_recovery(attempt).await.unwrap(), expected); }

    #[rustfmt::skip]
    async fn expect_promotion_error(store: &Store, attempt: LegacyRecoveryPromotionAttempt, expected: WorkspaceStoreError) { assert_eq!(store.promote_legacy_recovery(attempt).await, Err(expected)); }

    #[allow(clippy::unwrap_used)]
    async fn changes(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(store.pool())
            .await
            .unwrap()
    }

    type PromotionSnapshot = (
        String,
        Vec<(String, String, String, String, String)>,
        Vec<crate::AuditEntry>,
    );

    #[allow(clippy::unwrap_used)]
    async fn promotion_snapshot(store: &Store) -> PromotionSnapshot {
        let state: (String, i64, String, Option<String>, Option<String>, Option<String>, String) =
            sqlx::query_as("SELECT w.state,w.revision,h.recovery_state,h.ready_at,h.reason_code,h.active_resume_approval_id,a.available_decisions FROM workspaces w JOIN legacy_recovery_holds h ON h.workspace_id=w.id JOIN approvals a ON a.id=h.approval_id WHERE h.run_id='run-strict'")
                .fetch_one(store.pool()).await.unwrap();
        let leases = sqlx::query_as("SELECT lease_id,workspace_id,root_generation,owner_id,kind FROM workspace_leases ORDER BY lease_id")
            .fetch_all(store.pool()).await.unwrap();
        (
            format!("{state:?}"),
            leases,
            store.list_audit().await.unwrap(),
        )
    }

    #[allow(clippy::unwrap_used)]
    async fn assert_active_audit(store: &Store) {
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            (audit[0].actor.as_str(), audit[0].action.as_str()),
            ("legacy_recovery", "legacy_recovery.active")
        );
        assert_eq!(
            audit[0].detail,
            serde_json::json!({"run_id":"run-strict","cohort_id":"cohort-strict","workspace_id":"ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5","result_state":"active"})
        );
        store.verify_audit_chain().await.unwrap();
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

    #[allow(clippy::too_many_arguments, clippy::unwrap_used)]
    async fn insert_reader_hold(
        store: &Store,
        run_id: &str,
        approval_id: Option<&str>,
        state: &str,
        ready_at: Option<&str>,
        reason_code: Option<&str>,
        released_at: Option<&str>,
        active_resume_approval_id: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO runs (id,status,input,usage,created_at,workspace_id)
             VALUES (?,'running','{}','{}','2026-09-19T00:00:00.000Z',
                     'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5')",
        )
        .bind(run_id)
        .execute(store.pool())
        .await
        .unwrap();
        if let Some(approval_id) = approval_id {
            sqlx::query(
                "INSERT INTO approvals
                 (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,
                  expires_at,created_at,workspace_id)
                 VALUES (? ,?,'tool','exec','test','{}','[]','pending',
                         '2026-09-20T00:00:00.000Z','2026-09-19T00:00:00.000Z',
                         'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5')",
            )
            .bind(approval_id)
            .bind(run_id)
            .execute(store.pool())
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO legacy_recovery_holds
             (run_id,cohort_id,workspace_id,root_generation,original_status,recovery_state,
              approval_id,ready_at,reason_code,released_at,active_resume_approval_id)
             VALUES (?,'cohort-strict','ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5','g1','running',
                     ?,?,?,?,?,?)",
        )
        .bind(run_id)
        .bind(state)
        .bind(approval_id)
        .bind(ready_at)
        .bind(reason_code)
        .bind(released_at)
        .bind(active_resume_approval_id)
        .execute(store.pool())
        .await
        .unwrap();
    }
    #[tokio::test] #[allow(clippy::unwrap_used)] #[rustfmt::skip]
    async fn promotion_commits_once_and_stable_retry_observes_commit() {
let store = strict_facts_fixture().await; let attempt = ready_promotion_attempt(&store).await;
execute(&store, "UPDATE runs SET status='completed' WHERE id='run-strict'; UPDATE approvals SET status='corrupt' WHERE id='approval-strict'").await; expect_promotion(&store, attempt.clone(), LegacyRecoveryPromotionOutcome::Terminal).await; assert!(store.list_audit().await.unwrap().is_empty());
execute(&store, "UPDATE runs SET status='running' WHERE id='run-strict'; UPDATE approvals SET status='approved' WHERE id='approval-strict'").await;
assert!(matches!(store.promote_legacy_recovery(attempt.clone()).await.unwrap(), LegacyRecoveryPromotionOutcome::Admitted(_))); assert!(matches!(store.promote_legacy_recovery(attempt).await.unwrap(), LegacyRecoveryPromotionOutcome::ObservedCommitted(_)));
let audit = store.list_audit().await.unwrap(); assert_eq!(audit.len(), 1); assert_eq!(audit[0].action, "legacy_recovery.active"); store.verify_audit_chain().await.unwrap();
    }
    #[tokio::test] #[allow(clippy::unwrap_used)] #[rustfmt::skip]
async fn promotion_rejects_bad_decision_time_and_missing_workspace() {
let store = strict_facts_fixture().await; let attempt = ready_promotion_attempt(&store).await;
execute(&store, "UPDATE approvals SET available_decisions='[]' WHERE id='approval-strict'").await; let before = changes(&store).await;
expect_promotion(&store, attempt.clone(), LegacyRecoveryPromotionOutcome::WorkspaceUnavailable).await; assert_eq!(changes(&store).await, before);
execute(&store, "UPDATE approvals SET available_decisions='[\"approve\"]' WHERE id='approval-strict'").await;
for decision in [None, Some(r#"{"type":"deny"}"#), Some(r#"{"type":"abort"}"#)] {
    sqlx::query("UPDATE approvals SET decision=? WHERE id='approval-strict'").bind(decision).execute(store.pool()).await.unwrap(); expect_promotion(&store, attempt.clone(), LegacyRecoveryPromotionOutcome::WorkspaceUnavailable).await;
}
execute(&store, "UPDATE approvals SET decision='{' WHERE id='approval-strict'").await; expect_promotion_error(&store, attempt.clone(), WorkspaceStoreError::CorruptRow { table: "approvals", field: "decision" }).await;
execute(&store, "UPDATE approvals SET available_decisions='{' WHERE id='approval-strict'").await; expect_promotion_error(&store, attempt.clone(), WorkspaceStoreError::CorruptRow { table: "approvals", field: "available_decisions" }).await;
execute(&store, "UPDATE approvals SET decision='{\"type\":\"approve\"}' WHERE id='approval-strict'; UPDATE workspaces SET kind='orchestrator_scratch',expires_at='2026-09-19T00:00:00.001Z' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'").await;
let mut wrong_kind = ready_promotion_attempt(&store).await; wrong_kind.acquired_at = WorkspaceInstant::parse("2026-09-19T00:00:00.002Z").unwrap(); let before = changes(&store).await; expect_promotion(&store, wrong_kind, LegacyRecoveryPromotionOutcome::WorkspaceUnavailable).await; assert_eq!(changes(&store).await, before);
let missing = ready_promotion_attempt(&store).await; execute(&store, "PRAGMA foreign_keys=OFF; DELETE FROM workspaces WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'").await; expect_promotion_error(&store, missing, WorkspaceStoreError::CorruptRow { table: "legacy_recovery_holds", field: "workspace_id" }).await;
    }

    #[tokio::test] #[allow(clippy::unwrap_used)] #[rustfmt::skip]
    async fn promotion_rejects_each_clock_rollback_without_writes() { let updates = ["UPDATE runs SET created_at='2026-09-19T00:00:00.001Z'", "UPDATE approvals SET decided_at='2026-09-19T00:00:00.001Z'", "UPDATE legacy_recovery_cohorts SET created_at='2026-09-19T00:00:00.001Z'", "UPDATE workspaces SET created_at='2026-09-19T00:00:00.001Z'", ""];
for (index, update) in updates.into_iter().enumerate() { let store = strict_facts_fixture().await; let mut attempt = ready_promotion_attempt(&store).await;
if index == 4 { attempt.acquired_at = WorkspaceInstant::parse("2026-09-18T23:59:59.999Z").unwrap(); } else { attempt.acquired_at = WorkspaceInstant::parse("2026-09-19T00:00:00.002Z").unwrap(); execute(&store, update).await; }
let before = changes(&store).await; expect_promotion(&store, attempt, LegacyRecoveryPromotionOutcome::WorkspaceUnavailable).await; assert_eq!(changes(&store).await, before); let hold = store.get_legacy_recovery_hold("run-strict").await.unwrap(); let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log)").fetch_one(store.pool()).await.unwrap(); assert_eq!(hold.recovery_state, RecoveryState::Ready); assert_eq!(counts, (0, 0));
}}
    #[tokio::test] #[allow(clippy::unwrap_used)] #[rustfmt::skip]
    async fn promotion_rolls_back_post_audit_cohort_version_tamper() {
let store = strict_facts_fixture().await; let attempt = ready_promotion_attempt(&store).await;
execute(&store, "CREATE TRIGGER mutate_cohort_version AFTER INSERT ON audit_log WHEN NEW.action='legacy_recovery.active' BEGIN UPDATE legacy_recovery_cohorts SET migration_version=2 WHERE cohort_id='cohort-strict'; END").await;
expect_promotion_error(&store, attempt, WorkspaceStoreError::CorruptRow { table: "legacy_recovery_holds", field: "row" }).await;
let facts: (i64, String, i64, i64) = sqlx::query_as("SELECT migration_version,(SELECT recovery_state FROM legacy_recovery_holds WHERE run_id='run-strict'),(SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log) FROM legacy_recovery_cohorts WHERE cohort_id='cohort-strict'").fetch_one(store.pool()).await.unwrap(); assert_eq!(facts, (1, "ready".into(), 0, 0));
    }
    #[tokio::test] #[allow(clippy::unwrap_used)] #[rustfmt::skip]
    async fn promotion_rolls_back_missing_or_changed_audit() {
for trigger in ["DELETE FROM audit_log WHERE seq=NEW.seq", "UPDATE audit_log SET actor='tampered' WHERE seq=NEW.seq", "UPDATE audit_log SET detail=detail || ' ' WHERE seq=NEW.seq"] { let store = strict_facts_fixture().await; let attempt = ready_promotion_attempt(&store).await; execute(&store, &format!("CREATE TRIGGER tamper_audit AFTER INSERT ON audit_log BEGIN {trigger}; END")).await;
expect_promotion_error(&store, attempt, WorkspaceStoreError::CorruptRow { table: "audit_log", field: "row" }).await; let counts: (String,i64,i64) = sqlx::query_as("SELECT (SELECT recovery_state FROM legacy_recovery_holds WHERE run_id='run-strict'),(SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log)").fetch_one(store.pool()).await.unwrap(); assert_eq!(counts, ("ready".into(), 0, 0));
}}
    #[tokio::test] #[allow(clippy::unwrap_used)] #[rustfmt::skip]
async fn promotion_ttl_audit_raw_tamper_rolls_back_workspace_expiry() {
for (trigger, table) in [("UPDATE audit_log SET detail=detail || ' ' WHERE seq=NEW.seq", "audit_log"), ("UPDATE workspaces SET state='active' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'", "workspaces")] { let store = strict_facts_fixture().await; let mut attempt = ready_promotion_attempt(&store).await; attempt.acquired_at = WorkspaceInstant::parse("2026-09-19T00:00:00.002Z").unwrap();
execute(&store, &format!("UPDATE workspaces SET expires_at='2026-09-19T00:00:00.001Z' WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'; CREATE TRIGGER tamper_expiry_audit AFTER INSERT ON audit_log WHEN NEW.action='workspace.expired' BEGIN {trigger}; END")).await;
expect_promotion_error(&store, attempt, WorkspaceStoreError::CorruptRow { table, field: "row" }).await; let rows: (String,String,i64,i64) = sqlx::query_as("SELECT state,(SELECT recovery_state FROM legacy_recovery_holds WHERE run_id='run-strict'),(SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log) FROM workspaces WHERE id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'").fetch_one(store.pool()).await.unwrap(); assert_eq!(rows, ("active".into(), "ready".into(), 0, 0));
}
}

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_at_exact_expiry_commits_only_expiry_and_retry_is_read_only() {
        let store = strict_facts_fixture().await;
        let attempt = ready_promotion_attempt(&store).await;
        let mut attempt = attempt;
        attempt.acquired_at = WorkspaceInstant::parse("2026-09-20T00:00:00.000Z").unwrap();
        let clocks: (String, String) =
            sqlx::query_as("SELECT created_at,expires_at FROM workspaces WHERE id=?")
                .bind(attempt.hint.workspace_id.as_str())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(clocks.0 < clocks.1);
        assert_eq!(clocks.1, attempt.acquired_at.as_str());
        let before: (String, i64, String, Option<String>, i64, i64) = sqlx::query_as("SELECT w.state,w.revision,h.recovery_state,h.ready_at,(SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log) FROM workspaces w JOIN legacy_recovery_holds h ON h.workspace_id=w.id WHERE w.id=?")
            .bind(attempt.hint.workspace_id.as_str()).fetch_one(store.pool()).await.unwrap();
        assert_eq!(
            before,
            (
                "active".into(),
                1,
                "ready".into(),
                attempt
                    .hint
                    .ready_at
                    .as_ref()
                    .map(|v| v.as_str().to_owned()),
                0,
                0
            )
        );
        assert_eq!(
            store
                .promote_legacy_recovery(attempt.clone())
                .await
                .unwrap(),
            LegacyRecoveryPromotionOutcome::WorkspaceUnavailable
        );
        let expired: (String, i64, String, Option<String>, i64, i64) = sqlx::query_as("SELECT w.state,w.revision,h.recovery_state,h.ready_at,(SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log) FROM workspaces w JOIN legacy_recovery_holds h ON h.workspace_id=w.id WHERE w.id=?")
            .bind(attempt.hint.workspace_id.as_str()).fetch_one(store.pool()).await.unwrap();
        assert_eq!(
            expired,
            ("expired".into(), 2, "ready".into(), before.3.clone(), 0, 1)
        );
        let audit = store.list_audit().await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            (
                audit[0].seq,
                audit[0].ts.as_str(),
                audit[0].prev_hash.as_str()
            ),
            (1, attempt.acquired_at.as_str(), "genesis")
        );
        assert_eq!(audit[0].hash.len(), 64);
        assert_eq!(
            (audit[0].actor.as_str(), audit[0].action.as_str()),
            ("workspace_lifecycle", "workspace.expired")
        );
        assert_eq!(
            audit[0].detail,
            serde_json::json!({"id": attempt.hint.workspace_id.as_str(), "kind": "legacy_compat", "result_state": "expired", "reason": "ttl"})
        );
        store.verify_audit_chain().await.unwrap();
        let before_retry = changes(&store).await;
        assert_eq!(
            store.promote_legacy_recovery(attempt).await.unwrap(),
            LegacyRecoveryPromotionOutcome::WorkspaceUnavailable
        );
        assert_eq!(changes(&store).await, before_retry);
        assert_eq!(store.list_audit().await.unwrap(), audit);
        let after: (String, i64, String, Option<String>, i64) = sqlx::query_as("SELECT w.state,w.revision,h.recovery_state,h.ready_at,(SELECT count(*) FROM workspace_leases) FROM workspaces w JOIN legacy_recovery_holds h ON h.workspace_id=w.id WHERE w.id='ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5'").fetch_one(store.pool()).await.unwrap();
        assert_eq!(after, ("expired".into(), 2, "ready".into(), before.3, 0));
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_busy_for_workspace_or_run_owner_without_writes() {
        for same_workspace in [true, false] {
            let store = strict_facts_fixture().await;
            let attempt = ready_promotion_attempt(&store).await;
            let workspace = if same_workspace {
                attempt.hint.workspace_id.as_str()
            } else {
                execute(&store, "INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6','orchestrator_scratch','active','test','external','orchestrator','owner','serial','2026-09-19T00:00:00.000Z','2026-09-20T00:00:00.000Z',1,'/tmp/other','g2','unix',X'0101010101010101',X'0202020202020202')").await;
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6"
            };
            let owner = if same_workspace {
                "another-run"
            } else {
                "run-strict"
            };
            sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6',?,? ,?,'run','2026-09-19T00:00:00.000Z')")
                .bind(workspace).bind(if same_workspace { "g1" } else { "g2" }).bind(owner).execute(store.pool()).await.unwrap();
            let before = changes(&store).await;
            let snapshot = promotion_snapshot(&store).await;
            assert_eq!(
                store.promote_legacy_recovery(attempt).await.unwrap(),
                LegacyRecoveryPromotionOutcome::Busy
            );
            assert_eq!(changes(&store).await, before);
            assert_eq!(promotion_snapshot(&store).await, snapshot);
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_rejects_changed_and_non_oldest_ready_hints_without_writes() {
        let store = strict_facts_fixture().await;
        let attempt = ready_promotion_attempt(&store).await;
        execute(
            &store,
            "UPDATE legacy_recovery_holds SET reason_code='changed' WHERE run_id='run-strict'",
        )
        .await;
        let before = changes(&store).await;
        let snapshot = promotion_snapshot(&store).await;
        assert_eq!(
            store.promote_legacy_recovery(attempt).await.unwrap(),
            LegacyRecoveryPromotionOutcome::StaleHint
        );
        assert_eq!(changes(&store).await, before);
        assert_eq!(promotion_snapshot(&store).await, snapshot);

        for (run, approval, current_ready_at) in [
            ("run-aaa", "approval-aaa", None),
            (
                "run-earlier-time",
                "approval-earlier-time",
                Some("2026-09-19T00:00:00.001Z"),
            ),
        ] {
            let store = strict_facts_fixture().await;
            let mut attempt = ready_promotion_attempt(&store).await;
            if let Some(ready_at) = current_ready_at {
                execute(&store, &format!("UPDATE legacy_recovery_holds SET ready_at='{ready_at}' WHERE run_id='run-strict'")).await;
                attempt.hint = store.get_legacy_recovery_hold("run-strict").await.unwrap();
                assert_eq!(attempt.hint.ready_at.as_ref().unwrap().as_str(), ready_at);
            }
            insert_reader_hold(
                &store,
                run,
                Some(approval),
                "ready",
                Some("2026-09-19T00:00:00.000Z"),
                None,
                None,
                None,
            )
            .await;
            execute(&store, &format!("UPDATE approvals SET status='approved',decision='{{\"type\":\"approve\"}}',available_decisions='[\"approve\"]',decided_at='2026-09-19T00:00:00.000Z' WHERE id='{approval}'")).await;
            let before = changes(&store).await;
            let snapshot = promotion_snapshot(&store).await;
            assert_eq!(
                store.promote_legacy_recovery(attempt).await.unwrap(),
                LegacyRecoveryPromotionOutcome::StaleHint
            );
            assert_eq!(changes(&store).await, before);
            assert_eq!(promotion_snapshot(&store).await, snapshot);
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_ignored_cas_rolls_back_lease_and_audit_then_retries() {
        let store = strict_facts_fixture().await;
        let attempt = ready_promotion_attempt(&store).await;
        let before = promotion_snapshot(&store).await;
        execute(&store, "CREATE TRIGGER ignore_promotion BEFORE UPDATE OF recovery_state ON legacy_recovery_holds WHEN NEW.recovery_state='active' BEGIN SELECT RAISE(IGNORE); END").await;
        assert_eq!(
            store.promote_legacy_recovery(attempt.clone()).await,
            Err(WorkspaceStoreError::Database)
        );
        assert_eq!(promotion_snapshot(&store).await, before);
        execute(&store, "DROP TRIGGER ignore_promotion").await;
        assert!(matches!(
            store.promote_legacy_recovery(attempt).await.unwrap(),
            LegacyRecoveryPromotionOutcome::Admitted(_)
        ));
        assert_active_audit(&store).await;
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_post_audit_valid_mutations_roll_back_and_retry() {
        for (name, mutation) in [
            (
                "lease_owner",
                "UPDATE workspace_leases SET owner_id='other-run' WHERE lease_id='wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'",
            ),
            (
                "hold_reason",
                "UPDATE legacy_recovery_holds SET reason_code='tampered' WHERE run_id='run-strict'",
            ),
            (
                "approval_decisions",
                "UPDATE approvals SET available_decisions='[\"deny\"]' WHERE id='approval-strict'",
            ),
        ] {
            let store = strict_facts_fixture().await;
            let attempt = ready_promotion_attempt(&store).await;
            let before = promotion_snapshot(&store).await;
            execute(&store, &format!("CREATE TRIGGER tamper_{name} AFTER INSERT ON audit_log WHEN NEW.action='legacy_recovery.active' BEGIN {mutation}; END")).await;
            assert_eq!(
                store.promote_legacy_recovery(attempt.clone()).await,
                Err(WorkspaceStoreError::CorruptRow {
                    table: "legacy_recovery_holds",
                    field: "row"
                })
            );
            assert_eq!(promotion_snapshot(&store).await, before);
            execute(&store, &format!("DROP TRIGGER tamper_{name}")).await;
            assert!(matches!(
                store.promote_legacy_recovery(attempt).await.unwrap(),
                LegacyRecoveryPromotionOutcome::Admitted(_)
            ));
            assert_active_audit(&store).await;
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_same_hint_wal_race_admits_once_and_reopens_durably() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promotion-race.db");
        let seed = strict_facts_file(&path).await;
        let attempt = ready_promotion_attempt(&seed).await;
        drop(seed);
        let left = Store::open(&path).await.unwrap();
        let right = Store::open(&path).await.unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let a = Arc::clone(&barrier);
        let b = Arc::clone(&barrier);
        let (l, r) = tokio::join!(
            async {
                a.wait().await;
                left.promote_legacy_recovery(attempt.clone()).await.unwrap()
            },
            async {
                b.wait().await;
                right
                    .promote_legacy_recovery(attempt.clone())
                    .await
                    .unwrap()
            },
        );
        assert_eq!(
            usize::from(matches!(l, LegacyRecoveryPromotionOutcome::Admitted(_)))
                + usize::from(matches!(r, LegacyRecoveryPromotionOutcome::Admitted(_))),
            1
        );
        assert_eq!(
            usize::from(matches!(
                l,
                LegacyRecoveryPromotionOutcome::ObservedCommitted(_)
            )) + usize::from(matches!(
                r,
                LegacyRecoveryPromotionOutcome::ObservedCommitted(_)
            )),
            1
        );
        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(
            reopened
                .get_legacy_recovery_hold("run-strict")
                .await
                .unwrap()
                .recovery_state,
            RecoveryState::Active
        );
        let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log WHERE action='legacy_recovery.active')").fetch_one(reopened.pool()).await.unwrap();
        assert_eq!(counts, (1, 1));
        let lease: (String, String, String, String, String) = sqlx::query_as(
            "SELECT lease_id,workspace_id,root_generation,owner_id,kind FROM workspace_leases",
        )
        .fetch_one(reopened.pool())
        .await
        .unwrap();
        assert_eq!(
            lease,
            (
                "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7".into(),
                "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5".into(),
                "g1".into(),
                "run-strict".into(),
                "run".into()
            )
        );
        let audit = reopened.list_audit().await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!((audit[0].seq, audit[0].prev_hash.as_str()), (1, "genesis"));
        assert_eq!(audit[0].hash.len(), 64);
        assert_eq!(
            (audit[0].actor.as_str(), audit[0].action.as_str()),
            ("legacy_recovery", "legacy_recovery.active")
        );
        assert_eq!(
            audit[0].detail,
            serde_json::json!({"run_id":"run-strict","cohort_id":"cohort-strict","workspace_id":"ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5","result_state":"active"})
        );
        reopened.verify_audit_chain().await.unwrap();
        assert!(matches!(
            reopened.promote_legacy_recovery(attempt).await.unwrap(),
            LegacyRecoveryPromotionOutcome::ObservedCommitted(_)
        ));
        assert_eq!(reopened.list_audit().await.unwrap(), audit);
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn promotion_rejected_commit_reopens_ready_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promotion-commit-reject.db");
        let store = strict_facts_file(&path).await;
        let attempt = ready_promotion_attempt(&store).await;
        let mut pinned = Vec::new();
        for _ in 0..4 {
            pinned.push(store.pool().acquire().await.unwrap());
        }
        let mut connection = store.pool().acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_commit_hook(|| false);
        drop(connection);
        assert_eq!(
            store.promote_legacy_recovery(attempt.clone()).await,
            Err(WorkspaceStoreError::Database)
        );
        drop(pinned);
        store.pool().close().await;
        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(
            reopened
                .get_legacy_recovery_hold("run-strict")
                .await
                .unwrap()
                .recovery_state,
            RecoveryState::Ready
        );
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM workspace_leases),(SELECT count(*) FROM audit_log)",
        )
        .fetch_one(reopened.pool())
        .await
        .unwrap();
        assert_eq!(counts, (0, 0));
        assert!(matches!(
            reopened.promote_legacy_recovery(attempt).await.unwrap(),
            LegacyRecoveryPromotionOutcome::Admitted(_)
        ));
        assert_active_audit(&reopened).await;
    }
    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_returns_none_for_empty_and_non_ready_states() {
        let store = strict_facts_fixture().await;
        execute(&store, "DELETE FROM legacy_recovery_holds").await;
        insert_reader_hold(
            &store,
            "run-awaiting",
            Some("approval-awaiting"),
            "awaiting_decision",
            None,
            None,
            None,
            None,
        )
        .await;
        insert_reader_hold(
            &store,
            "run-active",
            Some("approval-active"),
            "active",
            None,
            None,
            None,
            Some("approval-active"),
        )
        .await;
        insert_reader_hold(
            &store,
            "run-attention",
            None,
            "needs_attention",
            None,
            Some("repair_needed"),
            None,
            None,
        )
        .await;
        insert_reader_hold(
            &store,
            "run-released",
            None,
            "released",
            None,
            None,
            Some("2026-09-20T00:00:00.000Z"),
            None,
        )
        .await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(read_next_ready_tx(&mut tx).await.unwrap(), None);
        tx.rollback().await.unwrap();
        execute(&store, "DELETE FROM legacy_recovery_holds").await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(read_next_ready_tx(&mut tx).await.unwrap(), None);
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_orders_timestamp_then_binary_run_id() {
        let store = strict_facts_fixture().await;
        execute(&store, "DELETE FROM legacy_recovery_holds").await;
        insert_reader_hold(
            &store,
            "run-z",
            Some("approval-z"),
            "ready",
            Some("2026-09-20T00:00:00.124Z"),
            None,
            None,
            None,
        )
        .await;
        insert_reader_hold(
            &store,
            "run-a",
            Some("approval-a"),
            "ready",
            Some("2026-09-20T00:00:00.123Z"),
            None,
            None,
            None,
        )
        .await;
        insert_reader_hold(
            &store,
            "run-A",
            Some("approval-A"),
            "ready",
            Some("2026-09-20T00:00:00.123Z"),
            None,
            None,
            None,
        )
        .await;
        let mut tx = store.pool().begin().await.unwrap();
        let hold = read_next_ready_tx(&mut tx).await.unwrap().unwrap();
        assert_eq!(hold.run_id(), "run-A");
        assert_eq!(
            hold.ready_at().unwrap().as_str(),
            "2026-09-20T00:00:00.123Z"
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_uses_ready_index_without_temp_sort() {
        let store = strict_facts_fixture().await;
        let plan = sqlx::query(
            "EXPLAIN QUERY PLAN
             SELECT run_id,cohort_id,workspace_id,root_generation,original_status,
                    recovery_state,approval_id,ready_at,reason_code,released_at,
                    active_resume_approval_id
             FROM legacy_recovery_holds
             WHERE recovery_state = 'ready'
               AND recovery_state COLLATE BINARY = 'ready' COLLATE BINARY
             ORDER BY ready_at COLLATE BINARY ASC, run_id COLLATE BINARY ASC LIMIT 1",
        )
        .fetch_all(store.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String, _>("detail").unwrap())
        .collect::<Vec<_>>();
        assert!(
            plan.iter()
                .any(|detail| detail.contains("USING INDEX idx_legacy_recovery_ready")),
            "unexpected query plan: {plan:?}"
        );
        assert!(
            plan.iter().all(|detail| !detail.contains("TEMP B-TREE")),
            "unexpected temporary sort: {plan:?}"
        );
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_fails_closed_on_corrupt_first_candidate() {
        let store = strict_facts_fixture().await;
        execute(&store, "DELETE FROM legacy_recovery_holds").await;
        insert_reader_hold(
            &store,
            "run-first",
            Some("approval-first"),
            "ready",
            Some("2026-09-20T00:00:00.123Z"),
            None,
            None,
            None,
        )
        .await;
        insert_reader_hold(
            &store,
            "run-second",
            Some("approval-second"),
            "ready",
            Some("2026-09-20T00:00:00.124Z"),
            None,
            None,
            None,
        )
        .await;
        execute(
            &store,
            "PRAGMA ignore_check_constraints=ON;
             UPDATE legacy_recovery_holds SET original_status='corrupt' WHERE run_id='run-first';
             PRAGMA ignore_check_constraints=OFF",
        )
        .await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(
            read_next_ready_tx(&mut tx).await,
            Err(WorkspaceStoreError::CorruptRow {
                table: "legacy_recovery_holds",
                field: "original_status",
            })
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_maps_sql_errors_to_database() {
        let store = strict_facts_fixture().await;
        execute(&store, "DROP TABLE legacy_recovery_holds").await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(
            read_next_ready_tx(&mut tx).await,
            Err(WorkspaceStoreError::Database)
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_is_read_only_even_with_write_blocking_triggers() {
        let store = strict_facts_fixture().await;
        execute(
            &store,
            "UPDATE legacy_recovery_holds SET recovery_state='ready',
                    ready_at='2026-09-20T00:00:00.123Z'",
        )
        .await;
        let before: i64 = changes(&store).await;
        execute(
            &store,
            "CREATE TRIGGER reader_block_insert BEFORE INSERT ON legacy_recovery_holds
             BEGIN SELECT RAISE(ABORT, 'reader is read only'); END;
             CREATE TRIGGER reader_block_update BEFORE UPDATE ON legacy_recovery_holds
             BEGIN SELECT RAISE(ABORT, 'reader is read only'); END;
             CREATE TRIGGER reader_block_delete BEFORE DELETE ON legacy_recovery_holds
             BEGIN SELECT RAISE(ABORT, 'reader is read only'); END",
        )
        .await;
        let mut tx = store.pool().begin().await.unwrap();
        assert_eq!(
            read_next_ready_tx(&mut tx).await.unwrap().unwrap().run_id(),
            "run-strict"
        );
        tx.rollback().await.unwrap();
        assert_eq!(changes(&store).await, before);
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_next_ready_tx_leaves_transaction_ownership_to_caller() {
        let store = strict_facts_fixture().await;
        execute(
            &store,
            "UPDATE legacy_recovery_holds SET recovery_state='ready',
                    ready_at='2026-09-20T00:00:00.123Z'",
        )
        .await;
        let mut tx = store.pool().begin().await.unwrap();
        assert!(read_next_ready_tx(&mut tx).await.unwrap().is_some());
        sqlx::query(
            "UPDATE legacy_recovery_holds SET reason_code='caller_only'
             WHERE run_id='run-strict'",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        let reason: Option<String> = sqlx::query_scalar(
            "SELECT reason_code FROM legacy_recovery_holds WHERE run_id='run-strict'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(reason.as_deref(), Some("legacy_reason"));
    }
}
