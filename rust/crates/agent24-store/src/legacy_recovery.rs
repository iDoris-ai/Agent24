use crate::{
    Store, WorkspaceInstant, WorkspaceResult, WorkspaceStoreError, workspace_decode_support,
};
use agent24_protocol::{RunStatus, WorkspaceId};
use sqlx::sqlite::SqliteRow;

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
        let row = sqlx::query(
            "SELECT run_id,cohort_id,workspace_id,root_generation,original_status,
             recovery_state,approval_id,ready_at,reason_code,released_at,active_resume_approval_id
             FROM legacy_recovery_holds WHERE run_id = ? COLLATE BINARY LIMIT 1",
        )
        .bind(run_id)
        .fetch_optional(self.pool())
        .await
        .map_err(|_| WorkspaceStoreError::Database)?
        .ok_or(WorkspaceStoreError::NotFound)?;
        LegacyRecoveryHold::decode(row)
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

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
