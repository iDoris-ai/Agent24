use crate::{WorkspaceInstant, WorkspaceResult, WorkspaceStoreError, workspace_decode_support};
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

#[allow(dead_code)] // Wired by the strict hold getter slice that follows this codec.
fn bad(field: &'static str) -> WorkspaceStoreError {
    workspace_decode_support::bad_table("legacy_recovery_holds", field)
}

#[allow(dead_code)] // Wired by the strict hold getter slice that follows this codec.
fn check<T>(result: WorkspaceResult<T>, field: &'static str) -> WorkspaceResult<T> {
    result.map_err(|_| bad(field))
}

#[allow(dead_code)] // Wired by the strict hold getter slice that follows this codec.
fn opt_nonempty(row: &SqliteRow, field: &'static str) -> WorkspaceResult<Option<String>> {
    let value = workspace_decode_support::opt_text(row, field)?;
    if value.as_deref().is_some_and(|v| v.is_empty()) {
        return Err(workspace_decode_support::bad(field));
    }
    Ok(value)
}

impl LegacyRecoveryHold {
    #[allow(dead_code)] // Wired by the strict hold getter slice that follows this codec.
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

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
