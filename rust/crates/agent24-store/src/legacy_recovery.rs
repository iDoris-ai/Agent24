use crate::{WorkspaceInstant, WorkspaceResult, WorkspaceStoreError};
use agent24_protocol::{RunStatus, WorkspaceId};

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

impl LegacyRecoveryHold {
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
}
