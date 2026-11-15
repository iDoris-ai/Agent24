use agent24_protocol::Usage;
use agent24_store::{RunAdmissionDenial, StoreError, WorkspaceStoreError};

use crate::SkillAdmissionError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceStoreError),
    #[error("workspace run admission denied: {0:?}")]
    WorkspaceAdmission(RunAdmissionDenial),
    #[error(transparent)]
    WorkspaceService(#[from] agent24_workspace::WorkspaceError),
    #[error("workspace authority service unavailable")]
    WorkspaceServiceUnavailable,
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// A run that names a skill was refused before any row was written.
    #[error(transparent)]
    Skill(#[from] SkillAdmissionError),
}

pub(crate) fn zero_usage() -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cost_usd: 0.0,
    }
}

#[cfg(test)]
pub(crate) fn workspace_timestamp(raw: String) -> String {
    if raw.len() == 20 {
        raw.strip_suffix('Z')
            .map(|prefix| format!("{prefix}.000Z"))
            .unwrap_or(raw)
    } else {
        raw
    }
}

pub(crate) fn workspace_now_at(time: std::time::SystemTime) -> Result<String, AgentError> {
    agent24_core::util::iso8601_millis_at(time).map_err(|_| {
        AgentError::Workspace(WorkspaceStoreError::InvalidValue { field: "timestamp" })
    })
}

pub(crate) fn workspace_now() -> Result<String, AgentError> {
    workspace_now_at(std::time::SystemTime::now())
}

pub(crate) fn restore_authority_error_must_propagate(error: &AgentError) -> bool {
    matches!(
        error,
        AgentError::Workspace(WorkspaceStoreError::Database)
            | AgentError::WorkspaceService(
                agent24_workspace::WorkspaceError::InfrastructureUnavailable { .. }
            )
    )
}

pub(crate) fn add_usage(mut total: Usage, delta: &Usage) -> Usage {
    total.prompt_tokens = total.prompt_tokens.saturating_add(delta.prompt_tokens);
    total.completion_tokens = total
        .completion_tokens
        .saturating_add(delta.completion_tokens);
    total.total_tokens = total.total_tokens.saturating_add(delta.total_tokens);
    total.cost_usd += delta.cost_usd;
    total
}
